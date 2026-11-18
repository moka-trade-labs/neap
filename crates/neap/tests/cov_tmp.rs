#[path = "cov_harness.rs"]
mod h;
use h::*;
fn base() -> World {
    let mut w = World::new(100_000_000, 5_000_000);
    for (who, amt) in [(LP1, 1_000_000u128), (T1, 10_000), (T2, 2_000)] {
        w.step(Act::Register(who));
        w.step(Act::FtIn { who, amount: amt * USDC, msg: String::new() });
    }
    w.step(Act::LpDeposit { who: LP1, m: 0, amount: 500_000 * USDC });
    for (who, m, amt) in [(T1, 0, 5_000u128), (T2, 0, 1_000)] {
        w.step(Act::OpenAccount { who, m });
        w.step(Act::Deposit { who, m, amount: amt * USDC });
    }
    w
}
#[test]
fn a_same_trader_increase() {
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.fill(T1, 0, true, 1 * POS_SCALE, false, 100_000_000);
}
#[test]
fn b_second_trader_same_side() {
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.fill(T2, 0, true, 1 * POS_SCALE, false, 100_000_000);
}
#[test]
fn c_second_trader_opposite() {
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.fill(T2, 0, false, 1 * POS_SCALE, false, 100_000_000);
}
#[test]
fn d_second_trader_same_block() {
    let mut w = base();
    w.step(Act::Request { who: T1, m: 0, long: true, size_q: 10*POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Request { who: T2, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.step(Act::Execute { order: 0, by: KEEPER });
    w.step(Act::Execute { order: 1, by: KEEPER });
}
#[test]
fn e_second_trader_after_keeper_step_on_pool() {
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.step(Act::Request { who: T2, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.call(KEEPER, ZERO);
    let r = w.c.keeper_step(0, contract_id());
    println!("keeper_step on pool: {r}");
    w.check("ks pool");
    w.step(Act::Execute { order: 1, by: KEEPER });
}
#[test]
fn f_second_trader_after_keeper_step_on_t1() {
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.step(Act::Request { who: T2, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.call(KEEPER, ZERO);
    let r = w.c.keeper_step(0, user(T1));
    println!("keeper_step on t1: {r}");
    w.step(Act::Execute { order: 1, by: KEEPER });
}
#[test]
fn g_both_refreshed() {
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.step(Act::Request { who: T2, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.call(KEEPER, ZERO);
    println!("ks: {}", w.c.keeper_step(0, user(T1)));
    w.call(KEEPER, ZERO);
    println!("ks: {}", w.c.keeper_step(0, contract_id()));
    w.call(KEEPER, ZERO);
    println!("ks: {}", w.c.keeper_step(0, user(T2)));
    w.step(Act::Execute { order: 1, by: KEEPER });
}
#[test]
fn h_no_time_passes_but_two_blocks() {
    // T1 fills at block N+1, T2 requests at N+1, executes at N+2 with NO funding (flat price)
    let mut w = base();
    w.fill(T1, 0, true, 10 * POS_SCALE, false, 100_000_000);
    w.fill(T1, 0, false, 10 * POS_SCALE, true, 100_000_000);
    w.fill(T2, 0, true, 1 * POS_SCALE, false, 100_000_000);
}
fn two_open() -> World {
    let mut w = base();
    w.step(Act::Request { who: T1, m: 0, long: true, size_q: 10*POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Request { who: T2, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 30, reduce_only: false });
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.step(Act::Execute { order: 0, by: KEEPER });
    w.step(Act::Execute { order: 1, by: KEEPER });
    w
}
#[test]
fn i_close_while_other_stale() {
    let mut w = two_open();
    w.fill(T2, 0, false, POS_SCALE, true, 100_000_000);
}
#[test]
fn j_withdraw_margin_while_other_stale() {
    let mut w = two_open();
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.step(Act::WithdrawMargin { who: T2, m: 0, amount: 100 * USDC });
}
#[test]
fn k_increase_while_other_stale() {
    let mut w = two_open();
    w.fill(T2, 0, true, POS_SCALE, false, 100_000_000);
}
#[test]
fn l_lp_ops_while_stale() {
    let mut w = two_open();
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 100_000_000, age_ns: 0 });
    w.step(Act::LpDeposit { who: LP1, m: 0, amount: 100 * USDC });
    let s = w.c.get_lp_shares(0, user(LP1)).0;
    w.step(Act::LpRedeem { who: LP1, m: 0, shares: s / 10 });
}
#[test]
fn m_liquidation_path() {
    let mut w = base();
    w.fill(T2, 0, true, 85 * POS_SCALE, false, 100_000_000);
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 93_000_000, age_ns: 0 });
    w.step(Act::Walk { m: 0, max: 80 });
    println!("eff {}", w.effective(0));
    w.step(Act::Liquidate { m: 0, who: T2, by: KEEPER });
    println!("liqfee {:?} keeper wallet {}", w.liq_fees, w.wallet(KEEPER));
}
#[test]
fn n_liq() {
    let mut w = base();
    w.fill(T2, 0, true, 85 * POS_SCALE, false, 100_000_000);
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 91_000_000, age_ns: 0 });
    w.step(Act::Walk { m: 0, max: 80 });
    println!("eff {} acct {}", w.effective(0), near_sdk::serde_json::to_string(&w.c.get_account(0, user(T2))).unwrap());
    let before = w.c.get_market(0);
    println!("vault {} ins {}", before.vault.0, before.insurance.0);
    w.call(KEEPER, ZERO);
    let closed = w.c.liquidate(0, user(T2));
    println!("closed {} fee {:?}", closed.0, World::event_field("liquidation","fee"));
    let after = w.c.get_market(0);
    println!("vault {} ins {} custody {:?} keeper {}", after.vault.0, after.insurance.0, w.c.get_custody(), w.wallet(KEEPER));
    println!("acct {}", near_sdk::serde_json::to_string(&w.c.get_account(0, user(T2))).unwrap());
    println!("viol {:?}", w.violations());
    w.call(KEEPER, ZERO);
    println!("ks {}", w.c.keeper_step(0, user(T2)));
    println!("acct {}", near_sdk::serde_json::to_string(&w.c.get_account(0, user(T2))).unwrap());
}
