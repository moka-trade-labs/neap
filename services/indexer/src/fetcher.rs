//! Block fetcher: stream finalized blocks from neardata.xyz (one HTTP request
//! per block, receipt outcomes included) and extract neap events.
//!
//! Catches every receipt executed by the contract, including ones that arrive
//! through `ft_transfer_call` on the USDC contract (deposits, market creation)
//! and cross-contract callbacks. Logs from failed receipts are skipped.

use crate::config::Config;
use crate::db;
use crate::events;
use anyhow::{anyhow, Result};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value> {
    let mut req = client.get(url);
    if let Some(k) = crate::config::fastnear_key() {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(anyhow!("{url}: HTTP {status}"));
    }
    Ok(resp.json().await?)
}

/// Latest finalized block height.
async fn latest_block(client: &reqwest::Client, base: &str) -> Result<u64> {
    let b = get_json(client, &format!("{base}/v0/last_block/final")).await?;
    b["block"]["header"]["height"]
        .as_u64()
        .ok_or_else(|| anyhow!("no block height"))
}

pub struct BlockEvents {
    pub ts: DateTime<Utc>,
    pub events: Vec<(String, events::NeapEvent)>, // (receipt id, event)
}

/// Fetch one block. `Ok(None)` means the height was skipped (no block).
async fn fetch_block(
    client: &reqwest::Client,
    base: &str,
    contract_id: &str,
    height: u64,
) -> Result<Option<BlockEvents>> {
    let b = get_json(client, &format!("{base}/v0/block/{height}")).await?;
    if b.is_null() {
        return Ok(None);
    }
    let ts_ns = b["block"]["header"]["timestamp"].as_u64().unwrap_or(0);
    let ts = Utc
        .timestamp_opt((ts_ns / 1_000_000_000) as i64, (ts_ns % 1_000_000_000) as u32)
        .single()
        .unwrap_or_else(Utc::now);
    let mut out = Vec::new();
    for shard in b["shards"].as_array().into_iter().flatten() {
        for reo in shard["receipt_execution_outcomes"].as_array().into_iter().flatten() {
            let outcome = &reo["execution_outcome"]["outcome"];
            if outcome["executor_id"].as_str() != Some(contract_id) {
                continue;
            }
            let status = &outcome["status"];
            let ok = status.get("SuccessValue").is_some() || status.get("SuccessReceiptId").is_some();
            if !ok {
                continue;
            }
            let rid = reo["execution_outcome"]["id"].as_str().unwrap_or("").to_string();
            let logs: Vec<String> = outcome["logs"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default();
            for ev in events::parse_logs(&logs) {
                out.push((rid.clone(), ev));
            }
        }
    }
    Ok(Some(BlockEvents { ts, events: out }))
}

const MIN_BLOCK_PERIOD: std::time::Duration = std::time::Duration::from_millis(350);

/// Where to resume. Returns the cursor (last block already handled) and, when
/// a gap is skipped, the inclusive block range that was given up on.
///
/// - Saved cursor close to head: resume from it.
/// - Saved cursor more than `max_catchup` behind: jump to head, report the gap.
/// - Empty database with `START_BLOCK`: start there, however far back it is.
/// - Empty database otherwise: start at head.
fn start_cursor(saved: u64, start_block: u64, head: u64, max_catchup: u64) -> (u64, Option<(u64, u64)>) {
    if saved == 0 {
        return if start_block > 0 { (start_block - 1, None) } else { (head, None) };
    }
    if head.saturating_sub(saved) > max_catchup {
        return (head, Some((saved + 1, head)));
    }
    (saved, None)
}

/// Run the block fetcher loop. Sends events to the broadcast channel for WS.
pub async fn run(config: Config, pool: PgPool, tx: broadcast::Sender<String>) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let base = config.neardata_url.trim_end_matches('/').to_string();
    let interval = std::time::Duration::from_millis(config.poll_interval_ms);

    // A different contract than last run: the stored rows belong to the old one.
    if db::get_state(&pool, "contract").await?.as_deref() != Some(config.contract_id.as_str()) {
        db::reset_for_contract(&pool, &config.contract_id).await?;
        info!(contract = %config.contract_id, "contract changed, indexer tables reset");
    }

    let head = loop {
        match latest_block(&client, &base).await {
            Ok(h) => break h,
            Err(e) => {
                warn!(err = %e, "neardata unreachable, retrying");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        }
    };
    let saved = db::get_last_block(&pool).await?;
    let (mut cursor, skipped) = start_cursor(saved, config.start_block, head, config.max_catchup_blocks);
    if let Some((from, to)) = skipped {
        // Display-only data: skipping a long gap beats hammering the free block API.
        warn!(from, to, "too far behind, jumping to head; range recorded in skipped_ranges");
        db::record_skipped(&pool, from, to).await?;
    } else {
        info!(block = cursor, head, "block fetcher starting");
    }

    // neardata long-polls for blocks that are not produced yet, so one request
    // per block follows the chain head without polling `last_block`.
    let mut since_save = 0u32;
    loop {
        let h = cursor + 1;
        let started = std::time::Instant::now();
        match fetch_block(&client, &base, &config.contract_id, h).await {
            Ok(Some(be)) => {
                // Group by receipt: each receipt is written in one transaction.
                let mut order: Vec<&str> = Vec::new();
                let mut by_receipt: std::collections::HashMap<&str, Vec<&events::NeapEvent>> = Default::default();
                for (rid, ev) in &be.events {
                    if !by_receipt.contains_key(rid.as_str()) {
                        order.push(rid.as_str());
                    }
                    by_receipt.entry(rid.as_str()).or_default().push(ev);
                }
                for rid in order {
                    let evs = &by_receipt[rid];
                    // Retry transient database errors; an unwritten receipt stays
                    // unmarked, so `POST /tx` can still index it later.
                    let mut written = false;
                    for attempt in 1..=3u32 {
                        match db::index_receipt(&pool, rid, evs, h, be.ts).await {
                            Ok(fresh) => {
                                written = fresh;
                                break;
                            }
                            Err(e) => {
                                error!(block = h, receipt = rid, attempt, err = %e, "receipt write failed");
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            }
                        }
                    }
                    if !written {
                        continue;
                    }
                    for ev in evs {
                        let msg = serde_json::json!({
                            "event": ev.event,
                            "data": ev.data,
                            "block": h,
                            "ts": be.ts,
                            "receipt_id": rid,
                        });
                        let _ = tx.send(msg.to_string());
                    }
                }
                cursor = h;
                since_save += 1;
                if !be.events.is_empty() || since_save >= 20 {
                    db::set_last_block(&pool, cursor).await?;
                    since_save = 0;
                }
                // Stay under the free tier (~180 req/min) while catching up, without
                // adding delay at the head, where the long-poll already paces us.
                let spent = started.elapsed();
                if spent < MIN_BLOCK_PERIOD {
                    tokio::time::sleep(MIN_BLOCK_PERIOD - spent).await;
                }
            }
            Ok(None) => cursor = h,
            Err(e) => {
                warn!(block = h, err = %e, "block fetch failed, retrying");
                tokio::time::sleep(interval.max(std::time::Duration::from_secs(6))).await;
                // If the block API keeps refusing us, don't build a backlog we can never clear.
                if let Ok(head) = latest_block(&client, &base).await {
                    if head.saturating_sub(cursor) > config.max_catchup_blocks {
                        warn!(cursor, head, "too far behind, jumping to head; range recorded in skipped_ranges");
                        if let Err(e) = db::record_skipped(&pool, cursor + 1, head).await {
                            error!(err = %e, "could not record skipped range");
                        }
                        cursor = head;
                        db::set_last_block(&pool, cursor).await?;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::start_cursor;

    #[test]
    fn resumes_from_a_recent_cursor() {
        assert_eq!(start_cursor(1_000, 0, 1_100, 150), (1_000, None));
    }

    #[test]
    fn records_the_gap_when_jumping_to_head() {
        assert_eq!(start_cursor(1_000, 0, 2_000, 150), (2_000, Some((1_001, 2_000))));
    }

    #[test]
    fn start_block_is_honoured_on_an_empty_database() {
        // Previously this jumped to head, so START_BLOCK never took effect.
        assert_eq!(start_cursor(0, 500, 2_000, 150), (499, None));
    }

    #[test]
    fn empty_database_starts_at_head() {
        assert_eq!(start_cursor(0, 0, 2_000, 150), (2_000, None));
    }
}
