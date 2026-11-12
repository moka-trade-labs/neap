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
use crate::model::{
    ConditionalKind, ConditionalOrder, Market, Mode, Observation, ObservationView, Order,
    OrderSide, Owner, Payee, PriceSource, Tier,
};
use crate::policy::{self, Custody, StalenessLevel};
use percolator::{PortfolioAccountV16Account, POS_SCALE};

/// NEP-297 event envelope (docs/contract.md §8).
const EVENT_STANDARD: &str = "neap";
const EVENT_VERSION: &str = "1.0.0";

const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_WITHDRAW_CALLBACK: Gas = Gas::from_tgas(10);
const GAS_GET_POOL: Gas = Gas::from_tgas(5);
const GAS_ON_PRICE_READ: Gas = Gas::from_tgas(20);
/// Allowed oracle clock skew into the future.
const ORACLE_MAX_FUTURE_NS: u64 = 2_000_000_000;
/// Maximum order lifetime in blocks (~30 s at 600 ms/block).
const MAX_ORDER_EXPIRY_BLOCKS: u64 = 50;
/// Maximum conditional order lifetime in blocks (~30 days at 600 ms/block).
const MAX_CONDITIONAL_EXPIRY_BLOCKS: u64 = 30 * 24 * 60 * 100; // 4_320_000

// ── Fee split bps (spec §5, invariant 8) ──
const FEE_LISTER_BPS: u128 = 5_000; // 50%
const FEE_LP_BPS: u128 = 3_000; // 30%
const FEE_INSURANCE_BPS: u128 = 1_000; // 10%
const FEE_PROTOCOL_BPS: u128 = 1_000; // 10%
const LIQ_CALLER_BPS: u128 = 6_000; // 60%
// const LIQ_INSURANCE_BPS: u128 = 4_000; // 40% (remainder)
/// Min pool depth for Major tier in USDC atoms (6 decimals).
const DEPTH_FLOOR_MAJOR: u128 = 10_000_000_000;
/// Min pool depth for LongTail tier.
const DEPTH_FLOOR_LONG_TAIL: u128 = 1_000_000_000;

// ── Spread params per tier (spec §5) ──
const MAJOR_SPREAD_MIN_BPS: u64 = 5;
const MAJOR_K_SKEW_BPS: u64 = 45;
const MAJOR_K_SIZE_BPS: u64 = 5;
const LONGTAIL_SPREAD_MIN_BPS: u64 = 20;
const LONGTAIL_K_SKEW_BPS: u64 = 130;
const LONGTAIL_K_SIZE_BPS: u64 = 20;

#[near(serializers = [borsh])]
#[derive(BorshStorageKey)]
enum Key {
    Markets,
    Groups,
    Portfolios,
    Wallets,
    LpShares,
    Orders,
    Conditionals,
    FeeLedgers,
}

#[ext_contract(ext_ft)]
#[allow(dead_code)]
trait FungibleToken {
    fn ft_transfer(&mut self, receiver_id: AccountId, amount: U128, memo: Option<String>);
}

/// Ref Finance / Rhea DEX pool view (step 1.3).
#[ext_contract(ext_ref)]
#[allow(dead_code)]
trait RefFinancePool {
    fn get_pool(&self, pool_id: u64) -> serde_json::Value;
}

