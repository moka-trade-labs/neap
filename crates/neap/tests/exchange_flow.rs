//! End-to-end exchange flow through the wrapper glue (no NEAR runtime):
//! list a market, seed the LP pool, trade against it, move the oracle,
//! close, convert profit, withdraw, liquidate. Checks engine conservation
//! `V >= C_tot + I` after every step.

use neap::exchange::{self, ExError, Group, MarketParams, OracleMeta};
use percolator::{PortfolioAccountV16Account, POS_SCALE};

const USDC: u128 = 1_000_000; // 6 decimals
const PRICE: u64 = 100_000_000; // $100 per base unit, in quote atoms

fn params() -> MarketParams {
    MarketParams {
        initial_margin_bps: 1_000,   // 10x
        maintenance_margin_bps: 500, // 5%
        trading_fee_bps: 10,
        liquidation_fee_bps: 50,
        liquidation_fee_cap: 1_000 * USDC,
        min_liquidation_abs: 0,
        max_price_move_bps_per_slot: 30,
        max_accrual_dt_slots: 10,
        max_abs_funding_e9_per_slot: 100,
        h_min: 5,
        h_max: 50,
        min_nonzero_mm_req: 1_000,
        min_nonzero_im_req: 2_000,
    }
}

struct World {
    g: Group,
    meta: OracleMeta,
    pool: Box<PortfolioAccountV16Account>,
    slot: u64,
}

fn world() -> World {
    let g = Group::create([7; 32], &params(), PRICE, 100).expect("listing must validate");
    let pool = exchange::new_portfolio([7; 32], [0xAA; 32], [0xAA; 32]).unwrap();
    let mut w = World {
        g,
        meta: OracleMeta {
            target: PRICE,
            remainder_bps_num: 0,
            last_publish_ns: 0,
        },
        pool,
        slot: 100,
    };
    exchange::deposit(&mut w.g, &mut w.pool, 1_000_000 * USDC).unwrap();
    conserve(&w);
    w
}

fn trader(w: &mut World, seed: u8, margin: u128) -> Box<PortfolioAccountV16Account> {
    let mut p = exchange::new_portfolio([7; 32], [seed; 32], [seed; 32]).unwrap();
    exchange::deposit(&mut w.g, &mut p, margin).unwrap();
    conserve(w);
    p
}

fn conserve(w: &World) {
    let h = &w.g.header;
    let (v, c, i) = (h.vault.get(), h.c_tot.get(), h.insurance.get());
    assert!(v >= c + i, "conservation: V={v} C_tot={c} I={i}");
    w.g.clone_view_validate();
}

trait Validate {
    fn clone_view_validate(&self);
}
impl Validate for Group {
    fn clone_view_validate(&self) {
        let mut copy = Group::from_bytes(&self.to_bytes()).unwrap();
        copy.view().validate_shape().expect("shape");
        assert_eq!(copy.to_bytes(), self.to_bytes(), "byte round-trip");
    }
}

fn advance(w: &mut World, slots: u64) {
    w.slot += slots;
    exchange::catch_up(&mut w.g, &mut w.meta, &w.pool, w.slot).unwrap();
    conserve(w);
}

fn set_target(w: &mut World, price: u64) {
    w.meta.target = price;
}

fn walk_to_target(w: &mut World) -> u64 {
    let mut n = 0;
    while exchange::has_lag(&w.g, &w.meta).unwrap() {
        advance(w, 1);
        n += 1;
        assert!(n < 10_000, "catch-up never converged");
    }
    n
}

#[test]
fn listing_rejects_unsafe_envelope_and_zero_warmup() {
    let mut p = params();
    p.max_price_move_bps_per_slot = 100; // 100 bps * 10 slots = 10% > 5% MM
    assert!(matches!(
        Group::create([7; 32], &p, PRICE, 1),
        Err(ExError::Engine(_))
    ));
    let mut p = params();
    p.h_min = 0;
    assert!(Group::create([7; 32], &p, PRICE, 1).is_err(), "spec §9.2");
}

