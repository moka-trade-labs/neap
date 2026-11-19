# HANDOFF

## 2026-11-11: setup + plan step 1.1

Layout: `crates/engine` (vendored Percolator v16, untouched), `crates/neap` (contract, was `percolator-near`),
`crates/README.md` + `SECURITY.md` (port notes, past findings).

Step 1.1 done:
- Crate/lib/wasm renamed `percolator-near` → `neap`, struct `Perp` → `Neap`; sandbox crate `neap-sandbox`.
- `src/model.rs`: `Owner {Near, Sol}`, `Tier`, `Mode`, `PriceSource`, `Observation`, `Market` (replaces `MarketInfo`).
  Wallets, portfolios, LP shares are keyed by `Owner`. `Owner::Sol` exists but nothing constructs it yet (phase 2).
- Events: `standard: "neap"`, `version: "1.0.0"`, `fill` carries `owner` + `block` (spec §8).
- Verified: `cargo test` in `crates/neap` (22 old + 1 new pass), release wasm builds (560 KB).
  Not re-run: Kani, the sandbox gas test (`crates/neap/sandbox`).

Defaults I chose (unconfirmed):
- `PriceSource::Pushed` is a transitional variant so the owner-only `create_market` + `push_price` keep working;
  it is deleted in 1.3 (see `docs/contract.md` §3). Owner-created markets are `Tier::Major`, `Mode::Active`, bond 0.
- Public method args stay `AccountId` (views, `keeper_step`, `liquidate`); they wrap to `Owner::Near`. `on_withdraw`
  takes `Owner`. Switch views to `Owner` when Solana users land (2.3).
- `Mode` and `Tier` are stored and exposed but not enforced yet (1.2, 1.4).

Next: 1.2 observation + oracle rules (stale ladder, EMA, monotone `seq`/`source_ts`).

## 2026-11-11: workspace + Rust-only services

- Root `Cargo.toml` workspace: `crates/engine`, `crates/neap`, `services/*` (keeper, indexer, relayer, oracle-agent, types).
  Service crates are empty skeletons. Single root `Cargo.lock`, `.cargo/config.toml` (`RUST_MIN_STACK`).
- Excluded (own roots): `crates/neap/sandbox`, `crates/neap/sandbox/mock-ft`.
- Profiles moved to the root. `release` is now the contract's wasm profile (opt-level z); the engine's old
  `release` (lto fat, opt 3) is gone. `sbf` kept. Engine's `[workspace.metadata.kani]` became `[package.metadata.kani]`
  (not re-verified with `cargo kani`; check before relying on it).
- D21: all services are Rust, no TypeScript.
- Commands now run from the root: `cargo test -p neap`, `cargo test -p percolator --features fuzz`.

## 2026-11-12: steps 1.2–1.6

Steps 1.2 (observation + oracle rules), 1.3 (NEAR pool reader), 1.4 (permissionless create_market),
1.5 (pricing / OI caps), 1.6 (two-step orders) implemented in one pass. 37 tests pass, wasm builds.

### 1.2: Stale ladder + EMA
- `policy.rs`: `StalenessLevel` enum, `observation_staleness(age_ns)` with thresholds at 10 s / 60 s / 1 h.
  `ema_next()` (5% alpha EMA for pool-sourced prices). Pure functions, Kani-ready.
- `model.rs`: added `ema_e6` field to `Market`.
- `contract.rs`: `push_price` now computes the EMA on each update. `execute_order` checks the stale ladder
  before every fill: `NoOpens`/`ReduceOnly` → only reduce-only orders; `PriceUncertain` → no fills.
- Tests: `stale_fresh_allows_opens`, `stale_no_opens_blocks_open`, `stale_no_opens_allows_close`,
  `stale_reduce_only_blocks_open`, `stale_price_uncertain_classification`.

