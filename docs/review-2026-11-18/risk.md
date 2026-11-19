# Risk review: trading, margin, liquidation, funding, pricing (wrapper)

Scope: `crates/neap/src/{contract,exchange,policy,model}.rs` as they configure and call the vendored engine.
Engine math itself was treated as verified. All PoC tests are new files `crates/neap/tests/poc_risk_*.rs`;
each asserts the CORRECT behaviour and therefore FAILS today. No existing file was changed.

Test parameters used by every PoC are the repo's own (`contract_unit.rs::params()`): IM 10%, MM 5%, trading fee
10 bps, liquidation fee 50 bps, 30 bps/slot staircase, `max_accrual_dt_slots` 10, funding cap 100e-9/slot,
`h_min` 5, `h_max` 50. Note these are NOT the spec §5 numbers (MM 8%, liquidation fee 5%); where the spec
numbers change the size of a problem it is said so.

Severity summary

| ID | Sev | Tag | Title |
|---|---|---|---|
| RISK-1 | high | BUGFIX | Liquidation reward is paid without leaving the engine vault (custody diverges) |
| RISK-6 | high | DESIGN | F-1: profit strands after `h_max`; the documented keeper mitigation cannot exist on-chain; every keeper-filled TP strands |
| RISK-7 | high | DESIGN | No default OI caps: one taker can lever the LP pool to 1/IM, and the pool is then liquidatable for a bounty |
| RISK-8 | high | DESIGN | NearPool markets fill at a 5% EMA of spot: a lagging mark anyone can trade against |
| RISK-5 | medium | BUGFIX | `keeper_step` liquidates on an hours-old price (bypasses audit fix H-2) and pays no reward |
| RISK-2 | medium | BUGFIX | Dynamic spread never sees skew (`oi_long == oi_short` always in pool-to-peer) |
| RISK-3 | medium | BUGFIX | OI caps test the pool's mirror leg: skew-reducing opens and plain closes are refused at the cap |
| RISK-4 | medium | BUGFIX | `catch_up` silently stops after 8 chunks; fills run with the engine clock in the past and funding goes uncharged |
| RISK-11 | medium | DESIGN | Funding is normalised by OI, so any one-sided book pays the cap rate regardless of size |
| RISK-12 | medium | DESIGN | Leverage ratchet (spec §5) is not implemented; `ratcheted_leverage_max_bps` is dead code |
| RISK-13 | low | BUGFIX | Wrapper `effective_position_q` disagrees with the engine (floor vs ceil, ignores epoch reset) |
| RISK-9 | low | BUGFIX | `size_q > i128::MAX` wraps: a "Long" order sells |
| RISK-10 | low | BUGFIX | `quote` fee is floored, the engine charges ceil; quote also ignores pending catch-up |
| RISK-14 | low | DESIGN | Liquidation incentive disappears exactly for the worst accounts |

---

## RISK-1 (high, BUGFIX) Liquidation reward is paid without leaving the engine vault
- Where: `crates/neap/src/contract.rs:1539-1548` (`liquidate`).
- Scenario: any liquidation with `fee_charged > 0`. The engine moves the whole fee from the account's capital
  into its own insurance. The wrapper then calls `vault_out_to_wallet(caller, 60%)`, which only lowers the
  wrapper's `custody_vaults` counter and credits the caller's wallet. The engine still counts 100% of the fee
  as insurance. Observed after one liquidation: engine vault 1,005,000,000,000, custody vaults
  1,004,994,658,853; the gap is exactly the caller share (5,341,147). The same atoms are now claimable twice
  (caller wallet, and insurance: bad-debt cover or `claim_fees`). Invariant 1 breaks, the gap grows with every
  liquidation, and the last withdrawers hit the `custody` panic or a short token balance.
- Self-liquidation is not profitable by itself (the account pays 100% of the fee, the caller gets 60%), so
  this is an accounting hole, not a direct drain.
- Confidence: proved by run. PoC: not duplicated; the NEAR reviewer already added
  `crates/neap/tests/poc_near_liq_reward_custody.rs` for the same root cause.
