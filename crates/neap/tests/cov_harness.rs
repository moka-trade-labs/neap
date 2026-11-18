//! Shared harness for the `cov_*` coverage tests (no tests of its own).
//!
//! Other files include it with `#[path = "cov_harness.rs"] mod h;`.
//!
//! What it gives:
//! - `World`: the contract in the near-sdk mocked runtime plus a *shadow* of
//!   everything the contract cannot see or does not sum itself: the USDC
//!   balance the token contract would report for `neap`, fees charged by fills,
//!   fees claimed, known accounts.
//! - `Act`: one public-method call (or one receipt chain, e.g. `withdraw` +
//!   `on_withdraw`) as plain data, so a scenario is a `Vec<Act>` and can be
//!   replayed. The mocked runtime does not roll state back on panic, so
//!   `Fuzz::try_step` checkpoints the contract (root struct + mocked storage)
//!   before each act and restores it when the act is refused, like a reverted
//!   receipt.
//! - `World::check`: the invariants of docs/contract.md §6 that are checkable
//!   from the public surface, asserted after EVERY act:
//!     inv 1 custody   token balance == custody_wallets + custody_vaults,
//!                     custody_wallets == Σ wallet ledger,
//!                     custody_vaults == Σ engine vaults,
//!                     engine vault >= Σ capital + insurance (per market)
//!     inv 2 isolation an act on market m leaves every other market's group,
//!                     portfolios, LP shares and fee ledgers untouched
//!     inv 6 admin     admin acts leave every balance untouched
//!     inv 8 fees      Σ ledgers + Σ claimed == Σ fees charged by fills
//!     LP              Σ lp shares == lp_total_shares

#![allow(dead_code)]

use std::cell::Cell;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Once;

use neap::model::{ConditionalKind, Mode, OrderSide, Owner, Payee};
use neap::{MarketParamsJson, Neap};
use near_sdk::json_types::{U128, U64};
use near_sdk::serde_json;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken, PromiseOrValue, PromiseResult};

pub use percolator::POS_SCALE;

pub const USDC: u128 = 1_000_000;
pub const ONE: NearToken = NearToken::from_yoctonear(1);
pub const ZERO: NearToken = NearToken::from_yoctonear(0);
pub const STORAGE: NearToken = NearToken::from_millinear(200);
pub const BLOCK_NS: u64 = 600_000_000;

pub const PAYEES: [Payee; 4] = [Payee::Lister, Payee::Lp, Payee::Insurance, Payee::Protocol];

pub fn contract_id() -> AccountId {
    "neap.near".parse().unwrap()
}
pub fn usdc() -> AccountId {
    "usdc.near".parse().unwrap()
}
pub fn oracle() -> AccountId {
    "oracle.near".parse().unwrap()
}

/// User indices into `USERS`.
pub const OWNER: usize = 0;
pub const LP1: usize = 1;
pub const LP2: usize = 2;
pub const T1: usize = 3;
pub const T2: usize = 4;
pub const T3: usize = 5;
pub const KEEPER: usize = 6;
pub const LISTER: usize = 7;
pub const USERS: [&str; 8] = [
    "owner.near",
    "lp1.near",
    "lp2.near",
    "t1.near",
    "t2.near",
    "t3.near",
    "keeper.near",
    "lister.near",
];

pub fn user(i: usize) -> AccountId {
    USERS[i].parse().unwrap()
}

