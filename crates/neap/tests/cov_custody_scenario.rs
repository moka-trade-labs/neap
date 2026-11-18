//! Invariant 1 (custody), 2 (isolation), 6 (admin), 8 (fees) asserted after
//! EVERY step of a long mixed scenario through the public contract methods.
//! See `cov_harness.rs` for what `World::step` checks.

#[path = "cov_harness.rs"]
mod h;
use h::*;

const P0: u64 = 100_000_000; // market 0: $100
const P1: u64 = 5_000_000; // market 1: $5

/// Everything except liquidation. Leaves T2 ~8.5x long on market 0.
fn scenario_until_liquidation() -> World {
    let mut w = World::new(P0, P1);

    // ── funding and LP seeding (two LPs, two markets) ──
    for (who, amt) in [(LP1, 1_000_000), (LP2, 200_000), (T1, 10_000), (T2, 2_000), (T3, 5_000)] {
        w.step(Act::Register(who));
        w.step(Act::FtIn { who, amount: amt * USDC, msg: String::new() });
    }
    w.step(Act::LpDeposit { who: LP1, m: 0, amount: 500_000 * USDC });
    w.step(Act::LpDeposit { who: LP2, m: 0, amount: 100_000 * USDC });
    w.step(Act::LpDeposit { who: LP1, m: 1, amount: 100_000 * USDC });

    // ── refunds: unregistered sender, explicit Wallet msg, garbage msg ──
    w.step(Act::FtIn { who: KEEPER, amount: 77, msg: String::new() });
    assert_eq!(w.token, 1_217_000 * USDC, "unregistered sender fully refunded");
    w.step(Act::FtIn { who: T1, amount: 5, msg: "{\"action\":\"Wallet\"}".into() });
    w.step(Act::FtIn { who: T1, amount: 5, msg: "not json".into() });

    // ── margin accounts ──
    for (who, m, amt) in [(T1, 0, 5_000), (T2, 0, 1_000), (T3, 1, 2_000)] {
        w.step(Act::OpenAccount { who, m });
        w.step(Act::Deposit { who, m, amount: amt * USDC });
    }

    // ── fills on both markets ──
    w.fill(T1, 0, true, 10 * POS_SCALE, false, P0);
    w.fill(T2, 0, true, 85 * POS_SCALE, false, P0);
    w.fill(T3, 1, false, 1_000 * POS_SCALE, false, P1);
    assert_eq!(w.position(0, T1), 10 * POS_SCALE as i128);
    assert_eq!(w.position(1, T3), -(1_000 * POS_SCALE as i128));

    // ── order housekeeping: cancel, expire ──
    w.step(Act::Request { who: T1, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 5, reduce_only: false });
    let cancelled = *w.orders.last().unwrap();
    w.step(Act::Cancel { order: cancelled, who: T1 });
    w.step(Act::Request { who: T1, m: 0, long: true, size_q: POS_SCALE, limit: u64::MAX, expiry: 2, reduce_only: false });
    let expired = *w.orders.last().unwrap();
    w.step(Act::Tick(3));
    w.step(Act::Execute { order: expired, by: KEEPER });
    assert_eq!(w.position(0, T1), 10 * POS_SCALE as i128, "expired order did not fill");

    // ── conditionals: TP for T1's long, an SL that gets cancelled ──
    w.step(Act::PlaceCond {
        who: T1, m: 0, kind: 0, long: false, size_q: 10 * POS_SCALE,
        trigger: 101_000_000, limit: 0, expiry: 1_000, reduce_only: false,
    });
    let tp = *w.conds.last().unwrap();
    w.step(Act::PlaceCond {
        who: T1, m: 0, kind: 1, long: false, size_q: 10 * POS_SCALE,
        trigger: 90_000_000, limit: 0, expiry: 1_000, reduce_only: false,
    });
    let sl = *w.conds.last().unwrap();
    w.step(Act::CancelCond { cond: sl, who: T1 });

    // ── price +1.5% along the staircase, funding accrues (traders net long) ──
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 101_500_000, age_ns: 0 });
    w.step(Act::Walk { m: 0, max: 60 });
    assert_eq!(w.effective(0), 101_500_000);
    w.step(Act::ExecCond { cond: tp, by: KEEPER });
    assert_eq!(w.position(0, T1), 0, "TP closed the long");

    // ── keeper step, profit conversion after warmup, margin withdrawal ──
    w.step(Act::Tick(10));
    w.step(Act::KeeperStep { m: 0, who: T1, by: KEEPER });
    w.step(Act::Push { m: 0, price: 101_500_000, age_ns: 0 });
    w.step(Act::Convert { who: T1, m: 0 });
    let cap = w.capital(0, T1);
    assert!(cap > 5_000 * USDC, "T1 realized a profit: {cap}");
    w.step(Act::WithdrawMargin { who: T1, m: 0, amount: cap });

    // ── fee claims (lister and protocol), insurance donation ──
    w.step(Act::Insurance { who: LP2, m: 0, amount: 1_001 * USDC });
    w.step(Act::ClaimFees { m: 0, protocol: false });
    w.step(Act::ClaimFees { m: 0, protocol: true });
    w.step(Act::ClaimFees { m: 1, protocol: false });
    assert!(w.fees_claimed[0] > 0 && w.fees_claimed[1] > 0);

    // ── LP out (partial) and in again ──
    let shares = w.c.get_lp_shares(0, user(LP2)).0;
    w.step(Act::LpRedeem { who: LP2, m: 0, shares: shares / 2 });
    w.step(Act::LpDeposit { who: LP2, m: 0, amount: 10_000 * USDC });

    // ── withdraw: failed transfer re-credits, then a good one ──
    let bal = w.wallet(T1);
    w.step(Act::Withdraw { who: T1, amount: bal, transfer_ok: false });
    w.step(Act::Withdraw { who: T1, amount: bal / 2, transfer_ok: true });

    // ── permissionless listing with a bond ──
    w.step(Act::Register(LISTER));
    w.step(Act::FtIn { who: LISTER, amount: 60 * USDC, msg: create_market_msg("BONK-PERP", "LongTail") });
    assert_eq!(w.n_markets(), 3);

    // ── admin acts: must not move any balance ──
    w.step(Act::SetPauser(Some(KEEPER)));
    w.step(Act::SetOracle);
    w.step(Act::SetOwner);
    w.step(Act::SetOiCaps { m: 1, long: 5_000 * POS_SCALE, short: 5_000 * POS_SCALE, acct: 2_000 * POS_SCALE });
    w.step(Act::CreateMarketAdmin { price: 1_000_000 });
    w.step(Act::SetMode { m: 3, uncertain: true });

    // ── market 1 keeps trading under OI caps (dynamic spread path) ──
    w.fill(T3, 1, true, 400 * POS_SCALE, true, P1);
    w
}

