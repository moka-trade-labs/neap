//! Engine glue: owned market-group / portfolio state, (de)serialization to the
//! raw bytes the contract stores, and the wrapper-side sequencing the spec
//! requires around each engine instruction (oracle catch-up, lag gate, pool
//! pricing, funding sourcing).
//!
//! No near-sdk types appear here, so the whole exchange flow is testable with
//! plain `cargo test` and is identical inside and outside the wasm contract.
//!
//! ATOMICITY: engine `*_not_atomic` calls may leave partial writes on `Err`.
//! Callers MUST discard every in-memory value on error (the contract panics,
//! and NEAR reverts the receipt). Nothing here performs I/O.

use alloc::boxed::Box;
use alloc::vec::Vec;

use percolator::{
    AccrualStepV16, AutoCrankObservationV16, AutoCrankResultV16, AutoCrankWorkV16,
    EngineAssetSlotV16Account, LiquidationOutcomeV16, LiquidationRequestV16, Market,
    MarketGroupV16HeaderAccount, MarketGroupV16ViewMut, PortfolioAccountV16Account,
    PortfolioV16ViewMut, ProvenanceHeaderV16, ProvenanceHeaderV16Account, SideV16, TradeRequestV16,
    V16Config, V16Error, ADL_ONE, V16_MAX_ACCRUAL_PATH_STEPS,
};

use crate::policy;

/// Wrapper-owned bytes stored beside each engine asset slot (unused today;
/// alignment-1 so `Market<Wrapper>` has no padding).
pub type Wrapper = [u8; 32];

/// The single asset index used inside each market group. One listed market =
/// one engine market group, so per-market risk params and isolation hold.
pub const ASSET: usize = 0;

/// Bound on catch-up path calls in one transaction (each covers up to
/// `V16_MAX_ACCRUAL_PATH_STEPS` slots and `max_accrual_dt_slots`).
pub const MAX_CATCH_UP_CALLS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExError {
    Engine(V16Error),
    Corrupt,
    OracleLag,
    OracleNotCaughtUp,
    BadPrice,
    Slippage,
    ZeroSize,
    Overflow,
    LpShares,
}

impl From<V16Error> for ExError {
    fn from(e: V16Error) -> Self {
        ExError::Engine(e)
    }
}

pub type ExResult<T> = Result<T, ExError>;

/// Wrapper oracle state per market (spec §1.7: raw target kept separate from
/// the engine's effective price).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OracleMeta {
    /// Latest validated normalized external target.
    pub target: u64,
    /// Sub-atom price-move remainder carried between catch-up calls.
    pub remainder_bps_num: u16,
    /// Publish timestamp of `target` (ns).
    pub last_publish_ns: u64,
}

/// Market risk parameters chosen at listing time (immutable afterwards).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MarketParams {
    pub initial_margin_bps: u64,
    pub maintenance_margin_bps: u64,
    pub trading_fee_bps: u64,
    pub liquidation_fee_bps: u64,
    pub liquidation_fee_cap: u128,
    pub min_liquidation_abs: u128,
    pub max_price_move_bps_per_slot: u64,
    pub max_accrual_dt_slots: u64,
    pub max_abs_funding_e9_per_slot: u64,
    /// Admission warmup in slots. MUST be > 0 for public markets (spec §9.2).
    pub h_min: u64,
    pub h_max: u64,
    pub min_nonzero_mm_req: u128,
    pub min_nonzero_im_req: u128,
}

impl MarketParams {
    pub fn to_engine_config(&self) -> ExResult<V16Config> {
        if self.h_min == 0 {
            // spec §9 obligation 2: untrusted public PnL needs nonzero admission warmup.
            return Err(ExError::Engine(V16Error::InvalidConfig));
        }
        let mut cfg = V16Config::public_user_fund_with_market_slots(1, 1, self.h_min, self.h_max);
        cfg.initial_margin_bps = self.initial_margin_bps;
        cfg.maintenance_margin_bps = self.maintenance_margin_bps;
        cfg.max_trading_fee_bps = self.trading_fee_bps;
        cfg.liquidation_fee_bps = self.liquidation_fee_bps;
        cfg.liquidation_fee_cap = self.liquidation_fee_cap;
        cfg.min_liquidation_abs = self.min_liquidation_abs;
        cfg.max_price_move_bps_per_slot = self.max_price_move_bps_per_slot;
        cfg.max_accrual_dt_slots = self.max_accrual_dt_slots;
        cfg.min_funding_lifetime_slots = self.max_accrual_dt_slots;
        cfg.max_abs_funding_e9_per_slot = self.max_abs_funding_e9_per_slot;
        cfg.min_nonzero_mm_req = self.min_nonzero_mm_req;
        cfg.min_nonzero_im_req = self.min_nonzero_im_req;
        // The engine re-validates everything, including the exact per-notional
        // solvency envelope (spec §1.6), in `new_dynamic`.
        cfg.validate_public_user_fund()?;
        Ok(cfg)
    }
}

