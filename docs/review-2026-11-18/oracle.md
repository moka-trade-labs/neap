# Oracle and order lifecycle review (ORA)

Scope: `crates/neap/src/contract.rs`, `exchange.rs`, `policy.rs`, `model.rs` (oracle, two-step orders, conditionals).
Status: FINAL. 13 PoC tests, all failing today as intended. Baseline suites untouched and green (`contract_unit` 35, `exchange_flow` 10, `policy_props` 3).

PoC files (new, each test asserts correct behaviour and FAILS today):
- `/home/user/neap/crates/neap/tests/poc_oracle_conditional_gates.rs` (5 tests, 5 fail)
- `/home/user/neap/crates/neap/tests/poc_oracle_nearpool.rs` (3 tests, 3 fail)
- `/home/user/neap/crates/neap/tests/poc_oracle_stale_and_timing.rs` (5 tests, 5 fail)

Note on the PoCs: "refused" is detected with `catch_unwind`, so a test passes once the call panics for any reason. When fixing, check the panic message is the intended one. The NearPool tests call the `on_price_read` callback directly with a mocked `get_pool` result; no real DEX is involved.

---

## ORA-1 (high, BUGFIX) Opening `Limit` conditionals bypass OI caps and the per-account cap
- Where: `contract.rs:1055-1187` (`execute_conditional`); compare the cap block in `execute_order` at `contract.rs:700-728`.
- Scenario: caps set to 5 units per side, 2 per account. Trader places `Limit`, `Long`, `reduce_only=false`, size 40, `trigger_price = u64::MAX`, `limit_price = u64::MAX`. `mark <= trigger` is always true, so after the next observation anyone executes it: a 40-unit open fills. The conditional is a market order with no cap check.
- Confidence: proved by test `opening_conditional_must_respect_oi_cap`.
- Fix: move mode, staleness, reduce-only, OI cap and per-account cap checks into one shared fill helper used by both `execute_order` and `execute_conditional`.

## ORA-2 (high, BUGFIX) Market modes are not enforced on fills
- Where: `contract.rs:659` (`if info.mode == Mode::ReduceOnly && !order.reduce_only`), and no mode check anywhere in `execute_conditional` (`contract.rs:1055-1187`). `place_conditional` (`contract.rs:1003`) accepts an opening `Limit` while the market is already `ReduceOnly`.
- Scenario A: market is `ReduceOnly` (admin or band trip). Trader places an opening `Limit` conditional with an always-true trigger and opens a position. Prior audit H-3 was fixed for `execute_order` only.
- Scenario B: pauser escalates to `PriceUncertain`. The check is `== ReduceOnly`, so a pending opening order that would have been blocked in `ReduceOnly` now fills. Tightening re-enables opens. Same for `Recovery`, `WindDown`, `Closed`.
- Confidence: proved by tests `opening_conditional_must_not_fill_in_reduce_only_market`, `opening_order_must_not_fill_in_price_uncertain_market`.
- Fix: fills allowed only in `Active`, or in `ReduceOnly` when the order is reduce-only; nothing in any other mode. Apply in the shared helper. Reject non-reduce-only placement in `ReduceOnly`.

