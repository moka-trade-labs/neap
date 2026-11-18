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

// PoC (ORA-3, ORA-4, ORA-5): NearPool reader (`refresh_price`/`on_price_read`).
// Each test asserts the CORRECT behaviour and FAILS on the current code.

const TOK: u128 = 1_000_000; // 6-decimal base token (the only kind the reader supports)

fn pool_json(usdc_atoms: u128, token_atoms: u128) -> PromiseResult {
    PromiseResult::Successful(
        format!(
            r#"{{"pool_kind":"SIMPLE_POOL","token_account_ids":["usdc.near","tok.near"],"amounts":["{usdc_atoms}","{token_atoms}"],"total_fee":30,"shares_total_supply":"1"}}"#
        )
        .into_bytes(),
    )
}

fn create_msg(tier: &str, dex: &str) -> String {
    format!(
        r#"{{"action":"CreateMarket","symbol":"TOK-PERP","tier":"{tier}",
            "price_source":{{"NearPool":{{"dex":"{dex}","pool_id":7,"quote_pool":null}}}},
            "spread_bps":20,"max_oracle_age_ns":"60000000000",
            "params":{{"initial_margin_bps":"1000","maintenance_margin_bps":"500","trading_fee_bps":"10",
              "liquidation_fee_bps":"50","liquidation_fee_cap":"1000000000","min_liquidation_abs":"0",
              "max_price_move_bps_per_slot":"30","max_accrual_dt_slots":"10","max_abs_funding_e9_per_slot":"100",
              "h_min":"5","h_max":"50","min_nonzero_mm_req":"1000","min_nonzero_im_req":"2000"}}}}"#
    )
}

/// Lists NearPool market 1 (LongTail, 50 USDC bond), first observation at $1
/// with 5,000 USDC of quote depth, activates it.
fn setup_pool_market() -> (Env, Neap, AccountId) {
    let (mut e, mut c) = setup();
    let lister = accounts(4);
    e.call(&usdc(), ZERO);
    let _ = c.ft_on_transfer(lister.clone(), U128(50 * USDC), create_msg("LongTail", "ref.near"));
    assert_eq!(c.get_markets_count(), 2);
    e.tick(1);
    e.callback(pool_json(5_000 * USDC, 5_000 * TOK));
    c.on_price_read(1);
    e.call(&accounts(3), ZERO);
    c.activate_market(1);
    assert_eq!(c.get_market(1).mode, Mode::Active);
    assert_eq!(c.get_market(1).observation.price_e6.0, 1_000_000);
    (e, c, lister)
}

// ORA-3: the EMA is per-sample, not per-time, and `refresh_price` is
// permissionless and unthrottled. N refreshes landing in ONE block (one tx
// with N FunctionCall actions) pull the "smoothed" mark all the way to spot.
#[test]
fn ema_must_not_converge_to_spot_within_one_block() {
    let (mut e, mut c, _) = setup_pool_market();
    // Spot is pushed +15% (inside the 20% band) for a single block.
    e.tick(1);
    for _ in 0..100 {
        e.callback(pool_json(5_362 * USDC, 4_663 * TOK)); // ~1.15
        c.on_price_read(1);
    }
    let m = c.get_market(1);
    let moved_bps = (m.oracle_target.0 as i128 - 1_000_000) * 10_000 / 1_000_000;
    // One 5% EMA step of a 15% gap is 75 bps. Anything near 1500 means the
    // EMA gives no manipulation resistance at all.
    assert!(
        moved_bps <= 100,
        "mark target moved {moved_bps} bps in one block from one manipulated spot reading"
    );
}

// ORA-4: once spot is >20% away from the (stale, on-demand) EMA every reading
// is discarded, the EMA never moves, the market is ReduceOnly forever and its
// observation ages into PriceUncertain: no closes, no liquidations.
#[test]
fn oracle_must_recover_after_a_sustained_real_move() {
    let (mut e, mut c, _) = setup_pool_market();
    let seq0 = c.get_market(1).observation.seq.0;
    // The token really reprices to ~$1.30 and stays there for 2 hours; the
    // keeper refreshes every minute.
    for _ in 0..120 {
        e.tick(100);
        e.callback(pool_json(5_700 * USDC, 4_385 * TOK));
        c.on_price_read(1);
    }
    let m = c.get_market(1);
    assert!(
        m.observation.seq.0 > seq0,
        "120 consistent readings over 2 h were all dropped; observation is {} s old, mode {:?}",
        (e.ts - m.observation.source_ts.0) / 1_000_000_000,
        m.mode
    );
}

// ORA-5: the lister names the `dex` account. Nothing restricts it to Ref/Rhea,
// so the "pool" can be the lister's own contract returning any reserves. A
// "Major" market needs no bond at all.
#[test]
fn market_priced_by_lister_controlled_contract_must_not_activate() {
    let (mut e, mut c) = setup();
    let attacker = accounts(5);
    e.call(&usdc(), ZERO);
    let _ = c.ft_on_transfer(attacker.clone(), U128(1), create_msg("Major", "attacker-oracle.near"));
    let created = c.get_markets_count() == 2;
    let mut active = false;
    if created {
        e.tick(1);
        // The attacker's contract answers get_pool with whatever it likes.
        e.callback(pool_json(50_000_000 * USDC, 1_000 * TOK));
        c.on_price_read(1);
        e.call(&attacker, ZERO);
        let _ = reverts(|| c.activate_market(1));
        e.call(&attacker, ZERO);
        active = c.get_market(1).mode == Mode::Active;
    }
    assert!(
        !active,
        "1-atom 'Major' market priced by attacker-oracle.near went Active at ${}",
        c.get_market(1).observation.price_e6.0 / 1_000_000
    );
}