/// One market group: engine header plus its single asset slot, owned.
pub struct Group {
    pub header: MarketGroupV16HeaderAccount,
    pub markets: Vec<Market<Wrapper>>,
}

const HEADER_LEN: usize = core::mem::size_of::<MarketGroupV16HeaderAccount>();
const SLOT_LEN: usize = core::mem::size_of::<Market<Wrapper>>();
pub const PORTFOLIO_LEN: usize = core::mem::size_of::<PortfolioAccountV16Account>();

impl Group {
    pub fn create(
        group_id: [u8; 32],
        params: &MarketParams,
        init_price: u64,
        now_slot: u64,
    ) -> ExResult<Self> {
        if init_price == 0 || init_price > policy::MAX_PRICE {
            return Err(ExError::BadPrice);
        }
        let cfg = params.to_engine_config()?;
        let mut header = MarketGroupV16HeaderAccount::new_dynamic(group_id, cfg, 1, now_slot)?;
        let mut markets = alloc::vec![Market::new([0u8; 32], EngineAssetSlotV16Account::default())];
        header.activate_empty_asset_slot_not_atomic(
            ASSET as u32,
            &mut markets[ASSET].engine,
            init_price,
            now_slot,
        )?;
        let mut g = Group { header, markets };
        g.view().validate_shape()?;
        Ok(g)
    }

    pub fn from_bytes(bytes: &[u8]) -> ExResult<Self> {
        if bytes.len() < HEADER_LEN || (bytes.len() - HEADER_LEN) % SLOT_LEN != 0 {
            return Err(ExError::Corrupt);
        }
        let header: MarketGroupV16HeaderAccount =
            bytemuck::pod_read_unaligned(&bytes[..HEADER_LEN]);
        let markets = bytes[HEADER_LEN..]
            .chunks_exact(SLOT_LEN)
            .map(bytemuck::pod_read_unaligned::<Market<Wrapper>>)
            .collect::<Vec<_>>();
        MarketGroupV16HeaderAccount::validate_dynamic_market_group_account_len::<Wrapper>(
            bytes.len(),
            header.asset_slot_capacity.get() as usize,
        )?;
        Ok(Group { header, markets })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + SLOT_LEN * self.markets.len());
        out.extend_from_slice(bytemuck::bytes_of(&self.header));
        for m in &self.markets {
            out.extend_from_slice(bytemuck::bytes_of(m));
        }
        out
    }

    pub fn view(&mut self) -> MarketGroupV16ViewMut<'_, Wrapper> {
        MarketGroupV16ViewMut::new(&mut self.header, &mut self.markets)
    }

    pub fn asset(&self) -> ExResult<percolator::AssetStateV16> {
        Ok(self.markets[ASSET].engine.asset.try_to_runtime()?)
    }

    pub fn config(&self) -> ExResult<V16Config> {
        Ok(self.header.config.try_to_runtime_shape()?)
    }

    pub fn vault(&self) -> u128 {
        self.header.vault.get()
    }
}

pub fn new_portfolio(
    group_id: [u8; 32],
    portfolio_id: [u8; 32],
    owner: [u8; 32],
) -> ExResult<Box<PortfolioAccountV16Account>> {
    let header = ProvenanceHeaderV16Account::from_runtime(&ProvenanceHeaderV16::new(
        group_id,
        portfolio_id,
        owner,
    ));
    let mut acct: Box<PortfolioAccountV16Account> = Box::new(bytemuck::Zeroable::zeroed());
    acct.init_empty_in_place(header)?;
    Ok(acct)
}

