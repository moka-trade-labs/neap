//! Keeper configuration from environment variables.

use anyhow::{Context, Result};

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
    /// Market IDs to service (comma-separated, e.g. "0,1,2").
    pub market_ids: Vec<u32>,
    /// Loop interval in milliseconds.
    pub loop_interval_ms: u64,
    /// Fixed price to push (price_e6). 0 = skip price push (use for NearPool markets).
    pub push_price_e6: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let rpc_url = std::env::var("NEAR_RPC_URL")
            .unwrap_or_else(|_| "https://rpc.testnet.near.org".to_string());
        let contract_id = std::env::var("NEAP_CONTRACT")
            .context("NEAP_CONTRACT env var required")?;
        let signer_id = std::env::var("NEAP_SIGNER")
            .context("NEAP_SIGNER env var required")?;
        let signer_key = std::env::var("NEAP_SIGNER_KEY")
            .context("NEAP_SIGNER_KEY env var required")?;
        let market_ids: Vec<u32> = std::env::var("NEAP_MARKETS")
            .unwrap_or_else(|_| "0".to_string())
            .split(',')
            .map(|s| s.trim().parse().unwrap_or(0))
            .collect();
        let loop_interval_ms: u64 = std::env::var("NEAP_LOOP_MS")
            .unwrap_or_else(|_| "3000".to_string())
            .parse()
            .unwrap_or(3000);
        let push_price_e6: u64 = std::env::var("NEAP_PUSH_PRICE")
            .unwrap_or_else(|_| "0".to_string())
            .parse()
            .unwrap_or(0);

        Ok(Config {
            rpc_url,
            contract_id,
            signer_id,
            signer_key,
            market_ids,
            loop_interval_ms,
            push_price_e6,
        })
    }
}
