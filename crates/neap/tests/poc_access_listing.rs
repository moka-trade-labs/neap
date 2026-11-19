//! PoC (access-control / lifecycle review): permissionless listing via
//! `ft_transfer_call` CreateMarket, price-source immutability and the NearPool
//! band breach.
//!
//! Every test asserts the CORRECT behaviour, so each one FAILS on the current
//! code and should pass once the finding is fixed.

use std::panic::{catch_unwind, AssertUnwindSafe};

use neap::model::Mode;
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken, PromiseOrValue, PromiseResult};

const USDC: u128 = 1_000_000;
const ZERO: NearToken = NearToken::from_yoctonear(0);
const ONE: NearToken = NearToken::from_yoctonear(1);

fn contract_id() -> AccountId {
    "neap.near".parse().unwrap()
}
fn usdc() -> AccountId {
    "usdc.near".parse().unwrap()
}
fn oracle() -> AccountId {
    "oracle.near".parse().unwrap()
}

struct Env {
    height: u64,
    ts: u64,
}

impl Env {
    fn ctx(&self, who: &AccountId, deposit: NearToken) -> VMContextBuilder {
        let mut b = VMContextBuilder::new();
        b.current_account_id(contract_id())
            .predecessor_account_id(who.clone())
            .signer_account_id(who.clone())
            .attached_deposit(deposit)
            .account_balance(NearToken::from_near(1_000))
            .block_height(self.height)
            .block_timestamp(self.ts);
        b
    }
    fn call(&mut self, who: &AccountId, deposit: NearToken) {
        testing_env!(self.ctx(who, deposit).build());
    }
    /// Context for a `#[private]` callback with one promise result.
    fn callback(&mut self, result: Vec<u8>) {
        testing_env!(
            self.ctx(&contract_id(), ZERO).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![PromiseResult::Successful(result)]
        );
    }
    fn tick(&mut self, n: u64) {
        self.height += n;
        self.ts += n * 600_000_000;
    }
}

/// Spec §5 long-tail parameters (4x, MM 15%, 30 bps x 20 blocks).
fn longtail_params_json(im_bps: u64, mm_bps: u64) -> String {
    format!(
        r#"{{"initial_margin_bps":"{im_bps}","maintenance_margin_bps":"{mm_bps}",
        "trading_fee_bps":"5","liquidation_fee_bps":"50","liquidation_fee_cap":"1000000000",
        "min_liquidation_abs":"0","max_price_move_bps_per_slot":"30","max_accrual_dt_slots":"10",
        "max_abs_funding_e9_per_slot":"100","h_min":"5","h_max":"50",
        "min_nonzero_mm_req":"1000","min_nonzero_im_req":"2000"}}"#
    )
}

fn create_msg(symbol: &str, tier: &str, source: &str, im_bps: u64, mm_bps: u64) -> String {
    format!(
        r#"{{"action":"CreateMarket","symbol":"{symbol}","tier":"{tier}","price_source":{source},
        "spread_bps":20,"max_oracle_age_ns":"60000000000","params":{}}}"#,
        longtail_params_json(im_bps, mm_bps)
    )
}

const NEAR_POOL: &str = r#"{"NearPool":{"dex":"ref.near","pool_id":7,"quote_pool":null}}"#;

fn new_contract() -> (Env, Neap) {
    let mut e = Env {
        height: 100,
        ts: 1_000_000_000_000,
    };
    e.call(&accounts(0), NearToken::from_near(1));
    let c = Neap::new(accounts(0), usdc(), oracle());
    (e, c)
}

/// `ft_transfer_call(amount, msg)` from `sender`. Returns the refunded amount.
fn ft_call(e: &mut Env, c: &mut Neap, sender: &AccountId, amount: u128, msg: String) -> u128 {
    e.call(&usdc(), ZERO);
    match c.ft_on_transfer(sender.clone(), U128(amount), msg) {
        PromiseOrValue::Value(v) => v.0,
        PromiseOrValue::Promise(_) => 0,
    }
}

