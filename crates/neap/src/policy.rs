//! Pure wrapper policy for the NEAR perp contract.
//!
//! Nothing in this module touches storage, the NEAR runtime, or engine state.
//! Every function is total (returns `Option`/`bool`, never panics) so it can be
//! model-checked by Kani in isolation (`tests/` + `src/proofs.rs`).
//!
//! These are the decisions the percolator spec leaves to the wrapper (spec §9):
//! execution-price admissibility, funding-rate sourcing, oracle-update
//! acceptance, LP share accounting, and token custody bookkeeping.

/// Basis-point denominator.
pub const BPS: u128 = 10_000;

/// Engine price bound (`percolator::MAX_ORACLE_PRICE`), restated so this module
/// stays dependency-free for Kani.
pub const MAX_PRICE: u64 = 1_000_000_000_000;

/// Hard cap on the pool spread a market may be configured with (10%).
pub const MAX_SPREAD_BPS: u64 = 1_000;

/// Execution price for a taker filling against the LP pool: the effective
/// engine price moved by `spread_bps` AGAINST the taker, rounded against the
/// taker (buy rounds up, sell rounds down). Returns `None` if the spread is out
/// of range or the result leaves `1..=MAX_PRICE`.
pub fn pool_exec_price(effective: u64, spread_bps: u64, taker_buys: bool) -> Option<u64> {
    if effective == 0 || effective > MAX_PRICE || spread_bps > MAX_SPREAD_BPS {
        return None;
    }
    // effective <= 1e12 and BPS + spread <= 11_000, so every product fits u64
    // exactly (< 1.1e16); u64 keeps the arithmetic cheap to model-check.
    const B: u64 = BPS as u64;
    let px = if taker_buys {
        // ceil(p * (BPS + s) / BPS)
        (effective * (B + spread_bps)).div_ceil(B)
    } else {
        // floor(p * (BPS - s) / BPS)
        effective * (B - spread_bps) / B
    };
    if px == 0 || px > MAX_PRICE {
        return None;
    }
    Some(px as u64)
}

/// Taker slippage limit: a buy may not fill above `limit`, a sell not below it.
pub fn within_limit(exec_price: u64, limit_price: u64, taker_buys: bool) -> bool {
    if taker_buys {
        exec_price <= limit_price
    } else {
        exec_price >= limit_price
    }
}

/// Per-slot funding rate (e9) from the LP pool's inventory skew.
///
/// Engine convention (spec §5.3): a positive rate makes LONGS pay SHORTS.
/// `lp_net_q` is the pool's signed effective position. If traders are net long
/// the pool is short (`lp_net_q < 0`) and longs pay; if traders are net short
/// the pool is long and shorts pay. Magnitude scales linearly with
/// `|lp_net_q| / oi_q`, capped at `max_abs_e9` (which the engine also enforces).
/// This is what pays the pool for warehousing one-sided flow in a thin market.
pub fn skew_funding_rate_e9(lp_net_q: i128, oi_q: u128, max_abs_e9: u64) -> i128 {
    if oi_q == 0 || lp_net_q == 0 || max_abs_e9 == 0 {
        return 0;
    }
    let skew = lp_net_q.unsigned_abs().min(oi_q);
    // skew <= oi_q, so the quotient is <= max_abs_e9 and fits i128.
    let mag = match skew.checked_mul(max_abs_e9 as u128) {
        Some(n) => n / oi_q,
        None => (skew / oi_q).saturating_mul(max_abs_e9 as u128),
    };
    let mag = mag.min(max_abs_e9 as u128) as i128;
    if lp_net_q < 0 {
        mag
    } else {
        -mag
    }
}

/// Oracle update acceptance: strictly newer than the last accepted update,
/// not older than `max_age_ns` relative to `now_ns`, and not from the future
/// beyond `max_future_ns` of clock skew.
pub fn accept_oracle_update(
    last_publish_ns: u64,
    publish_ns: u64,
    now_ns: u64,
    max_age_ns: u64,
    max_future_ns: u64,
) -> bool {
    publish_ns > last_publish_ns
        && publish_ns <= now_ns.saturating_add(max_future_ns)
        && now_ns.saturating_sub(publish_ns) <= max_age_ns
}

