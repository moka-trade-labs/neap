//! near-sdk contract layer: storage, authorization, NEP-141 custody and async
//! withdrawals. All risk logic lives in the engine; all wrapper policy lives in
//! `policy` / `exchange`.
//!
//! Security model (see crates/SECURITY.md for the full list):
//! - One contract holds every market group, portfolio, the LP pools and the
//!   tokens. No cross-contract call ever happens between reading and writing
//!   engine state, so a panic reverts the whole transition.
//! - Every engine `Err` panics (receipt revert). No partial engine writes persist.
//! - User value-moving calls require exactly 1 yoctoNEAR (full-access key).
//! - Only the configured quote token may call `ft_on_transfer`.
//! - Only the configured oracle may push prices; updates must be newer and fresh.
//! - Funding rate, fee, spread and liquidation fee are never caller inputs.
//! - Outbound `ft_transfer` debits the wallet ledger first; a failed transfer
//!   re-credits the wallet in the callback (no engine re-entry).

use near_sdk::json_types::{I128, U128, U64};
use near_sdk::store::{LookupMap, Vector};
use near_sdk::{
    assert_one_yocto, env, ext_contract, near, require, serde_json, AccountId, BorshStorageKey,
    Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue, PromiseResult,
};

use crate::exchange::{self, ExError, Group, MarketParams};
use crate::model::{Market, Mode, Observation, ObservationView, Owner, PriceSource, Tier};
use crate::policy::{self, Custody};
use percolator::PortfolioAccountV16Account;

/// NEP-297 event envelope (docs/contract.md §8).
const EVENT_STANDARD: &str = "neap";
const EVENT_VERSION: &str = "1.0.0";

const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_WITHDRAW_CALLBACK: Gas = Gas::from_tgas(10);
/// Allowed oracle clock skew into the future.
const ORACLE_MAX_FUTURE_NS: u64 = 2_000_000_000;

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum Key {
    Markets,
    Groups,
    Portfolios,
    Wallets,
    LpShares,
}

#[ext_contract(ext_ft)]
#[allow(dead_code)]
trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

/// JSON form of [`MarketParams`].
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct MarketParamsJson {
    pub initial_margin_bps: U64,
    pub maintenance_margin_bps: U64,
    pub trading_fee_bps: U64,
    pub liquidation_fee_bps: U64,
    pub liquidation_fee_cap: U128,
    pub min_liquidation_abs: U128,
    pub max_price_move_bps_per_slot: U64,
    pub max_accrual_dt_slots: U64,
    pub max_abs_funding_e9_per_slot: U64,
    pub h_min: U64,
    pub h_max: U64,
    pub min_nonzero_mm_req: U128,
    pub min_nonzero_im_req: U128,
}

impl From<&MarketParamsJson> for MarketParams {
    fn from(j: &MarketParamsJson) -> Self {
        MarketParams {
            initial_margin_bps: j.initial_margin_bps.0,
            maintenance_margin_bps: j.maintenance_margin_bps.0,
            trading_fee_bps: j.trading_fee_bps.0,
            liquidation_fee_bps: j.liquidation_fee_bps.0,
            liquidation_fee_cap: j.liquidation_fee_cap.0,
            min_liquidation_abs: j.min_liquidation_abs.0,
            max_price_move_bps_per_slot: j.max_price_move_bps_per_slot.0,
            max_accrual_dt_slots: j.max_accrual_dt_slots.0,
            max_abs_funding_e9_per_slot: j.max_abs_funding_e9_per_slot.0,
            h_min: j.h_min.0,
            h_max: j.h_max.0,
            min_nonzero_mm_req: j.min_nonzero_mm_req.0,
            min_nonzero_im_req: j.min_nonzero_im_req.0,
        }
    }
}

/// JSON view of a [`Market`] registry entry plus live engine state.
#[near(serializers = [json])]
pub struct MarketView {
    pub symbol: String,
    pub tier: Tier,
    pub mode: Mode,
    pub price_source: PriceSource,
    pub observation: ObservationView,
    pub lister: Owner,
    pub bond: U128,
    pub oracle_target: U64,
    pub effective_price: U64,
    pub oi_long_q: U128,
    pub oi_short_q: U128,
    pub vault: U128,
    pub insurance: U128,
    pub pool_position_q: I128,
    pub funding_rate_e9_per_slot: I128,
    pub lp_total_shares: U128,
    pub lp_nav_lower: U128,
    pub lp_nav_upper: U128,
    pub slot_last: U64,
}

