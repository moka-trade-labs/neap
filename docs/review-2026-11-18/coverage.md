# Verification coverage review: Neap contract

Reviewer scope: test / proof / CI coverage. Date 2026-11-18.

**Two code states are referenced.** "Baseline" is commit `da7e8c0` (what the review started on). "Current" is the
working tree after fixes were applied to `contract.rs` / `exchange.rs` while this review ran. The matrix describes
baseline tests (`contract_unit.rs`, `exchange_flow.rs`, `policy_props.rs`, `proofs.rs`, sandbox); `poc_*` tests added
by other reviewers are named where they close a gap. My new tests were run on both states.

## 0. What I could and could not run

| Check | Result |
| --- | --- |
| `cargo test -p neap` (baseline) | green, 48 tests, 2.4 s warm |
| `cargo test -p neap` (current, with every `cov_*` / `poc_*` file) | my 3 tests pass; run stops at `poc_accounting_lp_profit` (2 failing, not mine); files after it not run in that pass |
| `cargo test -p percolator --features fuzz` | green, 18 s (binaries already built) |
| `cargo kani` | **not installed. No Kani harness was run, neap or engine.** |
| wasm build | **not run: `wasm32-unknown-unknown` target is not installed** (`can't find crate for core`) |
| sandbox tests (`crates/neap/sandbox`) | **not run** (needs the wasm, near-sandbox binary) |
| `cargo fmt --all --check` | **fails**: 95 diffs at baseline (neap: `contract.rs` 9, `exchange.rs` 2, `tests/contract_unit.rs` 21; the rest in `services/`) |
| `cargo clippy -p neap --all-targets` | exit 0 with warnings: neap lib 15, vendored engine lib 42 |
| `cargo clippy -p neap --all-targets -- -D warnings` | **fails**, on the engine lib first (42 errors: dead code, `is_multiple_of`, too-many-args) |

## 1. Coverage matrix

### 1a. Spec invariants (docs/contract.md §6)

| # | Invariant | What checks it at baseline | Negative cases | Gap |
| --- | --- | --- | --- | --- |
| 1 | Custody | `custody_matches_vaults` in `contract_unit.rs`: 4 call sites in 2 of 35 tests (`full_user_flow_through_contract`, `lp_round_trip_returns_at_most_deposit`). It checks only `custody_vaults == market 0 engine vault`. Kani: 3 harnesses on the `Custody` struct. Sandbox: one check at the end of `sandbox_flow_and_gas`. | none | **Spec says "checked after every call in tests"; it was not.** Never checked: token balance vs ledgers, Σ wallets vs `custody_wallets`, more than one market, after liquidation / fee claim / insurance deposit / failed withdraw / bond listing. The Kani proofs show `wallets + vaults` is conserved by the four ledger moves; they say nothing about `custody_vaults == Σ engine vaults`, which is exactly what the liquidation bug broke (COV-1). Now covered by `cov_*`. |
| 2 | Isolation | none | none | No baseline test has two markets. Now covered by `cov_*` (snapshot of every other market after each call). |
| 3 | No stale fills | `same_block_execute_refused` (orders) | orders only | No test for `execute_conditional` vs arm block at baseline. `poc_oracle_stale_and_timing` adds "price published before the request" and "first post-request observation". |
| 4 | Monotone oracle | `oracle_replay_rejected` (same `publish_ns`), Kani `proof_oracle_acceptance_is_monotone_and_fresh` | replay only | Future timestamp beyond 2 s, too-old update, `seq` strictly increasing, and the `on_price_read` path are untested in the mocked runtime. |
| 5 | No straddled promises | none (code comment + audit C-2) | none | No test that `on_price_read` leaves the engine group bytes unchanged, none that `on_withdraw` does. Failed-withdraw re-credit was untested at baseline (now in `cov_*` and `poc_near_misc`). |
| 6 | Admin can't take funds | `admin_methods_do_not_change_balances` | happy path | Covers `set_mode`, `set_oi_caps`, `set_pauser`; checks one wallet, one account, the two custody counters. Not covered: `set_oracle`, `set_owner`, owner `create_market`, LP shares, fee ledgers, insurance. Plan 1.9 says "every admin method leaves balances byte-identical". Now covered for all six by `cov_*`. |
| 7 | Signed actions | none | none | `execute_signed` does not exist (phase 2). Nothing to test yet; the invariant is listed as if in force. |
| 8 | Fees conserve | `fee_split_accrues_on_fill` (shares > 0, ordering), `claim_fees_pays_lister` | none | No test that the four shares sum to the fee charged, that Σ claimed ≤ Σ accrued over time, or where dust goes. Split is in `contract.rs`, so Kani cannot reach it. Sum and claimed ≤ accrued now asserted after every call by `cov_*`. |
| 9 | Price source immutable | none | none | True by absence of a setter. No test pins it. |