- Fix: before `store_group`, call `exchange::withdraw_insurance_surplus(&mut g, caller_share)` and pay the
  caller only what was actually withdrawn.

## RISK-6 (high, DESIGN) F-1 mitigation is not implementable as written; keeper-filled take-profits always strand
- Where: `crates/neap/src/contract.rs:1336-1348` (`convert_pnl`, owner-only, 1 yocto); `docs/contract.md:160,223`
  and `docs/decisions.md` D14 promise a keeper-callable `convert_pnl_for`, which does not exist.
  `keeper_step` (`contract.rs:1498`) does not convert either.
- Scenario: bob is long 10 with a resting take-profit and is offline. The keeper fills the TP (profit
  29.999 USDC). The profit is convertible only between `h_min` and `h_max` slots after the close: with the
  repo params that is slots 5 to 50, about 3 s to 30 s. Bob returns after 60 s: `convert_pnl` panics with
  `Engine(LockActive)`. Same after 2,000 blocks with the keeper stepping his account. Control: converting 10
  blocks after the fill works. So every TP, SL-in-profit or limit close that fills while the owner is away
  strands the gain, and so does a manual close if the user's second transaction is late. This is worse than
  the documented "frontend prompts, keeper converts".
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_f1_conditional.rs` (fails today).
- Fix options (founder decides): (a) add permissionless `convert_pnl_for(market, account)` and have
  `execute_order` / `execute_conditional` / `keeper_step` call conversion when the account is flat and matured
  (crates/SECURITY.md notes this is value-neutral for the owner only when h = 1, so it needs that check);
  (b) raise `h_max` to days so the window is usable; (c) the engine change in SECURITY.md option (c).
  At minimum the spec and D14 must stop claiming the keeper covers it.

## RISK-7 (high, DESIGN) No default OI caps; a taker can lever the LP pool to 1/IM and the pool is liquidatable for a bounty
- Where: caps default to 0 = uncapped at both creation paths (`contract.rs:305-307`, `448-450`); only the owner
  can set them (`set_oi_caps`, `contract.rs:1281`); nothing ties them to pool capital although spec §5 says
  `min(seed / max_adverse_move, 2% × pool_depth)`. `liquidate`/`keeper_step` accept the contract's own account
  id, i.e. the pool (`contract.rs:1521-1534`; deliberate per SECURITY.md W-1).
- Scenario (run): pool capital 1,000 USDC, no caps. Bob opens long 99 units at $100 (9,900 notional, 9.9x the
  pool); the engine accepts because the pool is margined at the same IM as a trader. Price +6%: pool loss 594.
  Carol calls `liquidate(0, neap.near)`: 24.88 units of the pool's short are closed by ADL against bob, the
  pool pays a 13.19 USDC fee, carol is credited 7.91, pool capital is 392.8. With the spec's 5% liquidation
  fee the same call takes about 132 USDC, a third of what the LPs had left, and 60% of it goes to whoever
  calls first, which can be the trader who built the position. The pool falls under maintenance after a
  move of only (IM − MM), 2% with spec params.
- Confidence: proved by exploratory run (numbers above); no PoC file because the "correct" bound is a
  founder decision.
- Fix: enforce a cap derived from pool NAV on every fill (for example pool notional ≤ NAV / max_adverse_move),
  set non-zero caps at creation, and decide whether a pool liquidation should pay a caller bounty at all.

## RISK-8 (high, DESIGN) NearPool markets fill at a 5% EMA of spot
- Where: `contract.rs:883-897` (`on_price_read`: `oracle_target = ema_e6`), `policy.rs` `EMA_ALPHA_BPS = 500`,
  fills at engine effective price ± spread (`exchange.rs:391-393`). Major-tier spread tops out at 55 bps.
- Scenario: the DEX pool moves +10% (under the 20% band). After the next observation the mark is +0.5%;
  it needs about 14 observations to cover half the move and about 60 to cover 95%. Anyone buys at
  mark + ≤55 bps, calls the permissionless `refresh_price` each block to walk the EMA up, then sells: a
  near-riskless gain paid by LPs, bounded only by OI caps (unset by default, RISK-7). The two-step order flow
  does not help because the lag persists across many observations. Liquidations also run on the lagged mark.
- Confidence: traced by reading; not reproduced (needs the callback mock; the oracle reviewer's
  `poc_oracle_nearpool.rs` exercises that path).
- Fix: use the smoothed price only as a manipulation guard (fill at spot when |spot − EMA| is inside a band,
  refuse opens outside it), or make opens pay the worse of spot and EMA.

## RISK-5 (medium, BUGFIX) `keeper_step` liquidates on an hours-old price and pays no reward
- Where: `contract.rs:1498-1517`; compare the staleness gate in `liquidate` at `contract.rs:1525-1531` (audit H-2).
- Scenario: bob long 400, mark 92, oracle silent for 2 h while the keeper cranks. `liquidate` refuses
  ("price uncertain"). `keeper_step(0, bob)` returns `RefreshAccount`, the second call returns `Liquidate`
  and bob's position goes 400 → 370.2 at the 2-hour-old mark. The whole fee stays in insurance and the caller
  gets nothing, so the "liquidator 60%" rule depends on which entry point was used. `keeper_step` also has no
  market-mode check.
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_keeper_step_stale.rs` (fails today).
- Fix: apply the same staleness gate before `auto_crank` when the engine is Live (keep the Recovery path
  reachable), and either pay the same split when the selected action is `Liquidate` or document that
  `keeper_step` is unrewarded.

