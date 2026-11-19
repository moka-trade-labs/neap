//! RISK-2: the dynamic spread must price real pool skew (spec §5).
//! PoC from the risk review. Asserts the CORRECT behaviour, so it fails until fixed.
#![allow(dead_code, unused_imports)]

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


fn setup_bob(e: &mut Env, c: &mut Neap) -> AccountId {
    let bob = accounts(2);
    fund(e, c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    bob
}

/// Push `target` each block until the staircase reaches it.
fn walk_to(e: &mut Env, c: &mut Neap, target: u64) {
    for _ in 0..400 {
        e.tick(1);
        e.call(&oracle(), NearToken::from_yoctonear(0));
        c.push_price(0, U64(target), U64(e.ts));
        if c.get_market(0).effective_price.0 == target {
            return;
        }
    }
    panic!("staircase did not reach target");
}

fn setup_small(lp: u128) -> (Env, Neap) {
    let mut e = Env { height: 100, ts: 1_000_000_000_000 };
    let owner = accounts(0);
    e.call(&owner, NearToken::from_near(1));
    let mut c = Neap::new(owner.clone(), usdc(), oracle());
    e.call(&owner, NearToken::from_near(1));
    c.create_market("SOL-PERP".into(), U64(PRICE), params(), 5, U64(60_000_000_000));
    let alice = accounts(1);
    fund(&mut e, &mut c, &alice, lp);
    e.call(&alice, STORAGE);
    c.lp_deposit(0, U128(lp));
    (e, c)
}

fn dump(c: &Neap, tag: &str) {
    let m = c.get_market(0);
    let (w, v) = c.get_custody();
    println!("[{tag}] eff={} oiL={} oiS={} vault={} custV={} custW={} ins={} poolpos={} fund={} navlo={} navhi={} slot_last={}",
        m.effective_price.0, m.oi_long_q.0, m.oi_short_q.0, m.vault.0, v.0, w.0, m.insurance.0, m.pool_position_q.0, m.funding_rate_e9_per_slot.0, m.lp_nav_lower.0, m.lp_nav_upper.0, m.slot_last.0);
}
fn acct(c: &Neap, who: &AccountId, tag: &str) {
    let a = c.get_account(0, who.clone()).unwrap();
    println!("[{tag}] cap={} pnl={} res={} feecr={} pos={}", a.capital.0, a.pnl.0, a.reserved_pnl.0, a.fee_credits.0, a.position_q.0);
}

/// Bob is long 50 of a 100 cap (pool short 50). A short that removes that skew
/// must pay only spread_min (5 bps, major tier); a long that doubles it must
/// pay more than the same-size long from a flat book did.
#[test]
fn skew_reducing_trade_pays_spread_min_and_skew_increasing_pays_more() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_oi_caps(0, U128(100 * POS_SCALE), U128(100 * POS_SCALE), U128(0));
    let flat_long = c.quote(0, OrderSide::Long, U128(50 * POS_SCALE)).1 .0;
    request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, 50 * POS_SCALE, u64::MAX, false);
    assert_eq!(c.get_market(0).pool_position_q.0, -(50 * POS_SCALE as i128));
    let more_skew = c.quote(0, OrderSide::Long, U128(50 * POS_SCALE)).1 .0;
    let less_skew = c.quote(0, OrderSide::Short, U128(50 * POS_SCALE)).1 .0;
    assert!(
        less_skew < more_skew,
        "skew-reducing short ({less_skew} bps) must be cheaper than skew-increasing long ({more_skew} bps)"
    );
    assert!(
        more_skew > flat_long,
        "long into an already-long book ({more_skew} bps) must cost more than from flat ({flat_long} bps)"
    );
}

/// Splitting one order into small clips must not make the skew premium vanish:
/// after 20 clips of 1 the book is as skewed as after one clip of 20.
#[test]
fn small_clips_still_pay_for_accumulated_skew() {
    let (mut e, mut c) = setup();
    let bob = setup_bob(&mut e, &mut c);
    e.call(&accounts(0), NearToken::from_yoctonear(1));
    c.set_oi_caps(0, U128(100 * POS_SCALE), U128(100 * POS_SCALE), U128(0));
    for _ in 0..40 {
        request_and_fill(&mut e, &mut c, &bob, OrderSide::Long, POS_SCALE, u64::MAX, false);
    }
    // Book is now 40% of cap one-sided: k_skew 45 bps * 0.4 = 18 bps of premium is due.
    let spread = c.quote(0, OrderSide::Long, U128(POS_SCALE)).1 .0;
    assert!(spread >= 5 + 18, "41st clip pays {spread} bps; skew premium is missing");
}
