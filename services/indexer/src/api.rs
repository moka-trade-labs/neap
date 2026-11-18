//! REST + WebSocket API (axum). See services/indexer/README.md for the reference.

use crate::chain::{now_ms, Chain, LiveAt, LiveMarkets};
use crate::limit::{client_ip, is_account_id, Limiter, ViewCache};
use axum::{
    extract::{ws::Message, ConnectInfo, Path, Query, Request, State, WebSocketUpgrade},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::broadcast;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub tx: broadcast::Sender<String>,
    pub chain: Chain,
    pub live: LiveMarkets,
    pub keeper_url: Option<String>,
    pub http: reqwest::Client,
    /// Bounds concurrent POST /tx lookups.
    pub ingest: Arc<tokio::sync::Semaphore>,
    pub live_at: LiveAt,
    pub guards: Arc<Guards>,
}

/// Abuse limits. The API is public and unauthenticated, runs on one small
/// instance, and shares its RPC quota with the keeper that pushes prices.
pub struct Guards {
    /// Routes that call an RPC per request (`/account/:owner`, `/view`, `/quote`).
    chain: Limiter,
    /// `POST /tx` and `POST /faucet`.
    write: Limiter,
    /// Everything else (database reads).
    read: Limiter,
    views: ViewCache,
    stats: std::sync::Mutex<Option<(std::time::Instant, Arc<Vec<Stats24>>)>>,
    /// Transactions being looked up right now, so one hash is not fetched twice.
    in_flight: std::sync::Mutex<std::collections::HashSet<String>>,
    ws: Arc<tokio::sync::Semaphore>,
    trusted_hops: Option<usize>,
}

impl Guards {
    pub fn from_env() -> Self {
        let n = |key: &str, default: u32| std::env::var(key).ok().and_then(|v| v.parse().ok()).unwrap_or(default);
        Self {
            chain: Limiter::per_minute(n("API_CHAIN_PER_MIN", 120)),
            write: Limiter::per_minute(n("API_WRITE_PER_MIN", 30)),
            read: Limiter::per_minute(n("API_READ_PER_MIN", 600)),
            views: ViewCache::new(std::time::Duration::from_millis(n("API_VIEW_CACHE_MS", 2000) as u64)),
            stats: Default::default(),
            in_flight: Default::default(),
            ws: Arc::new(tokio::sync::Semaphore::new(n("WS_MAX_CONNECTIONS", 500) as usize)),
            trusted_hops: std::env::var("TRUSTED_PROXY_HOPS").ok().and_then(|v| v.parse().ok()),
        }
    }
}

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Per-client rate limit and a request deadline for every route.
async fn guard(
    State(s): S,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let xff = req.headers().get("x-forwarded-for").and_then(|h| h.to_str().ok());
    // The keeper reports its transactions over loopback with no proxy in between.
    let internal = addr.ip().is_loopback() && xff.is_none();
    if !internal {
        let ip = client_ip(xff, addr.ip(), s.guards.trusted_hops);
        let path = req.uri().path();
        let limiter = if req.method() == Method::POST {
            &s.guards.write
        } else if path.starts_with("/view/") || path.ends_with("/quote") || is_live_account(path) {
            &s.guards.chain
        } else {
            &s.guards.read
        };
        if !limiter.allow(&ip, std::time::Instant::now()) {
            return err(StatusCode::TOO_MANY_REQUESTS, "rate limit exceeded, slow down");
        }
    }
    // The WebSocket upgrade returns at once; the socket itself is not timed.
    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(req)).await {
        Ok(r) => r,
        Err(_) => err(StatusCode::GATEWAY_TIMEOUT, "request timed out"),
    }
}

/// `/account/:owner` (live chain read), not its history sub-routes.
fn is_live_account(path: &str) -> bool {
    path.strip_prefix("/account/").map(|rest| !rest.is_empty() && !rest.contains('/')).unwrap_or(false)
}