/// Virtual share/asset offset for LP shares. The pool starts with
/// `VIRTUAL_SHARES` shares backed by `VIRTUAL_ASSETS` atoms that nobody owns,
/// which makes the classic first-depositor share-inflation attack unprofitable.
pub const VIRTUAL_SHARES: u128 = 1_000_000;
pub const VIRTUAL_ASSETS: u128 = 1;

/// Shares minted for `amount` deposited into a pool whose NAV is `nav`.
/// Rounds DOWN (in favour of existing LPs). `None` on overflow or zero shares.
pub fn lp_shares_for_deposit(amount: u128, total_shares: u128, nav: u128) -> Option<u128> {
    let num = amount.checked_mul(total_shares.checked_add(VIRTUAL_SHARES)?)?;
    let den = nav.checked_add(VIRTUAL_ASSETS)?;
    let shares = num / den;
    if shares == 0 {
        None
    } else {
        Some(shares)
    }
}

/// Atoms paid for redeeming `shares` from a pool whose NAV is `nav`.
/// Rounds DOWN (in favour of remaining LPs). `None` if `shares > total_shares`
/// or on overflow.
pub fn lp_assets_for_redeem(shares: u128, total_shares: u128, nav: u128) -> Option<u128> {
    if shares > total_shares {
        return None;
    }
    let num = shares.checked_mul(nav.checked_add(VIRTUAL_ASSETS)?)?;
    Some(num / total_shares.checked_add(VIRTUAL_SHARES)?)
}

/// Conservative LP NAV bounds from the pool portfolio's capital, PnL and fee
/// credits. Deposits are priced at the UPPER bound (new LPs pay for unrealized
/// gains), redemptions at the LOWER bound (unrealized gains are not paid out,
/// unrealized losses are). Neither side can be diluted by the other.
pub fn lp_nav_bounds(capital: u128, pnl: i128, fee_credits: i128) -> (u128, u128) {
    let fee_debt = if fee_credits < 0 {
        fee_credits.unsigned_abs()
    } else {
        0
    };
    let base = capital.saturating_sub(fee_debt);
    let lower = if pnl < 0 {
        base.saturating_sub(pnl.unsigned_abs())
    } else {
        base
    };
    let upper = if pnl > 0 {
        base.saturating_add(pnl as u128)
    } else {
        lower
    };
    (lower, upper)
}

// ─────────────────────── 1.2: staleness ladder ───────────────────────

/// Staleness time thresholds in nanoseconds (spec §2).
pub const STALE_NO_OPENS_NS: u64 = 10_000_000_000; // 10 s
pub const STALE_REDUCE_ONLY_NS: u64 = 60_000_000_000; // 60 s
pub const STALE_PRICE_UNCERTAIN_NS: u64 = 3_600_000_000_000; // 1 h

/// Freshness classification for the latest observation (spec §2, invariant 3).
/// `Ord` derived so callers can compare restriction levels directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum StalenessLevel {
    /// < 10 s: unrestricted.
    Fresh,
    /// 10 s – 60 s: no position increases (same as reduce-only for fills).
    NoOpens,
    /// 60 s – 1 h: close-only.
    ReduceOnly,
    /// ≥ 1 h: no fills at all.
    PriceUncertain,
}

/// Classify observation freshness. `age_ns = now_ns - observation.source_ts`.
pub fn observation_staleness(age_ns: u64) -> StalenessLevel {
    if age_ns < STALE_NO_OPENS_NS {
        StalenessLevel::Fresh
    } else if age_ns < STALE_REDUCE_ONLY_NS {
        StalenessLevel::NoOpens
    } else if age_ns < STALE_PRICE_UNCERTAIN_NS {
        StalenessLevel::ReduceOnly
    } else {
        StalenessLevel::PriceUncertain
    }
}

// ─────────────────────── 1.2: EMA ───────────────────────

/// EMA smoothing constant: 5% weight on each new sample.
pub const EMA_ALPHA_BPS: u64 = 500;

/// One EMA step. Truncates the result (conservative floor when rising,
/// conservative ceiling when falling). Returns `None` if either input is
/// outside `1..=MAX_PRICE`.
pub fn ema_next(current_ema: u64, new_price: u64) -> Option<u64> {
    if current_ema == 0 || current_ema > MAX_PRICE || new_price == 0 || new_price > MAX_PRICE {
        return None;
    }
    let alpha = EMA_ALPHA_BPS as u128;
    let one_minus = BPS - alpha;
    let num = alpha
        .checked_mul(new_price as u128)?
        .checked_add(one_minus.checked_mul(current_ema as u128)?)?;
    let result = u64::try_from(num / BPS).ok()?;
    if result == 0 || result > MAX_PRICE {
        None
    } else {
        Some(result)
    }
}

