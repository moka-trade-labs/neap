//! Contract data model: who owns what (`Owner`), and the per-market registry
//! entry (`Market`). Spec: docs/contract.md §3. No engine logic lives here.

use near_sdk::json_types::{U128, U64};
use near_sdk::{near, AccountId};

use crate::exchange::OracleMeta;

/// An account on Neap. NEAR accounts act through `predecessor_account_id`;
/// Solana keys (phase 2) act through `execute_signed`. The borsh discriminant
/// order is part of the storage layout: append only.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Owner {
    Near(AccountId),
    Sol([u8; 32]),
}

impl Owner {
    /// 32-byte key the engine uses to identify the portfolio owner. NEAR keeps
    /// `sha256(account_id)`; Solana is domain-separated (`:` is not a legal
    /// NEAR account character, so the two spaces cannot collide).
    pub fn engine_key(&self) -> [u8; 32] {
        match self {
            Owner::Near(a) => near_sdk::env::sha256_array(a.as_bytes()),
            Owner::Sol(pk) => {
                let mut b = b"sol:".to_vec();
                b.extend_from_slice(pk);
                near_sdk::env::sha256_array(&b)
            }
        }
    }

    /// Human-readable form for events and logs.
    pub fn label(&self) -> String {
        match self {
            Owner::Near(a) => a.to_string(),
            Owner::Sol(pk) => format!("sol:{}", hex(pk)),
        }
    }
}

fn hex(b: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        s.push(D[(x >> 4) as usize] as char);
        s.push(D[(x & 15) as usize] as char);
    }
    s
}

/// Risk tier chosen by the lister; parameters must fall inside its bounds
/// (docs/contract.md §5). Bounds are enforced from step 1.4.
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    Major,
    LongTail,
}

/// Market lifecycle (docs/contract.md §3). Mode transitions are wired up in
/// steps 1.2 (staleness ladder), 1.4 (Proposed -> Active) and 1.9 (admin).
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Proposed,
    Active,
    ReduceOnly,
    PriceUncertain,
    Recovery,
    WindDown,
    Closed,
}

/// Where a market's price comes from. Immutable after activation (invariant 9).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PriceSource {
    /// Transitional: the single trusted `oracle` account pushes prices. Removed
    /// when `NearPool` lands in step 1.3 (clean cutover; no market keeps it).
    Pushed,
    /// Read on-chain from a NEAR DEX pool (step 1.3).
    NearPool {
        dex: AccountId,
        pool_id: u64,
        quote_pool: Option<u64>,
    },
    /// Signed by a Shade Agent for another chain (phase 2).
    Tee { chain: Chain, pools: Vec<String> },
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chain {
    Solana,
}

/// The latest accepted price reading. `price_e6` is quote atoms per base unit
/// (USDC has 6 decimals, so this is USD scaled by 1e6).
#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    pub price_e6: u64,
    pub liquidity_usd: u128,
    pub source_ts: u64,
    pub seq: u64,
    pub block: u64,
}

/// One entry of the market registry.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Market {
    pub symbol: String,
    pub tier: Tier,
    pub mode: Mode,
    pub price_source: PriceSource,
    pub observation: Observation,
    pub lister: Owner,
    pub bond: u128,
    pub spread_bps: u64,
    pub max_oracle_age_ns: u64,
    // Staircase state (engine price walks toward `oracle_target`).
    pub oracle_target: u64,
    pub oracle_remainder_bps_num: u16,
    pub oracle_last_publish_ns: u64,
    pub lp_total_shares: u128,
}

impl Market {
    pub(crate) fn meta(&self) -> OracleMeta {
        OracleMeta {
            target: self.oracle_target,
            remainder_bps_num: self.oracle_remainder_bps_num,
            last_publish_ns: self.oracle_last_publish_ns,
        }
    }

    pub(crate) fn set_meta(&mut self, m: OracleMeta) {
        self.oracle_target = m.target;
        self.oracle_remainder_bps_num = m.remainder_bps_num;
        self.oracle_last_publish_ns = m.last_publish_ns;
    }
}

/// JSON view of an [`Observation`] with the big integers as strings.
#[near(serializers = [json])]
pub struct ObservationView {
    pub price_e6: U64,
    pub liquidity_usd: U128,
    pub source_ts: U64,
    pub seq: U64,
    pub block: U64,
}

impl From<&Observation> for ObservationView {
    fn from(o: &Observation) -> Self {
        Self {
            price_e6: U64(o.price_e6),
            liquidity_usd: U128(o.liquidity_usd),
            source_ts: U64(o.source_ts),
            seq: U64(o.seq),
            block: U64(o.block),
        }
    }
}