### 1b. Public methods (baseline `contract.rs`)

H = happy path tested, N = negative/abuse tested, – = no mocked-runtime test.

| Method | Baseline tests | H | N | Missing |
| --- | --- | --- | --- | --- |
| `new`, `create_market` (owner) | every `setup()` | H | – | non-owner call, bad params, storage underpayment, spread cap |
| `set_oracle`, `set_owner` | – | – | – | everything (now `poc_near_admin_yocto`, `cov_*` for balances) |
| `set_pauser` | `pauser_can_tighten_mode` | H | – | non-owner |
| `set_mode` | `admin_set_mode_tightens_only`, `admin_cannot_loosen_mode`, `stranger_cannot_set_mode` | H | N | good |
| `set_oi_caps` | `oi_cap_*` | H | – | non-owner; that it can also loosen (spec: tighten only) |
| `register`, `open_account` | setup helpers | H | – | double register refund, double open, storage underpayment, unknown market |
| `ft_on_transfer` (Wallet) | `foreign_token_cannot_credit_wallets`, `unregistered_sender_is_refunded_in_full` | H | N | garbage msg |
| `ft_on_transfer` (CreateMarket) | sandbox only | – | – | bond below minimum, tier, bond custody (now `poc_access_listing`, `poc_accounting_bond`, `poc_near_storage_grief`) |
| `withdraw` | `full_user_flow…`, `value_moving_calls_need_one_yocto` | H | N | over-balance, zero |
| `on_withdraw` | – | – | – | failed transfer path was never executed in any baseline test |
| `deposit`, `withdraw_margin` | `full_user_flow…`, `contract_account_cannot_act_as_user…` | H | N (one) | market mode gates, over-withdraw through the contract |
| `request_order` | many | H | – | bad expiry, zero size, no account, non-active market |
| `execute_order` | `same_block…`, `expired…`, `limit_price_respected`, `stale_*`, `oi_cap_blocks…` | H | N | per-account cap, ReduceOnly / PriceUncertain market mode, reduce-only validation failure, second trader |
| `cancel_order` | `cancel_order_works`, `cancel_by_stranger_refused` | H | N | good |
| `place_conditional`, `execute_conditional`, `cancel_conditional` | `conditional_tp…`, `conditional_sl…`, `conditional_does_not_trigger…`, `cancel_conditional_works` | H | N (trigger only) | arm-block rule, expiry, Limit kind, mode / OI caps / staleness on execute, stranger cancel, "triggers exactly once" (plan 1.7) |
| `refresh_price`, `on_price_read`, `activate_market` | sandbox `sandbox_refresh_price_near_pool` only | H | – | band breach, failed promise, bad JSON, zero reserves, depth floor, stale activation (partly `poc_oracle_nearpool`) |
| `quote` | `quote_returns_sane_values` (asserts `> 0` only) | H | – | equality with the real fill (`poc_risk_quote_fee`) |
| `claim_fees` | `claim_fees_pays_lister` | H | – | stranger, Lp/Insurance payee, Protocol payee, surplus exhausted |
| `convert_pnl` | `full_user_flow…` | H | – | during lag, nothing to convert |
| `lp_deposit`, `lp_redeem` | `lp_round_trip…`, `lp_deposit_refused_during_oracle_lag` | H | N (one) | redeem with pool exposed, two LPs, over-redeem |
| `insurance_deposit` | – (engine-level only in `exchange_flow.rs`) | – | – | everything |
| `push_price` | `only_oracle_pushes_prices`, `oracle_replay_rejected` | H | N | future ts, too old, price range |
| `crank` | used in flows | H | – | long gap (> 80 blocks) |
| `keeper_step` | – | – | – | everything in the mocked runtime |
| `liquidate` | – (engine-level in `exchange_flow.rs`; **not in the sandbox test any more**, although `crates/README.md` says it is) | – | – | the wrapper's reward payout was never executed by any test |
| views | `get_market`, `get_account`, `get_wallet`, `get_lp_shares`, `get_custody`, `get_fee_ledger`, `get_order` used | H | – | `get_conditional`, `get_next_order_id`, `scan_order_ids`, `scan_conditional_ids` never called |

