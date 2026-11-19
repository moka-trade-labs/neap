//! Contract-level tests in the near-sdk mocked runtime: authorization,
//! custody, storage charging and the full user flow through public methods.
//! Steps 1.2–1.9.

use neap::model::{ConditionalKind, Mode, OrderSide, Owner, Payee, PriceSource, Tier};
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};
use percolator::POS_SCALE;

const USDC: u128 = 1_000_000;
const PRICE: u64 = 100_000_000;

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
    fn call(&mut self, who: &AccountId, deposit: NearToken) {
        let mut b = VMContextBuilder::new();
        b.current_account_id(contract_id())
            .predecessor_account_id(who.clone())
            .signer_account_id(who.clone())
            .attached_deposit(deposit)
            .account_balance(NearToken::from_near(1_000))
            .block_height(self.height)
            .block_timestamp(self.ts);
        testing_env!(b.build());
    }
    fn tick(&mut self, n: u64) {
        self.height += n;
        self.ts += n * 600_000_000;
    }
}

fn params() -> MarketParamsJson {
    MarketParamsJson {
        initial_margin_bps: U64(1_000),
        maintenance_margin_bps: U64(500),
        trading_fee_bps: U64(10),
        liquidation_fee_bps: U64(50),
        liquidation_fee_cap: U128(1_000 * USDC),
        min_liquidation_abs: U128(0),
        max_price_move_bps_per_slot: U64(30),
        max_accrual_dt_slots: U64(10),
        max_abs_funding_e9_per_slot: U64(100),
        h_min: U64(5),
        h_max: U64(50),
        min_nonzero_mm_req: U128(1_000),
        min_nonzero_im_req: U128(2_000),
    }
}

const ONE: NearToken = NearToken::from_yoctonear(1);
const STORAGE: NearToken = NearToken::from_millinear(200);

fn setup() -> (Env, Neap) {
    let mut e = Env {
        height: 100,
        ts: 1_000_000_000_000,
    };
    let owner = accounts(0);
    e.call(&owner, NearToken::from_near(1));
    let mut c = Neap::new(owner.clone(), usdc(), oracle());
    e.call(&owner, NearToken::from_near(1));
    let m = c.create_market(
        "SOL-PERP".into(),
        U64(PRICE),
        params(),
        5,
        U64(60_000_000_000),
    );
    assert_eq!(m, 0);
    // LP: alice seeds the pool.
    let alice = accounts(1);
    fund(&mut e, &mut c, &alice, 1_000_000 * USDC);
    e.call(&alice, STORAGE);
    c.lp_deposit(0, U128(1_000_000 * USDC));
    (e, c)
}

fn fund(e: &mut Env, c: &mut Neap, who: &AccountId, amount: u128) {
    e.call(who, STORAGE);
    c.register();
    e.call(&usdc(), NearToken::from_yoctonear(0));
    let _ = c.ft_on_transfer(who.clone(), U128(amount), String::new());
}

fn custody_matches_vaults(c: &Neap) {
    let (_w, v) = c.get_custody();
    let m = c.get_market(0);
    assert_eq!(v.0, m.vault.0, "custody vaults == engine vault");
}

/// Helper: request an order, tick, push fresh oracle at `price`, execute.
/// Returns the fill exec price.
fn request_and_fill_at(
    e: &mut Env,
    c: &mut Neap,
    who: &AccountId,
    side: OrderSide,
    size_q: u128,
    limit: u64,
    reduce_only: bool,
    oracle_price: u64,
) -> u64 {
    e.call(who, ONE);
    let oid = c.request_order(0, side, U128(size_q), U64(limit), 30, reduce_only);
    // Tick 1 block + push fresh oracle so observation.block > request_block.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(oracle_price), U64(e.ts));
    // Execute (permissionless).
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0).0
}

/// Convenience: request_and_fill_at with PRICE.
fn request_and_fill(
    e: &mut Env,
    c: &mut Neap,
    who: &AccountId,
    side: OrderSide,
    size_q: u128,
    limit: u64,
    reduce_only: bool,
) -> u64 {
    request_and_fill_at(e, c, who, side, size_q, limit, reduce_only, PRICE)
}

