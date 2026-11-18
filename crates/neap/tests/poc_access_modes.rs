//! PoC (access-control / lifecycle review): market-mode enforcement.
//!
//! Every test asserts the CORRECT behaviour, so each one FAILS on the current
//! code and should pass once the finding is fixed. Harness copied from
//! `contract_unit.rs`.

use std::panic::{catch_unwind, AssertUnwindSafe};

use neap::model::{ConditionalKind, Mode, OrderSide};
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};
use percolator::POS_SCALE;

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

fn fund(e: &mut Env, c: &mut Neap, who: &AccountId, amount: u128) {
    e.call(who, STORAGE);
    c.register();
    e.call(&usdc(), ZERO);
    let _ = c.ft_on_transfer(who.clone(), U128(amount), String::new());
}

/// Owner = accounts(0), LP = accounts(1), trader bob = accounts(2) with 5k margin.
fn setup() -> (Env, Neap, AccountId) {
    let mut e = Env {
        height: 100,
        ts: 1_000_000_000_000,
    };
    let owner = accounts(0);
    e.call(&owner, NearToken::from_near(1));
    let mut c = Neap::new(owner.clone(), usdc(), oracle());
    e.call(&owner, NearToken::from_near(1));
    c.create_market("SOL-PERP".into(), U64(PRICE), params(), 5, U64(60_000_000_000));
    let alice = accounts(1);
    fund(&mut e, &mut c, &alice, 1_000_000 * USDC);
    e.call(&alice, STORAGE);
    c.lp_deposit(0, U128(1_000_000 * USDC));

    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    (e, c, bob)
}

fn push(e: &mut Env, c: &mut Neap, price: u64) {
    e.call(&oracle(), ZERO);
    c.push_price(0, U64(price), U64(e.ts));
}

fn open_long(e: &mut Env, c: &mut Neap, bob: &AccountId, size_q: u128) {
    e.call(bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(size_q), U64(u64::MAX), 30, false);
    e.tick(1);
    push(e, c, PRICE);
    e.call(&accounts(3), ZERO);
    c.execute_order(oid.0);
}

fn pos(c: &Neap, who: &AccountId) -> i128 {
    c.get_account(0, who.clone()).unwrap().position_q.0
}

/// ACL-1a: a position-OPENING resting limit (conditional) placed while the
/// market was Active must not fill after the admin forces ReduceOnly.
/// `execute_conditional` has no market-mode check at all (H-3 was only fixed
/// in `execute_order`).
#[test]
fn opening_conditional_must_not_fill_in_reduce_only_market() {
    let (mut e, mut c, bob) = setup();
    // Opening buy limit that is immediately "triggered" (mark <= trigger).
    e.call(&bob, STORAGE);
    let cid = c.place_conditional(
        0,
        ConditionalKind::Limit,
        OrderSide::Long,
        U128(10 * POS_SCALE),
        U64(PRICE * 2),
        U64(u64::MAX),
        1_000,
        false,
    );
    // Admin pauses opens.
    e.call(&accounts(0), ZERO);
    c.set_mode(0, Mode::ReduceOnly);
    assert_eq!(c.get_market(0).mode, Mode::ReduceOnly);

    e.tick(1);
    push(&mut e, &mut c, PRICE);
    e.call(&accounts(3), ZERO);
    let r = catch_unwind(AssertUnwindSafe(|| c.execute_conditional(cid.0)));
    assert!(
        r.is_err(),
        "opening conditional filled in a ReduceOnly market (position now {})",
        pos(&c, &bob)
    );
}

/// ACL-1b: an opening two-step order requested just before the admin moves the
/// market to PriceUncertain ("no fills") must not fill. `execute_order` only
/// checks `mode == ReduceOnly`.
#[test]
fn opening_order_must_not_fill_in_price_uncertain_market() {
    let (mut e, mut c, bob) = setup();
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(10 * POS_SCALE), U64(u64::MAX), 30, false);
    e.call(&accounts(0), ZERO);
    c.set_mode(0, Mode::PriceUncertain);

    e.tick(1);
    push(&mut e, &mut c, PRICE);
    e.call(&accounts(3), ZERO);
    let r = catch_unwind(AssertUnwindSafe(|| c.execute_order(oid.0)));
    assert!(
        r.is_err(),
        "opening order filled in a PriceUncertain market (position now {})",
        pos(&c, &bob)
    );
}