#[test]
fn open_profit_close_convert_withdraw_round_trip() {
    let mut w = world();
    let margin = 10_000 * USDC;
    let mut t = trader(&mut w, 1, margin);
    advance(&mut w, 1);

    // Buy 10 units ($1,000 notional) against the pool with 5 bps spread.
    let fill = exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        10 * POS_SCALE as i128,
        u64::MAX,
        5,
    )
    .unwrap();
    assert_eq!(
        fill.exec_price, 100_050_000,
        "buy pays oracle + 5 bps, rounded up"
    );
    conserve(&w);
    assert_eq!(
        exchange::effective_position_q(&w.g, &w.pool).unwrap(),
        -(10 * POS_SCALE as i128),
        "pool takes the other side"
    );
    // Traders net long => longs pay funding.
    assert!(exchange::funding_rate_e9(&w.g, &w.pool).unwrap() > 0);

    // Oracle +2%: walks up at <= 30 bps/slot, never jumps.
    set_target(&mut w, 102_000_000);
    let before = w.g.asset().unwrap().effective_price;
    advance(&mut w, 1);
    let after = w.g.asset().unwrap().effective_price;
    assert!(after > before && (after - before) as u128 * 10_000 <= 30 * before as u128);
    // Lagged: trading and withdrawing are refused (spec §9.7).
    assert_eq!(
        exchange::trade_against_pool(&mut w.g, &w.meta, &mut t, &mut w.pool, -1, 0, 5),
        Err(ExError::OracleLag)
    );
    assert_eq!(
        exchange::withdraw(&mut w.g, &w.meta, &mut t, 1),
        Err(ExError::OracleLag)
    );
    let steps = walk_to_target(&mut w);
    assert!(
        steps >= 6,
        "2% at 30 bps/slot needs >= 7 slots total, took {}",
        steps + 1
    );

    // Close the position.
    exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        -(10 * POS_SCALE as i128),
        0,
        5,
    )
    .unwrap();
    conserve(&w);
    assert_eq!(exchange::effective_position_q(&w.g, &t).unwrap(), 0);

    // Let the profit mature, convert, withdraw everything.
    advance(&mut w, 10);
    let converted = exchange::convert_pnl(&mut w.g, &w.meta, &mut t).unwrap();
    assert!(converted > 0);
    conserve(&w);
    let cap = t.capital.get();
    assert!(cap > margin, "profit realized into capital: capital={cap}");
    exchange::withdraw(&mut w.g, &w.meta, &mut t, cap).unwrap();
    assert_eq!(t.capital.get(), 0);
    conserve(&w);
}

#[test]
fn withdraw_free_margin_while_positioned() {
    let mut w = world();
    let mut t = trader(&mut w, 2, 1_000 * USDC);
    advance(&mut w, 1);
    // $5,000 notional at 10% IM needs ~$500 (+ fee).
    exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        50 * POS_SCALE as i128,
        u64::MAX,
        5,
    )
    .unwrap();
    advance(&mut w, 1);
    assert!(exchange::withdraw(&mut w.g, &w.meta, &mut t, 600 * USDC).is_err());
    exchange::withdraw(&mut w.g, &w.meta, &mut t, 400 * USDC).unwrap();
    conserve(&w);
}

#[test]
fn slippage_limit_and_zero_size_rejected() {
    let mut w = world();
    let mut t = trader(&mut w, 3, 1_000 * USDC);
    advance(&mut w, 1);
    assert_eq!(
        exchange::trade_against_pool(
            &mut w.g,
            &w.meta,
            &mut t,
            &mut w.pool,
            POS_SCALE as i128,
            PRICE,
            5
        ),
        Err(ExError::Slippage)
    );
    assert_eq!(
        exchange::trade_against_pool(&mut w.g, &w.meta, &mut t, &mut w.pool, 0, PRICE, 5),
        Err(ExError::ZeroSize)
    );
}

#[test]
fn over_leveraged_trade_is_rejected_by_engine_margin() {
    let mut w = world();
    let mut t = trader(&mut w, 4, 100 * USDC);
    advance(&mut w, 1);
    // $10,000 notional on $100 is 100x.
    assert!(exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        100 * POS_SCALE as i128,
        u64::MAX,
        5,
    )
    .is_err());
}