## ORA-3 (critical for NearPool markets, DESIGN) The mark is a per-sample EMA of a spot reserve ratio, with unthrottled permissionless sampling
- Where: `contract.rs:796-813` (`refresh_price`, no rate limit), `contract.rs:851-895` (`on_price_read`: spot `usdc/token` reserves, `oracle_target = ema`), `policy.rs:186-206` (`ema_next`, 5% per sample, no time term).
- Scenario 1 (manipulation): attacker moves pool spot +15% (inside the 20% band) and sends one transaction with 100 `refresh_price` actions. All 100 callbacks read the same manipulated reserves; the mark target moves 1490 bps. The band then re-centres on the new EMA, so the walk can be repeated. The EMA provides no manipulation resistance; cost is holding a thin pool (depth floor 1,000 USDC long-tail, 10,000 USDC major, quote side only) off-price for about two blocks.
- Scenario 2 (no manipulation needed): a real +5% move. With few refreshes the mark stays near the old price while staleness reads `Fresh`. Trader opens long at the lagging EMA, then calls `refresh_price` about 60 times so the EMA converges, then closes. Profit is about the move minus spreads, paid by the LP pool. The trader controls both when the mark is stale and when it catches up.
- Confidence: Scenario 1 proved by test `ema_must_not_converge_to_spot_within_one_block`; Scenario 2 traced by reading.
- Fix (founder decision): time-weighted accumulator (TWAP over N blocks, at most one sample per block), minimum liquidity re-checked on every observation, refuse opens when `|spot - mark|` exceeds a bound, and consider a second source cross-check for long-tail.

## ORA-4 (high, BUGFIX for the recovery path, DESIGN for band policy) A sustained move over 20% permanently kills a NearPool market's oracle
- Where: `contract.rs:858-881`. On a band breach the reading is dropped, the EMA is not updated, mode becomes `ReduceOnly`, and there is no way back.
- Scenario: token reprices to 1.30 and stays. Every later reading is also over 20% from the frozen EMA, so every reading is dropped. After 1 h the observation is `PriceUncertain`: `execute_order`, `execute_conditional` and `liquidate` all panic. Positions cannot be closed or liquidated; the pool carries unbounded unrealised exposure. Because refreshes are on demand, a quiet market can drift 20% with no manipulation at all. An attacker can also trip it deliberately on a thin pool for a few dollars.
- This is worse than acknowledged M-1 ("band breach -> ReduceOnly with no recovery path"): M-1 describes a stuck mode; the actual result is a permanently dead oracle and trapped positions, triggerable by anyone.
- Confidence: proved by test `oracle_must_recover_after_a_sustained_real_move` (120 consistent readings over 2 h all dropped, observation 7200 s old).
- Fix: keep recording out-of-band readings as a pending candidate and accept the new level after K consecutive consistent readings over a minimum time; add an owner or timed restore path out of `ReduceOnly`.

## ORA-5 (high, DESIGN) The lister chooses the `dex` account; nothing restricts it to Ref/Rhea
- Where: `contract.rs:391-451` (`CreateMarket` stores caller-supplied `price_source`), `contract.rs:799-804` (calls `get_pool` on that account), `contract.rs:400-403` (`Tier::Major` minimum bond is 0), `contract.rs:915-941` (activation checks only one fresh reading and a self-reported depth).
- Scenario: attacker sends 1 USDC atom with `CreateMarket{tier: Major, dex: attacker-oracle.near}`. Their contract answers `get_pool` with any reserves. `activate_market` succeeds; the market is `Active`, labelled Major, priced entirely by the attacker, with OI caps at 0 (uncapped). Any LP who deposits is the counterparty to an attacker-set price.
- Related: even with a real DEX, depth is a single reading at activation (flash liquidity passes it) and is never re-checked.
- Confidence: proved by test `market_priced_by_lister_controlled_contract_must_not_activate`.
- Fix (founder decision): owner-managed allowlist of DEX accounts; verify the pool contains the quote token and is a simple pool; require a non-zero bond for every permissionless tier or make Major owner-only; re-check depth on every observation.

## ORA-9 (medium, BUGFIX) TP/SL sized for the original position can never fire after a partial close
- Where: `contract.rs:1138-1146` (reduce-only check requires `size <= |position|`, panics otherwise).
- Scenario: long 10 with a stop-loss to sell 10. Trader closes 3 by hand. Price falls through the stop; `execute_conditional` panics "conditional does not reduce position" until expiry (up to 30 days). The trader believes they are protected.
- Confidence: proved by test `stop_loss_must_still_fire_after_partial_close`.
- Fix: for reduce-only orders clamp the fill to the remaining position; if the position is flat or flipped, remove the conditional and emit `order_cancelled`.