fn pool_json(usdc_atoms: u128, token_atoms: u128) -> Vec<u8> {
    format!(
        r#"{{"token_account_ids":["usdc.near","tok.near"],"amounts":["{usdc_atoms}","{token_atoms}"]}}"#
    )
    .into_bytes()
}

/// ACL-3: the listing bond is credited to the lister's own wallet and is not
/// locked, so it is no bond at all: the lister withdraws it in the next
/// transaction and the contract has paid the ~10 KB+ of storage for the new
/// market (no NEAR can be attached in `ft_on_transfer`).
#[test]
#[ignore = "open proposal P-3 in docs/review-2026-11-18.md: fails until jason decides"]
fn listing_bond_is_locked_not_withdrawable() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    let refunded = ft_call(
        &mut e,
        &mut c,
        &lister,
        50 * USDC,
        create_msg("TOK-PERP", "LongTail", NEAR_POOL, 2_500, 1_500),
    );
    assert_eq!(refunded, 0);
    assert_eq!(c.get_markets_count(), 1, "market was created");
    assert_eq!(c.get_market(0).bond.0, 50 * USDC);
    // Correct: the bond is escrowed, i.e. NOT spendable wallet balance.
    assert_eq!(
        c.get_wallet(lister.clone()).0,
        0,
        "the 50 USDC bond sits in the lister's withdrawable wallet"
    );
}

/// ACL-3 (cont.): prove the bond really leaves: `withdraw` succeeds for the
/// full bond right after listing.
#[test]
#[ignore = "open proposal P-3 in docs/review-2026-11-18.md: fails until jason decides"]
fn lister_cannot_withdraw_bond_right_after_listing() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    ft_call(
        &mut e,
        &mut c,
        &lister,
        50 * USDC,
        create_msg("TOK-PERP", "LongTail", NEAR_POOL, 2_500, 1_500),
    );
    e.call(&lister, ONE);
    let r = catch_unwind(AssertUnwindSafe(|| {
        let _ = c.withdraw(U128(50 * USDC));
    }));
    assert!(r.is_err(), "lister withdrew the whole bond; market stays listed");
}

/// ACL-4a: spec §4 "Params must fall within tier bounds". A long-tail market
/// with 10x leverage (IM 10% / MM 5%; the tier says 4x / 15%) must be refused.
#[test]
#[ignore = "open proposal P-3 in docs/review-2026-11-18.md: fails until jason decides"]
fn listing_params_outside_tier_bounds_refused() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    let r = catch_unwind(AssertUnwindSafe(|| {
        ft_call(
            &mut e,
            &mut c,
            &lister,
            50 * USDC,
            create_msg("TOK-PERP", "LongTail", NEAR_POOL, 1_000, 500),
        )
    }));
    // Either a panic (ft_resolve refunds) or a full refund is acceptable.
    let created = c.get_markets_count();
    assert!(
        created == 0 && r.map(|refund| refund == 50 * USDC).unwrap_or(true),
        "long-tail market listed with IM 10% / MM 5% (tier bound is 25% / 15%)"
    );
}

/// ACL-4b: `Tier::Major` is "protocol-listed" (bond 0) but anyone can pick it
/// through the permissionless path with 1 atom of USDC, skipping the bond and
/// getting the tighter major spreads/params.
#[test]
fn permissionless_listing_cannot_claim_major_tier() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    let r = catch_unwind(AssertUnwindSafe(|| {
        ft_call(
            &mut e,
            &mut c,
            &lister,
            1,
            create_msg("SOL-PERP", "Major", NEAR_POOL, 1_000, 800),
        )
    }));
    let _ = r;
    assert_eq!(
        c.get_markets_count(),
        0,
        "anyone listed a Major-tier market with a 0.000001 USDC 'bond'"
    );
}

