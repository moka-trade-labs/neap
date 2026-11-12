//! Block fetcher: poll NEAR RPC for new blocks and extract neap events.

use crate::config::Config;
use crate::db;
use crate::events;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// Fetch the latest finalized block height.
async fn latest_block(client: &reqwest::Client, rpc_url: &str) -> Result<u64> {
    let resp: Value = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0", "id": "idx",
            "method": "block",
            "params": {"finality": "final"}
        }))
        .send()
        .await?
        .json()
        .await?;
    resp["result"]["header"]["height"]
        .as_u64()
        .ok_or_else(|| anyhow!("no block height"))
}

/// Fetch a single block by height and extract receipt logs for our contract.
async fn fetch_block_events(
    client: &reqwest::Client,
    rpc_url: &str,
    contract_id: &str,
    height: u64,
) -> Result<Vec<events::NeapEvent>> {
    // Fetch the block's chunk details.
    let resp: Value = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0", "id": "idx",
            "method": "EXPERIMENTAL_changes",
            "params": {
                "block_id": height,
                "changes_type": "all_access_key_changes",
            }
        }))
        .send()
        .await?
        .json()
        .await?;
    // The cheapest way: use EXPERIMENTAL_receipts or tx_status.
    // For MVP, we use the simpler approach: query the block, then each chunk,
    // then filter receipts. But the free RPC doesn't expose chunk receipts well.
    //
    // Alternative: use EXPERIMENTAL_receipt with known tx hashes from the keeper.
    // For the hackathon, poll contract views instead.
    //
    // Simplest approach: use `EXPERIMENTAL_changes` to detect state changes,
    // or just poll the contract's event log via a view call.
    //
    // PRAGMATIC MVP: use the block's execution outcomes for our account.
    let resp: Value = client
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0", "id": "idx",
            "method": "block",
            "params": {"block_id": height}
        }))
        .send()
        .await?
        .json()
        .await?;

    let chunks = resp["result"]["chunks"]
        .as_array()
        .cloned()
        .unwrap_or_default();

    let mut all_events = Vec::new();
    for chunk_ref in &chunks {
        let chunk_hash = chunk_ref["chunk_hash"].as_str().unwrap_or("");
        if chunk_hash.is_empty() {
            continue;
        }
        let chunk: Value = client
            .post(rpc_url)
            .json(&json!({
                "jsonrpc": "2.0", "id": "idx",
                "method": "chunk",
                "params": {"chunk_id": chunk_hash}
            }))
            .send()
            .await?
            .json()
            .await?;

        // Check transactions that target our contract.
        let txs = chunk["result"]["transactions"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for tx in &txs {
            let receiver = tx["receiver_id"].as_str().unwrap_or("");
            if receiver != contract_id {
                continue;
            }
            let tx_hash = tx["hash"].as_str().unwrap_or("");
            if tx_hash.is_empty() {
                continue;
            }
            // Fetch the full execution outcome to get logs.
            let outcome: Value = client
                .post(rpc_url)
                .json(&json!({
                    "jsonrpc": "2.0", "id": "idx",
                    "method": "EXPERIMENTAL_tx_status",
                    "params": {
                        "tx_hash": tx_hash,
                        "sender_account_id": tx["signer_id"].as_str().unwrap_or(""),
                        "wait_until": "EXECUTED"
                    }
                }))
                .send()
                .await?
                .json()
                .await?;

            // Collect logs from all receipts.
            let receipts = outcome["result"]["receipts_outcome"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for receipt in &receipts {
                let executor = receipt["outcome"]["executor_id"].as_str().unwrap_or("");
                if executor != contract_id {
                    continue;
                }
                let logs: Vec<String> = receipt["outcome"]["logs"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_default();
                all_events.extend(events::parse_logs(&logs));
            }
        }
    }
    Ok(all_events)
}

/// Run the block fetcher loop. Sends events to the broadcast channel for WS.
pub async fn run(
    config: Config,
    pool: PgPool,
    tx: broadcast::Sender<String>,
) -> Result<()> {
    let client = reqwest::Client::new();
    let mut cursor = db::get_last_block(&pool).await?;
    if cursor == 0 && config.start_block > 0 {
        cursor = config.start_block;
    }
    if cursor == 0 {
        cursor = latest_block(&client, &config.rpc_url)
            .await
            .unwrap_or(0);
        info!(block = cursor, "starting from latest block");
    } else {
        info!(block = cursor, "resuming from saved cursor");
    }

    let interval = std::time::Duration::from_millis(config.poll_interval_ms);

    loop {
        let head = match latest_block(&client, &config.rpc_url).await {
            Ok(h) => h,
            Err(e) => {
                warn!(err = %e, "failed to fetch latest block");
                tokio::time::sleep(interval).await;
                continue;
            }
        };

        while cursor < head {
            cursor += 1;
            match fetch_block_events(&client, &config.rpc_url, &config.contract_id, cursor).await {
                Ok(evts) if !evts.is_empty() => {
                    for ev in &evts {
                        if let Err(e) = db::process_event(&pool, ev, cursor).await {
                            error!(block = cursor, event = %ev.event, err = %e, "db write failed");
                        }
                        // Broadcast to WS subscribers.
                        let json = serde_json::json!({
                            "event": ev.event,
                            "data": ev.data,
                            "block": cursor,
                        });
                        let _ = tx.send(json.to_string());
                    }
                }
                Ok(_) => {} // no neap events in this block
                Err(e) => {
                    warn!(block = cursor, err = %e, "block fetch failed, retrying");
                    cursor -= 1; // retry
                    tokio::time::sleep(interval).await;
                    break;
                }
            }
        }

        db::set_last_block(&pool, cursor).await?;
        tokio::time::sleep(interval).await;
    }
}
