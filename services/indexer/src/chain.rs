//! Read-only contract views over NEAR JSON-RPC, plus a live market cache.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tracing::warn;

#[derive(Clone)]
pub struct Chain {
    client: reqwest::Client,
    rpc_url: String,
    contract_id: String,
}

impl Chain {
    pub fn new(rpc_url: &str, contract_id: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            rpc_url: rpc_url.to_string(),
            contract_id: contract_id.to_string(),
        }
    }

    pub fn contract_id(&self) -> &str {
        &self.contract_id
    }

    /// Call a view method on the neap contract and parse its JSON result.
    pub async fn view(&self, method: &str, args: &Value) -> Result<Value> {
        use base64::Engine;
        let args_b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(args)?);
        let resp: Value = self
            .client
            .post(&self.rpc_url)
            .json(&json!({
                "jsonrpc": "2.0", "id": "idx", "method": "query",
                "params": {
                    "request_type": "call_function",
                    "finality": "final",
                    "account_id": self.contract_id,
                    "method_name": method,
                    "args_base64": args_b64,
                }
            }))
            .send()
            .await?
            .json()
            .await?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("rpc error: {err}"));
        }
        if let Some(err) = resp["result"].get("error") {
            return Err(anyhow!("view error: {err}"));
        }
        let bytes: Vec<u8> = resp["result"]["result"]
            .as_array()
            .context("no result")?
            .iter()
            .map(|v| v.as_u64().unwrap_or(0) as u8)
            .collect();
        Ok(serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}

/// Latest `get_market` view per market id (index = market id).
pub type LiveMarkets = Arc<RwLock<Vec<Value>>>;

/// Poll every market's view, keep the cache warm, upsert static fields into
/// Postgres, and broadcast a `market_state` WS message per market.
pub async fn run_live_poller(
    chain: Chain,
    pool: PgPool,
    live: LiveMarkets,
    tx: broadcast::Sender<String>,
    every: std::time::Duration,
) {
    let mut n: u64 = 0;
    loop {
        match refresh_once(&chain, &pool, &live, &tx, n % 20 == 0).await {
            Ok(()) => {}
            Err(e) => warn!(err = %e, "live market refresh failed"),
        }
        n += 1;
        tokio::time::sleep(every).await;
    }
}

async fn refresh_once(
    chain: &Chain,
    pool: &PgPool,
    live: &LiveMarkets,
    tx: &broadcast::Sender<String>,
    write_db: bool,
) -> Result<()> {
    let count = chain.view("get_markets_count", &json!({})).await?.as_u64().unwrap_or(0);
    let mut out = Vec::with_capacity(count as usize);
    for id in 0..count {
        let mut v = chain.view("get_market", &json!({ "market": id })).await?;
        if let Value::Object(ref mut o) = v {
            o.insert("id".into(), json!(id));
        }
        if write_db {
            if let Err(e) = crate::db::upsert_market_from_view(pool, id as i32, &v).await {
                warn!(market = id, err = %e, "market upsert failed");
            }
        }
        let _ = tx.send(json!({ "event": "market_state", "data": v }).to_string());
        out.push(v);
    }
    *live.write().await = out;
    Ok(())
}