/// ACL-4c: symbols are not validated: duplicates (spoofing an existing
/// market) and unbounded length (contract-paid storage) are both accepted.
#[test]
#[ignore = "open proposal P-3 in docs/review-2026-11-18.md: fails until jason decides"]
fn duplicate_or_oversized_symbol_refused() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    ft_call(
        &mut e,
        &mut c,
        &lister,
        50 * USDC,
        create_msg("TOK-PERP", "LongTail", NEAR_POOL, 2_500, 1_500),
    );
    let before = c.get_markets_count();
    let dup = catch_unwind(AssertUnwindSafe(|| {
        ft_call(
            &mut e,
            &mut c,
            &accounts(5),
            50 * USDC,
            create_msg("TOK-PERP", "LongTail", NEAR_POOL, 2_500, 1_500),
        )
    }));
    let _ = dup;
    assert_eq!(c.get_markets_count(), before, "duplicate symbol listed");

    let huge = "A".repeat(100_000);
    let big = catch_unwind(AssertUnwindSafe(|| {
        ft_call(
            &mut e,
            &mut c,
            &accounts(5),
            50 * USDC,
            create_msg(&huge, "LongTail", NEAR_POOL, 2_500, 1_500),
        )
    }));
    let _ = big;
    assert_eq!(c.get_markets_count(), before, "100 KB symbol listed");
}

/// ACL-5: invariant 9 (price source immutability). `push_price` never checks
/// `price_source == Pushed`, so the global oracle account (rotatable by the
/// owner with `set_oracle`, no timelock) can overwrite the price of a NearPool
/// market at will, i.e. the owner can switch any market's effective price
/// source after activation.
#[test]
fn oracle_cannot_push_price_into_near_pool_market() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    ft_call(
        &mut e,
        &mut c,
        &lister,
        50 * USDC,
        create_msg("TOK-PERP", "LongTail", NEAR_POOL, 2_500, 1_500),
    );
    // First pool read: 2,000 USDC vs 1,000 TOK → $2.00, depth ≥ long-tail floor.
    e.callback(pool_json(2_000 * USDC, 1_000 * USDC));
    c.on_price_read(0);
    e.call(&accounts(3), ZERO);
    c.activate_market(0);
    assert_eq!(c.get_market(0).mode, Mode::Active);

    // Owner rotates the oracle to an account it controls and pushes a price.
    let evil: AccountId = "evil.near".parse().unwrap();
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_oracle(evil.clone());
    e.tick(1);
    e.call(&evil, ZERO);
    let ts = e.ts;
    let r = catch_unwind(AssertUnwindSafe(|| c.push_price(0, U64(4_000_000), U64(ts))));
    assert!(
        r.is_err(),
        "pushed oracle overwrote a NearPool market: target is now {}",
        c.get_market(0).oracle_target.0
    );
}

/// ACL-6: one >20% move against the EMA permanently bricks a NearPool market.
/// The band check returns before the EMA or the observation is updated, so
/// every later read is compared with the same frozen EMA and rejected again.
/// The market first falls to ReduceOnly (no method can restore it), then after
/// 10 s closes need... a fresh observation that can never arrive; after 1 h the
/// staleness ladder forbids every fill and every liquidation, forever.
/// Correct: after the pool has held the new level for a sustained period the
/// market must accept observations again.
#[test]
fn near_pool_market_recovers_after_sustained_price_move() {
    let (mut e, mut c) = new_contract();
    let lister = accounts(4);
    ft_call(
        &mut e,
        &mut c,
        &lister,
        50 * USDC,
        create_msg("TOK-PERP", "LongTail", NEAR_POOL, 2_500, 1_500),
    );
    e.callback(pool_json(2_000 * USDC, 1_000 * USDC)); // $2.00
    c.on_price_read(0);
    e.call(&accounts(3), ZERO);
    c.activate_market(0);
    let seq0 = c.get_market(0).observation.seq.0;

    // The token genuinely reprices to $2.60 (+30%) and stays there for 2 hours,
    // with a keeper refreshing every ~6 s.
    for _ in 0..1_200 {
        e.tick(10);
        e.callback(pool_json(2_600 * USDC, 1_000 * USDC));
        c.on_price_read(0);
    }
    let m = c.get_market(0);
    assert!(
        m.observation.seq.0 > seq0,
        "no observation accepted in 2 h after a 30% move: mode {:?}, observation frozen at {} \
         (age {} s) => no fills, no liquidations, no recovery",
        m.mode,
        m.observation.price_e6.0,
        (e.ts - m.observation.source_ts.0) / 1_000_000_000
    );
}