// ─────────────────────── 1.5: dynamic spread + OI caps ───────────────────────

/// Dynamic execution spread (spec §5).
/// `net_oi_after`: `|oi_long_after − oi_short_after|` in engine units.
/// `oi_cap`: the relevant OI cap (use 1 if uncapped to avoid /0).
/// Returns spread in bps, clamped at `MAX_SPREAD_BPS`.
pub fn dynamic_spread_bps(
    spread_min_bps: u64,
    k_skew_bps: u64,
    k_size_bps: u64,
    net_oi_after: u128,
    oi_cap: u128,
    size_abs: u128,
) -> u64 {
    if oi_cap == 0 {
        return spread_min_bps.min(MAX_SPREAD_BPS);
    }
    let skew_bps = (net_oi_after.min(oi_cap) * BPS / oi_cap) as u64;
    let size_bps = (size_abs.min(oi_cap) * BPS / oi_cap) as u64;
    let total = spread_min_bps
        .saturating_add(k_skew_bps.saturating_mul(skew_bps) / BPS as u64)
        .saturating_add(k_size_bps.saturating_mul(size_bps) / BPS as u64);
    total.min(MAX_SPREAD_BPS)
}

/// Leverage ratchet: `Lmax = max(1×, L0 × (1 − |net_oi| / oi_cap))`.
/// Returns the adjusted max leverage in bps. `oi_cap == 0` ⇒ no ratchet.
pub fn ratcheted_leverage_max_bps(l0_bps: u64, net_oi_abs: u128, oi_cap: u128) -> u64 {
    if oi_cap == 0 || net_oi_abs == 0 {
        return l0_bps;
    }
    let skew_bps = (net_oi_abs.min(oi_cap) * BPS / oi_cap) as u64;
    let reduction = l0_bps.saturating_mul(skew_bps) / BPS as u64;
    l0_bps.saturating_sub(reduction).max(BPS as u64) // at least 1×
}

/// True if `new_oi ≤ cap` (cap == 0 means uncapped).
pub fn oi_within_cap(new_oi: u128, cap: u128) -> bool {
    cap == 0 || new_oi <= cap
}

// ─────────────────────── custody ───────────────────────

/// Token custody bookkeeping. The contract's NEP-141 balance must always be at
/// least `total()`: every token the contract holds is either in some user's
/// wallet ledger or inside some market group's engine vault (which itself
/// covers capital + insurance, `V >= C_tot + I`). Each method moves value
/// between buckets or across the boundary by exactly `amount`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Custody {
    pub wallets: u128,
    pub vaults: u128,
}

impl Custody {
    pub fn total(&self) -> Option<u128> {
        self.wallets.checked_add(self.vaults)
    }

    /// External tokens arrived (ft_on_transfer) into a wallet.
    pub fn external_in(&mut self, amount: u128) -> Option<()> {
        let w = self.wallets.checked_add(amount)?;
        self.total_with(w, self.vaults)?;
        self.wallets = w;
        Some(())
    }

    /// Tokens leave the contract from a wallet (ft_transfer on withdraw).
    pub fn external_out(&mut self, amount: u128) -> Option<()> {
        self.wallets = self.wallets.checked_sub(amount)?;
        Some(())
    }

    /// Wallet -> engine vault (margin deposit, LP deposit, insurance top-up).
    pub fn wallet_to_vault(&mut self, amount: u128) -> Option<()> {
        let w = self.wallets.checked_sub(amount)?;
        let v = self.vaults.checked_add(amount)?;
        self.wallets = w;
        self.vaults = v;
        Some(())
    }

    /// Engine vault -> wallet (margin withdraw, LP redeem).
    pub fn vault_to_wallet(&mut self, amount: u128) -> Option<()> {
        let v = self.vaults.checked_sub(amount)?;
        let w = self.wallets.checked_add(amount)?;
        self.wallets = w;
        self.vaults = v;
        Some(())
    }

    fn total_with(&self, wallets: u128, vaults: u128) -> Option<u128> {
        wallets.checked_add(vaults)
    }
}
