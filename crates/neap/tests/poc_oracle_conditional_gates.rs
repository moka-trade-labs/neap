#![allow(dead_code, unused_imports)]
use neap::model::{ConditionalKind, Mode, OrderSide, Owner, Payee, PriceSource, Tier};
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken, PromiseResult};
use percolator::POS_SCALE;
use std::panic::{catch_unwind, AssertUnwindSafe};

const USDC: u128 = 1_000_000;
const PRICE: u64 = 100_000_000;
const ONE: NearToken = NearToken::from_yoctonear(1);
const ZERO: NearToken = NearToken::from_yoctonear(0);
const STORAGE: NearToken = NearToken::from_millinear(200);

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
    fn ctx(&self, who: &AccountId, deposit: NearToken) -> near_sdk::VMContext {
        let mut b = VMContextBuilder::new();
        b.current_account_id(contract_id())
            .predecessor_account_id(who.clone())
            .signer_account_id(who.clone())
            .attached_deposit(deposit)
            .account_balance(NearToken::from_near(1_000))
            .block_height(self.height)
            .block_timestamp(self.ts);
        b.build()
    }
    fn call(&mut self, who: &AccountId, deposit: NearToken) {
        testing_env!(self.ctx(who, deposit));
    }
    /// Context for the `on_price_read` callback with a mocked `get_pool` result.
    fn callback(&mut self, result: PromiseResult) {
        testing_env!(
            self.ctx(&contract_id(), ZERO),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![result]
        );
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

/// Owner-listed `Pushed` market 0 at $100, LP alice seeds 1M USDC.
fn setup() -> (Env, Neap) {
    let mut e = Env {
        height: 100,
        ts: 1_000_000_000_000,
    };
    let owner = accounts(0);
    e.call(&owner, NearToken::from_near(1));
    let mut c = Neap::new(owner.clone(), usdc(), oracle());
    e.call(&owner, NearToken::from_near(1));
    let m = c.create_market("SOL-PERP".into(), U64(PRICE), params(), 5, U64(60_000_000_000));
    assert_eq!(m, 0);
    let alice = accounts(1);
    fund(&mut e, &mut c, &alice, 1_000_000 * USDC);
    e.call(&alice, STORAGE);
    c.lp_deposit(0, U128(1_000_000 * USDC));
    (e, c)
}

fn fund(e: &mut Env, c: &mut Neap, who: &AccountId, amount: u128) {
    e.call(who, STORAGE);
    c.register();
    e.call(&usdc(), ZERO);
    let _ = c.ft_on_transfer(who.clone(), U128(amount), String::new());
}

fn setup_bob(e: &mut Env, c: &mut Neap) -> AccountId {
    let bob = accounts(2);
    fund(e, c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    bob
}

fn push(e: &mut Env, c: &mut Neap, market: u32, price: u64) {
    e.call(&oracle(), ZERO);
    c.push_price(market, U64(price), U64(e.ts));
}

/// request -> next block -> fresh push -> execute.
fn request_and_fill(
    e: &mut Env,
    c: &mut Neap,
    who: &AccountId,
    side: OrderSide,
    size_q: u128,
    limit: u64,
    reduce_only: bool,
) -> u64 {
    e.call(who, ONE);
    let oid = c.request_order(0, side, U128(size_q), U64(limit), 30, reduce_only);
    e.tick(1);
    push(e, c, 0, PRICE);
    e.call(&accounts(3), ZERO);
    c.execute_order(oid.0).0
}

/// True if `f` panicked (the mocked receipt "reverted").
fn reverts<F: FnOnce()>(f: F) -> bool {
    catch_unwind(AssertUnwindSafe(f)).is_err()
}

// PoC (ORA-1, ORA-2, ORA-9, ORA-10): gates that `execute_order` applies but
// `execute_conditional` does not, and market modes neither honours.
// Each test asserts the CORRECT behaviour and FAILS on the current code.

// ORA-1: an opening `Limit` conditional ignores the market's OI caps.
#[test]
fn opening_conditional_must_respect_oi_cap() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_oi_caps(0, U128(5 * POS_SCALE), U128(5 * POS_SCALE), U128(2 * POS_SCALE));

    // "Limit buy" whose trigger is always crossed == a market order.
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::Limit,
        OrderSide::Long,
        U128(40 * POS_SCALE), // 8x the side cap, 20x the per-account cap
        U64(u64::MAX),
        U64(u64::MAX),
        1000,
        false,
    );
    e.tick(1);
    push(&mut e, &mut c, 0, PRICE);
    e.call(&accounts(3), ZERO);
    let refused = reverts(|| {
        c.execute_conditional(cid.0);
    });
    assert!(refused, "40-unit open filled through a 5-unit OI cap");
}