#[near(serializers = [json])]
pub struct AccountView {
    pub capital: U128,
    pub pnl: I128,
    pub reserved_pnl: U128,
    pub fee_credits: I128,
    pub position_q: I128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Neap {
    owner: AccountId,
    quote_token: AccountId,
    oracle: AccountId,
    markets: Vector<Market>,
    groups: LookupMap<u32, Vec<u8>>,
    portfolios: LookupMap<(u32, Owner), Vec<u8>>,
    wallets: LookupMap<Owner, u128>,
    lp_shares: LookupMap<(u32, Owner), u128>,
    custody_wallets: u128,
    custody_vaults: u128,
}

fn fail(e: ExError) -> ! {
    env::panic_str(&format!("percolator: {e:?}"))
}

fn ok<T>(r: Result<T, ExError>) -> T {
    r.unwrap_or_else(|e| fail(e))
}

/// Emit a NEP-297 event. Amounts must already be strings in `data`; every
/// event carries the block height.
fn emit(event: &str, mut data: serde_json::Value) {
    data["block"] = serde_json::json!(env::block_height());
    let line = serde_json::json!({
        "standard": EVENT_STANDARD,
        "version": EVENT_VERSION,
        "event": event,
        "data": [data],
    });
    env::log_str(&format!("EVENT_JSON:{line}"));
}

fn slot() -> u64 {
    env::block_height()
}

#[near]
impl Neap {
    #[init]
    pub fn new(owner: AccountId, quote_token: AccountId, oracle: AccountId) -> Self {
        Self {
            owner,
            quote_token,
            oracle,
            markets: Vector::new(Key::Markets),
            groups: LookupMap::new(Key::Groups),
            portfolios: LookupMap::new(Key::Portfolios),
            wallets: LookupMap::new(Key::Wallets),
            lp_shares: LookupMap::new(Key::LpShares),
            custody_wallets: 0,
            custody_vaults: 0,
        }
    }

    // ───────────────────────────── admin ─────────────────────────────

    /// List a new market (one isolated engine market group + its LP pool).
    #[payable]
    pub fn create_market(
        &mut self,
        symbol: String,
        init_price: U64,
        params: MarketParamsJson,
        spread_bps: u64,
        max_oracle_age_ns: U64,
    ) -> u32 {
        self.assert_owner();
        require!(spread_bps <= policy::MAX_SPREAD_BPS, "spread too wide");
        let start = env::storage_usage();
        let id = self.markets.len();
        let gid = self.group_id(id);
        let g = ok(Group::create(
            gid,
            &MarketParams::from(&params),
            init_price.0,
            slot(),
        ));
        let pool = ok(exchange::new_portfolio(
            gid,
            self.pool_key(),
            self.pool_key(),
        ));
        self.groups.insert(id, g.to_bytes());
        self.portfolios.insert(
            (id, Self::pool_owner()),
            exchange::portfolio_to_bytes(&pool),
        );
        let lister = Owner::Near(env::predecessor_account_id());
        self.markets.push(Market {
            symbol,
            tier: Tier::Major,
            mode: Mode::Active,
            price_source: PriceSource::Pushed,
            observation: Observation {
                price_e6: init_price.0,
                liquidity_usd: 0,
                source_ts: env::block_timestamp(),
                seq: 0,
                block: slot(),
            },
            lister,
            bond: 0,
            spread_bps,
            max_oracle_age_ns: max_oracle_age_ns.0,
            oracle_target: init_price.0,
            oracle_remainder_bps_num: 0,
            oracle_last_publish_ns: env::block_timestamp(),
            lp_total_shares: 0,
        });
        self.flush();
        self.charge_storage(start);
        id
    }

    pub fn set_oracle(&mut self, oracle: AccountId) {
        self.assert_owner();
        self.oracle = oracle;
    }

    pub fn set_owner(&mut self, owner: AccountId) {
        self.assert_owner();
        self.owner = owner;
    }

    // ─────────────────────── registration / storage ───────────────────────