#[test]
fn price_drop_makes_trader_liquidatable_and_liquidation_conserves() {
    let mut w = world();
    let mut t = trader(&mut w, 5, 1_000 * USDC);
    advance(&mut w, 1);
    // ~9x long: $9,000 notional on ~$1,000.
    exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        90 * POS_SCALE as i128,
        u64::MAX,
        5,
    )
    .unwrap();
    // -8%: equity falls below 5% MM.
    set_target(&mut w, 92_000_000);
    walk_to_target(&mut w);
    let out = exchange::liquidate(&mut w.g, &mut t).expect("under-MM account liquidates");
    assert!(out.closed_q > 0);
    conserve(&w);
    let left = exchange::effective_position_q(&w.g, &t).unwrap();
    assert!(
        left.unsigned_abs() < 90 * POS_SCALE,
        "position reduced, left {left}"
    );
    // Partial liquidation restores health: a second attempt makes no progress.
    assert!(exchange::liquidate(&mut w.g, &mut t).is_err());
}

#[test]
fn healthy_account_cannot_be_liquidated() {
    let mut w = world();
    let mut t = trader(&mut w, 6, 1_000 * USDC);
    advance(&mut w, 1);
    exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        10 * POS_SCALE as i128,
        u64::MAX,
        5,
    )
    .unwrap();
    advance(&mut w, 1);
    assert!(exchange::liquidate(&mut w.g, &mut t).is_err());
}

#[test]
fn flat_market_reanchors_instantly_after_long_idle() {
    let mut w = world();
    set_target(&mut w, 250_000_000);
    advance(&mut w, 1_000_000);
    let a = w.g.asset().unwrap();
    assert_eq!(
        a.effective_price, 250_000_000,
        "no exposure => no staircase"
    );
    assert_eq!(a.slot_last, w.slot);
}

#[test]
fn insurance_deposit_is_conserved() {
    let mut w = world();
    let v0 = w.g.vault();
    exchange::deposit_insurance(&mut w.g, 5_001 * USDC).unwrap();
    assert_eq!(w.g.vault(), v0 + 5_001 * USDC);
    assert_eq!(w.g.header.insurance.get(), 5_001 * USDC);
    conserve(&w);
}

/// FINDING F-1 (pinned): a winner's profit is backed by the loser's crystallized
/// principal only until the backing bucket's freshness horizon
/// (max(max_accrual_dt, h_max, ...) slots). If the winner does not convert before
/// then, the bucket expires, the backing is impaired, and conversion is refused.
/// The PnL is not deleted, but it is no longer convertible through the live path.
/// Wrapper mitigation: the frontend/keeper must prompt or perform conversion
/// inside the window; see percolator/SECURITY.md.
#[test]
fn finding_f1_unconverted_profit_loses_backing_after_horizon() {
    let mut w = world();
    let mut t = trader(&mut w, 9, 10_000 * USDC);
    advance(&mut w, 1);
    exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        10 * POS_SCALE as i128,
        u64::MAX,
        5,
    )
    .unwrap();
    set_target(&mut w, 102_000_000);
    walk_to_target(&mut w);
    exchange::trade_against_pool(
        &mut w.g,
        &w.meta,
        &mut t,
        &mut w.pool,
        -(10 * POS_SCALE as i128),
        0,
        5,
    )
    .unwrap();
    advance(&mut w, 60); // > h_max = 50
    let pnl_before = t.pnl.get();
    assert!(pnl_before > 0);
    let f = exchange::funding_rate_e9(&w.g, &w.pool).unwrap();
    let r = exchange::auto_crank(&mut w.g, &mut t, f, w.slot).unwrap();
    assert!(matches!(
        r.outcome,
        percolator::AutoCrankOutcomeV16::Progressed(
            percolator::PermissionlessProgressOutcomeV16::SourceBackingExpired { .. }
        )
    ));
    assert!(exchange::convert_pnl(&mut w.g, &w.meta, &mut t).is_err());
    assert_eq!(t.pnl.get(), pnl_before, "profit is stranded, not erased");
    conserve(&w);
}
