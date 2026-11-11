//! Contract-level tests in the near-sdk mocked runtime: authorization,
//! custody, storage charging and the full user flow through public methods.

use neap::model::{Mode, Owner, PriceSource, Tier};
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{I128, U128, U64};
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
    c.ft_on_transfer(who.clone(), U128(amount), String::new());
}

fn custody_matches_vaults(c: &Neap) {
    let (_w, v) = c.get_custody();
    let m = c.get_market(0);
    assert_eq!(v.0, m.vault.0, "custody vaults == engine vault");
}

#[test]
fn market_registry_shape_and_neap_events() {
    let (mut e, mut c) = setup();
    let m = c.get_market(0);
    assert_eq!(m.mode, Mode::Active);
    assert_eq!(m.tier, Tier::Major);
    assert_eq!(m.price_source, PriceSource::Pushed);
    assert_eq!(m.lister, Owner::Near(accounts(0)));
    assert_eq!(m.bond.0, 0);
    assert_eq!(m.observation.price_e6.0, PRICE);
    assert_eq!(m.observation.seq.0, 0);

    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE + 1_000_000), U64(e.ts));
    let o = c.get_market(0).observation;
    assert_eq!((o.price_e6.0, o.seq.0), (PRICE + 1_000_000, 1));

    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    e.tick(1);
    e.call(&bob, ONE);
    c.trade(0, I128(POS_SCALE as i128), U64(u64::MAX));
    let logs = near_sdk::test_utils::get_logs();
    let fill = logs
        .iter()
        .find(|l| l.contains("\"event\":\"fill\""))
        .expect("fill event");
    assert!(fill.starts_with("EVENT_JSON:"));
    assert!(fill.contains("\"standard\":\"neap\"") && fill.contains("\"version\":\"1.0.0\""));
    assert!(fill.contains(&format!("\"owner\":\"{bob}\"")) && fill.contains("\"block\":"));
}

#[test]
fn full_user_flow_through_contract() {
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    assert_eq!(c.get_wallet(bob.clone()).0, 5_000 * USDC);
    custody_matches_vaults(&c);

    e.tick(1);
    e.call(&bob, ONE);
    let px = c.trade(0, I128(10 * POS_SCALE as i128), U64(u64::MAX));
    assert_eq!(px.0, 100_050_000);
    let acct = c.get_account(0, bob.clone()).unwrap();
    assert_eq!(acct.position_q.0, 10 * POS_SCALE as i128);
    assert_eq!(c.get_market(0).pool_position_q.0, -(10 * POS_SCALE as i128));

    // Oracle +1%, keeper cranks, bob closes and withdraws everything.
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(101_000_000), U64(e.ts));
    for _ in 0..10 {
        e.tick(1);
        e.call(&accounts(3), NearToken::from_yoctonear(0));
        c.crank(0);
    }
    assert_eq!(c.get_market(0).effective_price.0, 101_000_000);
    e.call(&bob, ONE);
    c.trade(0, I128(-(10 * POS_SCALE as i128)), U64(0));
    e.tick(10);
    e.call(&bob, ONE);
    let converted = c.convert_pnl(0);
    assert!(converted.0 > 0, "profit converted");
    let cap = c.get_account(0, bob.clone()).unwrap().capital.0;
    assert!(cap > 5_000 * USDC);
    e.call(&bob, ONE);
    c.withdraw_margin(0, U128(cap));
    custody_matches_vaults(&c);
    e.call(&bob, ONE);
    let _ = c.withdraw(U128(5_000 * USDC + cap));
    assert_eq!(c.get_wallet(bob.clone()).0, 0);
    custody_matches_vaults(&c);
}

#[test]
#[should_panic(expected = "reserved account")]
fn contract_account_cannot_act_as_user_and_drain_the_pool() {
    // W-2: the pool portfolio is keyed by the contract account. If the contract
    // account (e.g. via a leftover full-access key) could call user methods, it
    // could withdraw LP capital as "its own" margin.
    let (mut e, mut c) = setup();
    e.call(&contract_id(), ONE);
    c.withdraw_margin(0, U128(1));
}

#[test]
#[should_panic(expected = "only the quote token")]
fn foreign_token_cannot_credit_wallets() {
    let (mut e, mut c) = setup();
    e.call(
        &"fake-usdc.near".parse().unwrap(),
        NearToken::from_yoctonear(0),
    );
    c.ft_on_transfer(accounts(1), U128(1_000_000 * USDC), String::new());
}

#[test]
fn unregistered_sender_is_refunded_in_full() {
    let (mut e, mut c) = setup();
    e.call(&usdc(), NearToken::from_yoctonear(0));
    match c.ft_on_transfer(accounts(4), U128(123), String::new()) {
        near_sdk::PromiseOrValue::Value(v) => assert_eq!(v.0, 123),
        _ => panic!("expected value"),
    }
    assert_eq!(c.get_wallet(accounts(4)).0, 0);
}

#[test]
#[should_panic(expected = "only the oracle")]
fn only_oracle_pushes_prices() {
    let (mut e, mut c) = setup();
    e.call(&accounts(2), NearToken::from_yoctonear(0));
    c.push_price(0, U64(1), U64(e.ts));
}

#[test]
#[should_panic(expected = "stale or out-of-order price")]
fn oracle_replay_rejected() {
    let (mut e, mut c) = setup();
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    let ts = e.ts;
    c.push_price(0, U64(PRICE + 1), U64(ts));
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(PRICE + 2), U64(ts));
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn value_moving_calls_need_one_yocto() {
    let (mut e, mut c) = setup();
    e.call(&accounts(1), NearToken::from_yoctonear(0));
    c.withdraw(U128(1));
}

#[test]
#[should_panic(expected = "OracleLag")]
fn lp_deposit_refused_during_oracle_lag() {
    // W-3: depositing at a stale mark would let a depositor front-run catch-up.
    let (mut e, mut c) = setup();
    let bob = accounts(2);
    fund(&mut e, &mut c, &bob, 10_000 * USDC);
    e.call(&bob, STORAGE);
    c.open_account(0);
    e.call(&bob, ONE);
    c.deposit(0, U128(5_000 * USDC));
    e.tick(1);
    e.call(&bob, ONE);
    c.trade(0, I128(10 * POS_SCALE as i128), U64(u64::MAX));
    e.tick(1);
    e.call(&oracle(), NearToken::from_yoctonear(0));
    c.push_price(0, U64(150_000_000), U64(e.ts));
    let carol = accounts(3);
    fund(&mut e, &mut c, &carol, 1_000 * USDC);
    e.call(&carol, STORAGE);
    c.lp_deposit(0, U128(1_000 * USDC));
}

#[test]
fn lp_round_trip_returns_at_most_deposit() {
    let (mut e, mut c) = setup();
    let alice = accounts(1);
    let shares = c.get_lp_shares(0, alice.clone());
    e.call(&alice, ONE);
    let back = c.lp_redeem(0, shares);
    assert!(back.0 <= 1_000_000 * USDC);
    assert!(back.0 >= 1_000_000 * USDC - 1);
    custody_matches_vaults(&c);
}
