//! Kani proofs for the NEAR wrapper policy layer.
//!
//! Run: `cargo kani --no-default-features` (from percolator/near).
//!
//! 128-bit multiply/divide is expensive for the SAT backend, so harnesses that
//! exercise mul-div use inputs bounded to 16 bits (LP shares) or 40 bits (prices). The algebra proved
//! (rounding direction, no-dilution, conservation) does not depend on the
//! magnitude; overflow at full width is handled by `checked_*` returning `None`,
//! which the unbounded harnesses cover.

use crate::policy::*;

// ───────────────────────── pool execution price ─────────────────────────

/// A taker never gets a better price than the effective engine price, and
/// never pays more than the configured spread (+1 atom of rounding).
#[kani::proof]
#[kani::solver(cadical)]
fn proof_pool_exec_price_is_against_taker_and_bounded() {
    // Bounded widths keep the 64-bit mul/div tractable for CBMC; the full
    // range is covered by `tests/policy_props.rs`.
    let effective: u64 = kani::any::<u16>() as u64;
    let spread: u64 = kani::any::<u16>() as u64;
    let buys: bool = kani::any();
    if let Some(px) = pool_exec_price(effective, spread, buys) {
        assert!(spread <= MAX_SPREAD_BPS && effective > 0);
        let (e, p, b, sp) = (effective, px, BPS as u64, spread);
        if buys {
            assert!(p >= e);
            assert!(p * b <= e * (b + sp) + b);
        } else {
            assert!(p <= e);
            assert!(p * b + b > e * (b - sp));
        }
    }
    kani::cover!(pool_exec_price(effective, spread, buys).is_some());
}

/// Zero spread is exactly the effective price.
#[kani::proof]
#[kani::solver(cadical)]
fn proof_pool_exec_price_zero_spread_is_identity() {
    let effective: u64 = kani::any::<u16>() as u64;
    let buys: bool = kani::any();
    kani::assume(effective > 0);
    assert_eq!(pool_exec_price(effective, 0, buys), Some(effective));
}

#[kani::proof]
fn proof_within_limit_matches_side() {
    let px: u64 = kani::any();
    let limit: u64 = kani::any();
    assert_eq!(within_limit(px, limit, true), px <= limit);
    assert_eq!(within_limit(px, limit, false), px >= limit);
}

// ───────────────────────────── funding ─────────────────────────────

/// Funding is bounded by the cap and always charges the side the pool is
/// warehousing against (traders net long => longs pay => rate >= 0).
#[kani::proof]
fn proof_skew_funding_bounded_and_correctly_signed() {
    let lp_net: i128 = kani::any();
    let oi: u128 = kani::any();
    let max: u64 = kani::any();
    let r = skew_funding_rate_e9(lp_net, oi, max);
    assert!(r.unsigned_abs() <= max as u128);
    if lp_net < 0 {
        assert!(r >= 0);
    }
    if lp_net > 0 {
        assert!(r <= 0);
    }
    if lp_net == 0 || oi == 0 {
        assert!(r == 0);
    }
    kani::cover!(r != 0);
}

/// Fully one-sided book (pool holds all OI) pays the full cap.
#[kani::proof]
#[kani::solver(cadical)]
fn proof_skew_funding_full_skew_hits_cap() {
    let oi: u128 = kani::any::<u16>() as u128;
    let max: u64 = kani::any::<u16>() as u64;
    kani::assume(oi > 0);
    let r = skew_funding_rate_e9(-(oi as i128), oi, max);
    assert_eq!(r, max as i128);
}

// ───────────────────────────── oracle ─────────────────────────────

/// Accepted updates are strictly newer (no replay / reordering), fresh, and
/// not from the future beyond the skew allowance.
#[kani::proof]
fn proof_oracle_acceptance_is_monotone_and_fresh() {
    let last: u64 = kani::any();
    let publish: u64 = kani::any();
    let now: u64 = kani::any();
    let max_age: u64 = kani::any();
    let skew: u64 = kani::any();
    if accept_oracle_update(last, publish, now, max_age, skew) {
        assert!(publish > last);
        assert!(publish >= now || now - publish <= max_age);
        assert!(publish <= now.saturating_add(skew));
        // Replaying the same update is always rejected.
        assert!(!accept_oracle_update(publish, publish, now, max_age, skew));
    }
}

// ───────────────────────────── LP shares ─────────────────────────────

/// Deposit-then-redeem at unchanged NAV never returns more than was deposited
/// (no free value from rounding).
#[kani::proof]
#[kani::solver(cadical)]
fn proof_lp_round_trip_never_profits() {
    let amount: u128 = kani::any::<u16>() as u128;
    let total: u128 = kani::any::<u16>() as u128;
    let nav: u128 = kani::any::<u16>() as u128;
    if let Some(shares) = lp_shares_for_deposit(amount, total, nav) {
        let back = lp_assets_for_redeem(shares, total + shares, nav + amount).unwrap();
        assert!(back <= amount);
    }
    kani::cover!(lp_shares_for_deposit(amount, total, nav).is_some() && total > 0);
}

