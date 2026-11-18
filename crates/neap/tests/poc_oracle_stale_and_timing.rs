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

// PoC (ORA-6, ORA-7, ORA-11): stale-ladder gaps and two-step order timing.
// Each test asserts the CORRECT behaviour and FAILS on the current code.

const HOUR_BLOCKS: u64 = 6_000;

// ORA-7: margin can be withdrawn against a mark that is 2 hours old (the
// ladder calls >= 1 h "PriceUncertain: no fills, no liquidation").
#[test]
fn withdraw_margin_must_honour_stale_ladder() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 40 * POS_SCALE, u64::MAX, false);
    e.tick(2 * HOUR_BLOCKS); // oracle is dead for 2 h
    e.call(&bob, ONE);
    let refused = reverts(|| c.withdraw_margin(0, U128(500 * USDC)));
    assert!(refused, "margin withdrawn from an open position at a 2 h old price");
}

// ORA-7: LP shares are redeemed at a NAV marked 2 hours ago while the pool
// carries open exposure (the exiting LP leaves the unmarked loss behind).
#[test]
fn lp_redeem_must_honour_stale_ladder() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 40 * POS_SCALE, u64::MAX, false);
    e.tick(2 * HOUR_BLOCKS);
    let alice = accounts(1);
    let shares = c.get_lp_shares(0, alice.clone()).0;
    e.call(&alice, ONE);
    let refused = reverts(|| {
        c.lp_redeem(0, U128(shares / 2));
    });
    assert!(refused, "LP redeemed at a 2 h old NAV with open pool exposure");
}

// ORA-6: invariant 3 compares BLOCKS, not price times. A price published
// before the order was requested satisfies `observation.block > request_block`
// as long as its transaction lands one block later.
#[test]
fn fill_must_not_use_a_price_published_before_the_request() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.tick(30);
    let stale_publish = e.ts - 5_000_000_000; // 5 s before the request
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(10 * POS_SCALE), U64(u64::MAX), 30, false);
    e.tick(1);
    e.call(&oracle(), ZERO);
    c.push_price(0, U64(PRICE), U64(stale_publish));
    e.call(&bob, ZERO);
    let refused = reverts(|| {
        c.execute_order(oid.0);
    });
    assert!(refused, "order filled at a price published 5 s BEFORE it was requested");
}

// ORA-6: the requester may execute at any later moment of their choosing
// against an observation taken long before; the order is not bound to the
// first post-request observation.
#[test]
fn fill_must_bind_to_the_first_post_request_observation() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(10 * POS_SCALE), U64(u64::MAX), 30, false);
    e.tick(1);
    push(&mut e, &mut c, 0, PRICE); // first post-request observation
    // Bob sits on the order. 14 blocks (8.4 s) later the real market has moved
    // but the on-chain observation has not; he executes it himself.
    e.tick(14);
    e.call(&bob, ZERO);
    let refused = reverts(|| {
        c.execute_order(oid.0);
    });
    assert!(refused, "requester executed 14 blocks later against the old observation (free option)");
}

// ORA-11: `request_order` grows contract storage but only takes 1 yocto.
#[test]
fn request_order_must_pay_for_its_storage() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&bob, ONE);
    let before = near_sdk::env::storage_usage();
    let ok = !reverts(|| {
        c.request_order(0, OrderSide::Long, U128(POS_SCALE), U64(u64::MAX), 30, false);
    });
    let grew = near_sdk::env::storage_usage() - before;
    assert!(!(ok && grew > 0), "order stored ({grew} bytes) for 1 yoctoNEAR; the contract pays");
}