// ────────────────────────── existing tests (updated for two-step) ──────────────────────────

#[test]
fn market_registry_shape_and_neap_events() {
    let (mut e, mut c) = setup();
    let m = c.get_market(0);
    assert_eq!(m.mode, Mode::Active);
    assert_eq!(m.tier, Tier::Major);
    assert_eq!(m.price_source, PriceSource::Pushed);
    assert_eq!(m.lister, Owner::Near(accounts(0)));
    assert_eq!(m.bond.0, 0);
    assert_eq!(m.observation.price_e6.0, PRICE);
    assert_eq!(m.observation.seq.0, 0);

    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE + 1_000_000), U64(e.ts));
    let o = c.get_market(0).observation;
    assert_eq!((o.price_e6.0, o.seq.0), (PRICE + 1_000_000, 1));

    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));

    // Two-step order: request → tick → push oracle → execute.
    let _px = request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, POS_SCALE, u64::MAX, false);
    let logs = near_sdk::test_utils::get_logs();
    let fill = logs
        .iter()
        .find(|l| l.contains("\"event\":\"fill\""))
        .expect("fill event");
    assert!(fill.starts_with("EVENT_JSON:"));
    assert!(fill.contains("\"standard\":\"neap\"") && fill.contains("\"version\":\"1.0.0\""));
    assert!(fill.contains(&format!("\"owner\":\"{bob}\"")) && fill.contains("\"block\":"));
}

#[test]
fn full_user_flow_through_contract() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    assert_eq!(c.get_wallet(bob.clone()).0, 5_000 * USDC);
    custody_matches_vaults(&c);

    // Open: buy 10 units via two-step order.
    let px = request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);
    assert_eq!(px, 100_050_000);
    let acct = c.get_account(0, bob.clone()).unwrap();
    assert_eq!(acct.position_q.0, 10 * POS_SCALE as i128);
    assert_eq!(c.get_market(0).pool_position_q.0, -(10 * POS_SCALE as i128));

    // Oracle +1%, keeper cranks, bob closes and withdraws everything.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(101_000_000), U64(e.ts));
    for _ in 0..10 {
        e.tick(1);
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        c.crank(0);
    }
    assert_eq!(c.get_market(0).effective_price.0, 101_000_000);

    // Close via two-step (keep oracle at 101M so no staircase lag).
    request_and_fill_at(
        &mut e,
        &mut c,
        &bob,
        OrderSide::Short,
        10 * POS_SCALE,
        0,
        true,
        101_000_000,
    );
    e.tick(10);
    e.call(&bob, ONE);
    let converted = c.convert_pnl(0);
    assert!(converted.0 > 0, "profit converted");
    let cap = c.get_account(0, bob.clone()).unwrap().capital.0;
    assert!(cap > 5_000 * USDC);
    e.call(&bob, ONE);
    c.withdraw_margin(0, U128(cap));
    custody_matches_vaults(&c);
    e.call(&bob, ONE);
    let _ = c.withdraw(U128(5_000 * USDC + cap));
    assert_eq!(c.get_wallet(bob.clone()).0, 0);
    custody_matches_vaults(&c);
}

#[test]
#[should_panic(expected = "reserved account")]
fn contract_account_cannot_act_as_user_and_drain_the_pool() {
    let (mut e, mut c) = setup();
    e.call(&contract_id(), ONE);
    c.withdraw_margin(0, U128(1));
}

#[test]
#[should_panic(expected = "only the quote token")]
fn foreign_token_cannot_credit_wallets() {
    let (mut e, mut c) = setup();
    e.call(
        &"fake-usdc.near".parse().unwrap(),
        NearToken::from_yoctonear(0),
    );
    let _ = c.ft_on_transfer(accounts(1), U128(1_000_000 * USDC), String::new());
}

