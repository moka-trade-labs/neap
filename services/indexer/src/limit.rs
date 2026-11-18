//! Small in-memory guards for a public API on one instance: a per-client
//! fixed-window rate limiter, a short-lived response cache for contract views,
//! and the client address behind the host's proxy.

use serde_json::Value;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Limiter {
    window: Duration,
    max: u32,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl Limiter {
    pub fn per_minute(max: u32) -> Self {
        Self { window: Duration::from_secs(60), max, hits: Mutex::new(HashMap::new()) }
    }

    /// Count one request from `key`. False when the window's budget is spent.
    pub fn allow(&self, key: &str, now: Instant) -> bool {
        let mut hits = self.hits.lock().unwrap();
        if hits.len() > 20_000 {
            hits.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        }
        let e = hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(e.0) >= self.window {
            *e = (now, 0);
        }
        e.1 += 1;
        e.1 <= self.max
    }
}

/// Results of contract views, kept for a moment so many browsers asking the
/// same thing cost one RPC call. The RPC quota is shared with the keeper.
pub struct ViewCache {
    ttl: Duration,
    items: Mutex<HashMap<String, (Instant, Value)>>,
}

impl ViewCache {
    pub fn new(ttl: Duration) -> Self {
        Self { ttl, items: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, key: &str, now: Instant) -> Option<Value> {
        let items = self.items.lock().unwrap();
        items.get(key).filter(|(at, _)| now.duration_since(*at) < self.ttl).map(|(_, v)| v.clone())
    }

    pub fn put(&self, key: String, v: Value, now: Instant) {
        let mut items = self.items.lock().unwrap();
        if items.len() > 5_000 {
            items.retain(|_, (at, _)| now.duration_since(*at) < self.ttl);
        }
        items.insert(key, (now, v));
    }
}

/// The caller's address. Behind a proxy the socket peer is the proxy, so the
/// address comes from `X-Forwarded-For`. Its leftmost entry is whatever the
/// client sent; each proxy appends the address it saw. With `trusted_hops`
/// set to the number of proxies in front of the service, the entry that many
/// from the right is used, which a client cannot forge. Unset keeps the old
/// behaviour (leftmost entry).
pub fn client_ip(forwarded_for: Option<&str>, peer: IpAddr, trusted_hops: Option<usize>) -> String {
    let parts: Vec<&str> = forwarded_for
        .map(|h| h.split(',').map(str::trim).filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let picked = match trusted_hops {
        Some(n) if n >= 1 => parts.len().checked_sub(n).and_then(|i| parts.get(i)),
        Some(_) => None,
        None => parts.first(),
    };
    picked.map(|s| s.to_string()).unwrap_or_else(|| peer.to_string())
}

/// A plausible NEAR account id (named or implicit). Cheap filter before RPC.
pub fn is_account_id(s: &str) -> bool {
    (2..=64).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_blocks_over_budget_and_recovers() {
        let l = Limiter::per_minute(2);
        let t = Instant::now();
        assert!(l.allow("a", t));
        assert!(l.allow("a", t));
        assert!(!l.allow("a", t));
        assert!(l.allow("b", t), "other clients are unaffected");
        assert!(l.allow("a", t + Duration::from_secs(61)), "a new window starts");
    }

    #[test]
    fn cache_expires() {
        let c = ViewCache::new(Duration::from_secs(2));
        let t = Instant::now();
        c.put("k".into(), serde_json::json!(1), t);
        assert_eq!(c.get("k", t + Duration::from_secs(1)), Some(serde_json::json!(1)));
        assert_eq!(c.get("k", t + Duration::from_secs(3)), None);
    }

    #[test]
    fn client_ip_cannot_be_forged_when_hops_are_set() {
        let peer: IpAddr = "10.0.0.1".parse().unwrap();
        let xff = Some("6.6.6.6, 203.0.113.9, 172.16.0.2");
        assert_eq!(client_ip(xff, peer, None), "6.6.6.6"); // legacy: client-controlled
        assert_eq!(client_ip(xff, peer, Some(1)), "172.16.0.2");
        assert_eq!(client_ip(xff, peer, Some(2)), "203.0.113.9");
        assert_eq!(client_ip(xff, peer, Some(9)), "10.0.0.1");
        assert_eq!(client_ip(None, peer, Some(1)), "10.0.0.1");
        assert_eq!(client_ip(None, peer, None), "10.0.0.1");
    }

    #[test]
    fn account_ids() {
        assert!(is_account_id("alice.testnet"));
        assert!(is_account_id(&"a".repeat(64)));
        assert!(!is_account_id("Alice.testnet"));
        assert!(!is_account_id("a"));
        assert!(!is_account_id("x y"));
        assert!(!is_account_id(&"a".repeat(65)));
    }
}