/// ACL-2: users must always be able to exit. Once the pauser (a limited,
/// tighten-only key) sets PriceUncertain there is no way back (no loosen
/// method exists for anyone) and `request_order` refuses even reduce-only
/// orders, so an open position can never be closed again by its owner, while
/// `liquidate` keeps working against it. With a fresh oracle price available
/// the owner must be able to request (and get filled on) a close.
#[test]
fn trader_can_close_position_in_price_uncertain_market() {
    let (mut e, mut c, bob) = setup();
    open_long(&mut e, &mut c, &bob, 10 * POS_SCALE);
    assert_eq!(pos(&c, &bob), (10 * POS_SCALE) as i128);

    let pauser: AccountId = "pauser.near".parse().unwrap();
    e.call(&accounts(0), ZERO);
    c.set_pauser(Some(pauser.clone()));
    e.call(&pauser, ZERO);
    c.set_mode(0, Mode::PriceUncertain);

    // The oracle is alive and fresh; only the admin mode stands in the way.
    e.tick(1);
    push(&mut e, &mut c, PRICE);
    e.call(&bob, ONE);
    let r = catch_unwind(AssertUnwindSafe(|| {
        c.request_order(0, OrderSide::Short, U128(10 * POS_SCALE), U64(0), 30, true)
    }));
    assert!(
        r.is_ok(),
        "reduce-only close refused in PriceUncertain: position is stuck forever"
    );
}

/// ACL-2 (second half): no account can ever move a market back to Active.
/// The owner (later the 2-of-3 multisig) must be able to restore a market the
/// pauser, or the automatic band breach, put into ReduceOnly.
#[test]
fn owner_can_restore_market_after_reduce_only() {
    let (mut e, mut c, _bob) = setup();
    e.call(&accounts(0), ZERO);
    c.set_mode(0, Mode::ReduceOnly);
    e.call(&accounts(0), ZERO);
    let r = catch_unwind(AssertUnwindSafe(|| c.set_mode(0, Mode::Active)));
    assert!(
        r.is_ok() && c.get_market(0).mode == Mode::Active,
        "no recovery path: ReduceOnly is permanent for every market"
    );
}

/// ACL-9: a trader in a PriceUncertain (admin) market cannot top up margin
/// (`deposit` requires Active|ReduceOnly) but can still be liquidated.
#[test]
fn trader_can_add_margin_in_price_uncertain_market() {
    let (mut e, mut c, bob) = setup();
    open_long(&mut e, &mut c, &bob, 10 * POS_SCALE);
    e.call(&accounts(0), ZERO);
    c.set_mode(0, Mode::PriceUncertain);
    e.call(&bob, ONE);
    let r = catch_unwind(AssertUnwindSafe(|| c.deposit(0, U128(100 * USDC))));
    assert!(r.is_ok(), "margin top-up refused while liquidations stay enabled");
}

/// ACL-8: with the oracle dead for 2 h (staleness = PriceUncertain: no fills,
/// no liquidations) an account with an OPEN position can still pull its free
/// margin out at the frozen mark. Value extraction against a price nobody
/// trusts must be refused for non-flat accounts.
#[test]
fn margin_withdrawal_with_open_position_refused_when_price_is_hours_stale() {
    let (mut e, mut c, bob) = setup();
    open_long(&mut e, &mut c, &bob, 10 * POS_SCALE);
    // 2 h pass with no oracle update.
    e.height += 5;
    e.ts += 2 * 3_600 * 1_000_000_000;
    let before = c.get_wallet(bob.clone()).0;
    e.call(&bob, ONE);
    let r = catch_unwind(AssertUnwindSafe(|| c.withdraw_margin(0, U128(3_000 * USDC))));
    assert!(
        r.is_err(),
        "withdrew {} from a leveraged account against a 2h-old price",
        c.get_wallet(bob.clone()).0 - before
    );
}