/// Same risk parameters as `contract_unit.rs`.
pub fn params() -> MarketParamsJson {
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

/// JSON `msg` for a permissionless `CreateMarket` bond transfer.
pub fn create_market_msg(symbol: &str, tier: &str) -> String {
    let p = serde_json::to_value(params()).unwrap();
    serde_json::json!({
        "action": "CreateMarket",
        "symbol": symbol,
        "tier": tier,
        "price_source": { "NearPool": { "dex": "ref.near", "pool_id": 7, "quote_pool": null } },
        "spread_bps": 20,
        "max_oracle_age_ns": "60000000000",
        "params": p,
    })
    .to_string()
}

/// One public call (or one receipt chain) as replayable data.
#[derive(Clone, Debug)]
pub enum Act {
    Tick(u64),
    Register(usize),
    /// `ft_on_transfer` from the USDC contract. The shadow token balance grows
    /// by `amount - refund`.
    FtIn {
        who: usize,
        amount: u128,
        msg: String,
    },
    OpenAccount {
        who: usize,
        m: u32,
    },
    Deposit {
        who: usize,
        m: u32,
        amount: u128,
    },
    WithdrawMargin {
        who: usize,
        m: u32,
        amount: u128,
    },
    Request {
        who: usize,
        m: u32,
        long: bool,
        size_q: u128,
        limit: u64,
        expiry: u64,
        reduce_only: bool,
    },
    Execute {
        order: u64,
        by: usize,
    },
    Cancel {
        order: u64,
        who: usize,
    },
    /// kind: 0 = TakeProfit, 1 = StopLoss, 2 = Limit.
    PlaceCond {
        who: usize,
        m: u32,
        kind: u8,
        long: bool,
        size_q: u128,
        trigger: u64,
        limit: u64,
        expiry: u64,
        reduce_only: bool,
    },
    ExecCond {
        cond: u64,
        by: usize,
    },
    CancelCond {
        cond: u64,
        who: usize,
    },
    /// Oracle push with `publish_ns = now - age_ns`.
    Push {
        m: u32,
        price: u64,
        age_ns: u64,
    },
    Crank {
        m: u32,
    },
    /// tick(1) + crank until the engine price reaches the target (at most `max`).
    Walk {
        m: u32,
        max: u32,
    },
    KeeperStep {
        m: u32,
        who: usize,
        by: usize,
    },
    Liquidate {
        m: u32,
        who: usize,
        by: usize,
    },
    Convert {
        who: usize,
        m: u32,
    },
    LpDeposit {
        who: usize,
        m: u32,
        amount: u128,
    },
    LpRedeem {
        who: usize,
        m: u32,
        shares: u128,
    },
    Insurance {
        who: usize,
        m: u32,
        amount: u128,
    },
    /// Claimed by the market's lister (`protocol == false`) or by the owner.
    ClaimFees {
        m: u32,
        protocol: bool,
    },
    /// `withdraw` then the `on_withdraw` callback with the given transfer result.
    Withdraw {
        who: usize,
        amount: u128,
        transfer_ok: bool,
    },
    // ── admin ──
    SetOiCaps {
        m: u32,
        long: u128,
        short: u128,
        acct: u128,
    },
    SetMode {
        m: u32,
        uncertain: bool,
    },
    SetPauser(Option<usize>),
    SetOracle,
    SetOwner,
    CreateMarketAdmin {
        price: u64,
    },
}

impl Act {
    /// The one market this act is allowed to touch (invariant 2).
    pub fn market(&self) -> Option<u32> {
        use Act::*;
        match self {
            OpenAccount { m, .. }
            | Deposit { m, .. }
            | WithdrawMargin { m, .. }
            | Request { m, .. }
            | PlaceCond { m, .. }
            | Push { m, .. }
            | Crank { m }
            | Walk { m, .. }
            | KeeperStep { m, .. }
            | Liquidate { m, .. }
            | Convert { m, .. }
            | LpDeposit { m, .. }
            | LpRedeem { m, .. }
            | Insurance { m, .. }
            | ClaimFees { m, .. }
            | SetOiCaps { m, .. }
            | SetMode { m, .. } => Some(*m),
            _ => None,
        }
    }

    pub fn is_admin(&self) -> bool {
        use Act::*;
        matches!(
            self,
            SetOiCaps { .. }
                | SetMode { .. }
                | SetPauser(_)
                | SetOracle
                | SetOwner
                | CreateMarketAdmin { .. }
        )
    }
}

pub struct World {
    pub height: u64,
    pub ts: u64,
    pub c: Neap,
    /// What `usdc.ft_balance_of(neap)` would be.
    pub token: u128,
    /// Trading fees charged by fills, per market (from `fill` /
    /// `conditional_triggered` events).
    pub fees_charged: Vec<u128>,
    /// Fees paid out by `claim_fees`, per market.
    pub fees_claimed: Vec<u128>,
    /// Liquidation fee reported by `liquidation` events, per market.
    pub liq_fees: Vec<u128>,
    pub liquidations: u64,
    pub fills: u64,
    /// Order / conditional ids handed out so far (may be gone already).
    pub orders: Vec<u64>,
    pub conds: Vec<u64>,
    /// Successful acts, in order.
    pub log: Vec<Act>,
}

impl World {
    /// Fresh contract with two owner-listed `Pushed` markets:
    /// market 0 at `p0`, market 1 at `p1`. No balances.
    pub fn new(p0: u64, p1: u64) -> World {
        let mut w = World {
            height: 100,
            ts: 1_000_000_000_000,
            // Placeholder, replaced below once the runtime is mocked.
            c: {
                // Start from EMPTY mocked storage: a previous world in this
                // thread may have flushed its state.
                testing_env!(VMContextBuilder::new().build());
                near_sdk::mock::with_mocked_blockchain(|b| b.take_storage());
                Self::ctx(
                    100,
                    1_000_000_000_000,
                    &user(OWNER),
                    NearToken::from_near(1),
                    vec![],
                );
                Neap::new(user(OWNER), usdc(), oracle())
            },
            token: 0,
            fees_charged: vec![],
            fees_claimed: vec![],
            liq_fees: vec![],
            liquidations: 0,
            fills: 0,
            orders: vec![],
            conds: vec![],
            log: vec![],
        };
        for (sym, px) in [("SOL-PERP", p0), ("NEAR-PERP", p1)] {
            w.call(OWNER, NearToken::from_near(1));
            w.c.create_market(sym.into(), U64(px), params(), 5, U64(60_000_000_000));
        }
        w.sync_markets();
        w.check("genesis");
        w
    }

    /// Rebuild a world by replaying a log of acts that all succeeded before.
    pub fn replay(p0: u64, p1: u64, log: &[Act]) -> World {
        let mut w = World::new(p0, p1);
        for a in log {
            w.apply(a);
            w.log.push(a.clone());
        }
        w
    }

    fn ctx(height: u64, ts: u64, who: &AccountId, deposit: NearToken, results: Vec<PromiseResult>) {
        let mut b = VMContextBuilder::new();
        b.current_account_id(contract_id())
            .predecessor_account_id(who.clone())
            .signer_account_id(who.clone())
            .attached_deposit(deposit)
            .account_balance(NearToken::from_near(1_000))
            .block_height(height)
            .block_timestamp(ts);
        testing_env!(
            b.build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            results
        );
    }

    pub fn call(&mut self, who: usize, deposit: NearToken) {
        Self::ctx(self.height, self.ts, &user(who), deposit, vec![]);
    }

    pub fn call_as(&mut self, who: &AccountId, deposit: NearToken) {
        Self::ctx(self.height, self.ts, who, deposit, vec![]);
    }

    /// Context for a `#[private]` callback with one promise result.
    pub fn callback(&mut self, result: PromiseResult) {
        Self::ctx(self.height, self.ts, &contract_id(), ZERO, vec![result]);
    }

    pub fn tick(&mut self, n: u64) {
        self.height += n;
        self.ts += n * BLOCK_NS;
    }

    pub fn n_markets(&self) -> u32 {
        self.c.get_markets_count()
    }

    fn sync_markets(&mut self) {
        let n = self.n_markets() as usize;
        self.fees_charged.resize(n, 0);
        self.fees_claimed.resize(n, 0);
        self.liq_fees.resize(n, 0);
    }

    /// `data[0][field]` of the last NEP-297 event named `event` in this call's logs.
    pub fn event_field(event: &str, field: &str) -> Option<String> {
        for l in get_logs().iter().rev() {
            let Some(js) = l.strip_prefix("EVENT_JSON:") else {
                continue;
            };
            let v: serde_json::Value = serde_json::from_str(js).expect("event is JSON");
            if v["event"] == event {
                return Some(match &v["data"][0][field] {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
            }
        }
        None
    }

    pub fn has_event(event: &str) -> bool {
        get_logs()
            .iter()
            .any(|l| l.contains(&format!("\"event\":\"{event}\"")))
    }

    pub fn market_lister(&self, m: u32) -> AccountId {
        match self.c.get_market(m).lister {
            Owner::Near(a) => a,
            Owner::Sol(_) => panic!("sol lister"),
        }
    }

    pub fn effective(&self, m: u32) -> u64 {
        self.c.get_market(m).effective_price.0
    }

    pub fn target(&self, m: u32) -> u64 {
        self.c.get_market(m).oracle_target.0
    }

    pub fn wallet(&self, who: usize) -> u128 {
        self.c.get_wallet(user(who)).0
    }

    pub fn position(&self, m: u32, who: usize) -> i128 {
        self.c
            .get_account(m, user(who))
            .map(|a| a.position_q.0)
            .unwrap_or(0)
    }

    pub fn capital(&self, m: u32, who: usize) -> u128 {
        self.c
            .get_account(m, user(who))
            .map(|a| a.capital.0)
            .unwrap_or(0)
    }

    /// The one market `a` may touch, resolving order ids to their market.
    pub fn act_market(&self, a: &Act) -> Option<u32> {
        match a {
            Act::Execute { order, .. } | Act::Cancel { order, .. } => {
                self.c.get_order(*order).map(|o| o.market)
            }
            Act::ExecCond { cond, .. } | Act::CancelCond { cond, .. } => {
                self.c.get_conditional(*cond).map(|o| o.market)
            }
            _ => a.market(),
        }
    }

    /// Run one act. Panics (like the contract would) if it is refused.
    /// Does NOT check invariants; use `step`.
    pub fn apply(&mut self, a: &Act) {
        match a.clone() {
            Act::Tick(n) => self.tick(n),
            Act::Register(who) => {
                self.call(who, STORAGE);
                self.c.register();
            }
            Act::FtIn { who, amount, msg } => {
                self.call_as(&usdc(), ZERO);
                match self.c.ft_on_transfer(user(who), U128(amount), msg) {
                    PromiseOrValue::Value(refund) => {
                        assert!(refund.0 <= amount, "refund above the transfer");
                        self.token += amount - refund.0;
                    }
                    _ => panic!("ft_on_transfer returned a promise"),
                }
                self.sync_markets();
            }
            Act::OpenAccount { who, m } => {
                self.call(who, STORAGE);
                self.c.open_account(m);
            }
            Act::Deposit { who, m, amount } => {
                self.call(who, ONE);
                self.c.deposit(m, U128(amount));
            }
            Act::WithdrawMargin { who, m, amount } => {
                self.call(who, ONE);
                self.c.withdraw_margin(m, U128(amount));
            }
            Act::Request {
                who,
                m,
                long,
                size_q,
                limit,
                expiry,
                reduce_only,
            } => {
                self.call(who, ONE);
                let side = if long {
                    OrderSide::Long
                } else {
                    OrderSide::Short
                };
                let id =
                    self.c
                        .request_order(m, side, U128(size_q), U64(limit), expiry, reduce_only);
                self.orders.push(id.0);
            }
            Act::Execute { order, by } => {
                let m = self.c.get_order(order).map(|o| o.market);
                self.call(by, ZERO);
                let px = self.c.execute_order(order);
                if px.0 > 0 {
                    let fee: u128 = Self::event_field("fill", "fee")
                        .expect("fill event")
                        .parse()
                        .unwrap();
                    self.fees_charged[m.unwrap() as usize] += fee;
                    self.fills += 1;
                }
                assert!(
                    self.c.get_order(order).is_none(),
                    "executed/expired order must be gone"
                );
            }
            Act::Cancel { order, who } => {
                self.call(who, ONE);
                self.c.cancel_order(order);
            }
            Act::PlaceCond {
                who,
                m,
                kind,
                long,
                size_q,
                trigger,
                limit,
                expiry,
                reduce_only,
            } => {
                self.call(who, STORAGE);
                let kind = match kind {
                    0 => ConditionalKind::TakeProfit,
                    1 => ConditionalKind::StopLoss,
                    _ => ConditionalKind::Limit,
                };
                let side = if long {
                    OrderSide::Long
                } else {
                    OrderSide::Short
                };
                let id = self.c.place_conditional(
                    m,
                    kind,
                    side,
                    U128(size_q),
                    U64(trigger),
                    U64(limit),
                    expiry,
                    reduce_only,
                );
                self.conds.push(id.0);
            }
            Act::ExecCond { cond, by } => {
                let m = self.c.get_conditional(cond).map(|o| o.market);
                self.call(by, ZERO);
                let px = self.c.execute_conditional(cond);
                if px.0 > 0 {
                    let fee: u128 = Self::event_field("conditional_triggered", "fee")
                        .expect("conditional_triggered event")
                        .parse()
                        .unwrap();
                    self.fees_charged[m.unwrap() as usize] += fee;
                    self.fills += 1;
                }
                assert!(
                    self.c.get_conditional(cond).is_none(),
                    "executed/expired conditional must be gone"
                );
            }
            Act::CancelCond { cond, who } => {
                self.call(who, ONE);
                self.c.cancel_conditional(cond);
            }
            Act::Push { m, price, age_ns } => {
                self.call_as(&oracle(), ZERO);
                self.c.push_price(m, U64(price), U64(self.ts - age_ns));
            }
            Act::Crank { m } => {
                self.call(KEEPER, ZERO);
                self.c.crank(m);
            }
            Act::Walk { m, max } => {
                for _ in 0..max {
                    if self.effective(m) == self.target(m) {
                        break;
                    }
                    self.tick(1);
                    self.call(KEEPER, ZERO);
                    self.c.crank(m);
                }
            }
            Act::KeeperStep { m, who, by } => {
                self.call(by, ZERO);
                self.c.keeper_step(m, user(who));
            }
            Act::Liquidate { m, who, by } => {
                self.call(by, ZERO);
                let closed = self.c.liquidate(m, user(who));
                let fee: u128 = Self::event_field("liquidation", "fee")
                    .expect("liquidation event")
                    .parse()
                    .unwrap();
                self.liq_fees[m as usize] += fee;
                if closed.0 > 0 {
                    self.liquidations += 1;
                }
            }
            Act::Convert { who, m } => {
                self.call(who, ONE);
                self.c.convert_pnl(m);
            }
            Act::LpDeposit { who, m, amount } => {
                self.call(who, STORAGE);
                self.c.lp_deposit(m, U128(amount));
            }
            Act::LpRedeem { who, m, shares } => {
                self.call(who, ONE);
                self.c.lp_redeem(m, U128(shares));
            }
            Act::Insurance { who, m, amount } => {
                self.call(who, ONE);
                self.c.insurance_deposit(m, U128(amount));
            }
            Act::ClaimFees { m, protocol } => {
                let (who, payee) = if protocol {
                    (user(OWNER), Payee::Protocol)
                } else {
                    (self.market_lister(m), Payee::Lister)
                };
                let before = self.c.get_fee_ledger(m, payee).0;
                let wallet_before = self.c.get_wallet(who.clone()).0;
                self.call_as(&who, ONE);
                let got = self.c.claim_fees(m, payee).0;
                assert!(got <= before, "claimed more than accrued");
                assert_eq!(
                    self.c.get_fee_ledger(m, payee).0,
                    before - got,
                    "ledger debited by the payout"
                );
                assert_eq!(
                    self.c.get_wallet(who).0,
                    wallet_before + got,
                    "payout lands in the wallet"
                );
                self.fees_claimed[m as usize] += got;
            }
            Act::Withdraw {
                who,
                amount,
                transfer_ok,
            } => {
                let before = self.wallet(who);
                self.call(who, ONE);
                let _ = self.c.withdraw(U128(amount));
                assert_eq!(self.wallet(who), before - amount, "debit first");
                // In flight: the tokens have not left yet, the ledger is already debited.
                let (cw, cv) = self.c.get_custody();
                assert!(
                    self.token >= cw.0 + cv.0,
                    "in-flight withdraw: token balance covers the ledgers"
                );
                let result = if transfer_ok {
                    PromiseResult::Successful(vec![])
                } else {
                    PromiseResult::Failed
                };
                self.callback(result);
                let ok = self.c.on_withdraw(Owner::Near(user(who)), U128(amount));
                assert_eq!(ok, transfer_ok);
                if transfer_ok {
                    self.token -= amount;
                } else {
                    assert_eq!(
                        self.wallet(who),
                        before,
                        "failed transfer re-credits exactly"
                    );
                }
            }
            Act::SetOiCaps {
                m,
                long,
                short,
                acct,
            } => {
                self.call(OWNER, NearToken::from_yoctonear(1));
                self.c.set_oi_caps(m, U128(long), U128(short), U128(acct));
            }
            Act::SetMode { m, uncertain } => {
                self.call(OWNER, NearToken::from_yoctonear(1));
                self.c.set_mode(
                    m,
                    if uncertain {
                        Mode::PriceUncertain
                    } else {
                        Mode::ReduceOnly
                    },
                );
            }
            Act::SetPauser(p) => {
                self.call(OWNER, NearToken::from_yoctonear(1));
                self.c.set_pauser(p.map(user));
            }
            Act::SetOracle => {
                self.call(OWNER, NearToken::from_yoctonear(1));
                self.c.set_oracle(oracle());
            }
            Act::SetOwner => {
                self.call(OWNER, NearToken::from_yoctonear(1));
                self.c.set_owner(user(OWNER));
            }
            Act::CreateMarketAdmin { price } => {
                self.call(OWNER, NearToken::from_near(1));
                self.c.create_market(
                    "X-PERP".into(),
                    U64(price),
                    params(),
                    5,
                    U64(60_000_000_000),
                );
                self.sync_markets();
            }
        }
    }

    /// Everything market `m` owns, as a string: engine group view, every known
    /// portfolio (users + the pool), LP shares and fee ledgers.
    pub fn market_snapshot(&self, m: u32) -> String {
        let mut s = serde_json::to_string(&self.c.get_market(m)).unwrap();
        for a in USERS
            .iter()
            .map(|u| u.parse::<AccountId>().unwrap())
            .chain([contract_id()])
        {
            s.push_str(&serde_json::to_string(&self.c.get_account(m, a.clone())).unwrap());
            s.push_str(&self.c.get_lp_shares(m, a).0.to_string());
            s.push('|');
        }
        for p in PAYEES {
            s.push_str(&self.c.get_fee_ledger(m, p).0.to_string());
            s.push('|');
        }
        s
    }

    /// Every balance in the first `n_markets` markets and every wallet
    /// (invariant 6: admin acts must leave this byte-identical).
    pub fn balance_snapshot(&self, n_markets: u32) -> String {
        let mut s = format!("{:?}", self.c.get_custody());
        for i in 0..USERS.len() {
            s.push_str(&format!("w{}={};", i, self.wallet(i)));
        }
        s.push_str(&format!("wc={};", self.c.get_wallet(contract_id()).0));
        for m in 0..n_markets {
            let v = self.c.get_market(m);
            s.push_str(&format!(
                "m{m}:v={},i={},lp={},pool={};",
                v.vault.0, v.insurance.0, v.lp_total_shares.0, v.pool_position_q.0
            ));
            for a in USERS
                .iter()
                .map(|u| u.parse::<AccountId>().unwrap())
                .chain([contract_id()])
            {
                s.push_str(&serde_json::to_string(&self.c.get_account(m, a.clone())).unwrap());
                s.push_str(&self.c.get_lp_shares(m, a).0.to_string());
            }
            for p in PAYEES {
                s.push_str(&format!("f={};", self.c.get_fee_ledger(m, p).0));
            }
        }
        s
    }

    /// The custody / LP / fee invariants. Returns the first violation.
    pub fn violations(&self) -> Vec<String> {
        let mut out = vec![];
        let (cw, cv) = self.c.get_custody();
        let (cw, cv) = (cw.0, cv.0);

        // inv 1a: the token balance backs the wrapper ledgers exactly.
        if self.token != cw + cv {
            out.push(format!(
                "inv1: token balance {} != custody wallets {} + vaults {}",
                self.token, cw, cv
            ));
        }
        // inv 1b: the wallet counter is the sum of the wallet ledger.
        let wallets: u128 = (0..USERS.len()).map(|i| self.wallet(i)).sum::<u128>()
            + self.c.get_wallet(contract_id()).0;
        if wallets != cw {
            out.push(format!("inv1: Σ wallets {wallets} != custody_wallets {cw}"));
        }
        // inv 1c: the vault counter is the sum of the engine vaults.
        let mut vaults = 0u128;
        for m in 0..self.n_markets() {
            let v = self.c.get_market(m);
            vaults += v.vault.0;
            // inv 1d: engine conservation V >= C_tot + I over every portfolio we know.
            let capital: u128 = USERS
                .iter()
                .map(|u| u.parse::<AccountId>().unwrap())
                .chain([contract_id()])
                .filter_map(|a| self.c.get_account(m, a))
                .map(|a| a.capital.0)
                .sum();
            if v.vault.0 < capital + v.insurance.0 {
                out.push(format!(
                    "inv1: market {m} engine vault {} < Σ capital {} + insurance {}",
                    v.vault.0, capital, v.insurance.0
                ));
            }
            // LP share ledger.
            let shares: u128 = (0..USERS.len())
                .map(|i| self.c.get_lp_shares(m, user(i)).0)
                .sum();
            if shares != v.lp_total_shares.0 {
                out.push(format!(
                    "lp: market {m} Σ shares {shares} != lp_total_shares {}",
                    v.lp_total_shares.0
                ));
            }
            // inv 8: every atom of trading fee is on exactly one ledger or was paid out.
            let ledgers: u128 = PAYEES.iter().map(|p| self.c.get_fee_ledger(m, *p).0).sum();
            let (charged, claimed) = (self.fees_charged[m as usize], self.fees_claimed[m as usize]);
            if ledgers + claimed != charged {
                out.push(format!(
                    "inv8: market {m} Σ ledgers {ledgers} + claimed {claimed} != fees charged {charged}"
                ));
            }
        }
        if vaults != cv {
            out.push(format!(
                "inv1: Σ engine vaults {vaults} != custody_vaults {cv} (token balance {}, Σ wallets + Σ engine vaults {})",
                self.token,
                wallets + vaults
            ));
        }
        out
    }

    pub fn check(&self, label: &str) {
        let v = self.violations();
        assert!(v.is_empty(), "after `{label}`:\n  {}", v.join("\n  "));
    }

    /// Apply one act that MUST succeed, then assert every invariant
    /// (custody, isolation, admin, fees, LP).
    pub fn step(&mut self, a: Act) {
        let label = format!("#{} {:?}", self.log.len(), a);
        let n = self.n_markets();
        let target = self.act_market(&a);
        let others: Vec<(u32, String)> = (0..n)
            .filter(|m| Some(*m) != target)
            .map(|m| (m, self.market_snapshot(m)))
            .collect();
        let balances = a.is_admin().then(|| self.balance_snapshot(n));
        println!("step {label}"); // shown only when the test fails
        self.apply(&a);
        for (m, before) in others {
            assert_eq!(
                before,
                self.market_snapshot(m),
                "inv2 isolation: `{label}` changed market {m}"
            );
        }
        if let Some(before) = balances {
            assert_eq!(
                before,
                self.balance_snapshot(n),
                "inv6: admin act `{label}` changed a balance"
            );
        }
        self.check(&label);
        self.log.push(a);
    }

    pub fn steps(&mut self, acts: impl IntoIterator<Item = Act>) {
        for a in acts {
            self.step(a);
        }
    }

    /// `keeper_step` every OTHER user holding a position in `m`. Needed before
    /// a position-increasing fill: the engine refuses it (`LockActive`) while
    /// another exposed account has not been refreshed in the current slot
    /// (see `cov_liveness.rs`).
    pub fn refresh_others(&mut self, m: u32, who: usize) {
        for i in 0..USERS.len() {
            if i != who && self.position(m, i) != 0 {
                self.step(Act::KeeperStep {
                    m,
                    who: i,
                    by: KEEPER,
                });
            }
        }
    }

    /// request → next block → fresh oracle push at `price` → (refresh the
    /// other exposed accounts for an open) → execute.
    pub fn fill(
        &mut self,
        who: usize,
        m: u32,
        long: bool,
        size_q: u128,
        reduce_only: bool,
        price: u64,
    ) {
        self.step(Act::Request {
            who,
            m,
            long,
            size_q,
            limit: if long { u64::MAX } else { 0 },
            expiry: 30,
            reduce_only,
        });
        let order = *self.orders.last().unwrap();
        self.step(Act::Tick(1));
        self.step(Act::Push {
            m,
            price,
            age_ns: 0,
        });
        if !reduce_only {
            self.refresh_others(m, who);
        }
        self.step(Act::Execute { order, by: KEEPER });
    }
}

// ───────────────────────── fuzz driver ─────────────────────────

thread_local! {
    static QUIET: Cell<bool> = const { Cell::new(false) };
}
static HOOK: Once = Once::new();

/// Silence the panic message of an EXPECTED contract refusal on this thread.
fn quiet<T>(f: impl FnOnce() -> T) -> std::thread::Result<T> {
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.with(|q| q.get()) {
                default(info);
            }
        }));
    });
    QUIET.with(|q| q.set(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    QUIET.with(|q| q.set(false));
    r
}

/// xorshift64* (no dependency).
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn range(&mut self, lo: u128, hi: u128) -> u128 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next() as u128 * (hi - lo + 1) >> 64)
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// Everything in a `World` except the contract, plus the contract's root
/// struct (borsh) and the full mocked storage: enough to undo one call.
pub struct Checkpoint {
    root: Vec<u8>,
    storage: std::collections::HashMap<Vec<u8>, Vec<u8>>,
    height: u64,
    ts: u64,
    token: u128,
    fees_charged: Vec<u128>,
    fees_claimed: Vec<u128>,
    liq_fees: Vec<u128>,
    liquidations: u64,
    fills: u64,
    orders: Vec<u64>,
    conds: Vec<u64>,
}