## ORA-10 (low, BUGFIX) Buy-side conditional with `limit_price = 0` is accepted but can never fill
- Where: `contract.rs:979-1050` has no equivalent of the `request_order` check at `contract.rs:578-581`; `model.rs:223` documents `0 = market for sells` only.
- Scenario: stop-loss for a short (side `Long`) placed with limit 0. `within_limit` needs `exec <= 0`; the stop is dead and the storage deposit is spent.
- Confidence: proved by test `buy_conditional_with_zero_limit_must_be_rejected_at_placement`.
- Fix: reject at placement.

## ORA-6 (high, DESIGN) The two-step flow does not stop price picking: the fill is not bound to the first post-request observation, and "newer" is measured in blocks, not price time
- Where: `contract.rs:652-656` and `contract.rs:1076-1080` (only `observation.block > request_block`); `contract.rs:630` (anyone, including the requester, executes at any time up to 50 blocks); `contract.rs:566-625` (no margin escrow although spec section 4 says `request_order` "escrows margin"; cancel and expiry are free).
- Scenario A (standing option): trader keeps one request alive (re-request every 50 blocks, gas only). An observation lands one block later. From then on the trader can fill instantly at the last on-chain price whenever the real market has moved away from it and the observation is still `Fresh` (under 10 s). That is the stale-price pick the two-step was meant to prevent (D13). Proved: `fill_must_bind_to_the_first_post_request_observation` (executed 14 blocks after the only observation).
- Scenario B (pre-request price): `push_price` accepts `publish_ns` up to `max_oracle_age_ns` old. A price published 5 s before the request, landing one block after it, satisfies invariant 3. Anyone watching the same off-chain feed knows that price before requesting. Proved: `fill_must_not_use_a_price_published_before_the_request`.
- Scenario C (traced): for NearPool markets the trader triggers the observation themselves with `refresh_price`, so they choose when and against which pool state the "newer" observation is taken (see ORA-3).
- Fix (founder decision): store `request_ts`/`request_seq`; fill only against the first observation with `source_ts > request_ts` (or `seq == request_seq + 1`), within a short execution window, else expire; escrow margin or charge a cancel/expiry fee so the option is not free.

## ORA-7 (high, BUGFIX) The stale ladder is honoured only by `execute_order`, `execute_conditional` and `liquidate`
- Where: no staleness check in `withdraw_margin` (`contract.rs:546-558`), `convert_pnl` (`contract.rs:1338-1348`), `lp_deposit` (`contract.rs:1354-1389`), `lp_redeem` (`contract.rs:1394-1420`), `keeper_step` (`contract.rs:1498-1517`). `has_lag` does not help: when the oracle is simply dead, effective price equals the last target.
- Scenario: oracle silent for 2 h, real price has moved against a long. The trader withdraws free margin computed at the old mark, leaving the loss to the pool when the price resumes. An LP who knows the pool is about to be marked down redeems at the old NAV and leaves the loss to the remaining LPs. `lp_deposit` and `convert_pnl` have the mirror problem.
- Confidence: `withdraw_margin` and `lp_redeem` proved by tests `withdraw_margin_must_honour_stale_ladder`, `lp_redeem_must_honour_stale_ladder`. `convert_pnl`, `lp_deposit` traced by reading. `keeper_step`: no gate in the code, but I could not demonstrate a stale liquidation; after a 1 h gap the engine returned `InvalidConfig` on the second step, so this one is suspected only.
- Also `liquidate` uses the ladder at `< PriceUncertain`, so it liquidates at prices up to 59 minutes old. That matches the spec wording; flagging it for a decision, not as a bug.
- Fix: one `require_price_usable(market, action)` helper called by every value-moving method. Withdraw/convert/LP need at least "not ReduceOnly-stale" while the account or pool has exposure; flat accounts can always withdraw.

