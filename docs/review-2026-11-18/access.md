# Access control, market lifecycle, pause paths, upgrade safety (reviewer: access)

Scope: crates/neap/src/{contract.rs, model.rs, exchange.rs, policy.rs}, docs/contract.md, docs/decisions.md,
docs/security-audit-2026-11-12.md, crates/SECURITY.md, CONTRACT.md, deploy/. Engine internals treated as verified.
PoC tests (new files only, all assert the CORRECT behaviour, so all 13 FAIL today):
- /home/user/neap/crates/neap/tests/poc_access_modes.rs   (6 tests, 6 fail)
- /home/user/neap/crates/neap/tests/poc_access_listing.rs (7 tests, 7 fail)
Run: `cargo test -p neap --test poc_access_modes --test poc_access_listing`. No existing file was modified.

## (a) Method x caller x mode table

Checks: O = assert_owner, P = pauser-or-owner, Or = predecessor == oracle, QT = predecessor == quote token,
priv = #[private], 1y = assert_one_yocto (exactly 1), >=1y = at least 1 yocto + storage charged, U = `user()`
(rejects the contract account), st = storage charged to caller. Mode columns: Y allowed, N refused, "-" n/a.
Modes Recovery / WindDown / Closed exist in model.rs but NO code path ever sets them (unreachable).