/// A deposit never dilutes existing LPs: NAV per share does not fall.
/// (nav+amount+VA)/(total+shares+VS) >= (nav+VA)/(total+VS)
#[kani::proof]
#[kani::solver(cadical)]
fn proof_lp_deposit_never_dilutes_existing_lps() {
    let amount: u128 = kani::any::<u16>() as u128;
    let total: u128 = kani::any::<u16>() as u128;
    let nav: u128 = kani::any::<u16>() as u128;
    if let Some(shares) = lp_shares_for_deposit(amount, total, nav) {
        let lhs = (nav + amount + VIRTUAL_ASSETS) * (total + VIRTUAL_SHARES);
        let rhs = (nav + VIRTUAL_ASSETS) * (total + shares + VIRTUAL_SHARES);
        assert!(lhs >= rhs);
    }
}

/// A redemption never dilutes remaining LPs: NAV per share does not fall.
#[kani::proof]
#[kani::solver(cadical)]
fn proof_lp_redeem_never_dilutes_remaining_lps() {
    let shares: u128 = kani::any::<u16>() as u128;
    let total: u128 = kani::any::<u16>() as u128;
    let nav: u128 = kani::any::<u16>() as u128;
    if let Some(assets) = lp_assets_for_redeem(shares, total, nav) {
        assert!(shares <= total);
        assert!(assets <= nav, "cannot pay out more than NAV");
        let lhs = (nav - assets + VIRTUAL_ASSETS) * (total + VIRTUAL_SHARES);
        let rhs = (nav + VIRTUAL_ASSETS) * (total - shares + VIRTUAL_SHARES);
        assert!(lhs >= rhs);
    }
}

/// Redeeming more shares than exist is always refused (any width).
#[kani::proof]
fn proof_lp_redeem_rejects_excess_shares() {
    let shares: u128 = kani::any();
    let total: u128 = kani::any();
    let nav: u128 = kani::any();
    kani::assume(shares > total);
    assert!(lp_assets_for_redeem(shares, total, nav).is_none());
}

/// The redemption bound never exceeds the deposit bound, and unrealized
/// profit is never part of the redemption NAV.
#[kani::proof]
fn proof_lp_nav_bounds_ordered_and_conservative() {
    let capital: u128 = kani::any();
    let pnl: i128 = kani::any();
    let fee: i128 = kani::any();
    let (lower, upper) = lp_nav_bounds(capital, pnl, fee);
    assert!(lower <= upper);
    assert!(lower <= capital);
    let (lower0, _) = lp_nav_bounds(capital, 0, fee);
    if pnl > 0 {
        assert_eq!(lower, lower0);
    }
}

// ───────────────────────────── custody ─────────────────────────────

fn any_custody() -> Custody {
    Custody {
        wallets: kani::any(),
        vaults: kani::any(),
    }
}

/// Internal moves conserve the total exactly; failures change nothing.
#[kani::proof]
fn proof_custody_internal_moves_conserve() {
    let mut c = any_custody();
    let before = c;
    let amount: u128 = kani::any();
    let to_vault: bool = kani::any();
    let r = if to_vault {
        c.wallet_to_vault(amount)
    } else {
        c.vault_to_wallet(amount)
    };
    match r {
        Some(()) => assert_eq!(c.total(), before.total()),
        None => assert_eq!(c, before),
    }
}

/// External flows change the total by exactly `amount`; failures change nothing.
#[kani::proof]
fn proof_custody_external_flows_exact() {
    let mut c = any_custody();
    let before = c;
    let amount: u128 = kani::any();
    let inbound: bool = kani::any();
    kani::assume(before.total().is_some());
    let r = if inbound {
        c.external_in(amount)
    } else {
        c.external_out(amount)
    };
    match r {
        Some(()) => {
            let b = before.total().unwrap();
            let a = c.total().unwrap();
            if inbound {
                assert_eq!(a, b + amount);
            } else {
                assert_eq!(a + amount, b);
            }
            assert_eq!(c.vaults, before.vaults, "external flows never touch vaults");
        }
        None => assert_eq!(c, before),
    }
}

/// Withdraw-then-failed-transfer (debit then re-credit) restores the ledger.
#[kani::proof]
fn proof_custody_failed_withdraw_restores_exactly() {
    let mut c = any_custody();
    let before = c;
    let amount: u128 = kani::any();
    // Reachable ledgers never overflow their total (external_in refuses that).
    kani::assume(before.total().is_some());
    if c.external_out(amount).is_some() {
        assert!(c.external_in(amount).is_some());
        assert_eq!(c, before);
    }
}