### 1.3: NEAR pool reader
- `ext_ref` trait for `get_pool` on Ref Finance / Rhea.
- `refresh_price(market)` → Promise chain → `on_price_read` callback.
- `on_price_read` parses the pool JSON, computes `price_e6 = usdc_atoms * 1e6 / token_atoms` (assumes 6-decimal
  tokens for now), writes the observation, updates EMA, and feeds the EMA as the staircase target.
  Band check: > 20% move from EMA → market drops to ReduceOnly.
- Not sandbox-tested yet (needs a mock pool contract; the logic is unit-testable via the policy functions).

### 1.4: Permissionless create_market
- `ft_on_transfer` now parses `msg` as `FtMsg` JSON: `Wallet` (default), `CreateMarket{symbol, tier, price_source, ...}`.
- `CreateMarket` via `ft_transfer_call(bond)` → `Proposed` market. Min bond: 50 USDC for LongTail, 0 for Major.
- `activate_market(id)`: permissionless, checks first observation is fresh, seq > 0, and liquidity ≥ depth floor
  (10 k USDC Major, 1 k LongTail).

### 1.5: Pricing + OI caps
- `policy.rs`: `dynamic_spread_bps(spread_min, k_skew, k_size, net_oi_after, oi_cap, size_abs)`,
  `ratcheted_leverage_max_bps`, `oi_within_cap`.
- Tier-level spread params: Major (5 / 45 / 5 bps), LongTail (20 / 130 / 20 bps).
- `Market` gains `oi_cap_long`, `oi_cap_short`, `per_account_cap` (0 = uncapped).
- `execute_order` enforces OI caps before fill. `compute_spread()` uses dynamic spread when caps are set,
  falls back to fixed `spread_bps` when uncapped.
- `quote(market, side, size_q)` view returns `(exec_price, spread, fee)`.
- `set_oi_caps` admin method.
- Tests: `oi_cap_blocks_oversized_order`, `oi_cap_allows_within_limit`, `quote_returns_sane_values`.

### 1.6: Two-step orders
- `model.rs`: `Order { id, market, owner, side, size_q, limit_price, request_block, expiry_block, reduce_only }`,
  `OrderSide { Long, Short }`.
- `contract.rs`: `orders: LookupMap<u64, Order>`, `next_order_id: u64`.
- `request_order(market, side, size_q, limit_price, expiry_blocks, reduce_only)` → stores pending order.
  Max expiry 50 blocks (~30 s). Requires 1 yocto and an open account. No wallet escrow (D22).
- `execute_order(order_id)` → permissionless. Checks: not expired, `observation.block > request_block` (invariant 3),
  stale ladder, reduce-only validation, OI caps, limit price, then fills via `exchange::trade_against_pool`.
- `cancel_order(order_id)` → owner only.
- Public `trade` removed; fills only happen through `execute_order`.
- Tests: `same_block_execute_refused`, `expired_order_is_cleaned_up`, `limit_price_respected`,
  `cancel_order_works`, `cancel_by_stranger_refused`. All prior tests updated to use two-step flow.

Defaults I chose (unconfirmed):
- D22: no wallet escrow on `request_order`. The user's existing margin account covers the fill at execute time.
  Pro: simple, no third custody bucket. Con: a user can place overlapping orders that exceed their margin;
  only the first to execute succeeds. Full escrow is a follow-up.
- EMA alpha is 500 bps (5%); only active for `NearPool` staircase targets. `Pushed` markets still use the
  raw price as the staircase target.
- `on_price_read` assumes 6-decimal base tokens. A `base_decimals` field in `PriceSource::NearPool` is needed
  for tokens with other decimal counts (e.g. NEAR with 24 decimals).
- Band check in `on_price_read` uses 20% (2000 bps). The spec doesn't pin this; adjust if too tight for
  volatile tokens.

Half-done / not started:
- Property tests for 1.5 (price monotone in size, skew-reducing cheaper) — the policy functions are there
  but need the randomized exhaustive checks like `policy_props.rs`.
- Sandbox test for 1.3 (mock pool contract).
- `PriceSource::NearPool { quote_pool }` — the two-promise join for non-USDC-quoted pools isn't wired yet.

## 2026-11-12: steps 1.7–1.9

