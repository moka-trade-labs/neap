//! Database operations: write parsed events into Postgres.

use crate::events::NeapEvent;
use anyhow::Result;
use sqlx::PgPool;
use tracing::{info, warn};

/// Get the last indexed block height (or 0).
pub async fn get_last_block(pool: &PgPool) -> Result<u64> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT value FROM indexer_state WHERE key = 'last_block'")
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(v,)| v.parse().ok()).unwrap_or(0))
}

/// Update the last indexed block height.
pub async fn set_last_block(pool: &PgPool, block: u64) -> Result<()> {
    sqlx::query(
        "INSERT INTO indexer_state (key, value) VALUES ('last_block', $1)
         ON CONFLICT (key) DO UPDATE SET value = $1",
    )
    .bind(block.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// Process a single neap event and write it to the database.
pub async fn process_event(pool: &PgPool, ev: &NeapEvent, block: u64) -> Result<()> {
    match ev.event.as_str() {
        "market_created" => {
            let id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let symbol = ev.data["symbol"].as_str().unwrap_or("");
            let tier = ev.data["tier"].as_str().unwrap_or("");
            let lister = ev.data["lister"].as_str().unwrap_or("");
            let bond: i64 = ev.data["bond"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            sqlx::query(
                "INSERT INTO markets (id, symbol, tier, mode, lister, bond)
                 VALUES ($1, $2, $3, 'Proposed', $4, $5)
                 ON CONFLICT (id) DO UPDATE SET symbol=$2, tier=$3, lister=$4, bond=$5",
            )
            .bind(id)
            .bind(symbol)
            .bind(tier)
            .bind(lister)
            .bind(bond)
            .execute(pool)
            .await?;
            info!(market = id, symbol, "market_created");
        }
        "market_activated" => {
            let id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            sqlx::query("UPDATE markets SET mode = 'Active' WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
            info!(market = id, "market_activated");
        }
        "market_mode_changed" => {
            let id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let mode = ev.data["mode"].as_str().unwrap_or("");
            sqlx::query("UPDATE markets SET mode = $2 WHERE id = $1")
                .bind(id)
                .bind(mode)
                .execute(pool)
                .await?;
            info!(market = id, mode, "market_mode_changed");
        }
        "price_observed" => {
            let market_id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let price_e6: i64 = ev.data["price_e6"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let ema_e6: i64 = ev.data["ema_e6"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let liquidity: i64 = ev.data["liquidity_usd"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let seq: i64 = ev.data["seq"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            sqlx::query(
                "INSERT INTO observations (market_id, price_e6, ema_e6, liquidity, seq, block)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(market_id)
            .bind(price_e6)
            .bind(ema_e6)
            .bind(liquidity)
            .bind(seq)
            .bind(block as i64)
            .execute(pool)
            .await?;
        }
        "order_requested" => {
            let id: i64 = ev.data["id"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let market_id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let owner = ev.data["owner"].as_str().unwrap_or("");
            let side = ev.data["side"].as_str().unwrap_or("");
            let size_q = ev.data["size_q"].as_str().unwrap_or("0");
            let limit_price: i64 = ev.data["limit_price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            // Ensure market exists (for testnet where we might miss market_created).
            sqlx::query(
                "INSERT INTO markets (id, symbol, tier, mode, lister) VALUES ($1, '', '', 'Active', '')
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(market_id)
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO orders (id, market_id, owner, side, size_q, limit_price, status, request_block)
                 VALUES ($1, $2, $3, $4, $5, $6, 'pending', $7)
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(id)
            .bind(market_id)
            .bind(owner)
            .bind(side)
            .bind(size_q)
            .bind(limit_price)
            .bind(block as i64)
            .execute(pool)
            .await?;
        }
        "fill" => {
            let market_id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let owner = ev.data["owner"].as_str().unwrap_or("");
            let order_id: Option<i64> = ev.data["order_id"]
                .as_str()
                .and_then(|s| s.parse().ok());
            let size_q = ev.data["size_q"].as_str().unwrap_or("0");
            let price: i64 = ev.data["price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let fee = ev.data["fee"].as_str().unwrap_or("0");
            sqlx::query(
                "INSERT INTO fills (market_id, owner, order_id, size_q, price, fee, block)
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(market_id)
            .bind(owner)
            .bind(order_id)
            .bind(size_q)
            .bind(price)
            .bind(fee)
            .bind(block as i64)
            .execute(pool)
            .await?;
            // Mark order as filled.
            if let Some(oid) = order_id {
                sqlx::query(
                    "UPDATE orders SET status='filled', fill_price=$2, fee=$3, updated_at=now()
                     WHERE id=$1",
                )
                .bind(oid)
                .bind(price)
                .bind(fee)
                .execute(pool)
                .await?;
            }
            info!(market = market_id, owner, price, "fill");
        }
        "order_cancelled" => {
            let id: i64 = ev.data["id"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            sqlx::query("UPDATE orders SET status='cancelled', updated_at=now() WHERE id=$1")
                .bind(id)
                .execute(pool)
                .await?;
        }
        "order_expired" => {
            let id: i64 = ev.data["id"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            sqlx::query("UPDATE orders SET status='expired', updated_at=now() WHERE id=$1")
                .bind(id)
                .execute(pool)
                .await?;
        }
        "conditional_placed" => {
            let id: i64 = ev.data["id"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let market_id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let owner = ev.data["owner"].as_str().unwrap_or("");
            let kind = ev.data["kind"].as_str().unwrap_or("");
            let side = ev.data["side"].as_str().unwrap_or("");
            let size_q = ev.data["size_q"].as_str().unwrap_or("0");
            let trigger_price: i64 = ev.data["trigger_price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            sqlx::query(
                "INSERT INTO conditionals (id, market_id, owner, kind, side, size_q, trigger_price, status)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending')
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(id)
            .bind(market_id)
            .bind(owner)
            .bind(kind)
            .bind(side)
            .bind(size_q)
            .bind(trigger_price)
            .execute(pool)
            .await?;
        }
        "conditional_triggered" => {
            let id: i64 = ev.data["id"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let fill_price: i64 = ev.data["fill_price"]
                .as_str()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            sqlx::query(
                "UPDATE conditionals SET status='triggered', fill_price=$2, updated_at=now() WHERE id=$1",
            )
            .bind(id)
            .bind(fill_price)
            .execute(pool)
            .await?;
        }
        "liquidation" => {
            let market_id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let account = ev.data["account"].as_str().unwrap_or("");
            let caller = ev.data["caller"].as_str().unwrap_or("");
            let closed_q = ev.data["closed_q"].as_str().unwrap_or("0");
            let fee = ev.data["fee"].as_str().unwrap_or("0");
            sqlx::query(
                "INSERT INTO liquidations (market_id, account, caller, closed_q, fee, block)
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(market_id)
            .bind(account)
            .bind(caller)
            .bind(closed_q)
            .bind(fee)
            .bind(block as i64)
            .execute(pool)
            .await?;
            info!(market = market_id, account, "liquidation");
        }
        "fees_claimed" => {
            let market_id = ev.data["market"].as_u64().unwrap_or(0) as i32;
            let payee = ev.data["payee"].as_str().unwrap_or("");
            let owner = ev.data["owner"].as_str().unwrap_or("");
            let amount = ev.data["amount"].as_str().unwrap_or("0");
            sqlx::query(
                "INSERT INTO fee_claims (market_id, payee, owner, amount, block)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(market_id)
            .bind(payee)
            .bind(owner)
            .bind(amount)
            .bind(block as i64)
            .execute(pool)
            .await?;
        }
        other => {
            // Ignore unknown events gracefully.
            warn!(event = other, "unknown neap event");
        }
    }
    Ok(())
}
