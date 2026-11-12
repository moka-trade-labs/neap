//! Core keeper loop: one pass per market per tick.
//!
//! Flow: push_price → execute pending orders → execute triggered conditionals
//!       → crank → (liquidation scanning is best-effort).
//!
//! Every step is idempotent: if a tx fails (already executed, expired, etc.)
//! the keeper logs it and moves on. Restart-safe.

use crate::config::Config;
use crate::contract;
use crate::rpc::NearRpc;
use anyhow::Result;
use tracing::{error, info, warn};

/// Run one full keeper pass for a single market.
pub async fn tick(rpc: &NearRpc, config: &Config, market: u32) -> Result<()> {
    // 1. Push price (for Pushed markets) or refresh (for NearPool).
    if config.push_price_e6 > 0 {
        match contract::push_price(rpc, market, config.push_price_e6).await {
            Ok(hash) => info!(market, hash, price = config.push_price_e6, "pushed price"),
            Err(e) => warn!(market, err = %e, "push_price failed (stale/duplicate?)"),
        }
    } else {
        // NearPool market: call refresh_price.
        match contract::refresh_price(rpc, market).await {
            Ok(hash) => info!(market, hash, "refresh_price sent"),
            Err(e) => warn!(market, err = %e, "refresh_price failed"),
        }
    }

    // 2. Crank the staircase to the current block.
    match contract::crank(rpc, market).await {
        Ok(hash) => info!(market, hash, "cranked"),
        Err(e) => warn!(market, err = %e, "crank failed"),
    }

    // 3. Scan and execute pending orders.
    let next_id = contract::get_next_order_id(rpc).await.unwrap_or(0);
    if next_id > 0 {
        // Scan in batches of 50.
        let mut from = 0u64;
        while from < next_id {
            let batch_size = 50u32;
            let ids = contract::scan_order_ids(rpc, from, batch_size).await.unwrap_or_default();
            for id in &ids {
                match contract::execute_order(rpc, *id).await {
                    Ok(hash) => info!(market, order_id = id, hash, "executed order"),
                    Err(e) => {
                        // Expected failures: expired, stale observation, already executed.
                        warn!(market, order_id = id, err = %e, "execute_order skipped");
                    }
                }
            }
            from += batch_size as u64;
        }
    }

    // 4. Scan and execute triggered conditionals.
    if next_id > 0 {
        let mut from = 0u64;
        while from < next_id {
            let batch_size = 50u32;
            let ids = contract::scan_conditional_ids(rpc, from, batch_size)
                .await
                .unwrap_or_default();
            for id in &ids {
                match contract::execute_conditional(rpc, *id).await {
                    Ok(hash) => info!(market, cond_id = id, hash, "triggered conditional"),
                    Err(e) => {
                        // Expected: trigger not crossed, expired.
                        warn!(market, cond_id = id, err = %e, "execute_conditional skipped");
                    }
                }
            }
            from += batch_size as u64;
        }
    }

    // 5. Crank again after fills (settles funding, advances the engine).
    match contract::crank(rpc, market).await {
        Ok(_) => {}
        Err(e) => warn!(market, err = %e, "post-fill crank failed"),
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

    let rpc = NearRpc::new(
        &config.rpc_url,
        &config.contract_id,
        &config.signer_id,
        &config.signer_key,
    )
    .await?;

    let interval = std::time::Duration::from_millis(config.loop_interval_ms);
    let mut tick_count: u64 = 0;

    loop {
        tick_count += 1;

        // Refresh access key every 100 ticks to avoid stale nonce/block_hash.
        if tick_count % 100 == 1 {
            if let Err(e) = rpc.refresh_access_key().await {
                error!(err = %e, "failed to refresh access key");
            }
        }

        for &market in &config.market_ids {
            if let Err(e) = tick(&rpc, &config, market).await {
                error!(market, tick = tick_count, err = %e, "keeper tick failed");
            }
        }

        tokio::time::sleep(interval).await;
    }
}
