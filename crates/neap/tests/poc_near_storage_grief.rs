//! PoC (NEAR-2/3/4: contract-paid storage). Asserts the CORRECT behaviour, so it FAILS today.
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


fn create_msg(tier: &str, symbol: &str, source: &str) -> String {
    format!(
        r#"{{"action":"CreateMarket","symbol":"{symbol}","tier":"{tier}","price_source":{source},"spread_bps":20,"max_oracle_age_ns":"600000000000","params":{{"initial_margin_bps":"1000","maintenance_margin_bps":"500","trading_fee_bps":"10","liquidation_fee_bps":"50","liquidation_fee_cap":"1000000000","min_liquidation_abs":"0","max_price_move_bps_per_slot":"30","max_accrual_dt_slots":"10","max_abs_funding_e9_per_slot":"100","h_min":"5","h_max":"50","min_nonzero_mm_req":"1000","min_nonzero_im_req":"2000"}}}}"#
    )
}
const POOL: &str = r#"{"NearPool":{"dex":"ref.near","pool_id":1,"quote_pool":null}}"#;

/// NEAR-2a: anyone can list a `Major` market with a 1-atom "bond" and no
/// NEAR for the ~group+portfolio bytes the contract then stakes for forever.
#[test]
fn zero_bond_major_listing_by_a_stranger_is_refused() {
    let (mut e, mut c) = setup();
    let mallory = accounts(4);
    let before_markets = c.get_markets_count();
    e.call(&usdc(), NearToken::from_yoctonear(0));
    let before = env::storage_usage();
    let r = c.ft_on_transfer(mallory.clone(), U128(1), create_msg("Major", "X", POOL));
    let grew = env::storage_usage() - before;
    eprintln!("storage bytes the contract paid for one free listing: {grew}");
    let refunded = match r {
        near_sdk::PromiseOrValue::Value(v) => v.0,
        _ => panic!("value"),
    };
    assert_eq!(refunded, 1, "the 1-atom transfer is refunded");
    assert_eq!(c.get_markets_count(), before_markets, "no market created");
    assert_eq!(grew, 0, "no storage staked by the contract");
}

/// NEAR-2b: the lister bond must be locked, not credited to a withdrawable
/// wallet. Today the 50 USDC comes straight back with `withdraw`, so listing
/// (and its storage) is free and repeatable.
#[test]
fn lister_bond_is_not_withdrawable() {
    let (mut e, mut c) = setup();
    let mallory = accounts(4);
    e.call(&usdc(), NearToken::from_yoctonear(0));
    let _ = c.ft_on_transfer(mallory.clone(), U128(50 * USDC), create_msg("LongTail", "X", POOL));
    assert_eq!(c.get_market(1).bond.0, 50 * USDC);
    assert_eq!(c.get_wallet(mallory.clone()).0, 0, "bond must not sit in the lister's free wallet");
}

/// NEAR-3: `price_source` (and `symbol`) are unbounded user input stored at the
/// contract's expense. One ft_transfer_call can make the contract stake NEAR
/// for hundreds of KB.
#[test]
fn listing_payload_size_is_bounded() {
    let (mut e, mut c) = setup();
    let mallory = accounts(4);
    let pools: Vec<String> = (0..400).map(|_| format!("\"{}\"", "a".repeat(1000))).collect();
    let source = format!(r#"{{"Tee":{{"chain":"Solana","pools":[{}]}}}}"#, pools.join(","));
    e.call(&usdc(), NearToken::from_yoctonear(0));
    let before = env::storage_usage();
    let _ = c.ft_on_transfer(mallory, U128(50 * USDC), create_msg("LongTail", "X", &source));
    let grew = env::storage_usage() - before;
    eprintln!("storage bytes paid by the contract: {grew}");
    assert!(grew < 20_000, "unbounded listing payload stored for free: {grew} bytes");
}