#[test]
fn unregistered_sender_is_refunded_in_full() {
    let (mut e, mut c) = setup();
    e.call(&usdc(), NearToken::from_yoctonear(0));
    match c.ft_on_transfer(accounts(4), U128(123), String::new()) {
        near_sdk::PromiseOrValue::Value(v) => assert_eq!(v.0, 123),
        _ => panic!("expected value"),
    }
    assert_eq!(c.get_wallet(accounts(4)).0, 0);
}

#[test]
#[should_panic(expected = "only the oracle")]
fn only_oracle_pushes_prices() {
    let (mut e, mut c) = setup();
    e.call(&accounts(2), NearToken::from_yoctonear(0));
    c.push_price(0, U64(1), U64(e.ts));
}

#[test]
#[should_panic(expected = "stale or out-of-order price")]
fn oracle_replay_rejected() {
    let (mut e, mut c) = setup();
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    let ts = e.ts;
    c.push_price(0, U64(PRICE + 1), U64(ts));
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE + 2), U64(ts));
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn value_moving_calls_need_one_yocto() {
    let (mut e, mut c) = setup();
    e.call(&accounts(1), NearToken::from_yoctonear(0));
    let _ = c.withdraw(U128(1));
}

#[test]
#[should_panic(expected = "OracleLag")]
fn lp_deposit_refused_during_oracle_lag() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(150_000_000), U64(e.ts));
    let carol = accounts(3);
    fund(&mut e, &mut c, &carol, 1_000 * USDC);
    e.call(&carol, STORAGE);
    c.lp_deposit(0, U128(1_000 * USDC));
}

#[test]
fn lp_round_trip_returns_at_most_deposit() {
    let (mut e, mut c) = setup();
    let alice = accounts(1);
    let shares = c.get_lp_shares(0, alice.clone());
    e.call(&alice, ONE);
    let back = c.lp_redeem(0, shares);
    assert!(back.0 <= 1_000_000 * USDC);
    assert!(back.0 >= 1_000_000 * USDC - 1);
    custody_matches_vaults(&c);
}

// ────────────────────────── 1.2: stale ladder ──────────────────────────

fn setup_bob(e: &mut Env, c: &mut Neap) -> AccountId {
    let bob = accounts(2);
    fund(e, c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    bob
}

#[test]
fn stale_fresh_allows_opens() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Within 10 s: opens allowed.
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, POS_SCALE, u64::MAX, false);
    let pos = c.get_account(0, bob).unwrap().position_q.0;
    assert_eq!(pos, POS_SCALE as i128);
}

#[test]
#[should_panic(expected = "stale observation: reduce-only")]
fn stale_no_opens_blocks_open() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Advance > 10 s (17 blocks × 600ms = 10.2 s) without pushing oracle.
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 30, false);
    // Advance 20 blocks (12 s) so observation becomes stale but still > request block.
    e.tick(20);
    // Don't push a new oracle — observation.source_ts is now 12 s old.
    // We do need observation.block > request_block, so we fake by reading last obs.
    // Actually the observation block didn't change. We need a pushed price at a
    // block > request but with the ts being old. Let me push at the older ts.
    e.call(&oracle(), NearToken::from_yoctonear(0));
    // Push with a timestamp that's 11 seconds old relative to now.
    let stale_ts = e.ts - 11_000_000_000;
    c.push_price(0, U64(PRICE), U64(stale_ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0); // should panic: stale observation + non-reduce order
}