    /// Create the caller's wallet ledger entry (required before depositing tokens).
    #[payable]
    pub fn register(&mut self) {
        let who = Self::user();
        if self.wallets.contains_key(&who) {
            self.refund_deposit(env::attached_deposit());
            return;
        }
        let start = env::storage_usage();
        self.wallets.insert(who, 0);
        self.flush();
        self.charge_storage(start);
    }

    /// Create the caller's portfolio in `market` (pays its ~9.5 KB storage).
    #[payable]
    pub fn open_account(&mut self, market: u32) {
        let who = Self::user();
        self.market(market);
        require!(
            !self.portfolios.contains_key(&(market, who.clone())),
            "account exists"
        );
        let start = env::storage_usage();
        let gid = self.group_id(market);
        let key = who.engine_key();
        let p = ok(exchange::new_portfolio(gid, key, key));
        self.portfolios
            .insert((market, who), exchange::portfolio_to_bytes(&p));
        self.flush();
        self.charge_storage(start);
    }

    // ───────────────────────────── custody ─────────────────────────────

    /// NEP-141 receiver. Credits the sender's wallet. Unregistered senders and
    /// any token other than the quote token are refused (full refund).
    pub fn ft_on_transfer(
        &mut self,
        sender_id: AccountId,
        amount: U128,
        msg: String,
    ) -> PromiseOrValue<U128> {
        require!(
            env::predecessor_account_id() == self.quote_token,
            "only the quote token"
        );
        let _ = msg;
        let who = Owner::Near(sender_id);
        let Some(bal) = self.wallets.get(&who).copied() else {
            return PromiseOrValue::Value(amount);
        };
        let mut c = self.custody();
        c.external_in(amount.0)
            .unwrap_or_else(|| env::panic_str("custody overflow"));
        self.wallets.insert(who, bal + amount.0);
        self.set_custody(c);
        PromiseOrValue::Value(U128(0))
    }

    /// Send `amount` of the caller's wallet balance back to the caller.
    #[payable]
    pub fn withdraw(&mut self, amount: U128) -> Promise {
        assert_one_yocto();
        require!(amount.0 > 0, "zero amount");
        let who = Self::user();
        self.debit_wallet(&who, amount.0);
        let mut c = self.custody();
        c.external_out(amount.0)
            .unwrap_or_else(|| env::panic_str("custody underflow"));
        self.set_custody(c);
        let Owner::Near(to) = who.clone() else {
            env::panic_str("withdraw to a NEAR account only")
        };
        ext_ft::ext(self.quote_token.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(GAS_FT_TRANSFER)
            .ft_transfer(to, amount, None)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_WITHDRAW_CALLBACK)
                    .on_withdraw(who, amount),
            )
    }

    /// Re-credit the wallet if the outbound transfer failed.
    #[private]
    pub fn on_withdraw(&mut self, account: Owner, amount: U128) -> bool {
        match env::promise_result(0) {
            PromiseResult::Successful(_) => true,
            _ => {
                let bal = self.wallets.get(&account).copied().unwrap_or(0);
                let mut c = self.custody();
                c.external_in(amount.0)
                    .unwrap_or_else(|| env::panic_str("custody overflow"));
                self.wallets.insert(account, bal + amount.0);
                self.set_custody(c);
                false
            }
        }
    }

    // ───────────────────────────── trading ─────────────────────────────

    /// Move wallet funds into the caller's margin account in `market`.
    #[payable]
    pub fn deposit(&mut self, market: u32, amount: U128) {
        assert_one_yocto();
        let who = Self::user();
        self.debit_wallet(&who, amount.0);
        let mut g = self.load_group(market);
        let mut p = self.load_portfolio(market, &who);
        ok(exchange::deposit(&mut g, &mut p, amount.0));
        self.store_group(market, &g);
        self.store_portfolio(market, &who, &p);
        self.vault_in(amount.0);
    }

    /// Move free margin (above initial margin, excluding unrealized profit)
    /// from `market` back to the wallet. Works with open positions.
    #[payable]
    pub fn withdraw_margin(&mut self, market: u32, amount: U128) {
        assert_one_yocto();
        let who = Self::user();
        let (mut g, mut info, _pool) = self.load_market_caught_up(market);
        let mut p = self.load_portfolio(market, &who);
        ok(exchange::withdraw(&mut g, &info.meta(), &mut p, amount.0));
        self.store_group(market, &g);
        self.store_portfolio(market, &who, &p);
        self.save_market(market, &mut info);
        self.vault_out_to_wallet(&who, amount.0);
    }