## RISK-2 (medium, BUGFIX) Dynamic spread never sees skew
- Where: `contract.rs:1681-1712` (`compute_spread`), `policy.rs:212-229` (`dynamic_spread_bps`).
- Scenario: the pool is the counterparty to every fill, so engine `oi_eff_long_q == oi_eff_short_q` at all
  times (observed in every run). `net_oi_after = |oi_long + size − oi_short|` is therefore just `size`.
  With cap 100: a long of 50 from flat quotes 29 bps; with the pool already short 50 a further long of 50
  quotes 29 bps and a short of 50 that removes the skew also quotes 29 bps. Spec §5 says skew-reducing trades
  pay only `spread_min`. Because both terms scale with the order's own size and are floored twice, clips of
  1% of the cap pay `spread_min` only: after 40 such clips (book 40% one-sided) the 41st still pays 5 bps.
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_spread_skew.rs` (2 tests, fail today).
- Fix: compute skew from the pool's signed position after the fill (`|pool_pos − size_signed|`), charge
  `spread_min` when that magnitude falls, and round the bps terms up. The audit's M-6 (reduce-only overcharged)
  is a symptom of this; the same root cause also undercharges skew-increasing flow, which M-6 did not state.

## RISK-3 (medium, BUGFIX) OI caps test the pool's mirror leg
- Where: `contract.rs:700-728` (`execute_order`). Related: `execute_conditional` (`contract.rs:1055-1187`) has no
  cap check at all, long/short/per-account (PoC by the oracle reviewer:
  `poc_oracle_conditional_gates.rs::opening_conditional_must_respect_oi_cap`).
- Scenario: caps 10/10, bob long 10, so the pool is short 10 and engine short OI is 10. Carol opens a short of
  5, which would cut the pool's exposure in half: refused with "short OI cap exceeded" (10 + 5 > 10). Bob
  closing with a plain sell (not flagged reduce-only) is refused the same way. So at the cap the market
  refuses exactly the flow that de-risks the pool, the two "per-side" caps both act on max(longs, shorts),
  and the per-account check adds a closing order's full size too.
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_oi_cap_side.rs` (2 tests, fail today).
- Fix: cap the pool's net exposure after the fill (`|pool_pos_after| ≤ cap` for the side the pool would hold),
  skip the check when the fill lowers it, and run the same function from `execute_conditional`.

## RISK-4 (medium, BUGFIX) `catch_up` stops after 8 chunks and callers proceed anyway
- Where: `exchange.rs:293-350` (`MAX_CATCH_UP_CALLS = 8`, each chunk ≤ `max_accrual_dt_slots`), no caller checks
  `slot_last == now`; `ExError::OracleNotCaughtUp` (`exchange.rs:43`) is declared and never used.
  `has_lag` (`exchange.rs:360`) compares prices only.