Steps 1.7 (conditional orders), 1.8 (fees), 1.9 (admin roles) implemented. 48 tests pass, wasm builds.

### 1.7: Conditional orders
- `model.rs`: `ConditionalKind { TakeProfit, StopLoss, Limit }`, `ConditionalOrder` struct.
- `contract.rs`: `conditionals: LookupMap<u64, ConditionalOrder>`.
- `place_conditional(market, kind, side, size_q, trigger_price, limit_price, expiry_blocks, reduce_only)` —
  TP/SL forced reduce-only; storage charged; max 30 days expiry. Shares `next_order_id` with regular orders.
- `execute_conditional(cond_id)` — permissionless (keeper). Checks: not expired, `observation.block > arm_block`,
  mark crosses trigger, stale ladder, reduce-only validation, then fills. Fee split accrued.
- `cancel_conditional(cond_id)` — owner only.
- Trigger directions: `TP+Short` (close long) triggers mark ≥ trigger; `SL+Short` (close long) mark ≤ trigger;
  `TP+Long` (close short) mark ≤ trigger; `SL+Long` (close short) mark ≥ trigger.
- Tests: `conditional_tp_triggers_on_price_rise`, `conditional_does_not_trigger_below_threshold`,
  `conditional_sl_triggers_on_price_drop`, `cancel_conditional_works`.

### 1.8: Fee system
- `fee_ledgers: LookupMap<(u32, Payee), u128>` with `Payee { Lister, Lp, Insurance, Protocol }`.
- Trading fee split (each fill via `accrue_fee_split`): lister 50% / LP 30% / insurance 10% / protocol 10%.
  Rounding dust → protocol (invariant 8).
- Liquidation fee: engine's `fee_charged` → 60% to caller's wallet, 40% stays in insurance.
- `claim_fees(market, payee)` — lister and protocol can withdraw their accrued fees. LP and insurance
  fees stay in the pool/insurance fund.
- `get_fee_ledger(market, payee)` view.
- Tests: `fee_split_accrues_on_fill`, `claim_fees_pays_lister`.

### 1.9: Admin roles
- `pauser: Option<AccountId>` on Neap state.
- `set_mode(market, ReduceOnly | PriceUncertain)` — pauser or owner; tighten-only
  (Active→ReduceOnly→PriceUncertain, never loosen).
- `set_pauser(Option<AccountId>)` — owner only.
- `set_owner(AccountId)` — owner only (moved here from 1.1).
- Tests: `admin_set_mode_tightens_only`, `admin_cannot_loosen_mode`, `pauser_can_tighten_mode`,
  `stranger_cannot_set_mode`, `admin_methods_do_not_change_balances` (invariant 6).

### claim_fees fix (2026-11-12)
- `claim_fees` now calls `exchange::withdraw_insurance_surplus` on the engine group before moving
  tokens to the wallet. This keeps `custody_vaults == engine_vault` (invariant 1).
- New `exchange::withdraw_insurance_surplus(g, amount)` → tries full amount, falls back to available
  surplus if insurance was used for bad debt. Returns actual withdrawn amount.
- If surplus is exhausted, the ledger is unchanged and the caller can retry later.

### Sandbox tests (2026-11-12)
- `sandbox/mock-ref-pool/`: minimal Ref Finance pool mock (returns `get_pool` JSON).
- `sandbox_flow_and_gas`: updated to use `request_order` + `execute_order` (two-step).
  Full flow: deposit → open 10 → oracle +1% → crank → close → convert PnL → withdraw.
  Bob profits, custody invariant holds.
- `sandbox_refresh_price_near_pool` (1.3 + 1.4): creates a `NearPool` market via
  `ft_transfer_call(CreateMarket{...})`, calls `refresh_price` (cross-contract to mock),
  verifies `on_price_read` writes correct `price_e6=5_000_000` and `seq=1`,
  then `activate_market` succeeds.
- Both tests pass. 48 unit tests + 2 sandbox tests all green.

