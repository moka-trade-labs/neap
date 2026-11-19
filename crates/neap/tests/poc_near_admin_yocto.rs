//! PoC (NEAR-5: admin calls need 1 yocto). Asserts the CORRECT behaviour, so it FAILS today.
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


/// `set_owner` (and set_oracle / set_pauser / set_oi_caps / set_mode) must
/// require 1 yoctoNEAR so a function-call access key (what a dapp login
/// stores in the browser) cannot run them.
#[test]
#[should_panic(expected = "1 yoctoNEAR")]
fn set_owner_requires_one_yocto() {
    let (mut e, mut c) = setup();
    e.call(&accounts(0), NearToken::from_yoctonear(0));
    c.set_owner(accounts(5));
}

#[test]
#[should_panic(expected = "1 yoctoNEAR")]
fn set_oracle_requires_one_yocto() {
    let (mut e, mut c) = setup();
    e.call(&accounts(0), NearToken::from_yoctonear(0));
    c.set_oracle(accounts(5));
}