    /// Market order against the LP pool. `size_q > 0` buys, `< 0` sells, in
    /// engine units (`POS_SCALE` = 1 base unit). `limit_price` bounds slippage.
    #[payable]
    pub fn trade(&mut self, market: u32, size_q: I128, limit_price: U64) -> U64 {
        assert_one_yocto();
        let who = Self::user();
        let (mut g, mut info, mut pool) = self.load_market_caught_up(market);
        let mut p = self.load_portfolio(market, &who);
        let fill = ok(exchange::trade_against_pool(
            &mut g,
            &info.meta(),
            &mut p,
            &mut pool,
            size_q.0,
            limit_price.0,
            info.spread_bps,
        ));
        self.store_group(market, &g);
        self.store_portfolio(market, &who, &p);
        self.store_pool(market, &pool);
        self.save_market(market, &mut info);
        emit(
            "fill",
            serde_json::json!({
                "market": market,
                "owner": who.label(),
                "size_q": fill.size_q.to_string(),
                "price": fill.exec_price.to_string(),
                "fee": fill.fee.to_string(),
            }),
        );
        U64(fill.exec_price)
    }

    /// Convert matured, backed profit into withdrawable capital.
    #[payable]
    pub fn convert_pnl(&mut self, market: u32) -> U128 {
        assert_one_yocto();
        let who = Self::user();
        let (mut g, mut info, _pool) = self.load_market_caught_up(market);
        let mut p = self.load_portfolio(market, &who);
        let out = ok(exchange::convert_pnl(&mut g, &info.meta(), &mut p));
        self.store_group(market, &g);
        self.store_portfolio(market, &who, &p);
        self.save_market(market, &mut info);
        U128(out)
    }

    // ───────────────────────────── LP pool ─────────────────────────────

    /// Deposit wallet funds into the market's LP pool for shares.
    #[payable]
    pub fn lp_deposit(&mut self, market: u32, amount: U128) -> U128 {
        // W-4: at least 1 yocto (full-access key); a first deposit also pays the
        // storage of its share-ledger entry (checked at the end).
        require!(
            env::attached_deposit() >= NearToken::from_yoctonear(1),
            "attach 1 yoctoNEAR (or storage for a first LP deposit)"
        );
        let storage_start = env::storage_usage();
        let who = Self::user();
        self.debit_wallet(&who, amount.0);
        let (mut g, mut info, mut pool) = self.load_market_caught_up(market);
        // W-3: pricing shares off a stale mark would let a depositor front-run
        // the pending catch-up and dilute existing LPs.
        if ok(exchange::has_lag(&g, &info.meta())) {
            fail(ExError::OracleLag);
        }
        if pool.active_bitmap.iter().any(|w| w.get() != 0) {
            ok(exchange::refresh(&mut g, &mut pool));
        }
        let (_, nav_upper) =
            policy::lp_nav_bounds(pool.capital.get(), pool.pnl.get(), pool.fee_credits.get());
        let shares = policy::lp_shares_for_deposit(amount.0, info.lp_total_shares, nav_upper)
            .unwrap_or_else(|| fail(ExError::LpShares));
        ok(exchange::deposit(&mut g, &mut pool, amount.0));
        info.lp_total_shares += shares;
        let key = (market, who.clone());
        let prev = self.lp_shares.get(&key).copied().unwrap_or(0);
        self.lp_shares.insert(key, prev + shares);
        self.store_group(market, &g);
        self.store_pool(market, &pool);
        self.save_market(market, &mut info);
        self.vault_in(amount.0);
        self.flush();
        self.charge_storage_after_yocto(storage_start);
        U128(shares)
    }

