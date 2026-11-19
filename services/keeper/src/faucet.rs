//! Testnet faucet: registers an account on the test USDC contract and mints
//! it test USDC (and optionally a little NEAR for gas). Listens on an
//! internal address; the public API forwards `POST /faucet` here.
//!
//! Rate limits (in memory): one drip per account per hour, five per IP per hour.

use crate::config::Config;
use crate::rpc::NearRpc;
use crate::status::Status;
use anyhow::Result;
use axum::{extract::State, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::{get, post}, Json, Router};
use near_primitives::types::AccountId;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

const WINDOW: Duration = Duration::from_secs(3600);
const PER_IP: usize = 5;

struct Faucet {
    rpc: Arc<NearRpc>,
    cfg: Config,
    by_account: Mutex<HashMap<String, Instant>>,
    by_ip: Mutex<HashMap<String, Vec<Instant>>>,
}

pub async fn serve(addr: String, rpc: Arc<NearRpc>, cfg: Config, status: Arc<Status>) -> Result<()> {
    // The faucet signs with the oracle key: it only exists on testnet.
    let faucet_on = cfg.contract_id.ends_with(".testnet");
    let state = Arc::new(Faucet {
        rpc,
        cfg,
        by_account: Mutex::new(HashMap::new()),
        by_ip: Mutex::new(HashMap::new()),
    });
    let mut app = Router::new().route("/status", get(move || { let s = status.clone(); async move { Json(s.json()) } }));
    if faucet_on {
        app = app.route("/faucet", post(drip));
    }
    let app = app.with_state(state);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(addr, faucet = faucet_on, "internal server listening (/status, /faucet)");
    axum::serve(listener, app).await?;
    Ok(())
}

fn bad(code: StatusCode, msg: impl ToString) -> axum::response::Response {
    (code, Json(json!({ "ok": false, "error": msg.to_string() }))).into_response()
}

async fn drip(State(f): State<Arc<Faucet>>, headers: HeaderMap, Json(body): Json<Value>) -> axum::response::Response {
    let Some(account) = body["account_id"].as_str().map(|s| s.trim().to_lowercase()) else {
        return bad(StatusCode::BAD_REQUEST, "body must be {\"account_id\": \"you.testnet\"}");
    };
    let Ok(account_id) = account.parse::<AccountId>() else {
        return bad(StatusCode::BAD_REQUEST, "invalid NEAR account id");
    };
    let ip = headers.get("x-client-ip").and_then(|h| h.to_str().ok()).unwrap_or("unknown").to_string();

    // Rate limits. The slot is taken here, before any transaction is sent, so
    // parallel requests cannot all pass the check; it is given back on failure.
    let reserved = Instant::now();
    {
        let now = reserved;
        let mut by_acc = f.by_account.lock().await;
        let mut by_ip = f.by_ip.lock().await;
        by_acc.retain(|_, t| now.duration_since(*t) < WINDOW);
        by_ip.retain(|_, v| {
            v.retain(|t| now.duration_since(*t) < WINDOW);
            !v.is_empty()
        });
        if let Some(t) = by_acc.get(&account) {
            let wait = WINDOW - now.duration_since(*t);
            return bad(StatusCode::TOO_MANY_REQUESTS, format!("already funded; try again in {} min", wait.as_secs() / 60 + 1));
        }
        if by_ip.get(&ip).map(|v| v.len()).unwrap_or(0) >= PER_IP {
            return bad(StatusCode::TOO_MANY_REQUESTS, "too many faucet requests from this IP; try later");
        }
        by_acc.insert(account.clone(), now);
        by_ip.entry(ip.clone()).or_default().push(now);
    }
    let release = || async {
        f.by_account.lock().await.remove(&account);
        if let Some(v) = f.by_ip.lock().await.get_mut(&ip) {
            v.retain(|t| *t != reserved);
        }
    };

    match f.rpc.account_exists(&account).await {
        Ok(true) => {}
        Ok(false) => {
            release().await;
            return bad(StatusCode::BAD_REQUEST, "account does not exist on chain yet; create it in your wallet first");
        }
        Err(e) => {
            release().await;
            warn!(account, err = %e, "faucet: account lookup failed");
            return bad(StatusCode::BAD_GATEWAY, "could not reach the network, try again later");
        }
    }

    let token: AccountId = match f.cfg.faucet_token.parse() {
        Ok(t) => t,
        Err(_) => {
            release().await;
            return bad(StatusCode::INTERNAL_SERVER_ERROR, "faucet misconfigured");
        }
    };
    let calls = vec![
        (
            "storage_deposit".to_string(),
            json!({ "account_id": account, "registration_only": true }),
            1_250_000_000_000_000_000_000u128, // 0.00125 NEAR
        ),
        (
            "mint".to_string(),
            json!({ "account_id": account, "amount": f.cfg.faucet_amount.to_string() }),
            0,
        ),
    ];
    let usdc_tx = match f.rpc.call_many(&token, calls, 30_000_000_000_000).await {
        Ok(h) => h,
        Err(e) => {
            release().await;
            // RPC error text can carry provider URLs: log it, don't return it.
            warn!(account, err = %e, "faucet: mint failed");
            return bad(StatusCode::BAD_GATEWAY, "mint failed, try again later");
        }
    };

    let mut near_tx = None;
    if f.cfg.faucet_near_yocto > 0 {
        if let Ok(v) = f.rpc.rpc_call("query", json!({"request_type":"view_account","finality":"final","account_id":account})).await {
            let bal: u128 = v["amount"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
            if bal < 500_000_000_000_000_000_000_000 {
                near_tx = f.rpc.transfer(&account_id, f.cfg.faucet_near_yocto).await.ok();
            }
        }
    }

    info!(account, usdc_tx, "faucet drip");
    Json(json!({
        "ok": true,
        "account_id": account,
        "token": f.cfg.faucet_token,
        "amount": f.cfg.faucet_amount.to_string(),
        "decimals": 6,
        "usdc_tx": usdc_tx,
        "near_tx": near_tx,
    }))
    .into_response()
}