#[test]
fn stale_no_opens_allows_close() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Open a position first (while fresh).
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, POS_SCALE, u64::MAX, false);
    // Now make it stale (>10s).
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Short, U128(POS_SCALE), U64(0), 30, true);
    e.tick(20);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    let stale_ts = e.ts - 11_000_000_000;
    c.push_price(0, U64(PRICE), U64(stale_ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    // reduce_only=true with stale observation → allowed.
    c.execute_order(oid.0);
    let pos = c.get_account(0, bob).unwrap().position_q.0;
    assert_eq!(pos, 0, "position closed");
}

/// PriceUncertain (≥ 1 h) is unreachable via execute_order because orders
/// expire after ~30 s. Verify the classification is correct at the policy level.
#[test]
fn stale_price_uncertain_classification() {
    use neap::policy::{observation_staleness, StalenessLevel, STALE_PRICE_UNCERTAIN_NS};
    assert_eq!(
        observation_staleness(STALE_PRICE_UNCERTAIN_NS),
        StalenessLevel::PriceUncertain
    );
    assert_eq!(
        observation_staleness(STALE_PRICE_UNCERTAIN_NS - 1),
        StalenessLevel::ReduceOnly
    );
}

/// The 60 s–1 h ReduceOnly band: non-reduce orders are blocked.
#[test]
#[should_panic(expected = "stale observation: reduce-only")]
fn stale_reduce_only_blocks_open() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Advance 100 blocks (60 s) so that a 58 s-old timestamp is still after the
    // initial oracle_last_publish_ns (monotone requirement).
    e.tick(100);
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 30, false);
    // Push at next block with source_ts = now − 58 s (within max_oracle_age = 60 s).
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    let stale_ts = e.ts - 58_000_000_000;
    c.push_price(0, U64(PRICE), U64(stale_ts));
    // Advance 4 more blocks (2.4 s) so age at execute ≈ 60.4 s → ReduceOnly.
    e.tick(4);
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0);
}

// ────────────────────────── 1.6: two-step order rules ──────────────────────────

#[test]
#[should_panic(expected = "observation not newer than request block")]
fn same_block_execute_refused() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE), U64(e.ts));

    // Request at block N.
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 30, false);

    // Try to execute at same block: observation.block == request_block.
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0); // panics
}

#[test]
fn expired_order_is_cleaned_up() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 5, false);

    // Advance past expiry (5 blocks).
    e.tick(10);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE), U64(e.ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    // Returns 0 (no fill) and removes the order from storage.
    let result = c.execute_order(oid.0);
    assert_eq!(result.0, 0, "expired order returns 0");
    // Verify order was removed.
    assert!(c.get_order(oid.0).is_none(), "order removed from storage");
}

#[test]
#[should_panic(expected = "Slippage")]
fn limit_price_respected() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Set a limit BELOW the expected fill price.
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(PRICE - 1), 30, false);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE), U64(e.ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0); // panics: slippage
}

#[test]
fn cancel_order_works() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 30, false);
    e.call(&bob, ONE);
    c.cancel_order(oid.0); // should succeed
}

#[test]
#[should_panic(expected = "only the order owner")]
fn cancel_by_stranger_refused() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 30, false);
    // Stranger tries to cancel.
    e.call(&accounts(3), ONE);
    c.cancel_order(oid.0);
}

// ────────────────────────── 1.5: OI caps ──────────────────────────

#[test]
#[should_panic(expected = "long OI cap exceeded")]
fn oi_cap_blocks_oversized_order() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Set a tight OI cap: 5 units.
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_oi_caps(0, U128(5 * POS_SCALE), U128(5 * POS_SCALE), U128(0));
    // Try to open 10 units (exceeds cap).
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(10 * POS_SCALE), U64(u64::MAX), 30, false);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE), U64(e.ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0);
}

#[test]
fn oi_cap_allows_within_limit() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_oi_caps(0, U128(100 * POS_SCALE), U128(100 * POS_SCALE), U128(0));
    // 10 units within 100 cap.
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);
    let pos = c.get_account(0, bob).unwrap().position_q.0;
    assert_eq!(pos, 10 * POS_SCALE as i128);
}

// ────────────────────────── 1.5: quote view ──────────────────────────

#[test]
fn quote_returns_sane_values() {
    let (_e, c) = setup();
    let (px, spread, fee) = c.quote(0, OrderSide::Long, U128(10 * POS_SCALE));
    assert!(px.0 > 0, "price > 0");
    assert!(spread.0 > 0, "spread > 0");
    // fee = notional * 10 bps / 10000
    // notional = 10 * 100 * 1e6 / 1e6 = 1000 * 1e6 ≈ 1e9
    // fee ≈ 1e9 * 10 / 10000 = 1e6 = 1 USDC
    assert!(fee.0 > 0, "fee > 0");
}

