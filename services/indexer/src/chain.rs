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
    rpc_urls: Vec<String>,
    next: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    contract_id: String,
}

impl Chain {
    pub fn new(rpc_url: &str, contract_id: &str) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client"),
            rpc_urls: rpc_url.split(',').map(|u| u.trim().to_string()).filter(|u| !u.is_empty()).collect(),
            next: Default::default(),
            contract_id: contract_id.to_string(),
        }
    }

    pub fn contract_id(&self) -> &str {
        &self.contract_id
    }

    /// Call a view method on the neap contract and parse its JSON result.
    /// Tries each RPC in turn (starting with the last one that worked).
    pub async fn view(&self, method: &str, args: &Value) -> Result<Value> {
        use std::sync::atomic::Ordering;
        let n = self.rpc_urls.len().max(1);
        let start = self.next.load(Ordering::Relaxed);
        let mut last = anyhow!("no rpc configured");
        for i in 0..n {
            let idx = (start + i) % n;
            match self.view_on(&self.rpc_urls[idx], method, args).await {
                Ok(v) => {
                    self.next.store(idx, Ordering::Relaxed);
                    return Ok(v);
                }
                Err(e) => {
                    // A contract-level error is the same on every RPC.
                    if e.to_string().starts_with("view error") {
                        return Err(e);
                    }
                    last = e;
                }
            }
        }
        Err(last)
    }

    async fn view_on(&self, rpc_url: &str, method: &str, args: &Value) -> Result<Value> {
        use base64::Engine;
        let args_b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(args)?);
        let mut req = self.client.post(rpc_url);
        if rpc_url.contains("fastnear") {
            if let Some(k) = crate::config::fastnear_key() {
                req = req.bearer_auth(k);
            }
        }
        let resp: Value = req
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
            let es = err.to_string();
            if es.contains("FunctionCallError") || es.contains("MethodNotFound") || es.contains("CONTRACT_EXECUTION_ERROR") {
                return Err(anyhow!("view error: {err}"));
            }
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
        let _ = crate::db::ensure_market(pool, id as i32).await;
        if let Err(e) = crate::db::upsert_observation_from_view(pool, id as i32, &v).await {
            warn!(market = id, err = %e, "observation upsert failed");
        }
        let _ = tx.send(json!({ "event": "market_state", "data": v }).to_string());
        out.push(v);
    }
    *live.write().await = out;
    Ok(())
}
