//! Live USD prices for Pushed markets (testnet demo oracle).
//! Coinbase spot and Kraken ticker are read together and cross-checked by
//! `decide`. Integer price_e6 out.

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::warn;

/// Parse a decimal string like "3.49545" into price_e6 without floats.
pub fn parse_e6(s: &str) -> Option<u64> {
    let s = s.trim();
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let mut frac: String = frac.chars().take(6).collect();
    while frac.len() < 6 {
        frac.push('0');
    }
    let v = int.parse::<u64>().ok()?.checked_mul(1_000_000)?.checked_add(frac.parse::<u64>().ok()?)?;
    (v > 0).then_some(v)
}

pub fn feed_symbol(market_symbol: &str) -> String {
    let s = market_symbol.to_uppercase();
    for suf in ["-PERP", "-USD", "/USD", "-USDC", "PERP"] {
        if let Some(x) = s.strip_suffix(suf) {
            return x.trim_end_matches(['-', '/']).to_string();
        }
    }
    s
}

async fn coinbase(client: &reqwest::Client, sym: &str) -> Result<u64> {
    let v: Value = client
        .get(format!("https://api.coinbase.com/v2/prices/{sym}-USD/spot"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    v["data"]["amount"].as_str().and_then(parse_e6).ok_or_else(|| anyhow!("coinbase: no price for {sym}"))
}

async fn kraken(client: &reqwest::Client, sym: &str) -> Result<u64> {
    let v: Value = client
        .get(format!("https://api.kraken.com/0/public/Ticker?pair={sym}USD"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let res = v["result"].as_object().ok_or_else(|| anyhow!("kraken: no result for {sym}"))?;
    let first = res.values().next().ok_or_else(|| anyhow!("kraken: empty"))?;
    first["c"][0].as_str().and_then(parse_e6).ok_or_else(|| anyhow!("kraken: bad price"))
}

/// Every source that answered right now (0, 1 or 2 prices). A failed source is logged and left out.
pub async fn sources(client: &reqwest::Client, sym: &str) -> Vec<u64> {
    let (c, k) = tokio::join!(coinbase(client, sym), kraken(client, sym));
    let mut out = Vec::new();
    for (name, r) in [("coinbase", c), ("kraken", k)] {
        match r {
            Ok(p) => out.push(p),
            Err(e) => warn!(sym, source = name, err = %e, "price source unavailable"),
        }
    }
    out
}

/// |a - b| > base * bps / 10_000, without overflow.
fn differs_by_more_than(a: u64, b: u64, base: u64, bps: u64) -> bool {
    a.abs_diff(b) as u128 * 10_000 > base as u128 * bps as u128
}

/// The price to push, or the reason to push nothing this tick.
///
/// - Two sources: they must agree within `max_spread_bps` of the lower one; the
///   result is their midpoint. No jump cap, because both confirm the move.
/// - One source: accepted only within `single_max_move_bps` of the last
///   on-chain observation (`last_onchain`, 0 = none yet).
/// - No source: refuse.
pub fn decide(sources: &[u64], last_onchain: u64, max_spread_bps: u64, single_max_move_bps: u64) -> Result<u64, String> {
    if sources.contains(&0) {
        return Err("a source returned a zero price".into());
    }
    match *sources {
        [] => Err("no price source answered".into()),
        [p] => {
            if last_onchain == 0 {
                return Err(format!("single source {p} and no on-chain price to check it against"));
            }
            if differs_by_more_than(p, last_onchain, last_onchain, single_max_move_bps) {
                return Err(format!(
                    "single source {p} is more than {single_max_move_bps} bps from the on-chain price {last_onchain}"
                ));
            }
            Ok(p)
        }
        [a, b] => {
            let (lo, hi) = (a.min(b), a.max(b));
            if differs_by_more_than(hi, lo, lo, max_spread_bps) {
                return Err(format!("sources disagree by more than {max_spread_bps} bps: {a} vs {b}"));
            }
            Ok(lo + (hi - lo) / 2)
        }
        _ => Err(format!("expected at most two sources, got {}", sources.len())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse() {
        assert_eq!(parse_e6("3.49545"), Some(3_495_450));
        assert_eq!(parse_e6("100"), Some(100_000_000));
        assert_eq!(parse_e6("0.000001234"), Some(1));
        assert_eq!(parse_e6("0"), None);
        assert_eq!(feed_symbol("NEAR-PERP"), "NEAR");
        assert_eq!(feed_symbol("sol-perp"), "SOL");
    }

    #[test]
    fn decide_two_sources_agree() {
        assert_eq!(decide(&[5_000_000, 5_000_000], 0, 100, 500), Ok(5_000_000));
        // Midpoint, in either order, rounded down.
        assert_eq!(decide(&[5_000_000, 5_040_001], 0, 100, 500), Ok(5_020_000));
        assert_eq!(decide(&[5_040_001, 5_000_000], 0, 100, 500), Ok(5_020_000));
        // Exactly 100 bps above the lower source is still inside.
        assert_eq!(decide(&[5_000_000, 5_050_000], 0, 100, 500), Ok(5_025_000));
        // Two agreeing sources are not capped against the on-chain price.
        assert_eq!(decide(&[10_000_000, 10_000_000], 5_000_000, 100, 500), Ok(10_000_000));
    }

    #[test]
    fn decide_two_sources_disagree() {
        assert!(decide(&[5_000_000, 5_050_001], 5_000_000, 100, 500).is_err());
        assert!(decide(&[5_050_001, 5_000_000], 5_000_000, 100, 500).is_err());
        assert!(decide(&[5_000_000, 10_000_000], 5_000_000, 100, 500).is_err());
        // A zero spread allowance demands equality.
        assert!(decide(&[5_000_000, 5_000_001], 0, 0, 500).is_err());
    }

    #[test]
    fn decide_single_source() {
        assert_eq!(decide(&[5_100_000], 5_000_000, 100, 500), Ok(5_100_000));
        // Exactly 500 bps either way is inside, one more atom is outside.
        assert_eq!(decide(&[5_250_000], 5_000_000, 100, 500), Ok(5_250_000));
        assert_eq!(decide(&[4_750_000], 5_000_000, 100, 500), Ok(4_750_000));
        assert!(decide(&[5_250_001], 5_000_000, 100, 500).is_err());
        assert!(decide(&[4_749_999], 5_000_000, 100, 500).is_err());
        // A new market needs both sources for its first price.
        assert!(decide(&[5_000_000], 0, 100, 500).is_err());
    }

    #[test]
    fn decide_refuses_none_zero_and_extra() {
        assert!(decide(&[], 5_000_000, 100, 500).is_err());
        assert!(decide(&[0], 5_000_000, 100, 500).is_err());
        assert!(decide(&[0, 5_000_000], 5_000_000, 100, 500).is_err());
        assert!(decide(&[5_000_000, 5_000_000, 5_000_000], 5_000_000, 100, 500).is_err());
    }

    #[test]
    fn decide_does_not_overflow() {
        let max = u64::MAX;
        assert_eq!(decide(&[max, max], 0, u64::MAX, u64::MAX), Ok(max));
        assert_eq!(decide(&[max, max - 1], 0, 100, 500), Ok(max - 1));
        assert!(decide(&[1, max], 0, 100, 500).is_err());
        assert_eq!(decide(&[max], max, 100, 500), Ok(max));
        assert!(decide(&[max], 1, 100, 500).is_err());
        assert!(decide(&[1], max, 100, 500).is_err());
        // Wide allowances multiply in u128 and still compare correctly.
        assert_eq!(decide(&[2, 3], 0, 5_000, 500), Ok(2));
        assert_eq!(decide(&[max / 2], max / 2 - 1, 100, u64::MAX), Ok(max / 2));
    }
}
