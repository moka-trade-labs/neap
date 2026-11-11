//! Randomized property checks for the LP share math at full production width
//! (the Kani harnesses for the same properties use bounded widths).

use neap::policy::*;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    /// Mix of tiny, mid and production-scale (<= 1e18) values.
    fn val(&mut self) -> u128 {
        match self.next() % 4 {
            0 => (self.next() % 10) as u128,
            1 => (self.next() % 1_000_000) as u128,
            2 => (self.next() % 1_000_000_000_000) as u128,
            _ => (self.next() % 1_000_000_000_000_000_000) as u128,
        }
    }
}

#[test]
fn lp_math_properties_at_scale() {
    let mut r = Lcg(0x5eed);
    for _ in 0..2_000_000 {
        let (amount, total, nav) = (r.val(), r.val(), r.val());
        if let Some(s) = lp_shares_for_deposit(amount, total, nav) {
            // no dilution of existing LPs
            assert!(
                (nav + amount + VIRTUAL_ASSETS) * (total + VIRTUAL_SHARES)
                    >= (nav + VIRTUAL_ASSETS) * (total + s + VIRTUAL_SHARES)
            );
            // round trip never profits
            let back = lp_assets_for_redeem(s, total + s, nav + amount).unwrap();
            assert!(back <= amount);
        }
        let shares = r.val().min(total);
        if let Some(a) = lp_assets_for_redeem(shares, total, nav) {
            assert!(a <= nav);
            assert!(
                (nav - a + VIRTUAL_ASSETS) * (total + VIRTUAL_SHARES)
                    >= (nav + VIRTUAL_ASSETS) * (total - shares + VIRTUAL_SHARES)
            );
        }
    }
}

/// Exhaustive over every spread (0..=MAX_SPREAD_BPS) for effective prices in
/// 1..=2000, then random at production scale: the pool price is always against
/// the taker and within the spread (+1 atom of rounding).
#[test]
fn pool_exec_price_against_taker_and_bounded() {
    fn check(e: u64, s: u64) {
        let (e128, s128) = (e as u128, s as u128);
        let buy = pool_exec_price(e, s, true).unwrap();
        let sell = pool_exec_price(e, s, false);
        assert!(buy >= e);
        assert!((buy as u128) * BPS < e128 * (BPS + s128) + BPS);
        if let Some(sell) = sell {
            assert!(sell <= e);
            assert!((sell as u128) * BPS + BPS > e128 * (BPS - s128));
        }
        if s == 0 {
            assert_eq!(buy, e);
            assert_eq!(sell, Some(e));
        }
    }
    for e in 1..=2_000u64 {
        for s in 0..=MAX_SPREAD_BPS {
            check(e, s);
        }
    }
    let mut r = Lcg(0xbeef);
    for _ in 0..2_000_000 {
        let e = 1 + r.next() % (MAX_PRICE / 2);
        check(e, r.next() % (MAX_SPREAD_BPS + 1));
    }
    assert!(pool_exec_price(1, MAX_SPREAD_BPS + 1, true).is_none());
    assert!(pool_exec_price(0, 0, true).is_none());
}

/// Funding never exceeds the cap, charges the side the pool warehouses
/// against, and a fully one-sided book pays exactly the cap.
#[test]
fn skew_funding_bounded_signed_and_saturating() {
    let mut r = Lcg(0xf00d);
    for _ in 0..2_000_000 {
        let oi = r.val();
        let net = (r.val() as i128) * if r.next() % 2 == 0 { 1 } else { -1 };
        let max = r.next() % 1_000_000;
        let f = skew_funding_rate_e9(net, oi, max);
        assert!(f.unsigned_abs() <= max as u128);
        if net < 0 {
            assert!(f >= 0);
        }
        if net > 0 {
            assert!(f <= 0);
        }
        if oi > 0 && oi <= i128::MAX as u128 {
            assert_eq!(skew_funding_rate_e9(-(oi as i128), oi, max), max as i128);
        }
    }
}