/// What to tell a caller about a failed chain read. Contract-level errors are
/// useful to the frontend; transport errors can carry provider URLs and keys.
fn chain_err(e: anyhow::Error) -> Response {
    let text = e.to_string();
    if text.starts_with("view error") {
        return err(StatusCode::BAD_REQUEST, text);
    }
    tracing::warn!(err = %text, "chain read failed");
    err(StatusCode::BAD_GATEWAY, "NEAR RPC unavailable, try again shortly")
}

/// A contract view through the short-lived cache.
async fn cached_view(s: &AppState, method: &str, args: &Value) -> anyhow::Result<Value> {
    let key = format!("{method}:{args}");
    let now = std::time::Instant::now();
    if let Some(v) = s.guards.views.get(&key, now) {
        return Ok(v);
    }
    let v = s.chain.view(method, args).await?;
    s.guards.views.put(key, v.clone(), now);
    Ok(v)
}

type S = State<Arc<AppState>>;

pub fn router(state: AppState) -> Router {
    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/config", get(config))
        .route("/markets", get(list_markets))
        .route("/markets/{id}", get(get_market))
        .route("/markets/{id}/candles", get(get_candles))
        .route("/markets/{id}/observations", get(get_observations))
        .route("/markets/{id}/fills", get(get_fills))
        .route("/markets/{id}/orders", get(get_orders))
        .route("/markets/{id}/conditionals", get(get_conditionals))
        .route("/markets/{id}/liquidations", get(get_liquidations))
        .route("/markets/{id}/quote", get(get_quote))
        .route("/accounts", get(list_accounts))
        .route("/account/{owner}", get(get_account))
        .route("/account/{owner}/fills", get(get_account_fills))
        .route("/account/{owner}/orders", get(get_account_orders))
        .route("/account/{owner}/conditionals", get(get_account_conditionals))
        .route("/account/{owner}/liquidations", get(get_account_liquidations))
        .route("/view/{method}", get(view_proxy))
        .route("/faucet", post(faucet))
        .route("/tx", post(report_tx))
        .route("/ws", get(ws_handler));
    let state = Arc::new(state);
    app.layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn err(code: StatusCode, msg: impl ToString) -> Response {
    (code, Json(json!({ "error": msg.to_string() }))).into_response()
}

fn db_err(e: sqlx::Error) -> Response {
    tracing::warn!(err = %e, "db query failed");
    err(StatusCode::INTERNAL_SERVER_ERROR, "database error")
}

// ── Query params ──

#[derive(Deserialize)]
struct Pagination {
    limit: Option<i64>,
    offset: Option<i64>,
    status: Option<String>,
}

impl Pagination {
    fn lim(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 500)
    }
    fn off(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[derive(Deserialize)]
struct CandleQuery {
    interval: Option<i32>, // seconds: 60, 300, 900, 3600, 86400
    limit: Option<i64>,
}

// ── Meta ──

async fn root() -> impl IntoResponse {
    Json(json!({
        "service": "neap-indexer",
        "docs": "https://github.com/moka-trade-labs/neap/blob/main/services/indexer/README.md",
    }))
}

/// Always HTTP 200 while the process serves requests (the host's health
/// check restarts the container on anything else, which cannot fix a
/// rate-limited RPC). `status` and `problems` say whether the data is fresh;
/// point an uptime monitor at `status == "ok"`.
async fn health(State(s): S) -> Response {
    let mut problems: Vec<String> = Vec::new();
    let last_block: Option<String> =
        match sqlx::query_as::<_, (String,)>("SELECT value FROM indexer_state WHERE key = 'last_block'")
            .fetch_optional(&s.pool)
            .await
        {
            Ok(r) => r.map(|(v,)| v),
            Err(_) => {
                problems.push("database unreachable".into());
                None
            }
        };
    let skipped: i64 = sqlx::query_as::<_, (i64,)>("SELECT count(*) FROM skipped_ranges")
        .fetch_one(&s.pool)
        .await
        .map(|(n,)| n)
        .unwrap_or(0);
    let markets = s.live.read().await.len();
    let live_at = s.live_at.load(std::sync::atomic::Ordering::Relaxed);
    let live_age_ms = (live_at > 0).then(|| now_ms().saturating_sub(live_at));
    match live_age_ms {
        None => problems.push("market state never loaded".into()),
        Some(age) if age > 15_000 => problems.push(format!("market state is {} s old", age / 1000)),
        _ => {}
    }
    // The keeper runs beside the indexer; its status is the only place a
    // stalled price feed or an empty gas account becomes visible.
    let mut keeper = Value::Null;
    if let Some(base) = &s.keeper_url {
        let r = s
            .http
            .get(format!("{}/status", base.trim_end_matches('/')))
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await;
        match r {
            Ok(r) if r.status().is_success() => keeper = r.json().await.unwrap_or(Value::Null),
            _ => problems.push("keeper not answering".into()),
        }
        let now = keeper["now_ms"].as_u64().unwrap_or(0);
        for m in keeper["markets"].as_array().into_iter().flatten() {
            let stuck = now.saturating_sub(m["advanced_at_ms"].as_u64().unwrap_or(now));
            if m["mode"] == "Active" && stuck > 30_000 {
                problems.push(format!("market {} price has not updated for {} s", m["id"], stuck / 1000));
            }
        }
    }
    Json(json!({
        "ok": true,
        "status": if problems.is_empty() { "ok" } else { "degraded" },
        "problems": problems,
        "contract": s.chain.contract_id(),
        "last_indexed_block": last_block,
        "skipped_ranges": skipped,
        "live_markets": markets,
        "live_age_ms": live_age_ms,
        "keeper": keeper,
        "faucet": s.keeper_url.is_some(),
    }))
    .into_response()
}

async fn config(State(s): S) -> Response {
    let testnet = s.chain.contract_id().ends_with(".testnet");
    Json(json!({
        "network": if testnet { "testnet" } else { "mainnet" },
        "contract": s.chain.contract_id(),
        "quote_token": std::env::var("NEAP_QUOTE_TOKEN").unwrap_or_else(|_| {
            if testnet { "usdc.fakes.testnet".into() } else { "17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1".into() }
        }),
        "quote_decimals": 6,
        "price_decimals": 6,
        "faucet": s.keeper_url.is_some(),
    }))
    .into_response()
}

// ── Markets ──

#[derive(sqlx::FromRow)]
pub struct Stats24 {
    market_id: i32,
    last_price: Option<i64>,
    open_24h: Option<i64>,
    high_24h: Option<i64>,
    low_24h: Option<i64>,
    trades_24h: Option<i64>,
}

/// 24h stats for every market, recomputed at most every five seconds.
async fn stats_24h(s: &AppState) -> Arc<Vec<Stats24>> {
    let now = std::time::Instant::now();
    if let Some((at, v)) = s.guards.stats.lock().unwrap().as_ref() {
        if now.duration_since(*at) < std::time::Duration::from_secs(5) {
            return v.clone();
        }
    }
    let fresh = Arc::new(query_stats_24h(&s.pool).await);
    *s.guards.stats.lock().unwrap() = Some((now, fresh.clone()));
    fresh
}

async fn query_stats_24h(pool: &PgPool) -> Vec<Stats24> {
    sqlx::query_as::<_, Stats24>(
        "WITH o AS (
            SELECT market_id, price_e6, block FROM observations WHERE ts > now() - interval '24 hours'
         )
         SELECT m.id AS market_id,
           (SELECT price_e6 FROM observations x WHERE x.market_id = m.id ORDER BY block DESC, id DESC LIMIT 1) AS last_price,
           (SELECT price_e6 FROM o WHERE o.market_id = m.id ORDER BY block ASC LIMIT 1) AS open_24h,
           (SELECT max(price_e6) FROM o WHERE o.market_id = m.id) AS high_24h,
           (SELECT min(price_e6) FROM o WHERE o.market_id = m.id) AS low_24h,
           (SELECT count(*) FROM fills f WHERE f.market_id = m.id AND f.ts > now() - interval '24 hours') AS trades_24h
         FROM markets m",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default()
}

fn merge_stats(mut v: Value, st: Option<&Stats24>) -> Value {
    if let (Value::Object(ref mut o), Some(st)) = (&mut v, st) {
        let change_bps = match (st.last_price, st.open_24h) {
            (Some(l), Some(op)) if op > 0 => Some((l - op) * 10_000 / op),
            _ => None,
        };
        o.insert(
            "stats_24h".into(),
            json!({
                "open": st.open_24h, "high": st.high_24h, "low": st.low_24h,
                "last": st.last_price, "change_bps": change_bps, "trades": st.trades_24h,
            }),
        );
    }
    v
}

async fn list_markets(State(s): S) -> Response {
    let live = s.live.read().await.clone();
    let stats = stats_24h(&s).await;
    let out: Vec<Value> = live
        .into_iter()
        .enumerate()
        .map(|(i, v)| merge_stats(v, stats.iter().find(|x| x.market_id == i as i32)))
        .collect();
    Json(out).into_response()
}

async fn get_market(State(s): S, Path(id): Path<u32>) -> Response {
    let cached = s.live.read().await.get(id as usize).cloned();
    let v = match cached {
        Some(v) => v,
        None => match cached_view(&s, "get_market", &json!({ "market": id })).await {
            Ok(mut v) => {
                if let Value::Object(ref mut o) = v {
                    o.insert("id".into(), json!(id));
                }
                v
            }
            Err(_) => return err(StatusCode::NOT_FOUND, "no such market"),
        },
    };
    let stats = stats_24h(&s).await;
    Json(merge_stats(v, stats.iter().find(|x| x.market_id == id as i32))).into_response()
}

#[derive(sqlx::FromRow, Serialize)]
struct CandleRow {
    time: i64, // unix seconds, bucket start
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    samples: i64,
}

/// OHLC candles built from price observations (oldest first, ready for charting).
async fn get_candles(State(s): S, Path(id): Path<i32>, Query(q): Query<CandleQuery>) -> Response {
    let interval = q.interval.unwrap_or(60).clamp(5, 86_400);
    let limit = q.limit.unwrap_or(300).clamp(1, 1000);
    let rows = sqlx::query_as::<_, CandleRow>(
        "SELECT * FROM (
           SELECT extract(epoch FROM b)::bigint AS time,
                  (array_agg(price_e6 ORDER BY block, id))[1] AS open,
                  max(price_e6) AS high,
                  min(price_e6) AS low,
                  (array_agg(price_e6 ORDER BY block DESC, id DESC))[1] AS close,
                  count(*) AS samples
           FROM (SELECT id, price_e6, block,
                        to_timestamp(floor(extract(epoch FROM ts) / $2) * $2) AS b
                 FROM observations
                 WHERE market_id = $1 AND ts >= now() - make_interval(secs => $2 * ($3 + 1))) t
           GROUP BY b ORDER BY b DESC LIMIT $3
         ) c ORDER BY time ASC",
    )
    .bind(id)
    .bind(interval as f64)
    .bind(limit)
    .fetch_all(&s.pool)
    .await;
    match rows {
        Ok(r) => Json(json!({ "market": id, "interval": interval, "candles": r })).into_response(),
        Err(e) => db_err(e),
    }
}

#[derive(sqlx::FromRow, Serialize)]
struct ObsRow {
    price_e6: i64,
    ema_e6: Option<i64>,
    seq: i64,
    block: i64,
    ts: chrono::DateTime<chrono::Utc>,
}

async fn get_observations(State(s): S, Path(id): Path<i32>, Query(p): Query<Pagination>) -> Response {
    sqlx::query_as::<_, ObsRow>(
        "SELECT price_e6, ema_e6, seq, block, ts FROM observations
         WHERE market_id = $1 ORDER BY block DESC, id DESC LIMIT $2 OFFSET $3",
    )
    .bind(id)
    .bind(p.lim())
    .bind(p.off())
    .fetch_all(&s.pool)
    .await
    .map(|r| Json(r).into_response())
    .unwrap_or_else(db_err)
}

// ── History tables ──

#[derive(sqlx::FromRow, Serialize)]
struct FillRow {
    id: i64,
    market_id: i32,
    owner: String,
    order_id: Option<i64>,
    side: Option<String>,
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
struct CondRow {
    id: i64,
    market_id: i32,
    owner: String,
    kind: String,
    side: String,
    size_q: String,
    trigger_price: i64,
    status: String,
    fill_price: Option<i64>,
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

const FILL_COLS: &str = "id, market_id, owner, order_id, side, size_q, price, fee, block, ts";
const ORDER_COLS: &str = "id, market_id, owner, side, size_q, limit_price, status, fill_price, fee, request_block, created_at, updated_at";
const COND_COLS: &str = "id, market_id, owner, kind, side, size_q, trigger_price, status, fill_price, created_at, updated_at";
const LIQ_COLS: &str = "id, market_id, account, caller, closed_q, fee, block, ts";

macro_rules! list_handler {
    ($name:ident, $row:ty, $cols:expr, $table:expr, $key:expr, $keyty:ty, $order:expr, $has_status:expr) => {
        async fn $name(State(s): S, Path(k): Path<$keyty>, Query(p): Query<Pagination>) -> Response {
            let status_filter = if $has_status && p.status.is_some() { " AND status = $4" } else { "" };
            let sql = format!(
                "SELECT {} FROM {} WHERE {} = $1{} ORDER BY {} LIMIT $2 OFFSET $3",
                $cols, $table, $key, status_filter, $order
            );
            let mut q = sqlx::query_as::<_, $row>(&sql).bind(k).bind(p.lim()).bind(p.off());
            if $has_status {
                if let Some(st) = &p.status {
                    q = q.bind(st.clone());
                }
            }
            q.fetch_all(&s.pool)
                .await
                .map(|r| Json(r).into_response())
                .unwrap_or_else(db_err)
        }
    };
}

list_handler!(get_fills, FillRow, FILL_COLS, "fills", "market_id", i32, "block DESC, id DESC", false);
list_handler!(get_orders, OrderRow, ORDER_COLS, "orders", "market_id", i32, "id DESC", true);
list_handler!(get_conditionals, CondRow, COND_COLS, "conditionals", "market_id", i32, "id DESC", true);
list_handler!(get_liquidations, LiqRow, LIQ_COLS, "liquidations", "market_id", i32, "block DESC, id DESC", false);
list_handler!(get_account_fills, FillRow, FILL_COLS, "fills", "owner", String, "block DESC, id DESC", false);
list_handler!(get_account_orders, OrderRow, ORDER_COLS, "orders", "owner", String, "id DESC", true);
list_handler!(get_account_conditionals, CondRow, COND_COLS, "conditionals", "owner", String, "id DESC", true);
list_handler!(get_account_liquidations, LiqRow, LIQ_COLS, "liquidations", "account", String, "block DESC, id DESC", false);

/// Every account the indexer has seen trade (the keeper's liquidation scan list).
async fn list_accounts(State(s): S) -> Response {
    sqlx::query_as::<_, (String, i32)>(
        "SELECT DISTINCT owner, market_id FROM orders WHERE owner <> ''
         UNION SELECT DISTINCT owner, market_id FROM fills
         UNION SELECT DISTINCT owner, market_id FROM conditionals",
    )
    .fetch_all(&s.pool)
    .await
    .map(|r| {
        Json(
            r.into_iter()
                .map(|(o, m)| json!({ "owner": o, "market": m }))
                .collect::<Vec<_>>(),
        )
        .into_response()
    })
    .unwrap_or_else(db_err)
}

// ── Live chain reads ──

/// Wallet balance plus the live position in every market.
async fn get_account(State(s): S, Path(owner): Path<String>) -> Response {
    if !is_account_id(&owner) {
        return err(StatusCode::BAD_REQUEST, "not a NEAR account id");
    }
    let wallet = cached_view(&s, "get_wallet", &json!({ "account": owner })).await.unwrap_or(Value::Null);
    let n = s.live.read().await.len();
    // One market after another would hold the request for seconds: read them together.
    let reads = (0..n).map(|m| {
        let (s, owner) = (&s, &owner);
        async move {
            let args = json!({ "market": m, "account": owner });
            let (acc, lp) =
                tokio::join!(cached_view(s, "get_account", &args), cached_view(s, "get_lp_shares", &args));
            (m, acc.unwrap_or(Value::Null), lp.unwrap_or(Value::Null))
        }
    });
    let mut positions = Vec::new();
    for (m, acc, lp) in futures::future::join_all(reads).await {
        if !acc.is_null() || lp.as_str().map(|x| x != "0").unwrap_or(false) {
            positions.push(json!({ "market": m, "account": acc, "lp_shares": lp }));
        }
    }
    Json(json!({ "owner": owner, "wallet": wallet, "markets": positions })).into_response()
}

#[derive(Deserialize)]
struct QuoteQuery {
    side: String,
    size_q: String,
}

async fn get_quote(State(s): S, Path(id): Path<u32>, Query(q): Query<QuoteQuery>) -> Response {
    if q.size_q.len() > 40 || !q.size_q.bytes().all(|b| b.is_ascii_digit()) || !matches!(q.side.as_str(), "Long" | "Short") {
        return err(StatusCode::BAD_REQUEST, "side must be Long or Short and size_q a positive integer");
    }
    match cached_view(&s, "quote", &json!({ "market": id, "side": q.side, "size_q": q.size_q })).await {
        Ok(Value::Array(a)) if a.len() == 3 => Json(json!({
            "exec_price": a[0], "spread_bps": a[1], "fee": a[2]
        }))
        .into_response(),
        Ok(v) => Json(v).into_response(),
        Err(e) => chain_err(e),
    }
}

const VIEWS: &[&str] = &[
    "get_market", "get_markets_count", "get_account", "get_wallet", "get_lp_shares",
    "get_custody", "get_next_order_id", "get_order", "get_conditional", "get_fee_ledger",
    "quote", "scan_order_ids", "scan_conditional_ids",
];

#[derive(Deserialize)]
struct ViewQuery {
    args: Option<String>,
}

/// `GET /view/get_account?args={"market":0,"account":"alice.testnet"}`
async fn view_proxy(State(s): S, Path(method): Path<String>, Query(q): Query<ViewQuery>) -> Response {
    if !VIEWS.contains(&method.as_str()) {
        return err(StatusCode::NOT_FOUND, format!("view not allowed; use one of {VIEWS:?}"));
    }
    let args: Value = match q.args.as_deref().map(serde_json::from_str).transpose() {
        Ok(v) => v.unwrap_or_else(|| json!({})),
        Err(e) => return err(StatusCode::BAD_REQUEST, format!("args must be JSON: {e}")),
    };
    if q.args.as_deref().map(str::len).unwrap_or(0) > 512 {
        return err(StatusCode::BAD_REQUEST, "args too long");
    }
    match cached_view(&s, &method, &args).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => chain_err(e),
    }
}

// ── Report a transaction so its events are indexed right away ──

async fn report_tx(State(s): S, Json(body): Json<Value>) -> Response {
    let hash = body["hash"].as_str().unwrap_or("").trim().to_string();
    let signer = body["signer_id"].as_str().unwrap_or("").trim().to_string();
    let b58 = |x: &str| x.chars().all(|c| c.is_ascii_alphanumeric());
    if !(32..=44).contains(&hash.len()) || !b58(&hash) || !is_account_id(&signer) {
        return err(StatusCode::BAD_REQUEST, "expected {hash, signer_id}");
    }
    // The same hash reported twice (page reload, retry) is looked up once.
    if !s.guards.in_flight.lock().unwrap().insert(hash.clone()) {
        return (StatusCode::ACCEPTED, Json(json!({ "ok": true }))).into_response();
    }
    let Ok(permit) = s.ingest.clone().try_acquire_owned() else {
        s.guards.in_flight.lock().unwrap().remove(&hash);
        return err(StatusCode::TOO_MANY_REQUESTS, "busy, retry shortly");
    };
    let s = s.clone();
    tokio::spawn(async move {
        let _permit = permit;
        match crate::ingest::ingest_tx(&s.chain, &s.pool, &s.tx, &hash, &signer).await {
            Ok(n) => tracing::info!(hash, events = n, "transaction indexed"),
            Err(e) => tracing::warn!(hash, err = %e, "transaction not indexed"),
        }
        s.guards.in_flight.lock().unwrap().remove(&hash);
    });
    (StatusCode::ACCEPTED, Json(json!({ "ok": true }))).into_response()
}

// ── Faucet (forwarded to the keeper, which holds the signing key) ──

async fn faucet(
    State(s): S,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let Some(base) = &s.keeper_url else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "faucet disabled");
    };
    let ip = client_ip(
        headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()),
        addr.ip(),
        s.guards.trusted_hops,
    );
    let resp = s
        .http
        .post(format!("{}/faucet", base.trim_end_matches('/')))
        .header("x-client-ip", ip)
        .json(&body)
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;
    match resp {
        Ok(r) => {
            let code = StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let v: Value = r.json().await.unwrap_or(Value::Null);
            (code, Json(v)).into_response()
        }
        Err(e) => {
            tracing::warn!(err = %e, "faucet forward failed");
            err(StatusCode::BAD_GATEWAY, "faucet unavailable, try again later")
        }
    }
}

