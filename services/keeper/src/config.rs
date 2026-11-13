//! Keeper configuration from environment variables.

use anyhow::{Context, Result};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Config {
    /// NEAR RPC endpoint.
    pub rpc_url: String,
    /// Contract account ID.
    pub contract_id: String,
    /// Signer account ID (the oracle / keeper account).
    pub signer_id: String,
    /// Signer private key (ed25519:... base58).
    pub signer_key: String,
    /// Market IDs to service. `None` = every market on the contract (auto-discovered each tick).
    pub market_ids: Option<Vec<u32>>,
    /// Loop interval in milliseconds.
    pub loop_interval_ms: u64,
    /// Fixed fallback price (price_e6) for Pushed markets when no live feed answers. 0 = none.
    pub push_price_e6: u64,
    /// Live feed symbol overrides per market id ("0=NEAR,1=SOL"). Default: market symbol minus "-PERP".
    pub price_feeds: HashMap<u32, String>,
    /// Push the live price for Pushed markets (default true).
    pub live_prices: bool,
    /// Indexer base URL for the liquidation scan's account list (e.g. http://127.0.0.1:10000).
    pub indexer_url: Option<String>,
    /// Liquidation scan period in ticks.
    pub liq_every_ticks: u64,
    /// Faucet: listen address (e.g. 127.0.0.1:4001). None = disabled.
    pub faucet_addr: Option<String>,
    /// Faucet: quote token contract (mintable test USDC).
    pub faucet_token: String,
    /// Faucet: amount in token atoms (6 decimals).
    pub faucet_amount: u128,
    /// Faucet: NEAR (yocto) to send when the account holds less than 0.5 NEAR. 0 = never.
    pub faucet_near_yocto: u128,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let contract_id = std::env::var("NEAP_CONTRACT").context("NEAP_CONTRACT env var required")?;
        let testnet = contract_id.ends_with(".testnet");
        let rpc_url = env_or(
            "NEAR_RPC_URL",
            if testnet { "https://test.rpc.fastnear.com,https://near-testnet.drpc.org,https://near-testnet.gateway.tatum.io,https://rpc.testnet.near.org" } else { "https://free.rpc.fastnear.com" },
        );
        let signer_id = std::env::var("NEAP_SIGNER").context("NEAP_SIGNER env var required")?;
        let signer_key = std::env::var("NEAP_SIGNER_KEY").context("NEAP_SIGNER_KEY env var required")?;
        let markets = env_or("NEAP_MARKETS", "auto");
        let market_ids = if markets == "auto" {
            None
        } else {
            Some(markets.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        };
        let price_feeds = env_or("NEAP_PRICE_FEEDS", "")
            .split(',')
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=')?;
                Some((k.trim().parse().ok()?, v.trim().to_uppercase()))
            })
            .collect();
        let near_drip: f64 = env_or("FAUCET_NEAR", "0").parse().unwrap_or(0.0);
        Ok(Config {
            rpc_url,
            contract_id,
            signer_id,
            signer_key,
            market_ids,
            loop_interval_ms: env_or("NEAP_LOOP_MS", "3000").parse().unwrap_or(3000),
            push_price_e6: env_or("NEAP_PUSH_PRICE", "0").parse().unwrap_or(0),
            price_feeds,
            live_prices: env_or("NEAP_LIVE_PRICES", "true") != "false",
            indexer_url: std::env::var("NEAP_INDEXER_URL").ok().filter(|s| !s.is_empty()),
            liq_every_ticks: env_or("NEAP_LIQ_EVERY", "5").parse().unwrap_or(5).max(1),
            faucet_addr: std::env::var("FAUCET_ADDR").ok().filter(|s| !s.is_empty()),
            faucet_token: env_or("FAUCET_TOKEN", "usdc.fakes.testnet"),
            faucet_amount: env_or("FAUCET_AMOUNT", "1000000000").parse().unwrap_or(1_000_000_000),
            faucet_near_yocto: (near_drip * 1e6) as u128 * 1_000_000_000_000_000_000,
        })
    }
}
