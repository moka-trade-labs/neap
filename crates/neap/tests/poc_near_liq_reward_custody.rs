//! PoC (NEAR-1: liquidation reward breaks custody). Asserts the CORRECT behaviour, so it FAILS today.
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


/// After a liquidation the wrapper's vault bucket must still equal the engine
/// vault (invariant 1). Today `liquidate` credits the caller's wallet with 60%
/// of the fee out of `custody_vaults` while the engine keeps 100% of the fee
/// in its insurance, so the engine vault is no longer fully backed.
#[test]
fn liquidation_reward_keeps_custody_equal_to_engine_vault() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 1_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(1_000 * USDC));
    // ~9x long.
    e.call(&bob, ONE);
    let oid = c.request_order(0, OrderSide::Long, U128(90 * POS_SCALE), U64(u64::MAX), 30, false);
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE), U64(e.ts));
    e.call(&accounts(3), NearToken::from_yoctonear(0));
    c.execute_order(oid.0);
    custody_matches_vaults(&c);
    // -8%.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(92_000_000), U64(e.ts));
    for _ in 0..40 {
        e.tick(1);
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        c.crank(0);
    }
    assert_eq!(c.get_market(0).effective_price.0, 92_000_000);
    let keeper = accounts(4);
    let (w0, v0) = c.get_custody();
    e.call(&keeper, NearToken::from_yoctonear(0));
    let closed = c.liquidate(0, bob.clone());
    assert!(closed.0 > 0);
    let reward = c.get_wallet(keeper.clone()).0;
    assert!(reward > 0, "keeper was paid");
    let (w1, v1) = c.get_custody();
    assert_eq!(w0.0 + v0.0, w1.0 + v1.0, "total custody conserved");
    // The engine must have released exactly what the wrapper paid out.
    custody_matches_vaults(&c);
}
