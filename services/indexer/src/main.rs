//! neap-indexer: NEAR events → Postgres + REST/WS API.
//!
//! Usage:
//!   NEAP_CONTRACT=neap-trade.testnet \
//!   DATABASE_URL=postgres://user:pass@host/neap \
//!   API_PORT=3000 \
//!   cargo run -p neap-indexer

mod api;
mod config;
mod db;
mod events;
mod fetcher;

use anyhow::Result;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = config::Config::from_env()?;
    info!(contract = %config.contract_id, port = config.api_port, "indexer starting");

    // Connect to Postgres and run migrations.
    let pool = PgPool::connect(&config.database_url).await?;
    sqlx::query(include_str!("../migrations/001_init.sql"))
        .execute(&pool)
        .await?;
    info!("database migrations applied");

    // Broadcast channel for WebSocket subscribers.
    let (tx, _) = broadcast::channel::<String>(256);

    // Spawn the block fetcher.
    let fetcher_pool = pool.clone();
    let fetcher_config = config.clone();
    let fetcher_tx = tx.clone();
    tokio::spawn(async move {
        if let Err(e) = fetcher::run(fetcher_config, fetcher_pool, fetcher_tx).await {
            tracing::error!(err = %e, "fetcher crashed");
        }
    });

    // Start the API server.
    let state = api::AppState { pool, tx };
    let app = api::router(state);
    let addr = format!("0.0.0.0:{}", config.api_port);
    info!(addr = %addr, "API server listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
