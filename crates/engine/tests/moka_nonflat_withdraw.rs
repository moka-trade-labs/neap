//! moka fork: spec §8.4 nonflat withdraw (`withdraw_with_open_positions_not_atomic`).
//!
//! The upstream engine only lets a FLAT account withdraw. These tests pin the
//! nonflat branch: free collateral above the initial-margin requirement is
//! withdrawable, nothing below it is, unrealized positive PnL never counts, a
//! raw-target/effective-price lag blocks the action, and conservation
//! `V >= C_tot + I` holds throughout.

use percolator::{
    v16_domain_count_for_market_slots, EngineAssetSlotV16Account, Market,
    MarketGroupV16HeaderAccount, MarketGroupV16ViewMut, PortfolioAccountV16Account,
    PortfolioV16ViewMut, ProvenanceHeaderV16, ProvenanceHeaderV16Account, TradeRequestV16,
    V16Config, V16Error, V16PodU64, POS_SCALE,
};

const SIZE_Q: u128 = 10 * POS_SCALE; // 10 units
const PRICE: u64 = 100; // notional 1_000

fn market_fixture() -> (MarketGroupV16HeaderAccount, Vec<Market<u64>>) {
    let cfg = V16Config::public_user_fund_with_market_slots(1, 1, 0, 10);
    let mut header = MarketGroupV16HeaderAccount::new_dynamic([1; 32], cfg, 1, 0).unwrap();
    let mut markets = vec![Market::new(0u64, EngineAssetSlotV16Account::default())];
    header
        .activate_empty_asset_slot_not_atomic(0, &mut markets[0].engine, PRICE, 1)
        .unwrap();
    // 10x initial / 10x maintenance, 5% per-slot move cap (same shape as the
    // upstream #448 fixture, which also tunes config after activation).
    header.config.maintenance_margin_bps = V16PodU64::new(1_000);
    header.config.initial_margin_bps = V16PodU64::new(1_000);
    header.config.max_price_move_bps_per_slot = V16PodU64::new(500);
    header.config.max_accrual_dt_slots = V16PodU64::new(1);
    header.config.min_funding_lifetime_slots = V16PodU64::new(1);
    (header, markets)
}

fn account_fixture(seed: u8) -> PortfolioAccountV16Account {
    let header = ProvenanceHeaderV16Account::from_runtime(&ProvenanceHeaderV16::new(
        [1; 32], [seed; 32], [3; 32],
    ));
    let _ = v16_domain_count_for_market_slots(1).unwrap();
    let mut account = PortfolioAccountV16Account::default();
    account.init_empty_in_place(header).unwrap();
    account
}

fn conservation(market: &MarketGroupV16ViewMut<'_, u64>) {
    let v = market.header.vault.get();
    let c = market.header.c_tot.get();
    let i = market.header.insurance.get();
    assert!(v >= c + i, "conservation broken: V={v} C_tot={c} I={i}");
}

fn open(
    market: &mut MarketGroupV16ViewMut<'_, u64>,
    long: &mut PortfolioV16ViewMut<'_>,
    short: &mut PortfolioV16ViewMut<'_>,
) {
    market.deposit_not_atomic(long, 1_000).unwrap();
    market.deposit_not_atomic(short, 1_000).unwrap();
    market
        .execute_trade_with_fee_loss_stale_scoped_not_atomic(
            long,
            short,
            TradeRequestV16 {
                asset_index: 0,
                size_q: SIZE_Q as i128,
                exec_price: PRICE,
                fee_bps: 0,
            },
            true,
        )
        .unwrap();
}

#[test]
fn moka_flat_withdraw_still_rejects_nonflat_account() {
    let (mut header, mut markets) = market_fixture();
    let (mut a, mut b) = (account_fixture(10), account_fixture(11));
    let mut market = MarketGroupV16ViewMut::new(&mut header, &mut markets);
    let (mut long, mut short) = (
        PortfolioV16ViewMut::new(&mut a),
        PortfolioV16ViewMut::new(&mut b),
    );
    open(&mut market, &mut long, &mut short);
    assert_eq!(
        market.withdraw_not_atomic(&mut long, 1),
        Err(V16Error::Stale),
        "upstream flat-only path is unchanged"
    );
}

