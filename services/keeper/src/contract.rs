//! Type-safe wrappers for neap contract view and call methods.

use crate::rpc::NearRpc;
use anyhow::Result;
use serde::Deserialize;
use serde_json::json;

// ── View response types ──

#[derive(Debug, Deserialize)]
pub struct MarketView {
    pub symbol: String,
    pub mode: String,
    pub effective_price: String,
    pub observation: ObservationView,
    pub oi_long_q: String,
    pub oi_short_q: String,
    pub vault: String,
    pub slot_last: String,
}

#[derive(Debug, Deserialize)]
pub struct ObservationView {
    pub price_e6: String,
    pub source_ts: String,
    pub seq: String,
    pub block: String,
}

#[derive(Debug, Deserialize)]
pub struct OrderView {
    pub id: u64,
    pub market: u32,
    pub side: String,
    pub size_q: String,
    pub limit_price: String,
    pub request_block: u64,
    pub expiry_block: u64,
    pub reduce_only: bool,
}

#[derive(Debug, Deserialize)]
pub struct ConditionalView {
    pub id: u64,
    pub market: u32,
    pub kind: String,
    pub side: String,
    pub size_q: String,
    pub trigger_price: String,
    pub arm_block: u64,
    pub expiry_block: u64,
}

#[derive(Debug, Deserialize)]
pub struct AccountView {
    pub capital: String,
    pub pnl: String,
    pub position_q: String,
}

// ── View calls ──

pub async fn get_markets_count(rpc: &NearRpc) -> Result<u32> {
    let v = rpc.view("get_markets_count", json!({})).await?;
    Ok(v.as_u64().unwrap_or(0) as u32)
}

pub async fn get_market(rpc: &NearRpc, market: u32) -> Result<MarketView> {
    let v = rpc.view("get_market", json!({"market": market})).await?;
    Ok(serde_json::from_value(v)?)
}

pub async fn get_next_order_id(rpc: &NearRpc) -> Result<u64> {
    let v = rpc.view("get_next_order_id", json!({})).await?;
    Ok(v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_u64())
        .unwrap_or(0))
}

pub async fn scan_order_ids(rpc: &NearRpc, from_id: u64, limit: u32) -> Result<Vec<u64>> {
    let v = rpc.view("scan_order_ids", json!({"from_id": from_id, "limit": limit})).await?;
    let ids: Vec<String> = serde_json::from_value(v).unwrap_or_default();
    Ok(ids.iter().filter_map(|s| s.parse().ok()).collect())
}

pub async fn scan_conditional_ids(rpc: &NearRpc, from_id: u64, limit: u32) -> Result<Vec<u64>> {
    let v = rpc.view("scan_conditional_ids", json!({"from_id": from_id, "limit": limit})).await?;
    let ids: Vec<String> = serde_json::from_value(v).unwrap_or_default();
    Ok(ids.iter().filter_map(|s| s.parse().ok()).collect())
}

pub async fn get_account(
    rpc: &NearRpc,
    market: u32,
    account: &str,
) -> Result<Option<AccountView>> {
    let v = rpc.view("get_account", json!({"market": market, "account": account})).await?;
    if v.is_null() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value(v)?))
}

// ── Transactions ──

pub async fn push_price(rpc: &NearRpc, market: u32, price_e6: u64) -> Result<String> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos() as u64;
    rpc.call(
        "push_price",
        json!({
            "market": market,
            "price": price_e6.to_string(),
            "publish_ns": ts.to_string(),
        }),
        0,
    )
    .await
}

pub async fn execute_order(rpc: &NearRpc, order_id: u64) -> Result<String> {
    rpc.call("execute_order", json!({"order_id": order_id}), 0).await
}

pub async fn execute_conditional(rpc: &NearRpc, cond_id: u64) -> Result<String> {
    rpc.call("execute_conditional", json!({"cond_id": cond_id}), 0).await
}

pub async fn crank(rpc: &NearRpc, market: u32) -> Result<String> {
    rpc.call("crank", json!({"market": market}), 0).await
}

pub async fn liquidate(rpc: &NearRpc, market: u32, account: &str) -> Result<String> {
    rpc.call("liquidate", json!({"market": market, "account": account}), 0).await
}

pub async fn refresh_price(rpc: &NearRpc, market: u32) -> Result<String> {
    rpc.call("refresh_price", json!({"market": market}), 0).await
}