Structural gaps: no baseline test has two traders, two LPs or two markets; none runs a liquidation through the
contract; `policy_props.rs` covers LP math, exec price and funding only (plan 1.5 asked for property tests on
spread monotonicity and caps; HANDOFF lists them as half-done).

## 2. Kani (`crates/neap/src/proofs.rs`, 14 harnesses, none run here)

| Policy item | Harness | Assessment |
| --- | --- | --- |
| `pool_exec_price` | 2 | Substantive, has a `cover`. Inputs are 16-bit (price ≤ 65 535 atoms = $0.065), so real prices rely on `policy_props.rs`. |
| `within_limit` | 1 | Restates the definition. Harmless, proves little. |
| `skew_funding_rate_e9` | 2 | Substantive (sign, bound, full skew = cap). |
| `accept_oracle_update` | 1 | Substantive. No `cover` that an update is ever accepted. |
| `lp_shares_for_deposit` / `lp_assets_for_redeem` | 4 | Algebra is right, but the domain is 16-bit `total_shares` and `nav`. Real pools have `total_shares ≈ 1e6 × deposits`, so the proved region is unreachable in production apart from the empty pool. Only the round-trip harness has a `cover`. |
| `lp_nav_bounds` | 1 | Substantive, full width. |
| `Custody` ledger ops | 3 | Substantive for what they state: the total moves by exactly `amount`. They do not relate the ledger to engine vaults or wallet entries. |
| `ema_next` | none | No harness, no property test. |
| `dynamic_spread_bps` | none | No harness, no property test (monotone in size and skew, ≤ `MAX_SPREAD_BPS`). |
| `ratcheted_leverage_max_bps` | none | No harness; also **never called by the contract**, so the §5 leverage ratchet is not enforced. |
| `observation_staleness` | none | Two boundary asserts in `stale_price_uncertain_classification`; the 10 s and 60 s edges are not pinned. |
| `oi_within_cap` | none | Trivial. |
| Trading fee split, liquidation split | none | Live in `contract.rs`, outside the Kani-visible layer. Moving them into `policy.rs` would make invariant 8 provable. |

No harness is vacuous through over-constrained `assume`s. The weakness is bounded domains, missing `cover`s and
scope. `crates/README.md` records that 5 of the 14 did not finish in 20 minutes; HANDOFF says Kani was not re-run
after the rename. AGENTS.md requires "Kani for touched kernels" before merge, but nothing runs it.

## 3. CI

`.github/` contains only `pull_request_template.md`. There is no workflow.

Minimal workflow, one job per row, on `pull_request` and `push` to main, Rust stable pinned (1.97 here),
`Swatinem/rust-cache`:

| Job | Command | Time here | Status today |
| --- | --- | --- | --- |
| fmt | `cargo fmt --all --check` | < 1 s | fails (95 diffs): format once, then gate. Exclude `crates/engine` if it must stay byte-identical to upstream. |
| clippy | `cargo clippy -p neap --all-targets` plus `cargo clippy --workspace --all-targets` | 30 s / 36 s | passes only without `-D warnings`. To gate, deny warnings for `neap` and services but not the vendored engine (per-crate `[lints]`, or `--no-deps` with `-D warnings`). |
| contract tests | `cargo test -p neap` | build ~45 s cold for the workspace test binaries; run 2.4 s at baseline, ~35 s now with `cov_*` and `poc_*` | green at baseline |
| engine tests | `cargo test -p percolator --features fuzz` | 18 s run | green |
| wasm | `rustup target add wasm32-unknown-unknown`; from `crates/neap`: `RUSTFLAGS="--cfg near -C link-arg=-s -C target-cpu=mvp" CARGO_TARGET_DIR=target-wasm cargo build --target wasm32-unknown-unknown --release`; fail if the wasm exceeds a size budget (README: 516 to 560 KB) | not measured (target missing) | unknown |
| sandbox (can be a second workflow) | build `mock-ft` and `mock-ref-pool` the same way, then `cd crates/neap/sandbox && cargo test --release -- --nocapture` (needs `ulimit -n 65535`) | not measured | unknown |
| long fuzz (nightly) | `cargo test -p neap --test cov_fuzz -- --ignored` | about 4 to 5 min | passes invariants on 12 seeds worth of earlier runs; see §5 |
| Kani (nightly or label-triggered) | `cargo kani --no-default-features` in `crates/neap` with a per-harness timeout; `cargo kani --tests --features fuzz --harness proof_moka_` in `crates/engine` | not measured | README: 9 of 14 finish |

