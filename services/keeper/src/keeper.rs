//! Core keeper loop.
//!
//! Three independent loops share one signer:
//! - price loop: for every market, push the live price (Pushed) or
//!   refresh_price (NearPool), activate Proposed markets, crank.
//! - order loop (every second): execute pending two-step orders as soon as an
//!   observation newer than the request exists (it asks for one if needed),
//!   and fire conditionals whose trigger the mark price has reached.
//! - liquidation loop: try to liquidate accounts that hold a position.
//!
//! Orders expire after at most 50 blocks (about 30 s), so the order loop must
//! never wait behind price updates or liquidations.
//!
//! Every step is idempotent: if a tx fails (already executed, expired, healthy
//! account, ...) the keeper logs it and moves on. Restart-safe.

use crate::config::Config;
use crate::contract;
use crate::price;
use crate::rpc::NearRpc;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

#[derive(Clone)]
struct MarketInfo {
    id: u32,
    symbol: String,
    mode: String,
    pushed: bool,
    seq: u64,
    obs_block: u64,
    price: u64,
}

fn num(v: &Value) -> u64 {
    v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

async fn market(rpc: &NearRpc, id: u32) -> Result<MarketInfo> {
    let v = rpc.view("get_market", json!({ "market": id })).await?;
    Ok(MarketInfo {
        id,
        symbol: v["symbol"].as_str().unwrap_or("").to_string(),
        mode: v["mode"].as_str().unwrap_or("").to_string(),
        pushed: v["price_source"] == Value::String("Pushed".into()),
        seq: num(&v["observation"]["seq"]),
        obs_block: num(&v["observation"]["block"]),
        price: num(&v["effective_price"]),
    })
}

async fn markets(rpc: &NearRpc, config: &Config) -> Vec<MarketInfo> {
    let ids: Vec<u32> = match &config.market_ids {
        Some(ids) => ids.clone(),
        None => (0..contract::get_markets_count(rpc).await.unwrap_or(0)).collect(),
    };
    let mut out = Vec::new();
    for id in ids {
        match market(rpc, id).await {
            Ok(m) => out.push(m),
            Err(e) => warn!(market = id, err = %e, "get_market failed"),
        }
    }
    out
}

/// Returns true when a price was pushed (push_price already catches the engine up, so no crank needed).
async fn update_price(rpc: &NearRpc, config: &Config, http: &reqwest::Client, m: &MarketInfo) -> bool {
    if m.pushed {
        let sym = config
            .price_feeds
            .get(&m.id)
            .cloned()
            .unwrap_or_else(|| price::feed_symbol(&m.symbol));
        let px = if config.live_prices {
            match price::usd_price_e6(http, &sym).await {
                Ok(p) => Some(p),
                Err(e) => {
                    warn!(market = m.id, sym, err = %e, "live price unavailable");
                    None
                }
            }
        } else {
            None
        };
        let px = px.or((config.push_price_e6 > 0).then_some(config.push_price_e6));
        if let Some(p) = px {
            match contract::push_price(rpc, m.id, p).await {
                Ok(hash) => {
                    info!(market = m.id, sym, price_e6 = p, hash, "pushed price");
                    return true;
                }
                Err(e) => warn!(market = m.id, err = %e, "push_price failed"),
            }
        }
        false
    } else {
        match contract::refresh_price(rpc, m.id).await {
            Ok(hash) => info!(market = m.id, hash, "refresh_price sent"),
            Err(e) => warn!(market = m.id, err = %e, "refresh_price failed"),
        }
        false
    }
}

/// Pending ids from `floor` up. Returns (ids, next_id).
async fn pending_ids(rpc: &NearRpc, conditionals: bool, floor: u64) -> (Vec<u64>, u64) {
    let next_id = match contract::get_next_order_id(rpc).await {
        Ok(n) => n,
        Err(_) => return (Vec::new(), floor),
    };
    let mut out = Vec::new();
    let mut from = floor;
    while from < next_id {
        let r = if conditionals {
            contract::scan_conditional_ids(rpc, from, 50).await
        } else {
            contract::scan_order_ids(rpc, from, 50).await
        };
        match r {
            Ok(ids) => out.extend(ids),
            // Unknown: keep the floor where it is.
            Err(_) => return (out, floor),
        }
        from += 50;
    }
    (out, next_id)
}

/// Orders and conditionals never change after placement, so one view each is enough.
#[derive(Clone)]
struct Pending {
    market: u32,
    /// request block (order) or arm block (conditional)
    block: u64,
    kind: String,
    long: bool,
    trigger: u64,
}

async fn pending_info(rpc: &NearRpc, conditionals: bool, id: u64) -> Option<Pending> {
    let v = if conditionals {
        rpc.view("get_conditional", json!({ "cond_id": id })).await.ok()?
    } else {
        rpc.view("get_order", json!({ "order_id": id })).await.ok()?
    };
    if v.is_null() {
        return None;
    }
    Some(Pending {
        market: num(&v["market"]) as u32,
        block: num(if conditionals { &v["arm_block"] } else { &v["request_block"] }),
        kind: v["kind"].as_str().unwrap_or("").to_string(),
        long: v["side"].as_str() == Some("Long"),
        trigger: num(&v["trigger_price"]),
    })
}

/// Mirror of the contract's trigger rule, with 0.3% slack because the mark
/// moves between this view and the transaction. A miss costs one cheap tx.
fn near_trigger(p: &Pending, mark: u64) -> bool {
    let up = mark as u128 * 1003 / 1000 >= p.trigger as u128; // mark has (almost) risen to trigger
    let down = mark as u128 * 997 / 1000 <= p.trigger as u128; // mark has (almost) fallen to trigger
    match (p.kind.as_str(), p.long) {
        ("TakeProfit", false) | ("StopLoss", true) => up,
        ("TakeProfit", true) | ("StopLoss", false) => down,
        ("Limit", true) => down,
        ("Limit", false) => up,
        _ => true,
    }
}

/// Ask for a fresh observation on one market and wait until it is on chain.
async fn prime(rpc: &NearRpc, config: &Config, http: &reqwest::Client, m: &MarketInfo) {
    update_price(rpc, config, http, m).await;
    // Pushed: lands in the tx's own block. NearPool: the pool read comes back
    // two receipts later.
    let wait = if m.pushed { 1200 } else { 3000 };
    tokio::time::sleep(Duration::from_millis(wait)).await;
}

async fn order_loop(rpc: Arc<NearRpc>, config: Config, http: reqwest::Client) {
    let mut order_floor = 0u64;
    let mut cond_floor = 0u64;
    let mut orders: HashMap<u64, Pending> = HashMap::new();
    let mut conds: HashMap<u64, Pending> = HashMap::new();
    let mut n: u64 = 0;
    loop {
        n += 1;
        // ── two-step orders: every pass ──
        let (ids, next) = pending_ids(&rpc, false, order_floor).await;
        order_floor = ids.iter().copied().min().unwrap_or(next);
        orders.retain(|id, _| ids.contains(id));
        let mut by_market: HashMap<u32, Vec<(u64, u64)>> = HashMap::new();
        for id in ids {
            if !orders.contains_key(&id) {
                if let Some(p) = pending_info(&rpc, false, id).await {
                    orders.insert(id, p);
                }
            }
            if let Some(p) = orders.get(&id) {
                by_market.entry(p.market).or_default().push((id, p.block));
            }
        }
        futures::future::join_all(by_market.into_iter().map(|(mid, list)| {
            let (rpc, config, http) = (&rpc, &config, &http);
            async move {
                let Ok(m) = market(rpc, mid).await else { return };
                if list.iter().any(|(_, req)| m.obs_block <= *req) {
                    prime(rpc, config, http, &m).await;
                }
                for (id, _) in list {
                    match contract::execute_order(rpc, id).await {
                        Ok(hash) => info!(id, market = mid, hash, "execute_order sent"),
                        Err(e) => warn!(id, market = mid, err = %e, "execute_order failed"),
                    }
                }
            }
        }))
        .await;

        // ── conditionals: every third pass, only when the mark is at the trigger ──
        if n % 3 == 0 {
            let (ids, next) = pending_ids(&rpc, true, cond_floor).await;
            cond_floor = ids.iter().copied().min().unwrap_or(next);
            conds.retain(|id, _| ids.contains(id));
            let mut marks: HashMap<u32, MarketInfo> = HashMap::new();
            for id in ids {
                if !conds.contains_key(&id) {
                    if let Some(p) = pending_info(&rpc, true, id).await {
                        conds.insert(id, p);
                    }
                }
                let Some(p) = conds.get(&id).cloned() else { continue };
                if !marks.contains_key(&p.market) {
                    if let Ok(m) = market(&rpc, p.market).await {
                        marks.insert(p.market, m);
                    }
                }
                let Some(m) = marks.get(&p.market) else { continue };
                if m.obs_block <= p.block || !near_trigger(&p, m.price) {
                    continue;
                }
                match contract::execute_conditional(&rpc, id).await {
                    Ok(hash) => info!(id, market = p.market, hash, "execute_conditional sent"),
                    Err(e) => debug!(id, err = %e, "execute_conditional skipped"),
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(1000)).await;
    }
}

async fn liquidation_scan(rpc: &NearRpc, http: &reqwest::Client, indexer: &str) {
    let list: Vec<Value> = match http.get(format!("{}/accounts", indexer.trim_end_matches('/'))).send().await {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(e) => {
            debug!(err = %e, "indexer unreachable for liquidation scan");
            return;
        }
    };
    for item in list {
        let (Some(owner), Some(market)) = (item["owner"].as_str(), item["market"].as_u64()) else {
            continue;
        };
        if owner.starts_with("sol:") {
            continue;
        }
        let acc = rpc
            .view("get_account", json!({ "market": market, "account": owner }))
            .await
            .unwrap_or(Value::Null);
        let pos = acc["position_q"].as_str().unwrap_or("0");
        if pos == "0" || acc.is_null() {
            continue;
        }
        match contract::liquidate(rpc, market as u32, owner).await {
            Ok(hash) => info!(market, owner, hash, "liquidate sent"),
            Err(e) => debug!(market, owner, err = %e, "not liquidatable"),
        }
    }
}

/// One price pass over every market.
async fn price_tick(rpc: &NearRpc, config: &Config, http: &reqwest::Client, ms: &[MarketInfo]) {
    // Markets are independent: update them concurrently so one slow tx does
    // not make every other market's price stale.
    futures::future::join_all(ms.iter().map(|m| async move {
        if m.mode == "WindDown" || m.mode == "Recovery" {
            return;
        }
        let pushed = update_price(rpc, config, http, m).await;
        if m.mode == "Proposed" && m.seq > 0 {
            match rpc.call("activate_market", json!({ "market": m.id }), 0).await {
                Ok(hash) => info!(market = m.id, hash, "activate_market sent"),
                Err(e) => debug!(market = m.id, err = %e, "activate_market refused"),
            }
        }
        if !pushed {
            if let Err(e) = contract::crank(rpc, m.id).await {
                warn!(market = m.id, err = %e, "crank failed");
            }
        }
    }))
    .await;
}

/// Run the keeper loops forever.
pub async fn run(config: Config) -> Result<()> {
    info!(
        contract = %config.contract_id,
        signer = %config.signer_id,
        markets = ?config.market_ids,
        interval_ms = config.loop_interval_ms,
        "keeper starting"
    );

    let rpc = Arc::new(
        NearRpc::new(&config.rpc_url, &config.contract_id, &config.signer_id, &config.signer_key).await?,
    );
    let http = reqwest::Client::builder().timeout(Duration::from_secs(8)).build()?;

    if let Some(addr) = config.faucet_addr.clone() {
        let (rpc, cfg) = (rpc.clone(), config.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::faucet::serve(addr, rpc, cfg).await {
                error!(err = %e, "faucet server stopped");
            }
        });
    }

    tokio::spawn(order_loop(rpc.clone(), config.clone(), http.clone()));

    if let Some(indexer) = config.indexer_url.clone() {
        let (rpc, http) = (rpc.clone(), http.clone());
        let every = Duration::from_millis((config.loop_interval_ms * config.liq_every_ticks).max(15_000));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                liquidation_scan(&rpc, &http, &indexer).await;
            }
        });
    }

    let cache: RwLock<Vec<MarketInfo>> = RwLock::new(Vec::new());
    let interval = Duration::from_millis(config.loop_interval_ms);
    let mut tick_count: u64 = 0;
    loop {
        tick_count += 1;
        if tick_count % 100 == 1 {
            if let Err(e) = rpc.refresh_access_key().await {
                error!(err = %e, "failed to refresh access key");
            }
        }
        // The market list changes rarely: re-read it every fifth pass.
        if tick_count % 5 == 1 || cache.read().await.is_empty() {
            let ms = markets(&rpc, &config).await;
            if !ms.is_empty() {
                *cache.write().await = ms;
            }
        }
        let ms = cache.read().await.clone();
        price_tick(&rpc, &config, &http, &ms).await;
        tokio::time::sleep(interval).await;
    }
}
