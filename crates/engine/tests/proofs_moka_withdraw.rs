//! moka fork: Kani proofs for the spec §8.4 nonflat-withdraw margin kernel
//! (`V16Core::kernel_nonflat_withdraw_allowed`). Run with:
//!   cargo kani --tests --features fuzz --harness proof_moka_
#![cfg(kani)]

use percolator::kani_nonflat_withdraw_allowed;

// Bounded but production-scale domain: 2^96 covers MAX_VAULT_TVL (1e16) by a wide margin.
const B: u128 = 1u128 << 96;

fn any_inputs() -> (u128, i128, i128, i128, u128, u128) {
    let capital: u128 = kani::any();
    let pnl: i128 = kani::any();
    let fee_credits: i128 = kani::any();
    let cert_eq: i128 = kani::any();
    let im: u128 = kani::any();
    let amount: u128 = kani::any();
    kani::assume(capital <= B && amount <= B && im <= B);
    kani::assume(pnl > -(B as i128) && pnl < B as i128);
    kani::assume(fee_credits > -(B as i128) && fee_credits < B as i128);
    kani::assume(cert_eq > -(B as i128) && cert_eq < B as i128);
    (capital, pnl, fee_credits, cert_eq, im, amount)
}

/// Soundness: an allowed withdrawal never exceeds capital and always leaves
/// no-positive-credit equity AND certified equity at or above IM.
#[kani::proof]
fn proof_moka_nonflat_withdraw_allowed_implies_im_covered() {
    let (capital, pnl, fee, cert_eq, im, amount) = any_inputs();
    if let Ok(true) = kani_nonflat_withdraw_allowed(capital, pnl, fee, cert_eq, im, amount) {
        assert!(amount <= capital);
        let no_pos = (capital - amount) as i128 + pnl.min(0) - fee.unsigned_abs() as i128;
        assert!(no_pos >= 0 && no_pos as u128 >= im);
        assert!(cert_eq - amount as i128 >= im as i128);
    }
    kani::cover!(
        kani_nonflat_withdraw_allowed(capital, pnl, fee, cert_eq, im, amount) == Ok(true),
        "some withdrawal is allowed"
    );
}

/// Positive PnL never increases how much can be withdrawn: the decision with
/// any positive PnL equals the decision with PnL clamped to zero.
#[kani::proof]
fn proof_moka_nonflat_withdraw_ignores_positive_pnl() {
    let (capital, pnl, fee, cert_eq, im, amount) = any_inputs();
    kani::assume(pnl > 0);
    assert_eq!(
        kani_nonflat_withdraw_allowed(capital, pnl, fee, cert_eq, im, amount),
        kani_nonflat_withdraw_allowed(capital, 0, fee, cert_eq, im, amount)
    );
    kani::cover!(pnl > 1_000, "large positive pnl reached");
}

/// Monotonicity: if `amount` is allowed, every smaller amount is allowed too
/// (no withdrawal ladder can be gamed by splitting).
#[kani::proof]
fn proof_moka_nonflat_withdraw_monotone_in_amount() {
    let (capital, pnl, fee, cert_eq, im, amount) = any_inputs();
    let smaller: u128 = kani::any();
    kani::assume(smaller <= amount);
    if let Ok(true) = kani_nonflat_withdraw_allowed(capital, pnl, fee, cert_eq, im, amount) {
        assert_eq!(
            kani_nonflat_withdraw_allowed(capital, pnl, fee, cert_eq, im, smaller),
            Ok(true)
        );
    }
    kani::cover!(smaller < amount, "strictly smaller amount reached");
}