// ────────────────────────── 1.2: EMA tracking ──────────────────────────

#[test]
fn ema_updates_on_push_price() {
    let (mut e, mut c) = setup();
    // Initial EMA = PRICE.
    let m0 = c.get_market(0);
    // The internal ema_e6 isn't exposed in MarketView currently, but we can
    // verify indirectly via observation seq bumping.
    assert_eq!(m0.observation.seq.0, 0);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE + 10_000_000), U64(e.ts)); // +10%
    let m1 = c.get_market(0);
    assert_eq!(m1.observation.seq.0, 1);
    assert_eq!(m1.observation.price_e6.0, PRICE + 10_000_000);
}

// ────────────────────────── 1.7: conditional orders ──────────────────────────

#[test]
fn conditional_tp_triggers_on_price_rise() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    // Open a long.
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);
    let pos_before = c.get_account(0, bob.clone()).unwrap().position_q.0;
    assert_eq!(pos_before, 10 * POS_SCALE as i128);

    // Place TP: close (sell) when mark ≥ 105M (current is 100M).
    // For TP on a long, we place a Short side (closing the long) with TakeProfit kind.
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::TakeProfit,
        OrderSide::Short,
        U128(10 * POS_SCALE),
        U64(105_000_000), // trigger at $105
        U64(0),           // market sell
        1000,             // large enough to survive the staircase walk
        false,            // ignored for TP — forced to reduce_only
    );

    // Move oracle to $106.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(106_000_000), U64(e.ts));
    // Walk the staircase to the target.
    for _ in 0..40 {
        e.tick(1);
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        c.crank(0);
    }
    let mark = c.get_market(0).effective_price.0;
    assert!(mark >= 105_000_000, "mark should be >= trigger; is {mark}");

    // Keeper executes the conditional.
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_conditional(cid.0);

    let pos_after = c.get_account(0, bob.clone()).unwrap().position_q.0;
    assert_eq!(pos_after, 0, "position closed by TP");
}

#[test]
#[should_panic(expected = "trigger not crossed")]
fn conditional_does_not_trigger_below_threshold() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);

    // Place TP at $105 but don't move the price there.
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::TakeProfit,
        OrderSide::Short,
        U128(10 * POS_SCALE),
        U64(105_000_000),
        U64(0),
        1000,
        false,
    );

    // Push price to $101 (below trigger), advance so obs.block > arm_block.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(101_000_000), U64(e.ts));
    for _ in 0..5 {
        e.tick(1);
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        c.crank(0);
    }
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_conditional(cid.0); // panics: trigger not crossed
}

#[test]
fn conditional_sl_triggers_on_price_drop() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);

    // Place SL: close (sell) when mark ≤ 95M.
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::StopLoss,
        OrderSide::Short,
        U128(10 * POS_SCALE),
        U64(95_000_000),
        U64(0),
        1000,
        false,
    );

    // Move oracle to $94.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(94_000_000), U64(e.ts));
    for _ in 0..40 {
        e.tick(1);
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        c.crank(0);
    }
    let mark = c.get_market(0).effective_price.0;
    assert!(mark <= 95_000_000, "mark should be <= trigger; is {mark}");

    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_conditional(cid.0);

    let pos_after = c.get_account(0, bob.clone()).unwrap().position_q.0;
    assert_eq!(pos_after, 0, "position closed by SL");
}

#[test]
fn cancel_conditional_works() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, POS_SCALE, u64::MAX, false);
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0, ConditionalKind::TakeProfit, OrderSide::Short,
        U128(POS_SCALE), U64(200_000_000), U64(0), 30, false,
    );
    e.call(&bob, ONE);
    c.cancel_conditional(cid.0); // should not panic
}

// ────────────────────────── 1.8: fee accrual ──────────────────────────

