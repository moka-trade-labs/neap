//! Live USD prices for Pushed markets (testnet demo oracle).
//! Coinbase spot first, Kraken ticker as fallback. Integer price_e6 out.

use anyhow::{anyhow, Result};
use serde_json::Value;

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
        .json()
        .await?;
    v["data"]["amount"].as_str().and_then(parse_e6).ok_or_else(|| anyhow!("coinbase: no price for {sym}"))
}

async fn kraken(client: &reqwest::Client, sym: &str) -> Result<u64> {
    let v: Value = client
        .get(format!("https://api.kraken.com/0/public/Ticker?pair={sym}USD"))
        .send()
        .await?
        .json()
        .await?;
    let res = v["result"].as_object().ok_or_else(|| anyhow!("kraken: no result for {sym}"))?;
    let first = res.values().next().ok_or_else(|| anyhow!("kraken: empty"))?;
    first["c"][0].as_str().and_then(parse_e6).ok_or_else(|| anyhow!("kraken: bad price"))
}

pub async fn usd_price_e6(client: &reqwest::Client, sym: &str) -> Result<u64> {
    match coinbase(client, sym).await {
        Ok(p) => Ok(p),
        Err(e1) => kraken(client, sym).await.map_err(|e2| anyhow!("{e1}; {e2}")),
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
}
