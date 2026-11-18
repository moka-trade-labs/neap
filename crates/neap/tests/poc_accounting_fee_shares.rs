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

/// ACC-3. Spec §5 / D11: 30% of trading fees belong to the seed LP. After a
/// round-trip trade the sole LP must be able to realise at least the accrued
/// LP share on exit. CURRENTLY FAILS: the LP ledger only ever grows, the atoms
/// sit in unbudgeted engine insurance, and the LP gets its deposit back, no more.
#[test]
fn lp_fee_share_reaches_the_lp() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(10_000 * USDC));
    fill(&mut e, &mut c, &bob, OrderSide::Long, 100 * POS_SCALE, u64::MAX, false, PRICE);
    fill(&mut e, &mut c, &bob, OrderSide::Short, 100 * POS_SCALE, 0, true, PRICE);
    for _ in 0..100 {
        push(&mut e, &mut c, PRICE);
    }
    let lp_fees = c.get_fee_ledger(0, Payee::Lp).0;
    assert!(lp_fees > 0);

    let alice = accounts(1);
    let shares = c.get_lp_shares(0, alice.clone());
    e.call(&alice, ONE);
    let back = c.lp_redeem(0, shares);
    assert!(
        back.0 + 1 >= 1_000_000 * USDC + lp_fees,
        "LP got {} back; deposit 1_000_000 USDC + accrued LP fee share {}",
        back.0,
        lp_fees
    );
}
