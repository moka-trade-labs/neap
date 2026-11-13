//! Indexer configuration from environment variables.

use anyhow::{Context, Result};

#[derive(Clone, Debug)]
pub struct Config {
    pub rpc_url: String,
    /// neardata.xyz base URL (block stream with receipt outcomes, one request per block).
    pub neardata_url: String,
    pub contract_id: String,
    pub database_url: String,
    pub api_port: u16,
    /// Block height to start indexing from (0 = latest).
    pub start_block: u64,
    /// Poll interval in milliseconds.
    pub poll_interval_ms: u64,
    /// Jump to the chain head when further behind than this.
    pub max_catchup_blocks: u64,
    /// How often to refresh live market state from contract views.
    pub live_refresh_ms: u64,
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let contract_id = std::env::var("NEAP_CONTRACT").context("NEAP_CONTRACT required")?;
        let mainnet = !contract_id.ends_with(".testnet");
        // API_PORT wins; PORT is what Render/Fly/Railway inject.
        let port = std::env::var("API_PORT")
            .or_else(|_| std::env::var("PORT"))
            .unwrap_or_else(|_| "3000".to_string());
        Ok(Self {
            rpc_url: env_or(
                "NEAR_RPC_URL",
                if mainnet { "https://free.rpc.fastnear.com" } else { "https://test.rpc.fastnear.com,https://rpc.testnet.near.org,https://near-testnet.drpc.org" },
            ),
            neardata_url: env_or(
                "NEARDATA_URL",
                if mainnet { "https://mainnet.neardata.xyz" } else { "https://testnet.neardata.xyz" },
            ),
            contract_id,
            database_url: std::env::var("DATABASE_URL")
                .context("DATABASE_URL required (postgres://...)")?,
            api_port: port.parse().unwrap_or(3000),
            start_block: env_or("START_BLOCK", "0").parse().unwrap_or(0),
            poll_interval_ms: env_or("POLL_INTERVAL_MS", "700").parse().unwrap_or(700),
            max_catchup_blocks: env_or("MAX_CATCHUP_BLOCKS", "150").parse().unwrap_or(150),
            live_refresh_ms: env_or("LIVE_REFRESH_MS", "3000").parse().unwrap_or(3000),
        })
    }
}