- Scenario: bob long 10, market untouched for 5,000 blocks (50 min; 80 blocks is enough with the repo params,
  160 with the spec's window of 20). The oracle push and `execute_order` each advance 80 slots, then the fill
  runs with engine slot 261 at block height 5102. Bob pays funding for 160 slots instead of 5,000 and exits;
  the backlog is later accrued against whatever the book is then (a flat book pays nothing, and a trader who
  joins the minority side just before the catch-up collects funding for time he was not in the market).
  Warmup and backing clocks are behind as well.
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_catch_up_backlog.rs` (fails today).
- Fix: have `catch_up` report whether it reached `now_slot`; fills, withdrawals, conversion, LP ops and
  liquidation return `OracleNotCaughtUp` when it did not; only `crank`/`push_price` may store partial progress.

## RISK-11 (medium, DESIGN) Funding pays the cap rate for any one-sided book
- Where: `exchange.rs:247-256`, `policy.rs:61-78`.
- Scenario: rate = cap × |pool_net| / max(oi_long, oi_short). Since the pool mirrors the traders, this is
  cap × |L − S| / max(L, S): 100% of the cap whenever traders are all on one side, whether that is 10 units
  or the whole OI cap against a 1M pool (observed: `funding_rate_e9_per_slot = 100` = cap with 10 units
  open). With the repo cap (100e-9 per slot, 0.6 s slots) that is about 1.44% per day on notional for the
  first trader in any market. The rate does not grow with how much risk the pool is actually carrying.
  Sign and direction are correct (checked below).
- Confidence: proved by run (rate), arithmetic for the annualisation.
- Fix: normalise by the OI cap or by pool NAV instead of current OI, and pick the cap from a target APR.

## RISK-12 (medium, DESIGN) Leverage ratchet is not implemented
- Where: `policy.rs:235-243` (`ratcheted_leverage_max_bps`) has no caller; spec §5 states
  `Lmax = max(1, L0·(1 − skew/OI_cap))`.
- Scenario: at 99% of the cap on the crowded side a new account still opens at full 10x.
- Confidence: traced by reading (grep: no call site).
- Fix: either enforce it in the fill path as a post-trade margin check on the taker, or delete it from the spec.

## RISK-13 (low, BUGFIX) Wrapper `effective_position_q` disagrees with the engine
- Where: `exchange.rs:221-244` vs engine `effective_abs_quantity_for_leg` (`crates/engine/src/v16.rs:1695-1717`).
- Scenario: the wrapper floors and ignores `epoch_snap`; the engine uses ceil and returns 0 for a leg from a
  prior epoch. After the ADL in the RISK-7 run bob shows 74,119,453 while the pool shows −74,119,454. After a
  full side reset the wrapper would report a phantom position: views are wrong, reduce-only validation
  passes for a position that no longer exists, and `funding_rate_e9` is computed from a phantom pool skew
  until the pool is touched.
- Confidence: rounding difference observed; the epoch case is traced by reading, not reproduced.
- Fix: expose and call the engine's own function (or mirror its epoch check and rounding).

## RISK-9 (low, BUGFIX) Order size above `i128::MAX` wraps
- Where: `contract.rs:685-689`, `1133-1137` (`order.size_q as i128`); `request_order` / `place_conditional` only
  require `size_q > 0`.
- Scenario: bob long 10 submits a `Long` order with `size_q = 2^128 − 3,000,000`; it fills as a SELL of 3 units
  (position 10 → 7), and the event says `side: Long`. It does not bypass caps (the unsigned size saturates the
  cap check when caps are set) or reduce-only, so impact is mislabelled orders and indexer confusion.
  `size_q == 2^127` panics on negation.
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_size_wrap.rs` (fails today).
- Fix: `require!(size_q <= MAX_POSITION_ABS_Q)` at request/placement.

## RISK-10 (low, BUGFIX) `quote` does not return what will fill
- Where: `contract.rs:946-972`.
- Scenario: fee is floored in `quote` and rounded up by the engine: size 1,234,567 quotes 123,518 and is
  charged 123,519; size 7 quotes 0 and is charged 1. `quote` also reads the stored effective price without
  the pending staircase step, always prices as non-reduce-only, truncates the fee with `as u64`, and returns
  price 0 instead of an error when the price is out of range.
- Confidence: proved by test. PoC: `crates/neap/tests/poc_risk_quote_fee.rs` (fails today).
- Fix: ceil the fee, return `U128`, and say in the doc that the price is pre-catch-up.

## RISK-14 (low, DESIGN) Liquidation incentive vanishes for the worst accounts
- Where: `contract.rs:1541-1548`.
- Scenario: the reward is 60% of the fee the engine managed to charge. An account with no capital left
  (or in the engine's `liquidation_lock` path) is charged 0, so the caller earns nothing for closing the
  positions that create bad debt; `min_liquidation_abs` is 0 in the repo params. Combined with RISK-5 a
  keeper using `keeper_step` is never paid.
- Confidence: traced by reading.
- Fix: a small fixed bounty from insurance for bankrupt closes, or accept and document reliance on the team keeper.

---

## Cross-references (found here, owned by another reviewer; not re-reported)
- `execute_conditional` has no market-mode check and no OI-cap check; `execute_order` only checks
  `mode == ReduceOnly` (`poc_access_modes.rs`, `poc_oracle_conditional_gates.rs`).
- Permissionless `CreateMarket` does not enforce tier bounds on IM/MM/fees/funding cap, `Tier::Major` needs no
  bond, the bond is credited to the lister's wallet (`poc_access_listing.rs`). For this area it means a lister
  chooses the liquidation fee, funding cap and spread (0 allowed) of a market that third-party LPs fund.
- The LP's 30% and insurance's 10% trading-fee ledgers only accumulate; nothing moves the LP share into pool
  NAV (accounting reviewer).

## Checked and found correct
- `pool_exec_price`: moves and rounds against the taker (buy ceil, sell floor), no overflow at `MAX_PRICE`,
  spread capped at 10%. `within_limit`: correct both directions.
- Funding sign and payer: pool short (traders net long) gives a positive rate, longs pay, the pool receives
  (run: 160 slots × 100e-9 × 1,000 USDC notional = 16,000 atoms added to pool NAV). Zero OI, zero skew and a
  zero cap give rate 0; magnitude is clamped to the cap; the rate is never caller-supplied.
- Funding is accrued before the pool's position changes (catch-up precedes the fill), apart from RISK-4.
- Reduce-only validation cannot flip or grow a position (`size ≤ |pos|` and opposite sign), in both
  `execute_order` and `execute_conditional`.
- Only the taker pays the trading fee; the pool pays none; the fee equals the engine's charge and lands in
  engine insurance; spread capture goes to the pool through the fill price.
- `to_engine_config`: `h_min == 0` refused; the engine re-validates shape and the exact solvency envelope
  (`validate_public_user_fund`), so the envelope inequality cannot be bypassed from the wrapper.
- Liquidation: eligibility, partial vs full size and fee rate are engine-selected from config, never caller
  input; partial liquidation observed (400 → 380.6); fee = ceil(closed notional × fee bps); the caller share is
  computed from the fee actually charged; self-liquidation loses 40% of the fee.
- Trades are refused while the effective price lags the target (`has_lag`); liquidation proceeds on the
  staircase price, which is the engine's design.
- `dynamic_spread_bps`, `ratcheted_leverage_max_bps`, `skew_funding_rate_e9`: no overflow or division by zero
  for any input; results clamped.
- No float arithmetic anywhere in the wrapper; release profile has `overflow-checks = true`.

## Not verified
- RISK-8 was not reproduced end to end.
- RISK-13's epoch-reset case was not reproduced (needs a full side ADL).
- Behaviour with the spec §5 parameter set (MM 8%, 5% liquidation fee, window 20) was reasoned from the
  repo-parameter runs, not re-run.
- Engine Recovery/Resolved modes through the wrapper were not exercised.