pub fn portfolio_from_bytes(bytes: &[u8]) -> ExResult<Box<PortfolioAccountV16Account>> {
    if bytes.len() != PORTFOLIO_LEN {
        return Err(ExError::Corrupt);
    }
    Ok(Box::new(bytemuck::pod_read_unaligned(bytes)))
}

pub fn portfolio_to_bytes(p: &PortfolioAccountV16Account) -> Vec<u8> {
    bytemuck::bytes_of(p).to_vec()
}

/// The pool portfolio's signed effective position in the market asset
/// (positive = pool long). `floor(basis * A_side / a_basis)` (spec §2).
pub fn effective_position_q(g: &Group, p: &PortfolioAccountV16Account) -> ExResult<i128> {
    let asset = g.asset()?;
    for leg in p.legs.iter() {
        let leg = leg.try_to_runtime()?;
        if !leg.active || leg.asset_index as usize != ASSET || leg.a_basis == 0 {
            continue;
        }
        let a_side = match leg.side {
            SideV16::Long => asset.a_long,
            SideV16::Short => asset.a_short,
        };
        // |basis| <= MAX_POSITION_ABS_Q (1e14), A <= ADL_ONE (1e15): product < 2^127.
        debug_assert!(a_side <= ADL_ONE);
        let mag = leg
            .basis_pos_q
            .unsigned_abs()
            .checked_mul(a_side)
            .ok_or(ExError::Overflow)?
            / leg.a_basis;
        let mag = i128::try_from(mag).map_err(|_| ExError::Overflow)?;
        return Ok(if leg.basis_pos_q < 0 { -mag } else { mag });
    }
    Ok(0)
}

/// Funding rate for the next accrual, sourced from pool skew (never from a caller).
pub fn funding_rate_e9(g: &Group, pool: &PortfolioAccountV16Account) -> ExResult<i128> {
    let asset = g.asset()?;
    let cfg = g.config()?;
    let oi = asset.oi_eff_long_q.max(asset.oi_eff_short_q);
    Ok(policy::skew_funding_rate_e9(
        effective_position_q(g, pool)?,
        oi,
        cfg.max_abs_funding_e9_per_slot,
    ))
}