    /// Burn LP shares for their conservative (lower-bound) NAV. Fails if the
    /// pool would drop below its initial-margin requirement (high utilization).
    #[payable]
    pub fn lp_redeem(&mut self, market: u32, shares: U128) -> U128 {
        assert_one_yocto();
        let who = Self::user();
        let key = (market, who.clone());
        let held = self.lp_shares.get(&key).copied().unwrap_or(0);
        require!(shares.0 > 0 && shares.0 <= held, "not enough shares");
        let (mut g, mut info, mut pool) = self.load_market_caught_up(market);
        if pool.active_bitmap.iter().any(|w| w.get() != 0) {
            ok(exchange::refresh(&mut g, &mut pool));
        }
        let (nav_lower, _) =
            policy::lp_nav_bounds(pool.capital.get(), pool.pnl.get(), pool.fee_credits.get());
        let assets = policy::lp_assets_for_redeem(shares.0, info.lp_total_shares, nav_lower)
            .unwrap_or_else(|| fail(ExError::LpShares));
        if assets > 0 {
            ok(exchange::withdraw(&mut g, &info.meta(), &mut pool, assets));
        }
        info.lp_total_shares -= shares.0;
        self.lp_shares.insert(key, held - shares.0);
        self.store_group(market, &g);
        self.store_pool(market, &pool);
        self.save_market(market, &mut info);
        if assets > 0 {
            self.vault_out_to_wallet(&who, assets);
        }
        U128(assets)
    }

    /// Donate wallet funds to the market's insurance fund.
    #[payable]
    pub fn insurance_deposit(&mut self, market: u32, amount: U128) {
        assert_one_yocto();
        let who = Self::user();
        self.debit_wallet(&who, amount.0);
        let mut g = self.load_group(market);
        ok(exchange::deposit_insurance(&mut g, amount.0));
        self.store_group(market, &g);
        self.vault_in(amount.0);
    }

    // ─────────────────────── oracle / keepers ───────────────────────

    /// Oracle pushes a new raw target. The engine price then walks toward it
    /// along the capped staircase; it never jumps.
    pub fn push_price(&mut self, market: u32, price: U64, publish_ns: U64) {
        require!(
            env::predecessor_account_id() == self.oracle,
            "only the oracle"
        );
        require!(
            price.0 > 0 && price.0 <= policy::MAX_PRICE,
            "price out of range"
        );
        let mut info = self.market(market);
        require!(
            policy::accept_oracle_update(
                info.oracle_last_publish_ns,
                publish_ns.0,
                env::block_timestamp(),
                info.max_oracle_age_ns,
                ORACLE_MAX_FUTURE_NS,
            ),
            "stale or out-of-order price"
        );
        let mut g = self.load_group(market);
        let pool = self.load_pool(market);
        let mut meta = info.meta();
        meta.target = price.0;
        meta.last_publish_ns = publish_ns.0;
        ok(exchange::catch_up(&mut g, &mut meta, &pool, slot()));
        info.set_meta(meta);
        info.observation = Observation {
            price_e6: price.0,
            liquidity_usd: info.observation.liquidity_usd,
            source_ts: publish_ns.0,
            seq: info.observation.seq + 1,
            block: slot(),
        };
        self.store_group(market, &g);
        self.save_market(market, &mut info);
    }

    /// Permissionless: advance the market to the current block.
    pub fn crank(&mut self, market: u32) {
        let (g, mut info, _pool) = self.load_market_caught_up(market);
        self.store_group(market, &g);
        self.save_market(market, &mut info);
    }

    /// Permissionless: run the engine-selected progress step (refresh, settle,
    /// liquidate, close, recovery) for `account` in `market`.
    pub fn keeper_step(&mut self, market: u32, account: AccountId) -> String {
        let account = Owner::Near(account);
        // W-6: outside Live mode the oracle catch-up is refused by the engine, but
        // recovery/resolution must stay reachable, so the keeper step skips it.
        let mut info = self.market(market);
        let mut g = self.load_group(market);
        let pool = self.load_pool(market);
        if exchange::is_live(&g) {
            let mut meta = info.meta();
            ok(exchange::catch_up(&mut g, &mut meta, &pool, slot()));
            info.set_meta(meta);
        }
        let funding = ok(exchange::funding_rate_e9(&g, &pool));
        let mut p = self.load_portfolio(market, &account);
        let r = ok(exchange::auto_crank(&mut g, &mut p, funding, slot()));
        self.store_group(market, &g);
        self.store_portfolio(market, &account, &p);
        self.save_market(market, &mut info);
        format!("{:?}", r.selected)
    }

