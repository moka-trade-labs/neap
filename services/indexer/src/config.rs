//! Indexer configuration from environment variables.

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub rpc_url: String,
    pub contract_id: String,
    pub database_url: String,
    pub api_port: u16,
    /// Block height to start indexing from (0 = latest).
    pub start_block: u64,
    /// Poll interval in milliseconds.
    pub poll_interval_ms: u64,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            rpc_url: std::env::var("NEAR_RPC_URL")
                .unwrap_or_else(|_| "https://rpc.testnet.near.org".to_string()),
            contract_id: std::env::var("NEAP_CONTRACT")
                .context("NEAP_CONTRACT required")?,
            database_url: std::env::var("DATABASE_URL")
                .context("DATABASE_URL required (postgres://...)")?,
            api_port: std::env::var("API_PORT")
                .unwrap_or_else(|_| "3000".to_string())
                .parse()
                .unwrap_or(3000),
            start_block: std::env::var("START_BLOCK")
                .unwrap_or_else(|_| "0".to_string())
                .parse()
                .unwrap_or(0),
            poll_interval_ms: std::env::var("POLL_INTERVAL_MS")
                .unwrap_or_else(|_| "1000".to_string())
                .parse()
                .unwrap_or(1000),
        })
    }
}
