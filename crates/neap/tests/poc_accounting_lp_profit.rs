#![allow(dead_code, unused_imports)]
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

fn push(e: &mut Env, c: &mut Neap, price: u64) {
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(price), U64(e.ts));
}

fn walk(e: &mut Env, c: &mut Neap, price: u64) {
    for _ in 0..200 {
        push(e, c, price);
        if c.get_market(0).effective_price.0 == price { return; }
    }
    panic!("did not reach target");
}

fn fill(e: &mut Env, c: &mut Neap, who: &AccountId, side: OrderSide, size_q: u128, limit: u64, ro: bool, px: u64) -> u64 {
    e.call(who, ONE);
    let oid = c.request_order(0, side, U128(size_q), U64(limit), 30, ro);
    push(e, c, px);
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0).0
}

/// Trader loses to the pool and closes; the pool is flat again.
fn trader_loses_to_pool(e: &mut Env, c: &mut Neap) {
    let bob = accounts(2);
    fund(e, c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(10_000 * USDC));
    fill(e, c, &bob, OrderSide::Long, 100 * POS_SCALE, u64::MAX, false, PRICE);
    walk(e, c, 97_000_000);
    fill(e, c, &bob, OrderSide::Short, 100 * POS_SCALE, 0, true, 97_000_000);
    assert_eq!(c.get_market(0).pool_position_q.0, 0);
    // Well past the warmup window (h_max = 50 slots).
    for _ in 0..100 {
        push(e, c, 97_000_000);
    }
}

/// ACC-2. The pool earned ~300 USDC from a losing trader. The sole LP must be
/// able to redeem it. CURRENTLY FAILS: the profit stays as pool `pnl` forever
/// (nobody can call convert_pnl for the pool) and redemption pays capital only.
#[test]
fn sole_lp_can_redeem_pool_profit() {
    let (mut e, mut c) = setup();
    trader_loses_to_pool(&mut e, &mut c);
    let alice = accounts(1);
    let shares = c.get_lp_shares(0, alice.clone());
    e.call(&alice, ONE);
    let back = c.lp_redeem(0, shares);
    assert!(
        back.0 >= 1_000_000 * USDC + 290 * USDC,
        "LP redeemed {} for a 1_000_000 USDC deposit after the pool earned ~300 USDC",
        back.0
    );
}

/// ACC-2b. After every LP has left, the unconverted profit is orphaned in the
/// pool (shares = 0, nav_upper > 0). The next depositor is priced at nav_upper
/// and redeems at nav_lower, so an immediate round trip must not lose more than
/// rounding. CURRENTLY FAILS: a 1_000 USDC round trip returns ~769 USDC.
#[test]
fn deposit_after_full_exit_does_not_lose_to_orphaned_pnl() {
    let (mut e, mut c) = setup();
    trader_loses_to_pool(&mut e, &mut c);
    let alice = accounts(1);
    let shares = c.get_lp_shares(0, alice.clone());
    e.call(&alice, ONE);
    c.lp_redeem(0, shares);

    let dave = accounts(5);
    fund(&mut e, &mut c, &dave, 1_000 * USDC);
    push(&mut e, &mut c, 97_000_000);
    e.call(&dave, STORAGE);
    let s = c.lp_deposit(0, U128(1_000 * USDC));
    e.call(&dave, ONE);
    let back = c.lp_redeem(0, s);
    assert!(
        back.0 >= 999 * USDC,
        "1_000 USDC LP round trip returned only {}",
        back.0
    );
}
