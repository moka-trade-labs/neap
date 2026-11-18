//! PoC (NEAR misc: exploration). Asserts the CORRECT behaviour, so it FAILS today.
#![allow(dead_code, unused_imports)]
use neap::model::{ConditionalKind, Mode, OrderSide, Owner, Payee, PriceSource, Tier};
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{U128, U64};
use near_sdk::test_utils::{accounts, VMContextBuilder};
use near_sdk::{env, testing_env, AccountId, NearToken};
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


fn cb(e: &Env, results: Vec<near_sdk::PromiseResult>) {
    let mut b = VMContextBuilder::new();
    b.current_account_id(contract_id())
        .predecessor_account_id(contract_id())
        .signer_account_id(accounts(2))
        .account_balance(NearToken::from_near(1_000))
        .block_height(e.height)
        .block_timestamp(e.ts);
    testing_env!(
        b.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        results
    );
}

#[test]
fn checked_failed_withdraw_recredits_once() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 1_000 * USDC);
    let (w0, v0) = c.get_custody();
    e.call(&bob, ONE);
    let _ = c.withdraw(U128(400 * USDC));
    assert_eq!(c.get_wallet(bob.clone()).0, 600 * USDC);
    cb(&e, vec![near_sdk::PromiseResult::Failed]);
    assert!(!c.on_withdraw(Owner::Near(bob.clone()), U128(400 * USDC)));
    assert_eq!(c.get_wallet(bob.clone()).0, 1_000 * USDC);
    assert_eq!(c.get_custody().0 .0, w0.0);
    assert_eq!(c.get_custody().1 .0, v0.0);
}

fn create_msg(tier: &str, source: &str) -> String {
    format!(
        r#"{{"action":"CreateMarket","symbol":"X","tier":"{tier}","price_source":{source},"spread_bps":20,"max_oracle_age_ns":"600000000000","params":{{"initial_margin_bps":"1000","maintenance_margin_bps":"500","trading_fee_bps":"10","liquidation_fee_bps":"50","liquidation_fee_cap":"1000000000","min_liquidation_abs":"0","max_price_move_bps_per_slot":"30","max_accrual_dt_slots":"10","max_abs_funding_e9_per_slot":"100","h_min":"5","h_max":"50","min_nonzero_mm_req":"1000","min_nonzero_im_req":"2000"}}}}"#
    )
}

fn pool_json(usdc_atoms: u128, tok: u128) -> Vec<u8> {
    format!(r#"{{"amounts":["{usdc_atoms}","{tok}"],"token_account_ids":["usdc.near","tok.near"]}}"#).into_bytes()
}

/// NEAR-6: the EMA is stepped once per callback, not once per block/time, and
/// refresh_price is permissionless, so N refreshes in one block collapse the
/// smoothing (and an attacker-chosen `dex` is accepted with no allow-list).
#[test]
fn ema_moves_at_most_one_step_per_block() {
    let (mut e, mut c) = setup();
    let mallory = accounts(4);
    e.call(&usdc(), NearToken::from_yoctonear(0));
    let src = r#"{"NearPool":{"dex":"evil-dex.near","pool_id":1,"quote_pool":null}}"#;
    let _ = c.ft_on_transfer(mallory.clone(), U128(50 * USDC), create_msg("LongTail", src));
    assert_eq!(c.get_markets_count(), 2);
    e.tick(1);
    cb(&e, vec![near_sdk::PromiseResult::Successful(pool_json(2_000_000_000_000, 2_000_000_000_000))]);
    c.on_price_read(1);
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    assert_eq!(c.get_market(1).observation.price_e6.0, 1_000_000);
    c.activate_market(1);
    assert_eq!(c.get_market(1).mode, Mode::Active);
    // same block: +15% spot, one refresh
    e.tick(1);
    cb(&e, vec![near_sdk::PromiseResult::Successful(pool_json(2_300_000_000_000, 2_000_000_000_000))]);
    c.on_price_read(1);
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    let one_step = c.get_market(1).oracle_target.0;
    for _ in 0..60 {
        cb(&e, vec![near_sdk::PromiseResult::Successful(pool_json(2_300_000_000_000, 2_000_000_000_000))]);
        c.on_price_read(1);
    }
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    let after = c.get_market(1).oracle_target.0;
    eprintln!("ema after 1 refresh {one_step}, after 61 same-block refreshes {after}");
    assert_eq!(after, one_step, "EMA must not advance more than once per block");
}

#[test]
fn explore_pool_account_as_keeper_target() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(10 * POS_SCALE), U64(u64::MAX), 30, false);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE), U64(e.ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0);
    e.tick(1);
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.keeper_step(0, contract_id())));
    eprintln!("keeper_step(pool) -> {r:?}");
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.liquidate(0, contract_id()).0));
    eprintln!("liquidate(pool) -> {r:?}");
}

/// NEAR-8: `size_q as i128` wraps for size_q >= 2^127, so a "Short" order buys.
#[test]
fn oversized_size_q_is_rejected_at_request() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    e.call(&bob, ONE);
    let wrapped = u128::MAX - 10 * POS_SCALE + 1; // as i128 == -(10 units)
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        c.request_order(0, OrderSide::Short, U128(wrapped), U64(u64::MAX), 30, false)
    }));
    if let Ok(oid) = &r {
        e.tick(1);
        e.call(&oracle(), NearToken::from_yoctonear(0));
        c.push_price(0, U64(PRICE), U64(e.ts));
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        let x = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.execute_order(oid.0).0));
        eprintln!("execute -> {x:?}");
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        eprintln!("position after a `Short` order: {}", c.get_account(0, bob.clone()).unwrap().position_q.0);
    }
    assert!(r.is_err(), "size_q above i128::MAX must be refused");
}

/// NEAR-4: per-call storage growth of request_order vs NEAR attached.
#[test]
fn pending_orders_do_not_grow_contract_paid_storage() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    let n = 300u128;
    let mut grew = 0u64;
    for _ in 0..n {
        e.call(&bob, ONE);
        let s = env::storage_usage();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.request_order(0, OrderSide::Long, U128(1), U64(1), 50, false)
        }));
        if r.is_err() { return; } // refused: a cap or a storage charge exists
        grew += env::storage_usage() - s;
    }
    eprintln!("{n} pending orders from one account: {grew} bytes for {n} yoctoNEAR");
    assert!(env::storage_byte_cost().as_yoctonear() * (grew as u128) <= n,
        "one account parked {grew} bytes of orders at the contract's expense");
}

/// NEAR-9: storage paid for a conditional is refunded when it is removed.
#[test]
fn cancelling_a_conditional_refunds_its_storage() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, STORAGE);
    let id = c.place_conditional(0, ConditionalKind::Limit, OrderSide::Long, U128(1), U64(1), U64(1), 100, false);
    e.call(&bob, ONE);
    c.cancel_conditional(id.0);
    let r = near_sdk::test_utils::get_created_receipts();
    eprintln!("receipts after cancel: {}", r.len());
    assert!(!r.is_empty(), "no refund transfer on cancel");
}