| Method (contract.rs line) | Caller / checks | Proposed | Active | ReduceOnly | PriceUncertain (admin) | Notes |
|---|---|---|---|---|---|---|
| `new` (229) | anyone, once (#[init] refuses re-init); not #[private] | - | - | - | - | safe only if deploy+init are one tx |
| `create_market` (254) | O, st | creates Active | | | | Tier/params/source unchecked (ack. H-1) |
| `set_oracle` (314) | O, no 1y, no timelock | Y | Y | Y | Y | global oracle, see ACL-5 |
| `set_owner` (1331) | O, no 1y, one step | | | | | ACL-10 |
| `set_pauser` (1325) | O, no 1y | | | | | |
| `set_oi_caps` (1281) | O, no 1y; can RAISE or zero (=uncapped) | Y | Y | Y | Y | spec says pauser `lower_caps`, tighten-only; ACL-11 |
| `set_mode` (1298) | P, no 1y; only ->ReduceOnly / ->PriceUncertain | N | Y | Y (->PU) | N | nobody can loosen: ACL-2 |
| `claim_fees` (1219) | 1y, U; Lister: predecessor == lister; Protocol: O; Lp/Insurance refused | Y | Y | Y | Y | no mode gate (fine) |
| `register` (323) | anyone, st | - | - | - | - | |
| `open_account` (337) | U, st | Y | Y | Y | Y | |
| `ft_on_transfer` Wallet (360) | QT; sender must be registered else refund | - | - | - | - | malformed msg -> Wallet (ack. M-2) |
| `ft_on_transfer` CreateMarket (391) | QT; anyone; bond >= 50 USDC LongTail / >= 0 Major; spread <= 10% | creates Proposed | | | | ACL-3, ACL-4, ACL-7 |
| `withdraw` (485) | 1y, U | - | - | - | - | never pausable (good for exit; no emergency stop: ACL-12) |
| `on_withdraw` (510) | priv | | | | | |
| `deposit` (529) | 1y, U | N | Y | Y | **N** | ACL-9 |
| `withdraw_margin` (546) | 1y, U; mode != Proposed; lag gate; NO staleness gate | N | Y | Y | Y | ACL-8 |
| `request_order` (566) | 1y, U; needs portfolio | N | Y | Y | **N (even reduce-only)** | ACL-2 |
| `execute_order` (630) | anyone; obs.block > request_block; staleness ladder | - | Y | reduce-only only | **Y, opens too** | ACL-1 |
| `cancel_order` (770) | 1y, order owner | Y | Y | Y | Y | |
| `place_conditional` (979) | >=1y, U, st | N | Y | Y (opening Limit accepted) | **N (TP/SL too)** | ACL-2 |
| `execute_conditional` (1055) | anyone; obs.block > arm_block; trigger; staleness | - | Y | **Y, opens too** | **Y, opens too** | no mode check at all: ACL-1 |
| `cancel_conditional` (1191) | 1y, cond owner | Y | Y | Y | Y | |
| `refresh_price` (796) | anyone; NearPool only | Y | Y | Y | Y | |
| `on_price_read` (818) | priv | Y | Y (band breach -> ReduceOnly) | Y | Y | ACL-6 |
| `activate_market` (915) | anyone; Proposed, seq > 0, fresh, depth floor | Y | N | N | N | ACL-7 |
| `push_price` (1438) | Or; monotone ts, max age; NO price_source check | Y | Y | Y | Y | ACL-5 |
| `convert_pnl` (1338) | 1y, U; lag gate | Y | Y | Y | Y | |
| `lp_deposit` (1354) | >=1y, U, st; lag gate | Y | Y | Y | Y | no mode gate (ack. M-5) |
| `lp_redeem` (1394) | 1y, U; engine IM check on pool | Y | Y | Y | Y | no staleness gate (same class as ACL-8) |
| `insurance_deposit` (1424) | 1y, U; engine Live only | Y | Y | Y | Y | |
| `crank` (1490) | anyone | Y | Y | Y | Y | |
| `keeper_step` (1498) | anyone; any account incl. the pool | Y | Y | Y | Y | |
| `liquidate` (1521) | anyone (U for caller); staleness < 1 h; NO mode check | Y | Y | Y | Y | liquidations continue while exits are blocked |
| views (`get_*`, `quote`, `scan_*`) | anyone | Y | Y | Y | Y | |

Exit matrix (can a user always get out?):

| Exit | ReduceOnly | PriceUncertain (admin mode) | Observation >= 1 h old | NearPool after band breach |
|---|---|---|---|---|
| Close position | yes (reduce-only order) | **no, permanently** | no (until a new observation) | **no after 60 s..1 h, permanently** (no new observation can ever be accepted) |
| Withdraw free margin | yes | yes (only above IM while position open) | yes (ACL-8: too permissive) | yes |
| Add margin | yes | **no** | yes | yes |
| LP redeem | yes, if pool stays above IM | same; pool can never be flattened because traders cannot close | yes | same |
| Withdraw wallet | always | always | always | always |
| Liquidation | on | on | **off** | **off after 1 h, permanently** |

There is no wind-down / settlement path in the wrapper: nothing sets WindDown or Closed, nothing triggers engine
resolution, no forced-close at a settlement price, no bond return. A market can only ever get tighter.

## (b) Findings

### ACL-0 (critical, ops) Full-access private key and seed phrase of the contract account are committed in the repo
- Where: /home/user/neap/CONTRACT.md:17-27 (private key, seed phrase, import command). The same account is
  contract + owner + oracle (CONTRACT.md:11, :62).
- Scenario: anyone who can read the repo redeploys arbitrary code on `neap-trade.testnet`, pushes prices, creates
  markets, or moves the account's USDC directly. Testnet today, but docs/plan.md 1.12 is a mainnet deploy with the
  same procedure, and crates/SECURITY.md W-2 already says "delete the contract account's full-access keys".
  Violates AGENTS.md "Secrets never go in the repo".
- Confidence: read directly. I did not copy the key anywhere and did not test it.
- Fix: rotate/delete that key now and purge it from history; for mainnet use separate accounts for contract,
  owner and oracle, and keep no full-access key on the contract account unless it is the declared upgrade multisig.
- Tag: BUGFIX (ops hygiene; the key rotation itself needs the founder / key holder).

### ACL-1 (high) Market mode is not enforced on fills: opening orders execute in ReduceOnly and PriceUncertain
- Where: contract.rs:1055-1187 (`execute_conditional` has no mode check at all); contract.rs:658-661
  (`execute_order` only tests `mode == ReduceOnly`, so PriceUncertain lets everything through).
- Scenario 1: bob places an opening `Limit` conditional (reduce_only=false, up to 30 days life) while Active. Admin
  sets ReduceOnly to stop exposure growth. Keeper executes the conditional: bob opens 10 units in a paused market.
  Any trader can pre-place such conditionals and keep opening after a pause; the H-3 fix covered one of two paths.
- Scenario 2: an opening two-step order requested before `set_mode(PriceUncertain)` fills afterwards.
- Confidence: proved by test: `opening_conditional_must_not_fill_in_reduce_only_market`,
  `opening_order_must_not_fill_in_price_uncertain_market` (poc_access_modes.rs).
- Fix: one shared `fill_allowed(mode, reduce_only)` used by both execute paths: Active -> any; ReduceOnly and
  PriceUncertain -> reduce-only only (or nothing in PriceUncertain, see ACL-2); otherwise refuse. Also refuse
  non-reduce-only `Limit` in `place_conditional` when mode != Active.
- Tag: BUGFIX.

### ACL-2 (high) Tighten-only modes have no recovery and PriceUncertain traps positions forever
- Where: contract.rs:1298-1322 (`set_mode`, nobody can loosen, owner included); contract.rs:586-590 and
  1002-1006 (`request_order` / `place_conditional` require Active|ReduceOnly, so reduce-only closes are refused in
  PriceUncertain); contract.rs:1521 (`liquidate` ignores mode).
- Scenario: the pauser (the "limited" key of D15) calls `set_mode(m, PriceUncertain)` on every market, by mistake
  or after key compromise. From then on, with a perfectly healthy oracle: no trader can close, no trader can add
  margin (ACL-9), funding keeps accruing, liquidations keep running, LPs can only redeem what is above the pool's
  IM and the pool can never be flattened. No method, for any role, ever returns the market to ReduceOnly/Active,
  and no settlement path exists. The same permanence applies to plain ReduceOnly (admin or automatic band breach):
  the market is dead for new business forever. This is worse than audit item M-1 states (it lists only the band
  breach and calls it "ReduceOnly with no recovery"): a key that is meant to have no fund access can freeze user
  funds irreversibly.
- Confidence: proved by test: `trader_can_close_position_in_price_uncertain_market`,
  `owner_can_restore_market_after_reduce_only`.
- Fix: (1) always accept and fill reduce-only orders in every post-activation mode when the observation is fresh;
  (2) owner-only (later multisig, ideally timelocked) loosening `PriceUncertain -> ReduceOnly -> Active`, pauser
  stays tighten-only; (3) a real WindDown/Closed path: settle at a final price, release LP and margin.
- Tag: (1) BUGFIX; (2) and (3) DESIGN (trust model, settlement rules).

### ACL-3 (high) The listing bond is not a bond, and listing is free contract-paid storage (DoS)
- Where: contract.rs:452-467 (bond credited to the lister's withdrawable wallet; comment "For now, hold in
  wallet"); contract.rs:457-459 (wallet entry created for unregistered senders); no storage charge anywhere in the
  CreateMarket branch (comment at :466 says the bond covers storage, but the bond leaves).
- Scenario: attacker sends 50 USDC with a CreateMarket msg, then `withdraw(50 USDC)`, and repeats. Each round costs
  only gas and permanently adds one Market entry, one engine group and one ~9.5 KB pool portfolio (contract.rs:335
  comment), i.e. on the order of 0.1+ NEAR of storage stake paid from the contract's own NEAR balance. Because
  `symbol` and `Tee{pools}` are unbounded (ACL-4), one call can lock far more (a 1 MB symbol is ~10 NEAR). When the
  free balance is gone, every call that needs net-new contract-paid storage fails: `request_order` (orders are not
  storage-charged), `liquidate` (new caller wallet), fee-ledger first writes. Trading and liquidations stop until
  someone tops the account up, and the junk markets can never be deleted.
- Confidence: bond part proved by test: `listing_bond_is_locked_not_withdrawable`,
  `lister_cannot_withdraw_bond_right_after_listing` (poc_access_listing.rs). Storage exhaustion traced by reading,
  not measured on a sandbox.
- Fix: hold the bond in a separate escrow ledger (counted in custody, not in `wallets`), released only on an
  explicit close/expiry path; require the lister to be registered and to have prepaid NEAR storage (NEP-145 style
  balance) covering the market; cap symbol length and `pools` length.
- Tag: BUGFIX for escrow + bounds; DESIGN for bond size, slashing/return rules.

### ACL-4 (high) Permissionless listing validates nothing the spec requires
- Where: contract.rs:391-451.
  a. Tier bounds (spec 4: "Params must fall within tier bounds", 5 table) are never checked: a LongTail market
     with IM 10% / MM 5% is accepted; only the engine's own envelope applies. `max_oracle_age_ns` is unchecked too.
  b. `Tier::Major` is selectable by anyone with min bond 0 (:401), i.e. 1 atom of USDC, although spec 5 says
     majors are protocol-listed; the lister then earns the 50% fee share with major spreads.
  c. `symbol` is unvalidated: duplicates of an existing market (a fake "SOL-PERP" next to the real one), empty or
     100 KB strings are accepted.
  d. OI caps stay 0 = uncapped for every permissionless market (only the owner can set them), although spec 5
     derives them from seed and pool depth; with caps 0 the lister-chosen flat `spread_bps` (up to 10%) is used.
- Scenario: a lister creates "SOL-PERP", Major tier, 1-atom bond, 10x leverage, uncapped OI. Users who pick the
  market by symbol trade or LP on attacker-chosen risk parameters (and, with ACL-7, an attacker-chosen price).
- Confidence: proved by test: `listing_params_outside_tier_bounds_refused`,
  `permissionless_listing_cannot_claim_major_tier`, `duplicate_or_oversized_symbol_refused`. (d) traced by reading.
- Fix: per-tier min/max table checked in the CreateMarket branch; Major only through the owner path; symbol
  charset/length limits and uniqueness; default OI caps set at activation from observed depth.
- Tag: BUGFIX for symbol + Major gate; DESIGN for the bound values and cap formula.

### ACL-5 (high) Invariant 9 broken: the pushed oracle can overwrite any market's price, whatever its price source
- Where: contract.rs:1438-1487 (`push_price` never checks `info.price_source == Pushed`); contract.rs:314
  (`set_oracle`, owner, instant).
- Scenario: a NearPool market is Active with observation $2.00. The owner calls `set_oracle(evil)`, and evil calls
  `push_price(market, 4.00)`: accepted, target becomes 4,000,000 and the engine walks there. The owner can thus
  steer the mark of every market (also ones listed by third parties), liquidate one side and collect through an
  LP or trading account: an indirect path to user funds that invariant 6's "no admin method changes a balance"
  wording misses. The audit's M-4 only covers `set_oracle` on Pushed markets.
- Confidence: proved by test: `oracle_cannot_push_price_into_near_pool_market`.
- Fix: `require!(matches!(info.price_source, PriceSource::Pushed))` in `push_price`; make the oracle per-market
  and immutable after activation, or put `set_oracle` behind the multisig + timelock.
- Tag: BUGFIX for the source check; DESIGN for the oracle rotation model.

### ACL-6 (high) One >20% move permanently bricks a NearPool market (no observation can ever be accepted again)
- Where: contract.rs:858-881. On a band breach the function returns before updating `ema_e6` or `observation`,
  so every later read is compared with the same frozen EMA.
- Scenario: token reprices from $2.00 to $2.60 and stays. Every `refresh_price` for the next 2 hours (1,200
  reads in the PoC) is rejected; mode is ReduceOnly with no way back (ACL-2); after 10 s without a fresh
  observation only reduce-only fills pass, after 1 h the staleness ladder forbids all fills and all liquidations
  (contract.rs:667, 1122, 1528). The market is frozen at a price 30% off, positions cannot be closed or
  liquidated, forever. Anyone who can move the pool spot 20% for one read (cheap on a long-tail pool whose depth
  floor is 1,000 USDC) can do this deliberately to any NearPool market. Audit M-1 describes this as "ReduceOnly
  with no recovery path"; the real effect is a total, permanent freeze.
- Confidence: proved by test: `near_pool_market_recovers_after_sustained_price_move`.
- Fix: on breach keep ReduceOnly but still record the reading: either clamp the sample to the band edge and feed
  it to the EMA, or track a candidate price and accept it after N consecutive consistent reads / T seconds; pair
  with the owner restore path of ACL-2.
- Tag: DESIGN (oracle policy), with a small BUGFIX option (clamped EMA step).

### ACL-7 (high) Lister-chosen `dex` account is trusted as the price and depth source
- Where: contract.rs:391-451 (any `PriceSource` accepted, incl. `NearPool{dex: <any account>}`), 796-812
  (`refresh_price` calls `get_pool` on that account), 818-856 (callback trusts the returned JSON), 915-933
  (`activate_market` only needs seq > 0, freshness and `liquidity_usd >= floor`, all supplied by that account).
- Scenario: the lister deploys a contract that answers `get_pool` with any reserves it likes, lists a market on
  it, reports 1,000+ USDC depth and activates. It then walks the price (up to 20% per read against a 5%-weight
  EMA) to drain whoever is on the other side: LPs if it trades, traders if it is the LP. There is no dex
  allowlist, no "listing checks passed" step (spec 4), and the depth floor is self-reported.
  Also: a permissionless `Pushed` market can be created (it then depends on the global oracle), and a `Tee` market
  stays Proposed forever with its storage unpaid.
- Confidence: traced by reading (the unit harness shows `on_price_read` accepts whatever the promise returns).
- Fix: owner-managed allowlist of dex accounts (and pool kinds); reject `Pushed`/`Tee` in the permissionless path
  until implemented; activation by owner/multisig approval for now.
- Tag: DESIGN (listing trust model).

### ACL-8 (medium) Margin can be withdrawn with an open position against an arbitrarily stale price
- Where: contract.rs:546-558 (`withdraw_margin`: only the lag gate; no staleness check), same for `lp_redeem`
  (1394-1420). `has_lag` is false when no new target arrived, so a dead oracle looks "caught up".
- Scenario: oracle silent for 2 h (fills and liquidations are refused as PriceUncertain). The real price has
  fallen 30%. A 10x long withdraws all margin above IM at the frozen mark (3,000 USDC in the PoC). When prices
  resume, the account is deeply underwater and the loss lands on the pool / insurance. LPs can likewise redeem at
  a stale NAV ahead of the loss.
- Confidence: proved by test: `margin_withdrawal_with_open_position_refused_when_price_is_hours_stale`.
- Fix: for non-flat accounts (and for LP redeem while the pool has a position) require staleness below a
  threshold; flat accounts must stay withdrawable in every state.
- Tag: DESIGN (which threshold; interacts with "users can always exit"). Possibly overlaps the oracle reviewer.

### ACL-9 (medium) Margin top-up refused in PriceUncertain while liquidation stays on
- Where: contract.rs:532 (`deposit` requires Active|ReduceOnly) vs 1521 (`liquidate`, no mode check).
- Scenario: after `set_mode(PriceUncertain)` a trader near maintenance cannot add margin and cannot close
  (ACL-2), but anyone can liquidate him for the 60% caller fee.
- Confidence: proved by test: `trader_can_add_margin_in_price_uncertain_market`.
- Fix: allow `deposit` in every post-activation mode (it only reduces risk).
- Tag: BUGFIX.

### ACL-10 (medium) Admin role handling: one-step ownership, no 1 yocto, owner == contract account in the deployment
- Where: contract.rs:1331-1334 (`set_owner`), 314, 1281, 1298, 1325.
- a. `set_owner(typo)` irreversibly loses the admin role: no market creation, no oracle rotation, no pauser
  change, protocol fees unclaimable. No propose/accept.
- b. No admin method requires 1 yocto, so a function-call access key on the owner account (the UX the docs plan
  for trading, architecture.md:49) can call `set_owner` / `set_oracle` / `set_mode`.
- c. In the documented deployment the owner is the contract account itself (CONTRACT.md:62). `claim_fees`
  resolves the caller through `user()`, which rejects the contract account (contract.rs:1221, 1772), so protocol
  fees and market 0's lister fees (lister = contract account) cannot be claimed until ownership moves, and the
  owner role is only as safe as the contract account's keys (ACL-0).
- Confidence: traced by reading.
- Fix: `propose_owner` + `accept_owner`; `assert_one_yocto` on every admin method; separate owner account.
- Tag: BUGFIX.

### ACL-11 (low) `set_oi_caps` is owner-only and not tighten-only
- Where: contract.rs:1281-1294. Spec 4: `lower_caps` by pauser, tighten only. The method can raise caps or set 0
  (= uncapped), and the pauser cannot use it at all. Setting a tiny non-zero cap also pushes the dynamic spread to
  the 10% maximum for every later fill including closes (compute_spread, contract.rs:1690-1711), which the pool
  collects. Not a direct fund move, but it is an owner lever on user exit cost.
- Confidence: traced by reading.
- Fix: pauser may only lower; raising goes through the owner/multisig; reduce-only fills pay `spread_min`
  (ack. M-6 would close the exit-cost lever).
- Tag: DESIGN.

### ACL-12 (medium) No upgrade path, no state versioning, no global pause
- Where: whole contract. Facts:
  - No `migrate` / upgrade method. Upgrades are redeploys with a full-access key on the contract account
    (CONTRACT.md step 9), which is the unrestricted power spec 2 calls the "residual risk": today it is held by at
    least two keys, one of them public (ACL-0), with no multisig or timelock.
  - `Neap` (contract.rs:182), `Market` (model.rs:114), `Order`, `ConditionalOrder` are plain Borsh structs with no
    version tag. The spec still has to add fields (`pending_refunds`, `nonces`, `tee_keys`, tier params, LP redeem
    delay). Adding a field to `Market` makes every stored market undecodable; adding one to `Neap` makes every
    call fail with "Cannot deserialize the contract state" until a migration runs, and no migration entry exists.
  - Engine bytes are raw POD (exchange.rs:150-217). The engine carries `V16_LAYOUT_DISCRIMINATOR` /
    `V16_ACCOUNT_VERSION` in each account header (good: a mismatch is detected), but the wrapper has no handling:
    after an engine layout bump `portfolio_from_bytes` returns Corrupt for every account. Portfolios live in a
    non-iterable `LookupMap`, so they cannot be migrated eagerly; without a lazy per-record migration every
    user's margin is frozen.
  - `new` is `#[init]` (re-init is refused by near-sdk) but not `#[private]`; safe only while deploy and init are
    batched in one transaction.
  - No contract-wide emergency stop. Pausing is per market, one transaction each, and market count is unbounded
    under permissionless listing; `withdraw`, `ft_on_transfer`, `lp_deposit`, `liquidate`, `claim_fees` cannot be
    stopped at all during an incident.
- Confidence: traced by reading.
- Fix: versioned enums (`VersionedMarket`, `VersionedOrder`, state version field) before mainnet; a `#[private]`
  `migrate` with `#[init(ignore_state)]`; lazy engine-record migration keyed on the header discriminator; an
  `upgrade` method gated by owner/multisig with a timelock and then removal of full-access keys; a global
  `paused` flag for the pauser that blocks new risk (opens, LP deposits, listings) but never exits.
- Tag: DESIGN (upgrade governance), BUGFIX for the versioned wrappers and `#[private]` on `new`.

## Out of my area, noticed in passing (not verified; for the fee/custody reviewer)
- `liquidate` pays the caller 60% of the fee with `vault_out_to_wallet` (contract.rs:1541-1547) without taking it
  out of the engine vault, unlike `claim_fees`, which first calls `withdraw_insurance_surplus`. If that is right,
  `custody_vaults` drifts below the engine vault by every caller payout. Please confirm or dismiss.
- `request_order` stores an Order with only 1 yocto attached (no storage charge); expired orders stay until
  someone executes them.
- `liquidate` / `keeper_step` accept the contract account as `account`, i.e. the pool portfolio (intended per
  SECURITY.md W-1); the caller then earns the 60% share on a pool liquidation.

## Checked and found correct
- `#[init]` cannot be called twice; `PanicOnDefault` set.
- `on_withdraw` and `on_price_read` are `#[private]`; `on_price_read` writes only oracle metadata, no engine state.
- `ft_on_transfer` checks predecessor == quote token; unregistered Wallet senders are refunded in full.
- Every user value-moving method requires 1 yocto (or >= 1 yocto + storage) and resolves the caller through
  `user()`, which rejects the contract account (pool key).
- `cancel_order` / `cancel_conditional`: owner only, 1 yocto. `claim_fees`: lister/owner only, Lp/Insurance refused.
- `set_mode` really is tighten-only and limited to ReduceOnly/PriceUncertain; Proposed markets cannot be forced.
- No admin method writes a wallet, portfolio, vault or fee ledger directly (invariant 6 holds literally; ACL-5 and
  ACL-11 are the indirect levers).
- `push_price`: oracle-only, monotone timestamp, max age, 2 s future skew.
- Activation with the $1 placeholder engine price is not exploitable: with zero OI `catch_up` re-anchors the
  engine straight to the target (exchange.rs:278-291), and fills are refused while the engine lags.
- Proposed markets refuse deposit, withdraw_margin, request_order and place_conditional.
- Expired order / conditional cleanup returns instead of panicking (M-3 fix holds).
- deploy/Dockerfile and deploy/start.sh only run the keeper/indexer; the signer key comes from the environment,
  not the image.