    /// Permissionless: liquidate an under-maintenance account.
    pub fn liquidate(&mut self, market: u32, account: AccountId) -> U128 {
        let account = Owner::Near(account);
        let (mut g, mut info, _pool) = self.load_market_caught_up(market);
        let mut p = self.load_portfolio(market, &account);
        let out = ok(exchange::liquidate(&mut g, &mut p));
        self.store_group(market, &g);
        self.store_portfolio(market, &account, &p);
        self.save_market(market, &mut info);
        U128(out.closed_q)
    }

    // ───────────────────────────── views ─────────────────────────────

    pub fn get_wallet(&self, account: AccountId) -> U128 {
        U128(
            self.wallets
                .get(&Owner::Near(account))
                .copied()
                .unwrap_or(0),
        )
    }

    pub fn get_lp_shares(&self, market: u32, account: AccountId) -> U128 {
        U128(
            self.lp_shares
                .get(&(market, Owner::Near(account)))
                .copied()
                .unwrap_or(0),
        )
    }

    pub fn get_custody(&self) -> (U128, U128) {
        (U128(self.custody_wallets), U128(self.custody_vaults))
    }

    pub fn get_markets_count(&self) -> u32 {
        self.markets.len()
    }

    pub fn get_market(&self, market: u32) -> MarketView {
        let info = self.market(market);
        let g = self.load_group(market);
        let pool = self.load_pool(market);
        let a = ok(g.asset());
        let (lo, hi) =
            policy::lp_nav_bounds(pool.capital.get(), pool.pnl.get(), pool.fee_credits.get());
        MarketView {
            symbol: info.symbol.clone(),
            tier: info.tier,
            mode: info.mode,
            price_source: info.price_source.clone(),
            observation: ObservationView::from(&info.observation),
            lister: info.lister.clone(),
            bond: U128(info.bond),
            oracle_target: U64(info.oracle_target),
            effective_price: U64(a.effective_price),
            oi_long_q: U128(a.oi_eff_long_q),
            oi_short_q: U128(a.oi_eff_short_q),
            vault: U128(g.vault()),
            insurance: U128(g.header.insurance.get()),
            pool_position_q: I128(ok(exchange::effective_position_q(&g, &pool))),
            funding_rate_e9_per_slot: I128(ok(exchange::funding_rate_e9(&g, &pool))),
            lp_total_shares: U128(info.lp_total_shares),
            lp_nav_lower: U128(lo),
            lp_nav_upper: U128(hi),
            slot_last: U64(a.slot_last),
        }
    }

    pub fn get_account(&self, market: u32, account: AccountId) -> Option<AccountView> {
        let bytes = self.portfolios.get(&(market, Owner::Near(account)))?;
        let p = ok(exchange::portfolio_from_bytes(bytes));
        let g = self.load_group(market);
        Some(AccountView {
            capital: U128(p.capital.get()),
            pnl: I128(p.pnl.get()),
            reserved_pnl: U128(p.reserved_pnl.get()),
            fee_credits: I128(p.fee_credits.get()),
            position_q: I128(ok(exchange::effective_position_q(&g, &p))),
        })
    }
}

// ───────────────────────────── internals ─────────────────────────────

impl Neap {
    /// The calling user. The contract account itself is reserved: its key in the
    /// portfolio map is the LP pool, so it must never act as a user (W-2).
    fn user() -> Owner {
        let who = env::predecessor_account_id();
        require!(who != env::current_account_id(), "reserved account");
        Owner::Near(who)
    }

    /// The LP pool's owner key in every market (the contract itself).
    fn pool_owner() -> Owner {
        Owner::Near(env::current_account_id())
    }

    /// Like `charge_storage`, but the 1 yocto security deposit is not refunded
    /// when no storage was added.
    fn charge_storage_after_yocto(&self, start: u64) {
        let used = env::storage_usage().saturating_sub(start);
        if used == 0 {
            return;
        }
        self.charge_storage(start);
    }

    fn assert_owner(&self) {
        require!(
            env::predecessor_account_id() == self.owner,
            "only the owner"
        );
    }

    fn market(&self, market: u32) -> Market {
        self.markets
            .get(market)
            .cloned()
            .unwrap_or_else(|| env::panic_str("no such market"))
    }

    fn save_market(&mut self, market: u32, info: &mut Market) {
        self.markets.set(market, info.clone());
    }