// ── WebSocket ──

async fn ws_handler(State(s): S, ws: WebSocketUpgrade) -> Response {
    // Each socket holds a task and a broadcast receiver: cap them.
    let Ok(permit) = s.guards.ws.clone().try_acquire_owned() else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "too many live connections");
    };
    // Clients only ever send pings; nothing large is expected inbound.
    ws.max_message_size(4096)
        .max_frame_size(4096)
        .on_upgrade(move |socket| async move {
            handle_ws(socket, s).await;
            drop(permit);
        })
}

const WS_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Send with a deadline: a client that stops reading must not pin its task.
async fn ws_send(socket: &mut axum::extract::ws::WebSocket, m: Message) -> bool {
    matches!(tokio::time::timeout(WS_SEND_TIMEOUT, socket.send(m)).await, Ok(Ok(())))
}

/// Server → client JSON messages: `{"event": "...", "data": {...}, ...}`.
/// On connect the client gets a `snapshot` with every market's live state.
/// `{"event":"resync"}` means this client fell behind and missed messages:
/// refetch over REST.
async fn handle_ws(mut socket: axum::extract::ws::WebSocket, s: Arc<AppState>) {
    let mut rx = s.tx.subscribe();
    let snap = json!({ "event": "snapshot", "data": *s.live.read().await }).to_string();
    if !ws_send(&mut socket, Message::Text(snap.into())).await {
        return;
    }
    let mut ping = tokio::time::interval(std::time::Duration::from_secs(25));
    loop {
        tokio::select! {
            msg = rx.recv() => match msg {
                Ok(m) => {
                    if !ws_send(&mut socket, Message::Text(m.into())).await { break; }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let m = json!({ "event": "resync" }).to_string();
                    if !ws_send(&mut socket, Message::Text(m.into())).await { break; }
                }
                Err(_) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => {}
            },
            _ = ping.tick() => {
                if !ws_send(&mut socket, Message::Ping(Vec::new().into())).await { break; }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_live_account;

    #[test]
    fn only_the_live_account_route_counts_as_a_chain_read() {
        assert!(is_live_account("/account/alice.testnet"));
        assert!(!is_live_account("/account/alice.testnet/fills"));
        assert!(!is_live_account("/account/"));
        assert!(!is_live_account("/accounts"));
    }
}
