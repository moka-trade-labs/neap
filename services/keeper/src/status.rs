//! What the keeper last saw, for `GET /status` on the internal server and for
//! the watchdog log lines. Holds no key material.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Clone, Serialize)]
pub struct MarketStatus {
    pub id: u32,
    pub symbol: String,
    pub mode: String,
    pub observation_seq: u64,
    /// Unix ms when the keeper last saw `observation_seq` increase.
    pub advanced_at_ms: u64,
}

#[derive(Default)]
pub struct Status {
    started_at_ms: u64,
    markets: Mutex<HashMap<u32, MarketStatus>>,
    balance: Mutex<Option<(u128, u64)>>,
    price_refusals: std::sync::atomic::AtomicU64,
}

impl Status {
    pub fn new() -> Self {
        Self { started_at_ms: now_ms(), ..Default::default() }
    }

    /// Record a market's observation. Returns how long (ms) it has been stuck
    /// at the same sequence number, 0 when it just advanced.
    pub fn observe(&self, id: u32, symbol: &str, mode: &str, seq: u64, now: u64) -> u64 {
        let mut m = self.markets.lock().unwrap();
        let e = m.entry(id).or_insert_with(|| MarketStatus {
            id,
            symbol: symbol.to_string(),
            mode: mode.to_string(),
            observation_seq: seq,
            advanced_at_ms: now,
        });
        e.mode = mode.to_string();
        e.symbol = symbol.to_string();
        if seq != e.observation_seq {
            e.observation_seq = seq;
            e.advanced_at_ms = now;
        }
        now.saturating_sub(e.advanced_at_ms)
    }

    pub fn set_balance(&self, yocto: u128) {
        *self.balance.lock().unwrap() = Some((yocto, now_ms()));
    }

    pub fn price_refused(&self) {
        self.price_refusals.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn json(&self) -> serde_json::Value {
        let mut markets: Vec<MarketStatus> = self.markets.lock().unwrap().values().cloned().collect();
        markets.sort_by_key(|m| m.id);
        let balance = *self.balance.lock().unwrap();
        serde_json::json!({
            "started_at_ms": self.started_at_ms,
            "now_ms": now_ms(),
            "markets": markets,
            "signer_balance_yocto": balance.map(|(b, _)| b.to_string()),
            "signer_balance_read_at_ms": balance.map(|(_, t)| t),
            "price_refusals": self.price_refusals.load(std::sync::atomic::Ordering::Relaxed),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stuck_time_resets_when_the_sequence_advances() {
        let s = Status::new();
        assert_eq!(s.observe(0, "NEAR-PERP", "Active", 7, 1_000), 0);
        assert_eq!(s.observe(0, "NEAR-PERP", "Active", 7, 31_000), 30_000);
        assert_eq!(s.observe(0, "NEAR-PERP", "Active", 8, 32_000), 0);
        assert_eq!(s.json()["markets"][0]["observation_seq"], 8);
    }
}
