//! Index one transaction by hash.
//!
//! The block feed is a free public API and drops out under rate limits. The
//! keeper and the frontend therefore report the hash of every transaction
//! they send; the indexer reads its outcome over JSON-RPC and stores the neap
//! events found in receipts the contract executed. Nothing in the request is
//! trusted except as a pointer: the events come from the chain. Receipts are
//! de-duplicated against the block feed.

use crate::chain::Chain;
use crate::{db, events};
use anyhow::{anyhow, Result};
use chrono::{TimeZone, Utc};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::broadcast;

pub async fn ingest_tx(
    chain: &Chain,
    pool: &PgPool,
    tx: &broadcast::Sender<String>,
    hash: &str,
    signer: &str,
) -> Result<usize> {
    let params = json!({ "tx_hash": hash, "sender_account_id": signer, "wait_until": "EXECUTED" });
    let mut outcome = None;
    let mut last = anyhow!("transaction not executed yet");
    let mut unknown = 0;
    for _ in 0..8 {
        match chain.rpc("tx", &params).await {
            Ok(v) => {
                let st = v["final_execution_status"].as_str().unwrap_or("");
                if matches!(st, "EXECUTED" | "EXECUTED_OPTIMISTIC" | "FINAL") {
                    outcome = Some(v);
                    break;
                }
            }
            Err(e) => {
                // A hash no RPC knows is either a few seconds early or made up:
                // give it three tries, not eight, so junk reports stay cheap.
                if e.to_string().contains("UNKNOWN_TRANSACTION") {
                    unknown += 1;
                }
                last = e;
                if unknown >= 3 {
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
    }
    let v = outcome.ok_or(last)?;
    let mut count = 0;
    for ro in v["receipts_outcome"].as_array().into_iter().flatten() {
        let o = &ro["outcome"];
        if o["executor_id"].as_str() != Some(chain.contract_id()) {
            continue;
        }
        let ok = o["status"].get("SuccessValue").is_some() || o["status"].get("SuccessReceiptId").is_some();
        if !ok {
            continue;
        }
        let logs: Vec<String> = o["logs"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        let evs = events::parse_logs(&logs);
        let rid = ro["id"].as_str().unwrap_or("");
        if evs.is_empty() || rid.is_empty() {
            continue;
        }
        // Read the block before touching the database: if this fails the
        // receipt stays unmarked and a later report or the block feed retries it.
        let b = chain.rpc("block", &json!({ "block_id": ro["block_hash"] })).await?;
        let height = b["header"]["height"].as_u64().ok_or_else(|| anyhow!("block without height"))?;
        let ts_ns = b["header"]["timestamp"].as_u64().unwrap_or(0);
        let ts = Utc
            .timestamp_opt((ts_ns / 1_000_000_000) as i64, (ts_ns % 1_000_000_000) as u32)
            .single()
            .unwrap_or_else(Utc::now);
        let refs: Vec<&events::NeapEvent> = evs.iter().collect();
        if !db::index_receipt(pool, rid, &refs, height, ts).await? {
            continue;
        }
        for ev in evs {
            let msg = json!({ "event": ev.event, "data": ev.data, "block": height, "ts": ts, "receipt_id": rid });
            let _ = tx.send(msg.to_string());
            count += 1;
        }
    }
    Ok(count)
}