fn install_storage(storage: std::collections::HashMap<Vec<u8>, Vec<u8>>) {
    let mut b = VMContextBuilder::new();
    b.current_account_id(contract_id()); // views resolve the pool by this id
    near_sdk::env::set_blockchain_interface(near_sdk::MockedBlockchain::new(
        b.build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        vec![],
        storage,
        Default::default(),
        None,
    ));
}

impl World {
    /// Flush the contract to the mocked storage and remember everything.
    pub fn checkpoint(self) -> (World, Checkpoint) {
        let World {
            height,
            ts,
            c,
            token,
            fees_charged,
            fees_claimed,
            liq_fees,
            liquidations,
            fills,
            orders,
            conds,
            log,
        } = self;
        let root = near_sdk::borsh::to_vec(&c).unwrap();
        drop(c); // collections flush on drop
        let storage = near_sdk::mock::with_mocked_blockchain(|b| b.take_storage());
        install_storage(storage.clone());
        let cp = Checkpoint {
            root,
            storage,
            height,
            ts,
            token,
            fees_charged: fees_charged.clone(),
            fees_claimed: fees_claimed.clone(),
            liq_fees: liq_fees.clone(),
            liquidations,
            fills,
            orders: orders.clone(),
            conds: conds.clone(),
        };
        let c: Neap = near_sdk::borsh::from_slice(&cp.root).unwrap();
        (
            World {
                height,
                ts,
                c,
                token,
                fees_charged,
                fees_claimed,
                liq_fees,
                liquidations,
                fills,
                orders,
                conds,
                log,
            },
            cp,
        )
    }