Next: 1.10 keeper (off-chain), 1.11 indexer + API, 1.12 security gate + mainnet deploy.

## 2026-11-13: backend deployed (keeper + indexer + API), path 1 live on testnet

Where things run and which accounts exist: `deploy/TESTNET.md`. API reference: `services/indexer/README.md`.

What changed in the services:
- Keeper runs three independent loops (price, orders, liquidations). Orders are picked up every second;
  if the last observation is not newer than the request, the keeper asks for one first (push or
  `refresh_price`) and then executes. Conditionals are only sent when the mark is within 0.3% of the trigger.
- Keeper auto-discovers markets, pushes a live Coinbase/Kraken price to `Pushed` markets, calls
  `refresh_price` on `NearPool` markets, and activates `Proposed` markets once they have an observation.
- Indexer has two event paths: the neardata block feed (rate-limited from Render, treat as a backup) and
  `POST /tx {hash, signer_id}`, which reads the transaction outcome over JSON-RPC. The keeper reports its own
  transactions; the frontend must report each wallet transaction. Receipts are de-duplicated (`seen_receipts`).
- Prices, market state and candles come from a 3 s `get_market` poll, not from events.
- Transactions are sent with `wait_until: INCLUDED`, so a keeper log line "sent" does not prove success.

Known gaps (not fixed, by priority):
1. Liquidation scan is naive: it takes accounts from the indexer (`/accounts`, so only accounts whose
   fills were indexed) and sends `liquidate` for every open position, relying on the contract to refuse.
   Needs an account enumeration view or an off-chain health check.
2. A user transaction the frontend never reports is only indexed if the block feed happens to work.
   A FastNEAR API key (`FASTNEAR_API_KEY` in Render) makes the block feed reliable.
3. `convert_pnl_for` does not exist, so the keeper cannot auto-convert profit on close (decision F-1).
4. Permissionless `Pushed` markets cannot activate (only the owner path creates them); fine for path 1.
5. `base_decimals` is lister-supplied and unchecked; a wrong value misprices the market.
6. The first deployment's key is committed in `CONTRACT.md`. Rotate it and purge it before mainnet.
7. Free Render instance and free Postgres (expires 2026-12-13); public RPCs with failover, no paid key.
8. Path 2 (Solana tokens through a TEE oracle, opening and creating from Solana) is not started in the
   services: `oracle-agent` and `relayer` are skeletons.

## 2026-11-18: off-chain review and hardening

Full findings, what was fixed and what is open: `docs/reviews/2026-11-18-offchain-review.md`.

What changed (services only; no contract or engine change):
- Keeper: prices from Coinbase and Kraken are cross-checked before a push; the fixed fallback price is
  dev-only; nonce never moves backwards; order retries back off; conditionals have their own task; watchdog
  `ALERT` logs and `GET /status`; faucet is testnet-only with a race-free limit.
- Indexer: each receipt is written in one transaction; out-of-order events no longer strand orders as
  `pending`; triggered conditionals appear as fills; `START_BLOCK` works; skipped ranges are recorded.
- API: per-client rate limits, request deadline, view cache, a `/health` that reports `ok`/`degraded`,
  bounded candle query, WebSocket caps.
- CI workflow, non-root container, private key removed from `CONTRACT.md`.

Not deployed. Nothing on Render was touched. New env vars all have safe defaults; none is required.
On deploy, behaviour changes to expect: the keeper pushes nothing when its two price sources disagree by
more than 1%, and the API returns HTTP 429 over the per-client limits.

Defaults I chose (unconfirmed by jason):
- Price agreement 1% between sources; single-source move cap 5% against the on-chain price.
- API limits per client per minute: 120 chain reads, 30 writes, 600 other.
- `/health` stays HTTP 200 when degraded, so Render does not restart-loop on an RPC outage.

Waiting on jason: section 3 of the review (leaked key, database expiry on 2026-12-13, oracle trust model,
key separation, liquidation view, market allow-list, event source, hosting).

Next: O1 backfill from `skipped_ranges`, O2 candle rollup and retention, O4 alert delivery.
