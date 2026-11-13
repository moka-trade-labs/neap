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
    let resp = client.get(url).send().await?;
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

/// Run the block fetcher loop. Sends events to the broadcast channel for WS.
pub async fn run(config: Config, pool: PgPool, tx: broadcast::Sender<String>) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let base = config.neardata_url.trim_end_matches('/').to_string();
    let interval = std::time::Duration::from_millis(config.poll_interval_ms);

    let mut cursor = db::get_last_block(&pool).await?;
    if cursor == 0 && config.start_block > 0 {
        cursor = config.start_block - 1;
    }
    if cursor == 0 {
        loop {
            match latest_block(&client, &base).await {
                Ok(h) => {
                    cursor = h;
                    break;
                }
                Err(e) => {
                    warn!(err = %e, "neardata unreachable, retrying");
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
            }
        }
        info!(block = cursor, "starting from latest block");
    } else {
        info!(block = cursor, "resuming from saved cursor");
    }

    loop {
        let head = match latest_block(&client, &base).await {
            Ok(h) => h,
            Err(e) => {
                warn!(err = %e, "failed to fetch latest block");
                tokio::time::sleep(interval).await;
                continue;
            }
        };
        if head.saturating_sub(cursor) > 50 {
            info!(cursor, head, lag = head - cursor, "catching up");
        }

        while cursor < head {
            let h = cursor + 1;
            match fetch_block(&client, &base, &config.contract_id, h).await {
                Ok(Some(be)) => {
                    for (rid, ev) in &be.events {
                        if let Err(e) = db::process_event(&pool, ev, h, be.ts).await {
                            error!(block = h, event = %ev.event, err = %e, "db write failed");
                        }
                        let msg = serde_json::json!({
                            "event": ev.event,
                            "data": ev.data,
                            "block": h,
                            "ts": be.ts,
                            "receipt_id": rid,
                        });
                        let _ = tx.send(msg.to_string());
                    }
                    cursor = h;
                    if !be.events.is_empty() {
                        db::set_last_block(&pool, cursor).await?;
                    }
                }
                Ok(None) => cursor = h,
                Err(e) => {
                    warn!(block = h, err = %e, "block fetch failed, retrying");
                    tokio::time::sleep(interval).await;
                    break;
                }
            }
        }
        db::set_last_block(&pool, cursor).await?;
        tokio::time::sleep(interval).await;
    }
}