    fn group_id(&self, market: u32) -> [u8; 32] {
        let mut seed = env::current_account_id().as_bytes().to_vec();
        seed.extend_from_slice(b"/market/");
        seed.extend_from_slice(&market.to_le_bytes());
        env::sha256_array(&seed)
    }

    fn pool_key(&self) -> [u8; 32] {
        Self::pool_owner().engine_key()
    }

    fn load_group(&self, market: u32) -> Group {
        let bytes = self
            .groups
            .get(&market)
            .unwrap_or_else(|| env::panic_str("no such market"));
        ok(Group::from_bytes(bytes))
    }

    fn store_group(&mut self, market: u32, g: &Group) {
        self.groups.insert(market, g.to_bytes());
    }

    fn load_portfolio(&self, market: u32, who: &Owner) -> Box<PortfolioAccountV16Account> {
        let bytes = self
            .portfolios
            .get(&(market, who.clone()))
            .unwrap_or_else(|| env::panic_str("no account in this market; call open_account"));
        ok(exchange::portfolio_from_bytes(bytes))
    }

    fn store_portfolio(&mut self, market: u32, who: &Owner, p: &PortfolioAccountV16Account) {
        self.portfolios
            .insert((market, who.clone()), exchange::portfolio_to_bytes(p));
    }

    fn load_pool(&self, market: u32) -> Box<PortfolioAccountV16Account> {
        self.load_portfolio(market, &Self::pool_owner())
    }

    fn store_pool(&mut self, market: u32, p: &PortfolioAccountV16Account) {
        self.store_portfolio(market, &Self::pool_owner(), p);
    }

    /// Load a market and run the oracle catch-up to the current block.
    fn load_market_caught_up(
        &mut self,
        market: u32,
    ) -> (Group, Market, Box<PortfolioAccountV16Account>) {
        let mut info = self.market(market);
        let mut g = self.load_group(market);
        let pool = self.load_pool(market);
        let mut meta = info.meta();
        ok(exchange::catch_up(&mut g, &mut meta, &pool, slot()));
        info.set_meta(meta);
        (g, info, pool)
    }

    fn custody(&self) -> Custody {
        Custody {
            wallets: self.custody_wallets,
            vaults: self.custody_vaults,
        }
    }

    fn set_custody(&mut self, c: Custody) {
        self.custody_wallets = c.wallets;
        self.custody_vaults = c.vaults;
    }

    fn debit_wallet(&mut self, who: &Owner, amount: u128) {
        let bal = self.wallets.get(who).copied().unwrap_or(0);
        require!(amount <= bal, "insufficient wallet balance");
        self.wallets.insert(who.clone(), bal - amount);
        // The custody totals move in the paired call (`vault_in` or
        // `Custody::external_out`), so Σ wallets == custody_wallets holds after
        // every public method returns.
    }

    fn vault_in(&mut self, amount: u128) {
        let mut c = self.custody();
        c.wallet_to_vault(amount)
            .unwrap_or_else(|| env::panic_str("custody"));
        self.set_custody(c);
    }

    fn vault_out_to_wallet(&mut self, who: &Owner, amount: u128) {
        let mut c = self.custody();
        c.vault_to_wallet(amount)
            .unwrap_or_else(|| env::panic_str("custody"));
        self.set_custody(c);
        let bal = self.wallets.get(who).copied().unwrap_or(0);
        self.wallets.insert(who.clone(), bal + amount);
    }

    fn flush(&mut self) {
        self.markets.flush();
        self.groups.flush();
        self.portfolios.flush();
        self.wallets.flush();
        self.lp_shares.flush();
    }

    /// Require the attached deposit to cover storage added since `start`;
    /// refund the excess.
    fn charge_storage(&self, start: u64) {
        let used = env::storage_usage().saturating_sub(start);
        let cost = env::storage_byte_cost().saturating_mul(used as u128);
        let attached = env::attached_deposit();
        require!(
            attached >= cost,
            format!(
                "attach at least {} yoctoNEAR for storage",
                cost.as_yoctonear()
            )
        );
        self.refund_deposit(attached.saturating_sub(cost));
    }

    fn refund_deposit(&self, amount: NearToken) {
        if !amount.is_zero() {
            Promise::new(env::predecessor_account_id()).transfer(amount);
        }
    }
}