/// Advance the engine toward `meta.target` up to `now_slot` along the canonical
/// capped staircase (spec §1.7, §9.6). Idempotent; bounded work.
///
/// - Flat market (no positions or loss state anywhere): re-anchor directly to the
///   target at `now_slot` (no exposure means no price/funding risk).
/// - Exposed market: feed `accrue_asset_path_to_not_atomic` the exact canonical
///   one-slot steps, carrying the sub-atom remainder in `meta`.
pub fn catch_up(
    g: &mut Group,
    meta: &mut OracleMeta,
    pool: &PortfolioAccountV16Account,
    now_slot: u64,
) -> ExResult<()> {
    if meta.target == 0 {
        return Err(ExError::BadPrice);
    }
    let asset = g.asset()?;
    if asset.slot_last >= now_slot && asset.raw_oracle_target_price == meta.target {
        return Ok(());
    }
    if asset.oi_eff_long_q == 0 && asset.oi_eff_short_q == 0 {
        match g
            .view()
            .reset_empty_asset_oracle_anchor_not_atomic(ASSET, meta.target, now_slot)
        {
            Ok(()) => {
                meta.remainder_bps_num = 0;
                return Ok(());
            }
            // Residual loss/position state somewhere: fall through to the path.
            Err(V16Error::LockActive) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let funding = funding_rate_e9(g, pool)?;
    for _ in 0..MAX_CATCH_UP_CALLS {
        let asset = g.asset()?;
        if asset.slot_last >= now_slot {
            break;
        }
        let cfg = g.config()?;
        let n = (now_slot - asset.slot_last)
            .min(cfg.max_accrual_dt_slots)
            .min(V16_MAX_ACCRUAL_PATH_STEPS as u64) as usize;
        let target_changed = asset.raw_oracle_target_price != meta.target;
        let anchor = if target_changed {
            asset.effective_price
        } else {
            asset.fund_px_last
        };
        let exposed = asset.oi_eff_long_q != 0 || asset.oi_eff_short_q != 0;
        let mut rem = if target_changed {
            0
        } else {
            meta.remainder_bps_num
        };
        let mut price = asset.effective_price;
        let mut steps = [AccrualStepV16 {
            effective_price: 0,
            funding_rate_e9: 0,
            price_move_remainder_before_bps_num: 0,
            price_move_remainder_after_bps_num: 0,
        }; V16_MAX_ACCRUAL_PATH_STEPS];
        for step in steps.iter_mut().take(n) {
            let before = rem;
            let (next, after) = percolator::canonical_accrual_price_step_v16(
                price,
                meta.target,
                anchor,
                cfg.max_price_move_bps_per_slot,
                exposed,
                before,
            )?;
            *step = AccrualStepV16 {
                effective_price: next,
                funding_rate_e9: funding,
                price_move_remainder_before_bps_num: before,
                price_move_remainder_after_bps_num: after,
            };
            price = next;
            rem = after;
        }
        let target_slot = asset.slot_last + n as u64;
        g.view().accrue_asset_path_to_not_atomic(
            ASSET,
            target_slot,
            meta.target,
            &steps[..n],
            true,
        )?;
        meta.remainder_bps_num = rem;
    }
    Ok(())
}

/// Engine market mode is Live (encoding 0, see engine `encode_market_mode`).
pub fn is_live(g: &Group) -> bool {
    g.header.mode == 0
}

/// True while the engine's effective price has not yet caught up to the raw
/// target. Spec §9.7: risk-increasing trades and extraction must be refused.
pub fn has_lag(g: &Group, meta: &OracleMeta) -> ExResult<bool> {
    let a = g.asset()?;
    Ok(a.effective_price != meta.target || a.raw_oracle_target_price != meta.target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub exec_price: u64,
    pub size_q: i128,
    pub fee: u128,
    pub notional: u128,
}

/// Taker trade against the LP pool (pool-to-peer). `size_q > 0` buys (taker
/// goes longer), `< 0` sells. The caller must have run `catch_up` for this slot.
pub fn trade_against_pool(
    g: &mut Group,
    meta: &OracleMeta,
    taker: &mut PortfolioAccountV16Account,
    pool: &mut PortfolioAccountV16Account,
    size_q: i128,
    limit_price: u64,
    spread_bps: u64,
) -> ExResult<Fill> {
    if size_q == 0 || size_q == i128::MIN {
        return Err(ExError::ZeroSize);
    }
    if has_lag(g, meta)? {
        return Err(ExError::OracleLag);
    }
    let taker_buys = size_q > 0;
    let effective = g.asset()?.effective_price;
    let exec_price =
        policy::pool_exec_price(effective, spread_bps, taker_buys).ok_or(ExError::BadPrice)?;
    if !policy::within_limit(exec_price, limit_price, taker_buys) {
        return Err(ExError::Slippage);
    }
    let fee_bps = g.config()?.max_trading_fee_bps;
    let request = TradeRequestV16 {
        asset_index: ASSET,
        size_q: size_q.abs(),
        exec_price,
        fee_bps,
    };
    let mut taker_v = PortfolioV16ViewMut::new(taker);
    let mut pool_v = PortfolioV16ViewMut::new(pool);
    let mut view = g.view();
    let out = if taker_buys {
        view.execute_trade_with_fee_loss_stale_scoped_not_atomic(
            &mut taker_v,
            &mut pool_v,
            request,
            true,
        )?
    } else {
        view.execute_trade_with_fee_loss_stale_scoped_not_atomic(
            &mut pool_v,
            &mut taker_v,
            request,
            false,
        )?
    };
    Ok(Fill {
        exec_price,
        size_q,
        fee: if taker_buys { out.fee_a } else { out.fee_b },
        notional: out.notional,
    })
}

pub fn deposit(g: &mut Group, p: &mut PortfolioAccountV16Account, amount: u128) -> ExResult<()> {
    let mut pv = PortfolioV16ViewMut::new(p);
    g.view().deposit_not_atomic(&mut pv, amount)?;
    Ok(())
}

/// Withdraw margin (flat or not), after oracle catch-up. Refuses while lagged.
pub fn withdraw(
    g: &mut Group,
    meta: &OracleMeta,
    p: &mut PortfolioAccountV16Account,
    amount: u128,
) -> ExResult<()> {
    if has_lag(g, meta)? {
        return Err(ExError::OracleLag);
    }
    let mut pv = PortfolioV16ViewMut::new(p);
    g.view()
        .withdraw_with_open_positions_not_atomic(&mut pv, amount)?;
    Ok(())
}

/// Convert matured, backed positive PnL into capital. Returns the amount converted.
pub fn convert_pnl(
    g: &mut Group,
    meta: &OracleMeta,
    p: &mut PortfolioAccountV16Account,
) -> ExResult<u128> {
    if has_lag(g, meta)? {
        return Err(ExError::OracleLag);
    }
    let mut pv = PortfolioV16ViewMut::new(p);
    Ok(g.view()
        .convert_released_pnl_to_capital_not_atomic(&mut pv)?)
}

pub fn liquidate(
    g: &mut Group,
    p: &mut PortfolioAccountV16Account,
) -> ExResult<LiquidationOutcomeV16> {
    let mut pv = PortfolioV16ViewMut::new(p);
    Ok(g.view()
        .liquidate_account_not_atomic(&mut pv, LiquidationRequestV16 { asset_index: ASSET })?)
}

/// One engine-selected permissionless progress step for `p` (refresh, settle,
/// liquidate, close, recovery). The keeper supplies nothing but the account.
pub fn auto_crank(
    g: &mut Group,
    p: &mut PortfolioAccountV16Account,
    pool_funding_e9: i128,
    now_slot: u64,
) -> ExResult<AutoCrankResultV16> {
    let effective = g.asset()?.effective_price;
    let obs = [AutoCrankObservationV16 {
        asset_index: ASSET,
        effective_price: effective,
        funding_rate_e9: pool_funding_e9,
    }];
    let mut pv = PortfolioV16ViewMut::new(p);
    Ok(g.view().permissionless_auto_crank_not_atomic(
        &mut pv,
        AutoCrankWorkV16 {
            now_slot,
            observations: &obs,
            resolved_close_fee_rate_per_slot: 0,
        },
    )?)
}

/// Fully refresh (touch) an account so its certificate and PnL are current.
pub fn refresh(g: &mut Group, p: &mut PortfolioAccountV16Account) -> ExResult<()> {
    let mut pv = PortfolioV16ViewMut::new(p);
    g.view().full_account_refresh_not_atomic(&mut pv)?;
    Ok(())
}

/// Insurance top-up, split evenly across the asset's long and short loss domains.
///
/// Live-only (W-8): the engine's insurance deposit has no mode gate, and after
/// resolution the payout snapshot is fixed, so a late donation would only strand
/// the donor's funds.
pub fn deposit_insurance(g: &mut Group, amount: u128) -> ExResult<()> {
    if !is_live(g) {
        return Err(ExError::Engine(V16Error::LockActive));
    }
    let (long_d, short_d) = percolator::v16_domain_pair_for_asset_index(ASSET)?;
    let half = amount / 2;
    let mut view = g.view();
    if half > 0 {
        view.deposit_domain_insurance_not_atomic(long_d, half)?;
    }
    if amount - half > 0 {
        view.deposit_domain_insurance_not_atomic(short_d, amount - half)?;
    }
    Ok(())
}

/// Withdraw from the engine's unbudgeted insurance surplus. This is how
/// accrued trading fees leave the engine vault to become claimable tokens.
/// Returns the amount actually withdrawn (may be less than requested if the
/// surplus is insufficient).
pub fn withdraw_insurance_surplus(g: &mut Group, amount: u128) -> ExResult<u128> {
    if amount == 0 {
        return Ok(0);
    }
    // Try the full amount first; if the surplus is too small, query the
    // available surplus and withdraw that instead.
    match g.view().withdraw_insurance_surplus_not_atomic(amount) {
        Ok(()) => Ok(amount),
        Err(_) => {
            // The insurance may have less surplus than the fee ledger claims.
            // Withdraw whatever is available.
            let insurance = g.header.insurance.get();
            let reserved = g
                .header
                .source_insurance_credit_reserved_total_atoms
                .get()
                + g.header.insurance_domain_budget_remaining_total.get();
            let available = insurance.saturating_sub(reserved);
            if available == 0 {
                return Ok(0);
            }
            let withdraw = amount.min(available);
            g.view()
                .withdraw_insurance_surplus_not_atomic(withdraw)?;
            Ok(withdraw)
        }
    }
}