## ORA-8 (medium, DESIGN) `push_price` is a single trusted account with no source, band or signature check
- Where: `contract.rs:1438-1487`, `contract.rs:314-317` (`set_oracle`, instant, acknowledged as M-4).
- Facts: no check that the market's `price_source` is `Pushed`, so the oracle account can also overwrite the price of an activated `NearPool` market (against invariant 9 in spirit). No deviation band or per-update cap; the only limit is the engine staircase, and trades are blocked while it lags. No mode check. There is no on-chain signature or attestation verification anywhere: `submit_price` and `sync_tee_keys` from spec section 4 do not exist, so `Tee` markets can be created but can never get an observation with depth and never activate.
- Scenario: oracle key compromised; attacker walks any market's mark anywhere within the staircase rate and liquidates or drains against the pool.
- Confidence: traced by reading.
- Fix: restrict `push_price` to `Pushed` markets, add a band against the EMA, and decide the TEE verification timeline (this is the stated trust model, hence DESIGN).

## ORA-11 (low, BUGFIX) Order storage accounting
- Where: `contract.rs:566-625` (`request_order` stores 115 bytes for 1 yocto; the contract pays), `contract.rs:1035` vs `contract.rs:1171`, `1202`, `1065` (conditional storage deposit is charged and never refunded on execute, cancel or expiry).
- Scenario: an account with a margin account loops `request_order`; each call locks contract NEAR until someone pays gas to expire it. No per-account order limit. Honest users lose their conditional storage deposit.
- Confidence: proved by test `request_order_must_pay_for_its_storage`; refund gap traced by reading.
- Fix: charge storage on request, refund the owner on removal, cap live orders per account.

## ORA-12 (low/info, BUGFIX) Smaller oracle issues, traced by reading
- Catch-up after an outage advances at most 80 slots per transaction (`exchange.rs:36`, `MAX_CATCH_UP_CALLS` x `max_accrual_dt_slots`), and nothing checks that the engine clock reached the current block: `ExError::OracleNotCaughtUp` (`exchange.rs:43`) is defined and never used. In the ORA-7 test the withdrawal succeeded with the engine about 11,900 slots behind.
- `on_price_read` ignores `pool_kind` (stable and rated pools do not price as `usdc/token`), uses only the first two tokens, and ignores `quote_pool` (non-USDC pools silently produce no observation). The first observation has no band check, so the acknowledged L-4 claim that "the band catches extreme decimals errors" does not hold for it: an 8-decimal token lists at 1/100 of its price; 18 and 24-decimal tokens round to 0 and never activate.
- `refresh_price` works in any mode and for `Proposed` markets; fine, but together with ORA-3 it needs a per-block limit.

## Checked and found correct
- `push_price`: caller must be the oracle account; `publish_ns` strictly increasing; future skew capped at 2 s; too-old updates rejected; a rejected update panics and changes nothing.
- `ema_next`: no overflow, bounds checked, first sample seeds the EMA.
- `on_price_read`: `#[private]`; a failed promise or malformed pool leaves state unchanged; never touches engine state (C-2 fix holds).
- Lag gate: `trade_against_pool`, `withdraw`, `convert_pnl` and `lp_deposit` refuse while the engine price has not reached the target, so a known-lagging staircase price cannot be traded or withdrawn against. `lp_redeem` is covered through `exchange::withdraw` when assets are non-zero.
- Same-block execution is refused for orders and conditionals; expired orders are removed without a panic; only the owner can cancel; ids come from one shared counter and are never reused.
- Trigger directions are right for all six kind/side combinations. TP and SL are forced reduce-only. A reduce-only order cannot flip or grow a position (flat or flipped position makes it revert).
- Limit semantics: buys fill at or below the limit, sells at or above, after spread, rounded against the taker.
- A third party executing someone else's order cannot change its price or size; they only choose timing inside the owner's limit (see ORA-6 for the timing risk).
