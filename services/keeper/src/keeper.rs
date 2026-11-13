//! Core keeper loop.
//!
//! Each tick: for every market, push the live price (Pushed) or refresh_price
//! (NearPool), activate Proposed markets once they have an observation, crank.
//! Then execute every pending order and conditional, crank again, and every
//! few ticks try to liquidate accounts that hold a position.
//!
//! Every step is idempotent: if a tx fails (already executed, expired, healthy
//! account, ...) the keeper logs it and moves on. Restart-safe.

use crate::config::Config;
use crate::contract;
use crate::price;
use crate::rpc::NearRpc;
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

struct MarketInfo {
    id: u32,
    symbol: String,
    mode: String,
    pushed: bool,
    seq: u64,
}

async fn markets(rpc: &NearRpc, config: &Config) -> Vec<MarketInfo> {
    let ids: Vec<u32> = match &config.market_ids {
        Some(ids) => ids.clone(),
        None => (0..contract::get_markets_count(rpc).await.unwrap_or(0)).collect(),
    };
    let mut out = Vec::new();
    for id in ids {
        match rpc.view("get_market", json!({ "market": id })).await {
            Ok(v) => out.push(MarketInfo {
                id,
                symbol: v["symbol"].as_str().unwrap_or("").to_string(),
                mode: v["mode"].as_str().unwrap_or("").to_string(),
                pushed: v["price_source"] == Value::String("Pushed".into()),
                seq: v["observation"]["seq"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
            }),
            Err(e) => warn!(market = id, err = %e, "get_market failed"),
        }
    }
    out
}

async fn update_price(rpc: &NearRpc, config: &Config, http: &reqwest::Client, m: &MarketInfo) {
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
                Ok(hash) => info!(market = m.id, sym, price_e6 = p, hash, "pushed price"),
                Err(e) => warn!(market = m.id, err = %e, "push_price failed"),
            }
        }
    } else {
        match contract::refresh_price(rpc, m.id).await {
            Ok(hash) => info!(market = m.id, hash, "refresh_price sent"),
            Err(e) => warn!(market = m.id, err = %e, "refresh_price failed"),
        }
    }
}

async fn scan_and_execute(rpc: &NearRpc, conditionals: bool) {
    let next_id = contract::get_next_order_id(rpc).await.unwrap_or(0);
    let mut from = 0u64;
    while from < next_id {
        let ids = if conditionals {
            contract::scan_conditional_ids(rpc, from, 50).await.unwrap_or_default()
        } else {
            contract::scan_order_ids(rpc, from, 50).await.unwrap_or_default()
        };
        for id in ids {
            let r = if conditionals {
                contract::execute_conditional(rpc, id).await
            } else {
                contract::execute_order(rpc, id).await
            };
            match r {
                Ok(hash) => info!(id, hash, conditionals, "executed"),
                // Expected: not triggered yet, expired, stale observation.
                Err(e) => debug!(id, err = %e, conditionals, "execute skipped"),
            }
        }
        from += 50;
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
            Ok(hash) => info!(market, owner, hash, "liquidated"),
            Err(e) => debug!(market, owner, err = %e, "not liquidatable"),
        }
    }
}

/// One full keeper pass.
pub async fn tick(rpc: &NearRpc, config: &Config, http: &reqwest::Client, n: u64) -> Result<()> {
    let ms = markets(rpc, config).await;
    for m in &ms {
        if m.mode == "WindDown" || m.mode == "Recovery" {
            continue;
        }
        update_price(rpc, config, http, m).await;
        if m.mode == "Proposed" && m.seq > 0 {
            match rpc.call("activate_market", json!({ "market": m.id }), 0).await {
                Ok(hash) => info!(market = m.id, hash, "activated market"),
                Err(e) => debug!(market = m.id, err = %e, "activate_market refused"),
            }
        }
        if let Err(e) = contract::crank(rpc, m.id).await {
            warn!(market = m.id, err = %e, "crank failed");
        }
    }
    scan_and_execute(rpc, false).await;
    scan_and_execute(rpc, true).await;
    if let Some(indexer) = &config.indexer_url {
        if n % config.liq_every_ticks == 0 {
            liquidation_scan(rpc, http, indexer).await;
        }
    }
    Ok(())
}

/// Run the keeper loop forever.
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
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()?;

    if let Some(addr) = config.faucet_addr.clone() {
        let (rpc, cfg) = (rpc.clone(), config.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::faucet::serve(addr, rpc, cfg).await {
                error!(err = %e, "faucet server stopped");
            }
        });
    }

    let interval = std::time::Duration::from_millis(config.loop_interval_ms);
    let mut tick_count: u64 = 0;
    loop {
        tick_count += 1;
        if tick_count % 100 == 1 {
            if let Err(e) = rpc.refresh_access_key().await {
                error!(err = %e, "failed to refresh access key");
            }
        }
        if let Err(e) = tick(&rpc, &config, &http, tick_count).await {
            error!(tick = tick_count, err = %e, "keeper tick failed");
        }
        tokio::time::sleep(interval).await;
    }
}
