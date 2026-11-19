//! neap-keeper: off-chain service that keeps the Neap perps contract running.
//!
//! Responsibilities:
//! - Push oracle prices (for Pushed markets) or call refresh_price (NearPool)
//! - Execute pending two-step orders
//! - Trigger conditional orders (TP/SL/Limit)
//! - Crank the oracle staircase
//! - Liquidate undercollateralized accounts (best-effort)
//!
//! Usage:
//!   NEAP_CONTRACT=neap-trade.testnet \
//!   NEAP_SIGNER=neap-trade.testnet \
//!   NEAP_SIGNER_KEY=ed25519:... \
//!   NEAP_MARKETS=0 \
//!   NEAP_PUSH_PRICE=5000000 \
//!   NEAP_LOOP_MS=5000 \
//!   cargo run -p neap-keeper

mod config;
mod contract;
mod faucet;
mod keeper;
mod price;
mod rpc;
mod status;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env()?;
    keeper::run(config).await
}