Timings were taken on a machine shared with other cargo jobs, with a warm cache: treat them as lower bounds.

## 4. Spec drift (docs/contract.md vs code)

Baseline unless marked.

1. **§6 inv 8 dust**: spec sends rounding dust to insurance; code sends it to protocol (`contract.rs:1724` baseline); HANDOFF and the audit say protocol.
2. **§6 inv 1 formula** adds "pending refunds" and "claimable fees" to wallets and vaults. There is no `pending_refunds`, and claimable fees sit inside the engine vault (insurance), so adding them double-counts.
3. **§5 LP 30% / insurance 10%**: the `Lp` and `Insurance` ledgers only ever grow; nothing pays the LP share to LPs.
4. **§4 methods in the spec, absent in code**: `claim_refund`, `storage_deposit` (NEP-145), `submit_price`, `sync_tee_keys`, `convert_pnl_for`, `execute_signed`, `pause_opens`, `lower_caps`, `set_tier_bounds`, `set_agent_registry`, `set_roles`, `withdraw(to = Intents)`, ft msgs `Margin`, `DepositAndOpen`, `Lp`, `For`.
5. **In code, absent in §4**: `register`, `open_account`, `convert_pnl`, `insurance_deposit`, `cancel_conditional`, `push_price`, owner `create_market(init_price)`, `set_oracle`, `set_owner`, `set_pauser`, `set_oi_caps`, `get_custody`, `get_lp_shares`, `get_conditional`, `get_next_order_id`, `scan_*`.
6. **§4 "admin: tighten only"**: `set_oi_caps` can raise or remove caps.
7. **§4 `request_order` "escrows margin"**: no escrow (HANDOFF D22, unconfirmed).
8. **§2 "unknown msg is refunded in full"**: an unparsable msg is credited to the sender's wallet (audit M-2).
9. **§4 LP "redeem after a 24 h delay"**: not implemented (audit L-3).
10. **§5 leverage ratchet**: `ratcheted_leverage_max_bps` exists but is never called.
11. **§5 tier parameters** (fee 5 bps, MM 8% / 15%, liquidation fee 5%): not enforced as bounds at baseline; tests run with 10 bps fee, 5% MM, 0.5% liquidation fee.
12. **§2 staleness "≥ 1 h → recovery"** and market modes: staleness is computed per call and never stored as a mode; `Recovery`, `WindDown`, `Closed` are never entered.
13. **§8 events**: baseline emits 12 of 20. Missing: `order_executed` (code emits `fill`), `funding_settled`, `deposit`, `withdraw`, `refund_parked`, `lp_deposit`, `lp_redeem`, `fees_accrued`. `order_cancelled` / `order_expired` carry no `owner`. Current code adds `deposit`, `withdraw`, `lp_deposit`, `lp_redeem` and seven events not in §8 (`wallet_deposit`, `margin_withdraw`, `withdraw_failed`, `oracle_changed`, `owner_changed`, `pauser_changed`, `oi_caps_changed`); still missing `order_executed`, `funding_settled`, `refund_parked`, `fees_accrued`.
14. **§7 gap list** still describes the pre-port state (direct `trade()`, `fill` only, owner-only listing).
15. **`crates/README.md`**: lists a `trade` method, "22 passed", and a sandbox flow with liquidation that the sandbox test no longer contains.
16. **Current code vs spec** (fixes landed without a spec diff that I could see): admin calls need 1 yocto, `push_price` refused on NearPool markets, everything but `crank` requires the market caught up to the current block, 1 h price gate on `withdraw_margin` / LP paths / `keeper_step`. AGENTS.md says spec first.

## 5. Tests added (new files only, in `crates/neap/tests/`)

| File | Tests | Baseline `da7e8c0` | Current tree |
| --- | --- | --- | --- |
| `cov_harness.rs` | none (shared harness: `World`, `Act`, `World::check`, `Fuzz` with checkpoint/rollback) | compiles | compiles |
| `cov_custody_scenario.rs` | `mixed_scenario_custody_holds_after_every_step` | pass | pass |
| | `mixed_scenario_with_liquidation_custody_holds` | **FAIL (COV-1)** | pass |
| `cov_fuzz.rs` | `fuzz_invariants_hold_smoke` (1 seed, 110 rounds, ~11 s) | not run in this final form | pass |
| | `fuzz_invariants_hold_long` (`#[ignore]`, 12 seeds × 220 rounds) | not run in this final form | not run in this final form |