/// `ft_on_transfer` message routing (spec §4).
#[near(serializers = [json])]
#[serde(tag = "action")]
enum FtMsg {
    /// Credit idle wallet (default for empty `msg`).
    Wallet,
    /// Bond for a new `Proposed` market (step 1.4).
    CreateMarket {
        symbol: String,
        tier: Tier,
        price_source: PriceSource,
        spread_bps: u64,
        max_oracle_age_ns: U64,
        params: MarketParamsJson,
    },
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
    pauser: Option<AccountId>,
    quote_token: AccountId,
    oracle: AccountId,
    markets: Vector<Market>,
    groups: LookupMap<u32, Vec<u8>>,
    portfolios: LookupMap<(u32, Owner), Vec<u8>>,
    wallets: LookupMap<Owner, u128>,
    lp_shares: LookupMap<(u32, Owner), u128>,
    orders: LookupMap<u64, Order>,
    conditionals: LookupMap<u64, ConditionalOrder>,
    fee_ledgers: LookupMap<(u32, Payee), u128>,
    custody_wallets: u128,
    custody_vaults: u128,
    next_order_id: u64,
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
            pauser: None,
            quote_token,
            oracle,
            markets: Vector::new(Key::Markets),
            groups: LookupMap::new(Key::Groups),
            portfolios: LookupMap::new(Key::Portfolios),
            wallets: LookupMap::new(Key::Wallets),
            lp_shares: LookupMap::new(Key::LpShares),
            orders: LookupMap::new(Key::Orders),
            conditionals: LookupMap::new(Key::Conditionals),
            fee_ledgers: LookupMap::new(Key::FeeLedgers),
            custody_wallets: 0,
            custody_vaults: 0,
            next_order_id: 0,
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
            ema_e6: init_price.0,
            lister,
            bond: 0,
            spread_bps,
            max_oracle_age_ns: max_oracle_age_ns.0,
            oracle_target: init_price.0,
            oracle_remainder_bps_num: 0,
            oracle_last_publish_ns: env::block_timestamp(),
            lp_total_shares: 0,
            oi_cap_long: 0,
            oi_cap_short: 0,
            per_account_cap: 0,
        });
        self.flush();
        self.charge_storage(start);
        id
    }

    pub fn set_oracle(&mut self, oracle: AccountId) {
        self.assert_owner();
        self.oracle = oracle;
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

    /// NEP-141 receiver. Routes based on `msg` (spec §4):
    /// - empty / `{"action":"Wallet"}` → credit wallet
    /// - `{"action":"CreateMarket",...}` → bond for a new Proposed market (step 1.4)
    /// Unregistered senders and any token other than the quote token are refused.
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
        let who = Owner::Near(sender_id.clone());

        // Parse msg; empty → Wallet.
        let action: FtMsg = if msg.is_empty() {
            FtMsg::Wallet
        } else {
            serde_json::from_str(&msg).unwrap_or(FtMsg::Wallet)
        };

        match action {
            FtMsg::Wallet => {
                let Some(bal) = self.wallets.get(&who).copied() else {
                    return PromiseOrValue::Value(amount); // unregistered → refund
                };
                let mut c = self.custody();
                c.external_in(amount.0)
                    .unwrap_or_else(|| env::panic_str("custody overflow"));
                self.wallets.insert(who, bal + amount.0);
                self.set_custody(c);
                PromiseOrValue::Value(U128(0))
            }
            FtMsg::CreateMarket {
                symbol,
                tier,
                price_source,
                spread_bps,
                max_oracle_age_ns,
                params,
            } => {
                // Step 1.4: permissionless market creation with bond.
                let min_bond = match tier {
                    Tier::Major => 0,    // protocol-listed majors don't need a bond
                    Tier::LongTail => 50_000_000, // 50 USDC
                };
                if amount.0 < min_bond {
                    // Bond too small → refund.
                    return PromiseOrValue::Value(amount);
                }
                require!(spread_bps <= policy::MAX_SPREAD_BPS, "spread too wide");
                let start = env::storage_usage();
                let id = self.markets.len();
                let gid = self.group_id(id);
                // Initial price of 0 — it will be set by the first observation.
                // For NearPool markets the engine is seeded with price=1 (placeholder)
                // and the real price arrives via refresh_price → on_price_read.
                let init_price = 1_000_000u64; // $1 placeholder
                let g = ok(Group::create(
                    gid,
                    &MarketParams::from(&params),
                    init_price,
                    slot(),
                ));
                let pool = ok(exchange::new_portfolio(gid, self.pool_key(), self.pool_key()));
                self.groups.insert(id, g.to_bytes());
                self.portfolios.insert(
                    (id, Self::pool_owner()),
                    exchange::portfolio_to_bytes(&pool),
                );
                self.markets.push(Market {
                    symbol: symbol.clone(),
                    tier,
                    mode: Mode::Proposed,
                    price_source,
                    observation: Observation {
                        price_e6: 0,
                        liquidity_usd: 0,
                        source_ts: 0,
                        seq: 0,
                        block: 0,
                    },
                    ema_e6: 0,
                    lister: who.clone(),
                    bond: amount.0,
                    spread_bps,
                    max_oracle_age_ns: max_oracle_age_ns.0,
                    oracle_target: init_price,
                    oracle_remainder_bps_num: 0,
                    oracle_last_publish_ns: 0,
                    lp_total_shares: 0,
                    oi_cap_long: 0,
                    oi_cap_short: 0,
                    per_account_cap: 0,
                });
                // Track the bond in custody.
                let mut c = self.custody();
                c.external_in(amount.0)
                    .unwrap_or_else(|| env::panic_str("custody overflow"));
                self.wallets
                    .entry(who.clone())
                    .or_insert(0);
                // Bond goes to the lister's wallet, then immediately into the
                // vault via LP seed (or held for refund). For now, hold in wallet.
                let bal = self.wallets.get(&who).copied().unwrap_or(0);
                self.wallets.insert(who.clone(), bal + amount.0);
                self.set_custody(c);
                self.flush();
                self.charge_storage(start);
                emit(
                    "market_created",
                    serde_json::json!({
                        "market": id,
                        "symbol": symbol,
                        "tier": format!("{:?}", tier),
                        "lister": who.label(),
                        "bond": amount.0.to_string(),
                    }),
                );
                PromiseOrValue::Value(U128(0))
            }
        }
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

    // ─────────────────────── 1.6: two-step orders ───────────────────────

    /// Request a pending order. Requires 1 yoctoNEAR and an open margin account.
    /// Returns the order ID. The keeper calls `execute_order` once a fresher
    /// observation is available (invariant 3: observation.block > request_block).
    #[payable]
    pub fn request_order(
        &mut self,
        market: u32,
        side: OrderSide,
        size_q: U128,
        limit_price: U64,
        expiry_blocks: u64,
        reduce_only: bool,
    ) -> U64 {
        assert_one_yocto();
        let who = Self::user();
        require!(size_q.0 > 0, "zero size");
        require!(
            side == OrderSide::Short || limit_price.0 > 0,
            "zero limit for a buy"
        );
        require!(
            expiry_blocks > 0 && expiry_blocks <= MAX_ORDER_EXPIRY_BLOCKS,
            "bad expiry"
        );
        let info = self.market(market);
        require!(
            info.mode == Mode::Active || info.mode == Mode::ReduceOnly,
            "market not active"
        );
        // Verify the caller has a margin account (the fill needs it).
        require!(
            self.portfolios.contains_key(&(market, who.clone())),
            "no account in this market; call open_account"
        );

        let id = self.next_order_id;
        self.next_order_id += 1;
        let order = Order {
            id,
            market,
            owner: who.clone(),
            side,
            size_q: size_q.0,
            limit_price: limit_price.0,
            request_block: slot(),
            expiry_block: slot() + expiry_blocks,
            reduce_only,
        };
        self.orders.insert(id, order);
        self.orders.flush();
        emit(
            "order_requested",
            serde_json::json!({
                "id": id.to_string(),
                "market": market,
                "owner": who.label(),
                "side": format!("{:?}", side),
                "size_q": size_q.0.to_string(),
                "limit_price": limit_price.0.to_string(),
                "reduce_only": reduce_only,
            }),
        );
        U64(id)
    }

    /// Execute a pending order. Permissionless (the keeper typically calls it).
    /// Requires that the market's latest observation was recorded in a block
    /// **after** the order's request block (invariant 3).
    pub fn execute_order(&mut self, order_id: u64) -> U64 {
        let order = self
            .orders
            .get(&order_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("no such order"));
        let market = order.market;

        // Expired?
        if slot() > order.expiry_block {
            self.orders.remove(&order_id);
            self.orders.flush();
            emit(
                "order_expired",
                serde_json::json!({ "id": order_id.to_string(), "market": market }),
            );
            env::panic_str("order expired");
        }

        let mut info = self.market(market);

        // Invariant 3: no stale fills (observation must be newer than request).
        require!(
            info.observation.block > order.request_block,
            "observation not newer than request block"
        );

        // 1.2: stale ladder
        let age_ns = env::block_timestamp().saturating_sub(info.observation.source_ts);
        let staleness = policy::observation_staleness(age_ns);
        match staleness {
            StalenessLevel::PriceUncertain => env::panic_str("price uncertain: no fills"),
            StalenessLevel::ReduceOnly | StalenessLevel::NoOpens => {
                require!(order.reduce_only, "stale observation: reduce-only");
            }
            StalenessLevel::Fresh => {}
        }

        // Load engine state and catch up.
        let mut g = self.load_group(market);
        let pool_acct = self.load_pool(market);
        let mut meta = info.meta();
        ok(exchange::catch_up(&mut g, &mut meta, &pool_acct, slot()));
        info.set_meta(meta);

        let mut p = self.load_portfolio(market, &order.owner);
        let mut pool = self.load_pool(market);

        // Reduce-only validation: the fill must shrink the user's position.
        let size_q_signed: i128 = if order.side == OrderSide::Long {
            order.size_q as i128
        } else {
            -(order.size_q as i128)
        };
        if order.reduce_only {
            let pos = ok(exchange::effective_position_q(&g, &p));
            let reduces = if size_q_signed > 0 {
                pos < 0 && size_q_signed <= pos.unsigned_abs() as i128
            } else {
                pos > 0 && size_q_signed.unsigned_abs() <= pos as u128
            };
            require!(reduces, "order does not reduce position");
        }

        // 1.5: OI cap enforcement
        if !order.reduce_only {
            let asset = ok(g.asset());
            if order.side == OrderSide::Long {
                require!(
                    policy::oi_within_cap(
                        asset.oi_eff_long_q.saturating_add(order.size_q),
                        info.oi_cap_long,
                    ),
                    "long OI cap exceeded"
                );
            } else {
                require!(
                    policy::oi_within_cap(
                        asset.oi_eff_short_q.saturating_add(order.size_q),
                        info.oi_cap_short,
                    ),
                    "short OI cap exceeded"
                );
            }
            if info.per_account_cap != 0 {
                let pos = ok(exchange::effective_position_q(&g, &p));
                let new_abs = (pos + size_q_signed).unsigned_abs();
                require!(
                    policy::oi_within_cap(new_abs, info.per_account_cap),
                    "per-account cap exceeded"
                );
            }
        }

        // 1.5: dynamic spread
        let spread = self.compute_spread(&g, &info, &order);

        // Execute the fill.
        let fill = ok(exchange::trade_against_pool(
            &mut g,
            &info.meta(),
            &mut p,
            &mut pool,
            size_q_signed,
            order.limit_price,
            spread,
        ));

        // 1.8: accrue trading fee split.
        self.accrue_fee_split(market, &info, fill.fee);

        self.store_group(market, &g);
        self.store_portfolio(market, &order.owner, &p);
        self.store_pool(market, &pool);
        self.save_market(market, &mut info);
        self.orders.remove(&order_id);
        self.orders.flush();

        emit(
            "fill",
            serde_json::json!({
                "market": market,
                "owner": order.owner.label(),
                "order_id": order_id.to_string(),
                "size_q": fill.size_q.to_string(),
                "price": fill.exec_price.to_string(),
                "fee": fill.fee.to_string(),
            }),
        );
        U64(fill.exec_price)
    }

    /// Cancel a pending order. Only the owner may cancel.
    #[payable]
    pub fn cancel_order(&mut self, order_id: u64) {
        assert_one_yocto();
        let order = self
            .orders
            .get(&order_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("no such order"));
        require!(
            order.owner == Self::user(),
            "only the order owner can cancel"
        );
        self.orders.remove(&order_id);
        self.orders.flush();
        emit(
            "order_cancelled",
            serde_json::json!({
                "id": order_id.to_string(),
                "market": order.market,
            }),
        );
    }

    // ─────────────────────── 1.3: NEAR pool reader ───────────────────────

    /// Permissionless: fetch the pool price for a `NearPool` market.
    /// The promise callback (`on_price_read`) writes the observation.
    pub fn refresh_price(&mut self, market: u32) -> Promise {
        let info = self.market(market);
        match &info.price_source {
            PriceSource::NearPool { dex, pool_id, .. } => {
                let dex = dex.clone();
                let pool_id = *pool_id;
                ext_ref::ext(dex)
                    .with_static_gas(GAS_GET_POOL)
                    .get_pool(pool_id)
                    .then(
                        Self::ext(env::current_account_id())
                            .with_static_gas(GAS_ON_PRICE_READ)
                            .on_price_read(market),
                    )
            }
            _ => env::panic_str("only NearPool markets use refresh_price"),
        }
    }

    /// Callback: compute price from the Ref Finance pool, write observation and
    /// EMA. Only writes state — never mutates the engine (atomicity rule).
    #[private]
    pub fn on_price_read(&mut self, market: u32) {
        let result = match env::promise_result(0) {
            PromiseResult::Successful(b) => b,
            _ => return, // RPC failure: leave market in current state
        };
        let pool_json: serde_json::Value = serde_json::from_slice(&result)
            .unwrap_or_else(|_| env::panic_str("bad pool json"));
        let amounts = pool_json["amounts"].as_array();
        let token_ids = pool_json["token_account_ids"].as_array();
        let (amounts, token_ids) = match (amounts, token_ids) {
            (Some(a), Some(t)) if a.len() >= 2 && t.len() >= 2 => (a, t),
            _ => return,
        };
        // Identify which side is our quote token (USDC).
        let qt = self.quote_token.to_string();
        let (usdc_atoms, token_atoms) =
            if token_ids[0].as_str() == Some(&qt) {
                let u: u128 = amounts[0].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                let t: u128 = amounts[1].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                (u, t)
            } else if token_ids[1].as_str() == Some(&qt) {
                let u: u128 = amounts[1].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                let t: u128 = amounts[0].as_str().and_then(|s| s.parse().ok()).unwrap_or(0);
                (u, t)
            } else {
                return; // pool doesn't contain our quote token
            };
        if usdc_atoms == 0 || token_atoms == 0 {
            return;
        }
        // price_e6 = usdc_atoms * 10^base_decimals / token_atoms
        // Assuming 6-decimal tokens for MVP; the base_decimals field can be
        // added to PriceSource::NearPool when tokens with other decimals land.
        let price_e6 = match u64::try_from(usdc_atoms * 1_000_000 / token_atoms) {
            Ok(p) if p > 0 && p <= policy::MAX_PRICE => p,
            _ => return,
        };
        let liquidity_usd = usdc_atoms; // conservative: quote-side reserve only

        let mut info = self.market(market);
        // Band check: reject if the price moved more than 20% from EMA.
        if info.ema_e6 > 0 {
            let diff = if price_e6 > info.ema_e6 {
                price_e6 - info.ema_e6
            } else {
                info.ema_e6 - price_e6
            };
            if diff as u128 * policy::BPS / info.ema_e6 as u128 > 2_000 {
                // Too far from EMA → reduce-only mode (spec: move beyond band).
                if info.mode == Mode::Active {
                    info.mode = Mode::ReduceOnly;
                    self.save_market(market, &mut info);
                    emit(
                        "market_mode_changed",
                        serde_json::json!({
                            "market": market,
                            "mode": "ReduceOnly",
                            "reason": "band_exceeded",
                        }),
                    );
                }
                return;
            }
        }

        info.ema_e6 = policy::ema_next(info.ema_e6, price_e6).unwrap_or(price_e6);
        info.observation = Observation {
            price_e6,
            liquidity_usd,
            source_ts: env::block_timestamp(),
            seq: info.observation.seq + 1,
            block: slot(),
        };
        // For NearPool markets the staircase target is the EMA (smoothed).
        let mut g = self.load_group(market);
        let pool = self.load_pool(market);
        let mut meta = info.meta();
        meta.target = info.ema_e6;
        meta.last_publish_ns = env::block_timestamp();
        ok(exchange::catch_up(&mut g, &mut meta, &pool, slot()));
        info.set_meta(meta);
        self.store_group(market, &g);
        self.save_market(market, &mut info);
        emit(
            "price_observed",
            serde_json::json!({
                "market": market,
                "price_e6": price_e6.to_string(),
                "ema_e6": info.ema_e6.to_string(),
                "liquidity_usd": liquidity_usd.to_string(),
                "seq": info.observation.seq.to_string(),
            }),
        );
    }

    // ─────────────────────── 1.4: permissionless markets ──────────────────

    /// Permissionless: activate a `Proposed` market after its first good
    /// observation (for NearPool: call `refresh_price` first).
    pub fn activate_market(&mut self, market: u32) {
        let mut info = self.market(market);
        require!(info.mode == Mode::Proposed, "not proposed");
        require!(info.observation.seq > 0, "no observation yet");
        let age_ns = env::block_timestamp().saturating_sub(info.observation.source_ts);
        require!(
            policy::observation_staleness(age_ns) == StalenessLevel::Fresh,
            "observation not fresh"
        );
        let depth_floor = match info.tier {
            Tier::Major => DEPTH_FLOOR_MAJOR,
            Tier::LongTail => DEPTH_FLOOR_LONG_TAIL,
        };
        require!(
            info.observation.liquidity_usd >= depth_floor,
            "insufficient pool depth"
        );
        info.mode = Mode::Active;
        self.save_market(market, &mut info);
        emit(
            "market_activated",
            serde_json::json!({
                "market": market,
                "symbol": info.symbol,
            }),
        );
    }

    // ─────────────────────── 1.5: quote view ───────────────────────

    /// View: preview the execution price, spread and fee for a hypothetical order.
    pub fn quote(&self, market: u32, side: OrderSide, size_q: U128) -> (U64, U64, U64) {
        let info = self.market(market);
        let g = self.load_group(market);
        let asset = ok(g.asset());
        let effective = asset.effective_price;

        let dummy_order = Order {
            id: 0,
            market,
            owner: Owner::Near("x.near".parse().unwrap()),
            side,
            size_q: size_q.0,
            limit_price: if side == OrderSide::Long { u64::MAX } else { 1 },
            request_block: 0,
            expiry_block: 0,
            reduce_only: false,
        };
        let spread = self.compute_spread(&g, &info, &dummy_order);
        let taker_buys = side == OrderSide::Long;
        let exec_price = policy::pool_exec_price(effective, spread, taker_buys).unwrap_or(0);
        let fee_bps = ok(g.config()).max_trading_fee_bps;
        let notional = (size_q.0 as u128)
            .saturating_mul(exec_price as u128)
            / POS_SCALE;
        let fee = notional * fee_bps as u128 / policy::BPS;
        (U64(exec_price), U64(spread), U64(fee as u64))
    }

    // ─────────────────────── 1.7: conditional orders ───────────────────────

    /// Place a conditional order (TP, SL or resting limit). Storage is charged
    /// to the caller. TP and SL are always reduce-only.
    #[payable]
    pub fn place_conditional(
        &mut self,
        market: u32,
        kind: ConditionalKind,
        side: OrderSide,
        size_q: U128,
        trigger_price: U64,
        limit_price: U64,
        expiry_blocks: u64,
        reduce_only: bool,
    ) -> U64 {
        require!(
            env::attached_deposit() >= NearToken::from_yoctonear(1),
            "attach at least 1 yoctoNEAR (storage)"
        );
        let storage_start = env::storage_usage();
        let who = Self::user();
        require!(size_q.0 > 0, "zero size");
        require!(trigger_price.0 > 0, "zero trigger");
        require!(
            expiry_blocks > 0 && expiry_blocks <= MAX_CONDITIONAL_EXPIRY_BLOCKS,
            "bad expiry"
        );
        let info = self.market(market);
        require!(
            info.mode == Mode::Active || info.mode == Mode::ReduceOnly,
            "market not active"
        );
        require!(
            self.portfolios.contains_key(&(market, who.clone())),
            "no account in this market"
        );
        // TP and SL are always reduce-only.
        let reduce_only = match kind {
            ConditionalKind::TakeProfit | ConditionalKind::StopLoss => true,
            ConditionalKind::Limit => reduce_only,
        };

        let id = self.next_order_id;
        self.next_order_id += 1;
        let cond = ConditionalOrder {
            id,
            market,
            owner: who.clone(),
            kind,
            side,
            size_q: size_q.0,
            trigger_price: trigger_price.0,
            limit_price: limit_price.0,
            arm_block: slot(),
            expiry_block: slot() + expiry_blocks,
            reduce_only,
        };
        self.conditionals.insert(id, cond);
        self.conditionals.flush();
        self.flush();
        self.charge_storage_after_yocto(storage_start);
        emit(
            "conditional_placed",
            serde_json::json!({
                "id": id.to_string(),
                "market": market,
                "owner": who.label(),
                "kind": format!("{:?}", kind),
                "side": format!("{:?}", side),
                "size_q": size_q.0.to_string(),
                "trigger_price": trigger_price.0.to_string(),
                "reduce_only": reduce_only,
            }),
        );
        U64(id)
    }

    /// Execute a triggered conditional order. Permissionless (keeper).
    /// The mark (engine effective price) must have crossed the trigger, and
    /// the observation must be from a block after the arm block.
    pub fn execute_conditional(&mut self, cond_id: u64) -> U64 {
        let cond = self
            .conditionals
            .get(&cond_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("no such conditional"));
        let market = cond.market;

        // Expired?
        if slot() > cond.expiry_block {
            self.conditionals.remove(&cond_id);
            self.conditionals.flush();
            emit(
                "order_expired",
                serde_json::json!({ "id": cond_id.to_string(), "market": market }),
            );
            env::panic_str("conditional expired");
        }

        let mut info = self.market(market);

        // Observation must be from after the arm block (invariant 3).
        require!(
            info.observation.block > cond.arm_block,
            "observation not newer than arm block"
        );

        // Check mark price has crossed the trigger.
        let (mut g, pool) = {
            let mut g = self.load_group(market);
            let pool = self.load_pool(market);
            let mut meta = info.meta();
            ok(exchange::catch_up(&mut g, &mut meta, &pool, slot()));
            info.set_meta(meta);
            (g, pool)
        };
        let mark = ok(g.asset()).effective_price;
        // `side` is the trade direction: Short = sell (closing a long), Long = buy (closing a short).
        let triggered = match cond.kind {
            ConditionalKind::TakeProfit => {
                if cond.side == OrderSide::Short {
                    mark >= cond.trigger_price // close long at high price
                } else {
                    mark <= cond.trigger_price // close short at low price
                }
            }
            ConditionalKind::StopLoss => {
                if cond.side == OrderSide::Short {
                    mark <= cond.trigger_price // stop long at low price
                } else {
                    mark >= cond.trigger_price // stop short at high price
                }
            }
            ConditionalKind::Limit => {
                if cond.side == OrderSide::Long {
                    mark <= cond.trigger_price // buy at low price
                } else {
                    mark >= cond.trigger_price // sell at high price
                }
            }
        };
        require!(triggered, "trigger not crossed");

        // Stale ladder
        let age_ns = env::block_timestamp().saturating_sub(info.observation.source_ts);
        let staleness = policy::observation_staleness(age_ns);
        match staleness {
            StalenessLevel::PriceUncertain => env::panic_str("price uncertain"),
            StalenessLevel::ReduceOnly | StalenessLevel::NoOpens => {
                require!(cond.reduce_only, "stale: reduce-only");
            }
            StalenessLevel::Fresh => {}
        }

        let mut p = self.load_portfolio(market, &cond.owner);
        let mut pool_p = self.load_pool(market);

        // Reduce-only validation.
        let size_q_signed: i128 = if cond.side == OrderSide::Long {
            cond.size_q as i128
        } else {
            -(cond.size_q as i128)
        };
        if cond.reduce_only {
            let pos = ok(exchange::effective_position_q(&g, &p));
            let reduces = if size_q_signed > 0 {
                pos < 0 && size_q_signed <= pos.unsigned_abs() as i128
            } else {
                pos > 0 && size_q_signed.unsigned_abs() <= pos as u128
            };
            require!(reduces, "conditional does not reduce position");
        }

        let spread = self.compute_spread(&g, &info, &Order {
            id: 0, market, owner: cond.owner.clone(), side: cond.side,
            size_q: cond.size_q, limit_price: cond.limit_price,
            request_block: 0, expiry_block: 0, reduce_only: cond.reduce_only,
        });

        let fill = ok(exchange::trade_against_pool(
            &mut g,
            &info.meta(),
            &mut p,
            &mut pool_p,
            size_q_signed,
            cond.limit_price,
            spread,
        ));

        // 1.8: accrue trading fee split.
        self.accrue_fee_split(market, &info, fill.fee);

        self.store_group(market, &g);
        self.store_portfolio(market, &cond.owner, &p);
        self.store_pool(market, &pool_p);
        self.save_market(market, &mut info);
        self.conditionals.remove(&cond_id);
        self.conditionals.flush();

        emit(
            "conditional_triggered",
            serde_json::json!({
                "id": cond_id.to_string(),
                "market": market,
                "owner": cond.owner.label(),
                "kind": format!("{:?}", cond.kind),
                "fill_price": fill.exec_price.to_string(),
                "size_q": fill.size_q.to_string(),
                "fee": fill.fee.to_string(),
            }),
        );
        U64(fill.exec_price)
    }

    /// Cancel a conditional order. Only the owner may cancel.
    #[payable]
    pub fn cancel_conditional(&mut self, cond_id: u64) {
        assert_one_yocto();
        let cond = self
            .conditionals
            .get(&cond_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("no such conditional"));
        require!(
            cond.owner == Self::user(),
            "only the conditional owner can cancel"
        );
        self.conditionals.remove(&cond_id);
        self.conditionals.flush();
        emit(
            "order_cancelled",
            serde_json::json!({
                "id": cond_id.to_string(),
                "market": cond.market,
            }),
        );
    }

    // ─────────────────────── 1.8: fees ───────────────────────

    /// Claim accrued fees for the lister or protocol. The tokens come from the
    /// engine's insurance surplus (already deposited into the vault).
    #[payable]
    pub fn claim_fees(&mut self, market: u32, payee: Payee) -> U128 {
        assert_one_yocto();
        let who = Self::user();
        let info = self.market(market);

        // Only the lister and the protocol owner can claim their respective fees.
        match payee {
            Payee::Lister => {
                require!(
                    Owner::Near(env::predecessor_account_id()) == info.lister,
                    "only the lister"
                );
            }
            Payee::Protocol => self.assert_owner(),
            // LP and Insurance fees stay in the pool/insurance fund.
            _ => env::panic_str("LP and insurance fees are not claimable directly"),
        }

        let key = (market, payee);
        let accrued = self.fee_ledgers.get(&key).copied().unwrap_or(0);
        if accrued == 0 {
            return U128(0);
        }
        self.fee_ledgers.insert(key, 0);
        self.fee_ledgers.flush();

        // Move from vault to wallet.
        self.vault_out_to_wallet(&who, accrued);

        emit(
            "fees_claimed",
            serde_json::json!({
                "market": market,
                "payee": format!("{:?}", payee),
                "amount": accrued.to_string(),
                "owner": who.label(),
            }),
        );
        U128(accrued)
    }

    /// View: get the fee ledger for a market + payee.
    pub fn get_fee_ledger(&self, market: u32, payee: Payee) -> U128 {
        U128(self.fee_ledgers.get(&(market, payee)).copied().unwrap_or(0))
    }

    // ─────────────────────── 1.9: admin roles ───────────────────────

    /// Admin: set OI caps for a market.
    pub fn set_oi_caps(
        &mut self,
        market: u32,
        oi_cap_long: U128,
        oi_cap_short: U128,
        per_account_cap: U128,
    ) {
        self.assert_owner();
        let mut info = self.market(market);
        info.oi_cap_long = oi_cap_long.0;
        info.oi_cap_short = oi_cap_short.0;
        info.per_account_cap = per_account_cap.0;
        self.save_market(market, &mut info);
    }

    /// Pauser or owner: tighten market mode. Can only move to a MORE
    /// restrictive mode (invariant 6: no fund access).
    pub fn set_mode(&mut self, market: u32, new_mode: Mode) {
        self.assert_pauser_or_owner();
        require!(
            new_mode == Mode::ReduceOnly || new_mode == Mode::PriceUncertain,
            "can only tighten to ReduceOnly or PriceUncertain"
        );
        let mut info = self.market(market);
        // Tighten-only: Active → ReduceOnly → PriceUncertain. Never loosen.
        let tightens = match (info.mode, new_mode) {
            (Mode::Active, Mode::ReduceOnly | Mode::PriceUncertain) => true,
            (Mode::ReduceOnly, Mode::PriceUncertain) => true,
            _ => false,
        };
        require!(tightens, "can only tighten mode");
        info.mode = new_mode;
        self.save_market(market, &mut info);
        emit(
            "market_mode_changed",
            serde_json::json!({
                "market": market,
                "mode": format!("{:?}", new_mode),
                "reason": "admin",
            }),
        );
    }

    /// Owner: set the pauser account.
    pub fn set_pauser(&mut self, pauser: Option<AccountId>) {
        self.assert_owner();
        self.pauser = pauser;
    }

    /// Owner: transfer ownership.
    pub fn set_owner(&mut self, owner: AccountId) {
        self.assert_owner();
        self.owner = owner;
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
        // Update EMA (step 1.2).
        info.ema_e6 = policy::ema_next(info.ema_e6, price.0).unwrap_or(price.0);
        let mut g = self.load_group(market);
        let pool = self.load_pool(market);
        let mut meta = info.meta();
        // For Pushed sources the raw price is the target; for NearPool markets
        // the EMA is the target (set in on_price_read).
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
        emit(
            "price_observed",
            serde_json::json!({
                "market": market,
                "price_e6": price.0.to_string(),
                "ema_e6": info.ema_e6.to_string(),
                "seq": info.observation.seq.to_string(),
            }),
        );
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
    /// Pays the caller 60% of the liquidation fee, 40% to insurance (spec §5).
    pub fn liquidate(&mut self, market: u32, account: AccountId) -> U128 {
        let caller = Self::user();
        let account = Owner::Near(account);
        let (mut g, mut info, _pool) = self.load_market_caught_up(market);
        let mut p = self.load_portfolio(market, &account);
        let out = ok(exchange::liquidate(&mut g, &mut p));
        self.store_group(market, &g);
        self.store_portfolio(market, &account, &p);
        self.save_market(market, &mut info);

        // 1.8: liquidation fee split. The engine already deducted the fee from
        // the liquidated account. We pay the caller 60% from the vault.
        let liq_fee = out.fee_charged;
        if liq_fee > 0 {
            let caller_share = liq_fee * LIQ_CALLER_BPS / policy::BPS;
            // 40% stays in the engine's insurance (already there).
            if caller_share > 0 {
                self.vault_out_to_wallet(&caller, caller_share);
            }
        }

        emit(
            "liquidation",
            serde_json::json!({
                "market": market,
                "account": account.label(),
                "caller": caller.label(),
                "closed_q": out.closed_q.to_string(),
                "fee": liq_fee.to_string(),
            }),
        );
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
    /// 1.5: compute the dynamic spread for an order against the current OI state.
    fn compute_spread(&self, g: &Group, info: &Market, order: &Order) -> u64 {
        let asset = match g.asset() {
            Ok(a) => a,
            Err(_) => return info.spread_bps, // fallback to fixed
        };
        let oi_long = asset.oi_eff_long_q;
        let oi_short = asset.oi_eff_short_q;

        // If OI caps are uncapped (0), fall back to fixed spread.
        let oi_cap = info.oi_cap_long.max(info.oi_cap_short);
        if oi_cap == 0 {
            return info.spread_bps;
        }

        let oi_long_after = if order.side == OrderSide::Long {
            oi_long.saturating_add(order.size_q)
        } else {
            oi_long
        };
        let oi_short_after = if order.side == OrderSide::Short {
            oi_short.saturating_add(order.size_q)
        } else {
            oi_short
        };
        let net_oi_after = oi_long_after.abs_diff(oi_short_after);

        let (spread_min, k_skew, k_size) = match info.tier {
            Tier::Major => (MAJOR_SPREAD_MIN_BPS, MAJOR_K_SKEW_BPS, MAJOR_K_SIZE_BPS),
            Tier::LongTail => (LONGTAIL_SPREAD_MIN_BPS, LONGTAIL_K_SKEW_BPS, LONGTAIL_K_SIZE_BPS),
        };
        policy::dynamic_spread_bps(spread_min, k_skew, k_size, net_oi_after, oi_cap, order.size_q)
    }

    /// 1.8: accrue the trading fee split into the per-market fee ledgers.
    /// The engine already charged the fee from the taker; this just records
    /// how to distribute it when `claim_fees` is called.
    fn accrue_fee_split(&mut self, market: u32, _info: &Market, fee: u128) {
        if fee == 0 {
            return;
        }
        let lister_share = fee * FEE_LISTER_BPS / policy::BPS;
        let lp_share = fee * FEE_LP_BPS / policy::BPS;
        let insurance_share = fee * FEE_INSURANCE_BPS / policy::BPS;
        // Protocol gets the remainder (rounding dust goes here, invariant 8).
        let protocol_share = fee - lister_share - lp_share - insurance_share;

        for (payee, amount) in [
            (Payee::Lister, lister_share),
            (Payee::Lp, lp_share),
            (Payee::Insurance, insurance_share),
            (Payee::Protocol, protocol_share),
        ] {
            if amount > 0 {
                let key = (market, payee);
                let prev = self.fee_ledgers.get(&key).copied().unwrap_or(0);
                self.fee_ledgers.insert(key, prev + amount);
            }
        }
        self.fee_ledgers.flush();
    }

    /// 1.9: check that the caller is the pauser or the owner.
    fn assert_pauser_or_owner(&self) {
        let caller = env::predecessor_account_id();
        let is_pauser = self.pauser.as_ref().map_or(false, |p| p == &caller);
        require!(
            caller == self.owner || is_pauser,
            "only the owner or pauser"
        );
    }

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
        self.orders.flush();
        self.conditionals.flush();
        self.fee_ledgers.flush();
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