#[test]
fn mixed_scenario_custody_holds_after_every_step() {
    let w = scenario_until_liquidation();
    assert!(w.fills >= 5, "fills: {}", w.fills);
    assert!(w.log.len() > 70, "steps: {}", w.log.len());
}

/// Same scenario, then the price drops ~10% and the keeper liquidates T2.
/// Custody (and every other invariant) is asserted after each step.
#[test]
fn mixed_scenario_with_liquidation_custody_holds() {
    let mut w = scenario_until_liquidation();
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 91_000_000, age_ns: 0 });
    w.step(Act::Walk { m: 0, max: 80 });
    assert_eq!(w.effective(0), 91_000_000);
    w.step(Act::Register(KEEPER));
    w.step(Act::Liquidate { m: 0, who: T2, by: KEEPER });
    assert_eq!(w.liquidations, 1);
    assert!(w.liq_fees[0] > 0, "a liquidation fee was charged");
    // 60% of the fee is paid to the caller in the same call (spec §5).
    assert_eq!(w.wallet(KEEPER), w.liq_fees[0] * 6_000 / 10_000);

    // The market keeps working afterwards.
    w.step(Act::Tick(1));
    w.step(Act::Push { m: 0, price: 91_000_000, age_ns: 0 });
    w.step(Act::KeeperStep { m: 0, who: T2, by: KEEPER });
    w.step(Act::ClaimFees { m: 0, protocol: true });
    let bal = w.wallet(KEEPER);
    w.step(Act::Withdraw { who: KEEPER, amount: bal, transfer_ok: true });
}