Runtime of the three files in `cargo test -p neap`: about 13 s. All three are rustfmt-clean.

What `World::check` asserts after **every** call (about 80 calls in the scenario, 150+ accepted calls in the smoke
fuzz): shadow USDC balance == `custody_wallets + custody_vaults`; `custody_wallets` == Σ wallet ledger over every
known account; `custody_vaults` == Σ engine vaults over all markets; per market, engine vault ≥ Σ capital +
insurance; Σ LP shares == `lp_total_shares`; Σ fee ledgers + Σ claimed == Σ fees charged by fills; an act on one
market leaves every other market's group, portfolios, LP shares and fee ledgers unchanged; admin acts leave every
balance unchanged. The scenario covers deposits, refunds, two LPs, three traders, two markets, orders (fill,
cancel, expire), TP and SL conditionals, funding, keeper step, profit conversion, fee claims, insurance donation,
LP out and in, failed then successful withdraw, bonded listing, six admin calls, liquidation.

Honest limits of the fuzz:
- Earlier, larger runs (6 seeds × 220 rounds per test, before the final edits, on baseline with `liquidate`
  excluded and on the current tree with it included) found **no invariant violation**, but they were thin where it
  matters: 8 to 44 fills per seed, 0 to 2 conditional executions, **zero liquidations**. The long test's
  `fills > 100` assert may not hold on 12 seeds; I did not run it after the last change.
- Most refused fills are `Engine(Stale)` (about 20 per seed), including at least one reduce-only close by the only
  positioned trader after a partial close and three price walks. I could not reproduce it in four hand-written
  variants and the automatic minimisation did not finish. **Not root-caused**; see COV-3.
- Rollback is modelled by restoring the contract root struct and the mocked storage, not by the real runtime.

### Findings

**COV-1, HIGH, BUGFIX. `liquidate` broke custody (baseline `contract.rs:1541-1547`). Fixed in the current tree.**
The engine keeps the whole liquidation fee in insurance; the wrapper then paid the caller 60% with
`vault_out_to_wallet`, lowering `custody_vaults` without withdrawing anything from the engine. Measured: fee
17 808 579, caller wallet +10 685 147, engine vault unchanged at 506 000 000 000, `custody_vaults`
505 989 314 853. Σ wallets + Σ engine vaults exceeded the token balance by the reward after every liquidation.
No baseline test ran `liquidate` through the contract. Caught by `mixed_scenario_with_liquidation_custody_holds`.
Also seen: an unregistered caller gets a wallet entry created without paying storage.

**COV-2, HIGH, DESIGN. An opening fill is refused while any other positioned account is unrefreshed in the current
block.** Baseline, market 0: T1 opens; one block later T2's open fails with `Engine(LockActive)`. Same-block
second open works; `keeper_step` on T1 unblocks it; `keeper_step` on the pool does not; closes, margin
withdrawals and LP calls are not affected. Every open therefore needs one `keeper_step` per positioned account in
the same block, which grows with open interest and lets idle position holders stall new opens. No baseline test
has two traders. I did not keep a failing test for it: the harness works around it (`World::refresh_others`).

**COV-3, UNKNOWN severity, not root-caused. `Engine(Stale)` refusals on fills, including a reduce-only close.**
See fuzz limits above. If a trader can be left unable to close until some keeper call happens, that is a
liveness bug; I have a failing trace but no minimal reproduction.

**COV-4, MEDIUM, process. Docs claim verification that does not exist.** Invariant 1 "checked after every call in
tests" (it was 4 call sites); README "Custody invariant … Kani-proved per operation" (the proofs do not cover the
engine link that COV-1 broke); README sandbox flow "keeper liquidates" (no liquidation in `sandbox/tests/gas.rs`);
audit "V >= C_tot + I checked in tests after every operation" (only in `exchange_flow.rs`, engine level).

**COV-5, LOW, DESIGN. Rounding dust goes to protocol, spec says insurance** (§4 item 1). No test written; decide
and fix the spec or the code.

Not written (covered by other reviewers' `poc_*` files, or out of time): `on_price_read` callback tests, oracle
future-timestamp, conditional arm-block, property tests for `dynamic_spread_bps` and `ema_next`.