// ORA-2: an opening conditional fills in a ReduceOnly market (H-3 was fixed
// for execute_order only).
#[test]
fn opening_conditional_must_not_fill_in_reduce_only_market() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::ReduceOnly);

    // Placement is even allowed while the market is already ReduceOnly.
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::Limit,
        OrderSide::Long,
        U128(10 * POS_SCALE),
        U64(u64::MAX),
        U64(u64::MAX),
        1000,
        false,
    );
    e.tick(1);
    push(&mut e, &mut c, 0, PRICE);
    e.call(&accounts(3), ZERO);
    let refused = reverts(|| {
        c.execute_conditional(cid.0);
    });
    assert!(refused, "position opened in a ReduceOnly market");
}

// ORA-2: tightening ReduceOnly -> PriceUncertain RE-ENABLES opening fills for
// pending two-step orders (the mode check is `== ReduceOnly` only).
#[test]
fn opening_order_must_not_fill_in_price_uncertain_market() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(10 * POS_SCALE), U64(u64::MAX), 30, false);
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_mode(0, Mode::PriceUncertain); // pauser hits the big red button
    e.tick(1);
    push(&mut e, &mut c, 0, PRICE);
    e.call(&accounts(3), ZERO);
    let refused = reverts(|| {
        c.execute_order(oid.0);
    });
    assert!(refused, "position opened in a PriceUncertain market");
}

// ORA-9: a stop-loss sized for the original position can never fire once the
// position has been partially closed (it should clamp to the remaining size).
#[test]
fn stop_loss_must_still_fire_after_partial_close() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 10 * POS_SCALE, u64::MAX, false);
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::StopLoss,
        OrderSide::Short,
        U128(10 * POS_SCALE),
        U64(99_000_000),
        U64(0),
        100_000,
        true,
    );
    // Bob takes some size off by hand.
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Short, 3 * POS_SCALE, 0, true);
    assert_eq!(c.get_account(0, bob.clone()).unwrap().position_q.0, 7 * POS_SCALE as i128);

    // Price falls through the stop.
    e.tick(1);
    push(&mut e, &mut c, 0, 97_000_000);
    for _ in 0..30 {
        e.tick(1);
        push(&mut e, &mut c, 0, 97_000_000);
    }
    assert!(c.get_market(0).effective_price.0 <= 99_000_000);
    e.call(&accounts(3), ZERO);
    let refused = reverts(|| {
        c.execute_conditional(cid.0);
    });
    assert!(!refused, "stop-loss is dead: 'conditional does not reduce position'");
    assert_eq!(c.get_account(0, bob).unwrap().position_q.0, 0);
}

// ORA-10: a buy-side conditional with limit_price 0 can never fill
// (`within_limit` needs exec <= 0) but is accepted at placement.
#[test]
fn buy_conditional_with_zero_limit_must_be_rejected_at_placement() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, STORAGE);
    let refused = reverts(|| {
        // Stop-loss for a short: buy when mark >= 105, "market" (limit 0),
        // exactly the convention the model documents for sells.
        c.place_conditional(
            0,
            ConditionalKind::StopLoss,
            OrderSide::Long,
            U128(POS_SCALE),
            U64(105_000_000),
            U64(0),
            1000,
            true,
        );
    });
    assert!(refused, "unfillable buy conditional accepted (request_order rejects the same input)");
}