#[test]
fn moka_nonflat_withdraw_releases_only_free_collateral() {
    let (mut header, mut markets) = market_fixture();
    let (mut a, mut b) = (account_fixture(10), account_fixture(11));
    let mut market = MarketGroupV16ViewMut::new(&mut header, &mut markets);
    let (mut long, mut short) = (
        PortfolioV16ViewMut::new(&mut a),
        PortfolioV16ViewMut::new(&mut b),
    );
    open(&mut market, &mut long, &mut short);

    // IM on 1_000 notional at 10% is 100: 950 would leave 50 < 100.
    let vault_before = market.header.vault.get();
    assert_eq!(
        market.withdraw_with_open_positions_not_atomic(&mut long, 950),
        Err(V16Error::InsufficientInitialMargin)
    );
    // The failed call may have refreshed the account, but it moved no value.
    assert_eq!(market.header.vault.get(), vault_before);

    market
        .withdraw_with_open_positions_not_atomic(&mut long, 800)
        .expect("free collateral above IM is withdrawable");
    assert_eq!(long.header.capital.get(), 200);
    assert_eq!(market.header.vault.get(), vault_before - 800);
    conservation(&market);

    // Exactly at the boundary: 200 - 100 = 100 == IM, allowed; one more atom is not.
    market
        .withdraw_with_open_positions_not_atomic(&mut long, 100)
        .expect("equity exactly at IM is allowed");
    assert_eq!(
        market.withdraw_with_open_positions_not_atomic(&mut long, 1),
        Err(V16Error::InsufficientInitialMargin)
    );
    conservation(&market);
    market.validate_shape().unwrap();
}

#[test]
fn moka_nonflat_withdraw_never_pays_out_unrealized_profit() {
    let (mut header, mut markets) = market_fixture();
    let (mut a, mut b) = (account_fixture(10), account_fixture(11));
    let mut market = MarketGroupV16ViewMut::new(&mut header, &mut markets);
    let (mut long, mut short) = (
        PortfolioV16ViewMut::new(&mut a),
        PortfolioV16ViewMut::new(&mut b),
    );
    open(&mut market, &mut long, &mut short);

    // Price walks up 5% per slot to 110: the long is +100 in PnL on paper.
    for (slot, price) in [(2u64, 105u64), (3, 110)] {
        market
            .set_asset_raw_oracle_target_not_atomic(0, price)
            .unwrap();
        market
            .accrue_asset_to_not_atomic(0, slot, price, 0, true)
            .unwrap();
    }
    // Fresh profit is still inside its warmup window. Withdrawing capital does
    // not touch positive PnL, so warmup does not block it -- but the profit
    // itself must never raise the limit (checked below, before and after maturity).
    assert_eq!(
        market.withdraw_with_open_positions_not_atomic(&mut long, 891),
        Err(V16Error::InsufficientInitialMargin)
    );
    for slot in 4u64..=20 {
        market
            .accrue_asset_to_not_atomic(0, slot, 110, 0, true)
            .unwrap();
    }
    // Notional is now 1_100 => IM 110. Capital 1_000. Without positive credit the
    // most that can leave is 890; the matured paper profit must not raise that.
    assert_eq!(
        market.withdraw_with_open_positions_not_atomic(&mut long, 891),
        Err(V16Error::InsufficientInitialMargin)
    );
    market
        .withdraw_with_open_positions_not_atomic(&mut long, 890)
        .unwrap();
    assert!(long.header.capital.get() == 110);
    conservation(&market);
    market.validate_shape().unwrap();
}

#[test]
fn moka_nonflat_withdraw_blocked_while_oracle_target_lags() {
    let (mut header, mut markets) = market_fixture();
    let (mut a, mut b) = (account_fixture(10), account_fixture(11));
    let mut market = MarketGroupV16ViewMut::new(&mut header, &mut markets);
    let (mut long, mut short) = (
        PortfolioV16ViewMut::new(&mut a),
        PortfolioV16ViewMut::new(&mut b),
    );
    open(&mut market, &mut long, &mut short);

    // Raw target jumps 50%, but the engine price can only step 5%/slot.
    market
        .set_asset_raw_oracle_target_not_atomic(0, 150)
        .unwrap();
    market
        .accrue_asset_to_not_atomic(0, 2, 105, 0, true)
        .unwrap();
    let err = market
        .withdraw_with_open_positions_not_atomic(&mut short, 1)
        .unwrap_err();
    assert!(
        matches!(err, V16Error::LockActive | V16Error::Stale),
        "lagged extraction must be rejected (spec §9 obligation 7), got {err:?}"
    );
}

#[test]
fn moka_nonflat_withdraw_routes_flat_accounts_to_flat_path() {
    let (mut header, mut markets) = market_fixture();
    let mut a = account_fixture(10);
    let mut market = MarketGroupV16ViewMut::new(&mut header, &mut markets);
    let mut acct = PortfolioV16ViewMut::new(&mut a);
    market.deposit_not_atomic(&mut acct, 500).unwrap();
    market
        .withdraw_with_open_positions_not_atomic(&mut acct, 500)
        .unwrap();
    assert_eq!(acct.header.capital.get(), 0);
    assert_eq!(market.header.vault.get(), 0);
    assert_eq!(
        market.withdraw_with_open_positions_not_atomic(&mut acct, 0),
        Ok(())
    );
}