    /// Undo everything since `cp` (a reverted receipt).
    pub fn rollback(self, cp: Checkpoint) -> World {
        let World { c, log, .. } = self;
        drop(c); // flushes half-written state; the storage is replaced next
        install_storage(cp.storage);
        let c: Neap = near_sdk::borsh::from_slice(&cp.root).unwrap();
        World {
            height: cp.height,
            ts: cp.ts,
            c,
            token: cp.token,
            fees_charged: cp.fees_charged,
            fees_claimed: cp.fees_claimed,
            liq_fees: cp.liq_fees,
            liquidations: cp.liquidations,
            fills: cp.fills,
            orders: cp.orders,
            conds: cp.conds,
            log,
        }
    }
}

/// A world that tolerates refused acts: a refused act is a reverted receipt,
/// modelled by restoring the checkpoint taken just before it (contract root
/// struct + mocked storage + shadow state).
pub struct Fuzz {
    pub w: Option<World>,
    pub p0: u64,
    pub p1: u64,
    pub refused: u64,
    pub accepted: u64,
    /// Why acts were refused: "ActKind: panic message" → count.
    pub refusals: std::collections::BTreeMap<String, u64>,
    pub last_refusal: String,
}

impl Fuzz {
    pub fn new(p0: u64, p1: u64) -> Fuzz {
        Fuzz {
            w: Some(World::new(p0, p1)),
            p0,
            p1,
            refused: 0,
            accepted: 0,
            refusals: Default::default(),
            last_refusal: String::new(),
        }
    }

