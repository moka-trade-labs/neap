//! REST + WebSocket API (axum).

use axum::{
    extract::{Path, Query, State, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub tx: broadcast::Sender<String>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/markets", get(list_markets))
        .route("/markets/{id}", get(get_market))
        .route("/markets/{id}/candles", get(get_candles))
        .route("/markets/{id}/fills", get(get_fills))
        .route("/markets/{id}/orders", get(get_orders))
        .route("/markets/{id}/liquidations", get(get_liquidations))
        .route("/account/{owner}/fills", get(get_account_fills))
        .route("/ws", get(ws_handler))
        .layer(CorsLayer::permissive())
        .with_state(Arc::new(state))
}

// ── Query params ──

#[derive(Deserialize)]
struct Pagination {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
struct CandleQuery {
    interval: Option<i32>,  // seconds: 60, 300, 900, 3600, 86400
    limit: Option<i64>,
}

// ── Handlers ──

async fn list_markets(
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let rows = sqlx::query_as::<_, MarketRow>(
        "SELECT id, symbol, tier, mode, lister, bond FROM markets ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

async fn get_market(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
) -> impl IntoResponse {
    let row = sqlx::query_as::<_, MarketRow>(
        "SELECT id, symbol, tier, mode, lister, bond FROM markets WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);
    Json(row)
}

async fn get_candles(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(q): Query<CandleQuery>,
) -> impl IntoResponse {
    let interval = q.interval.unwrap_or(60);
    let limit = q.limit.unwrap_or(100).min(1000);
    // Build candles from observations if the candles table is empty.
    let rows = sqlx::query_as::<_, ObsRow>(
        "SELECT price_e6, block, ts FROM observations
         WHERE market_id = $1 ORDER BY block DESC LIMIT $2",
    )
    .bind(id)
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

async fn get_fills(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(p): Query<Pagination>,
) -> impl IntoResponse {
    let limit = p.limit.unwrap_or(50).min(500);
    let offset = p.offset.unwrap_or(0);
    let rows = sqlx::query_as::<_, FillRow>(
        "SELECT id, market_id, owner, order_id, size_q, price, fee, block, ts
         FROM fills WHERE market_id = $1 ORDER BY block DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

async fn get_orders(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(p): Query<Pagination>,
) -> impl IntoResponse {
    let limit = p.limit.unwrap_or(50).min(500);
    let offset = p.offset.unwrap_or(0);
    let rows = sqlx::query_as::<_, OrderRow>(
        "SELECT id, market_id, owner, side, size_q, limit_price, status, fill_price, fee,
                request_block, created_at, updated_at
         FROM orders WHERE market_id = $1 ORDER BY id DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

async fn get_liquidations(
    State(state): State<Arc<AppState>>,
    Path(id): Path<i32>,
    Query(p): Query<Pagination>,
) -> impl IntoResponse {
    let limit = p.limit.unwrap_or(50).min(500);
    let offset = p.offset.unwrap_or(0);
    let rows = sqlx::query_as::<_, LiqRow>(
        "SELECT id, market_id, account, caller, closed_q, fee, block, ts
         FROM liquidations WHERE market_id = $1 ORDER BY block DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

async fn get_account_fills(
    State(state): State<Arc<AppState>>,
    Path(owner): Path<String>,
    Query(p): Query<Pagination>,
) -> impl IntoResponse {
    let limit = p.limit.unwrap_or(50).min(500);
    let offset = p.offset.unwrap_or(0);
    let rows = sqlx::query_as::<_, FillRow>(
        "SELECT id, market_id, owner, order_id, size_q, price, fee, block, ts
         FROM fills WHERE owner = $1 ORDER BY block DESC LIMIT $2 OFFSET $3",
    )
    .bind(owner)
    .bind(limit)
    .bind(offset)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();
    Json(rows)
}

// ── WebSocket ──

async fn ws_handler(
    State(state): State<Arc<AppState>>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state))
}

async fn handle_ws(
    mut socket: axum::extract::ws::WebSocket,
    state: Arc<AppState>,
) {
    let mut rx = state.tx.subscribe();
    while let Ok(msg) = rx.recv().await {
        if socket
            .send(axum::extract::ws::Message::Text(msg.into()))
            .await
            .is_err()
        {
            break;
        }
    }
}

// ── Row types ──

#[derive(sqlx::FromRow, Serialize)]
struct MarketRow {
    id: i32,
    symbol: String,
    tier: String,
    mode: String,
    lister: String,
    bond: i64,
}

#[derive(sqlx::FromRow, Serialize)]
struct ObsRow {
    price_e6: i64,
    block: i64,
    ts: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, Serialize)]
struct FillRow {
    id: i64,
    market_id: i32,
    owner: String,
    order_id: Option<i64>,
    size_q: String,
    price: i64,
    fee: String,
    block: i64,
    ts: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, Serialize)]
struct OrderRow {
    id: i64,
    market_id: i32,
    owner: String,
    side: String,
    size_q: String,
    limit_price: Option<i64>,
    status: String,
    fill_price: Option<i64>,
    fee: Option<String>,
    request_block: Option<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(sqlx::FromRow, Serialize)]
struct LiqRow {
    id: i64,
    market_id: i32,
    account: String,
    caller: String,
    closed_q: String,
    fee: String,
    block: i64,
    ts: chrono::DateTime<chrono::Utc>,
}