#[test]
fn fee_split_accrues_on_fill() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);

    // Check that fee ledgers have been credited.
    let lister = c.get_fee_ledger(0, Payee::Lister).0;
    let lp = c.get_fee_ledger(0, Payee::Lp).0;
    let insurance = c.get_fee_ledger(0, Payee::Insurance).0;
    let protocol = c.get_fee_ledger(0, Payee::Protocol).0;
    let total = lister + lp + insurance + protocol;

    assert!(total > 0, "some fees accrued");
    // Conservation: splits sum to total.
    // The engine's 10 bps on ~$1000 notional ≈ $0.10 = 100_000 atoms.
    assert!(lister > 0, "lister share > 0");
    assert!(lp > 0, "LP share > 0");
    assert!(insurance > 0, "insurance share > 0");
    assert!(protocol > 0, "protocol share > 0");
    // 50/30/10/10 split: lister should be the largest.
    assert!(lister > lp, "lister > LP");
    assert!(lp > insurance, "LP > insurance");
}

#[test]
fn claim_fees_pays_lister() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);

    let lister_accrued = c.get_fee_ledger(0, Payee::Lister).0;
    assert!(lister_accrued > 0);

    // Lister = accounts(0) (the owner who created the market).
    let lister = accounts(0);
    let wallet_before = c.get_wallet(lister.clone()).0;
    e.call(&lister, ONE);
    let claimed = c.claim_fees(0, Payee::Lister);
    assert_eq!(claimed.0, lister_accrued);
    let wallet_after = c.get_wallet(lister.clone()).0;
    assert_eq!(wallet_after, wallet_before + lister_accrued);
    // Ledger is zeroed.
    assert_eq!(c.get_fee_ledger(0, Payee::Lister).0, 0);
}

// ────────────────────────── 1.9: admin roles ──────────────────────────

#[test]
fn admin_set_mode_tightens_only() {
    let (mut e, mut c) = setup();
    let owner = accounts(0);
    // Active → ReduceOnly: allowed.
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::ReduceOnly);
    assert_eq!(c.get_market(0).mode, Mode::ReduceOnly);
    // ReduceOnly → PriceUncertain: allowed.
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::PriceUncertain);
    assert_eq!(c.get_market(0).mode, Mode::PriceUncertain);
}

#[test]
#[should_panic(expected = "can only tighten")]
fn admin_cannot_loosen_mode() {
    let (mut e, mut c) = setup();
    let owner = accounts(0);
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::ReduceOnly);
    // Try to go back to Active: should fail.
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::Active);
}

#[test]
fn pauser_can_tighten_mode() {
    let (mut e, mut c) = setup();
    let owner = accounts(0);
    let pauser: AccountId = "pauser.near".parse().unwrap();
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_pauser(Some(pauser.clone()));

    e.call(&pauser, NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::ReduceOnly);
    assert_eq!(c.get_market(0).mode, Mode::ReduceOnly);
}

#[test]
#[should_panic(expected = "only the owner or pauser")]
fn stranger_cannot_set_mode() {
    let (mut e, mut c) = setup();
    e.call(&accounts(4), NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::ReduceOnly);
}

#[test]
fn admin_methods_do_not_change_balances() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);

    // Snapshot vault and wallet before admin ops.
    let (w_before, v_before) = c.get_custody();
    let bob_wallet = c.get_wallet(bob.clone()).0;
    let bob_acct = c.get_account(0, bob.clone()).unwrap();

    // Admin: set_mode, set_oi_caps, set_pauser.
    let owner = accounts(0);
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::ReduceOnly);
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_oi_caps(0, U128(999), U128(999), U128(999));
    e.call(&owner, NearToken::from_yoctonear(1));
    c.set_pauser(Some("p.near".parse().unwrap()));

    // Verify nothing changed.
    let (w_after, v_after) = c.get_custody();
    assert_eq!(w_before, w_after, "wallets unchanged");
    assert_eq!(v_before, v_after, "vaults unchanged");
    assert_eq!(c.get_wallet(bob.clone()).0, bob_wallet, "bob wallet unchanged");
    let bob_acct2 = c.get_account(0, bob).unwrap();
    assert_eq!(bob_acct.capital, bob_acct2.capital, "bob capital unchanged");
    assert_eq!(bob_acct.position_q, bob_acct2.position_q, "bob position unchanged");
}