    pub fn w(&self) -> &World {
        self.w.as_ref().unwrap()
    }

    /// Try one act. `true` if the contract accepted it. Invariants are checked
    /// after an accepted act; a refused one is rolled back like a NEAR revert.
    pub fn try_step(&mut self, a: Act) -> bool {
        let (mut w, cp) = self.w.take().unwrap().checkpoint();
        let label = format!("#{} {:?}", w.log.len(), a);
        let n = w.n_markets();
        let target = w.act_market(&a);
        let others: Vec<(u32, String)> = (0..n)
            .filter(|m| Some(*m) != target)
            .map(|m| (m, w.market_snapshot(m)))
            .collect();
        let balances = a.is_admin().then(|| w.balance_snapshot(n));
        let r = quiet(|| w.apply(&a));
        match r {
            Ok(()) => {
                for (m, before) in others {
                    assert_eq!(
                        before,
                        w.market_snapshot(m),
                        "inv2 isolation: `{label}` changed market {m}"
                    );
                }
                if let Some(before) = balances {
                    assert_eq!(
                        before,
                        w.balance_snapshot(n),
                        "inv6: admin act `{label}` changed a balance"
                    );
                }
                w.check(&label);
                w.log.push(a);
                self.accepted += 1;
                self.w = Some(w);
                true
            }
            Err(e) => {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                let msg = msg
                    .split("panic_msg: ")
                    .last()
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect::<String>();
                let kind = format!("{a:?}")
                    .split([' ', '('])
                    .next()
                    .unwrap_or("")
                    .to_string();
                self.last_refusal = msg.clone();
                *self.refusals.entry(format!("{kind}: {msg}")).or_insert(0) += 1;
                let w = w.rollback(cp);
                // A revert must restore a state that satisfies every invariant.
                w.check(&format!("rollback of {label}"));
                self.w = Some(w);
                self.refused += 1;
                false
            }
        }
    }
}
