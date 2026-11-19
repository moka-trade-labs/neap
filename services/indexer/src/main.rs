//! neap-indexer: NEAR events → Postgres + REST/WS API.
//!
//! Usage:
//!   NEAP_CONTRACT=neap-trade.testnet \
//!   DATABASE_URL=postgres://user:pass@host/neap \
//!   API_PORT=3000 \
//!   cargo run -p neap-indexer

mod api;
mod chain;
mod config;
mod db;
mod events;
mod fetcher;
mod ingest;
mod limit;

use anyhow::Result;
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
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
    info!(contract = %config.contract_id, port = config.api_port, rpc = %config.rpc_url,
          neardata = %config.neardata_url, "indexer starting");

    // Connect to Postgres and run the (idempotent, multi-statement) schema.
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.database_url)
        .await?;
    sqlx::raw_sql(include_str!("../migrations/001_init.sql"))
        .execute(&pool)
        .await?;
    info!("database migrations applied");

    // Broadcast channel for WebSocket subscribers.
    let (tx, _) = broadcast::channel::<String>(1024);

    // Live market state from contract views (also seeds the markets table).
    let chain = chain::Chain::new(&config.rpc_url, &config.contract_id);
    let live: chain::LiveMarkets = Arc::new(RwLock::new(Vec::new()));
    let live_at: chain::LiveAt = Default::default();
    tokio::spawn(chain::run_live_poller(
        chain.clone(),
        pool.clone(),
        live.clone(),
        live_at.clone(),
        tx.clone(),
        std::time::Duration::from_millis(config.live_refresh_ms),
    ));

    // Block fetcher; restart it if it ever returns an error.
    {
        let (pool, config, tx) = (pool.clone(), config.clone(), tx.clone());
        tokio::spawn(async move {
            loop {
                if let Err(e) = fetcher::run(config.clone(), pool.clone(), tx.clone()).await {
                    tracing::error!(err = %e, "fetcher crashed, restarting in 5s");
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
        });
    }

    // Keep a free-tier host awake by pinging our own public URL.
    if let Ok(url) = std::env::var("KEEPALIVE_URL").or_else(|_| std::env::var("RENDER_EXTERNAL_URL")) {
        tokio::spawn(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap_or_default();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(600)).await;
                let _ = client.get(format!("{}/health", url.trim_end_matches('/'))).send().await;
            }
        });
    }

    let state = api::AppState {
        pool,
        tx,
        chain,
        live,
        keeper_url: std::env::var("KEEPER_INTERNAL_URL").ok(),
        http: reqwest::Client::new(),
        ingest: Arc::new(tokio::sync::Semaphore::new(16)),
        live_at,
        guards: Arc::new(api::Guards::from_env()),
    };
    let app = api::router(state);
    let addr = format!("0.0.0.0:{}", config.api_port);
    info!(addr = %addr, "API server listening");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await?;

    Ok(())
}
