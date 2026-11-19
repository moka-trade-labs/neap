# Accounting, custody and solvency review

Scope: crates/neap/src/{contract.rs, exchange.rs, policy.rs}. The engine (crates/engine) was treated as verified;
only how the wrapper calls it was reviewed. Line numbers refer to the files as of 2026-11-18.

PoC tests are new files only. Each asserts the CORRECT behaviour and therefore FAILS today. Note that this makes
`cargo test -p neap` red until the findings are fixed (7 failing tests across 4 new files); the pre-existing
tests are untouched and still pass.

| ID | Severity | Tag | Title | Confidence |
| --- | --- | --- | --- | --- |
| ACC-1 | high | BUGFIX | Liquidation reward is paid without leaving the engine vault | proved by test |
| ACC-2 | high | BUGFIX | LP pool profit is never converted, so LPs cannot redeem it and later depositors lose to it | proved by test |
| ACC-3 | high | DESIGN | LP 30%, insurance 10% and the 40% liquidation share are stranded; fee insurance does not cover bad debt | proved by test (LP part), traced by reading (insurance part) |
| ACC-4 | high | DESIGN | Lister bond is not locked; a market can be listed for 1 atom with contract-paid storage | proved by test |
| ACC-5 | medium | DESIGN | The LP pool portfolio is a valid target of `liquidate` / `keeper_step` | suspected (not tested) |
| ACC-6 | low | BUGFIX | Wallet entries are created without a storage deposit | traced by reading |

---

## ACC-1 (high, BUGFIX) Liquidation reward is paid without leaving the engine vault

- Where: crates/neap/src/contract.rs:1539-1548 (`liquidate`), using `vault_out_to_wallet` at 1895-1902.
- Mechanism: the engine moves the whole liquidation fee from the liquidated account's capital into engine
  insurance (still inside the engine vault). The wrapper then credits 60% to the caller's wallet and decrements
  `custody_vaults`, with no matching engine withdrawal. The comment "We pay the caller 60% from the vault" is not
  what the code does.
- Scenario: bob deposits 1_000 USDC, goes ~9x long, price falls 8%, carol calls `liquidate`.
  Before: custody_vaults = engine vault = 1_001_000_000_000. After: carol's wallet = 9_535_092,
  custody_vaults = 1_000_990_464_908, engine vault still 1_001_000_000_000. The reward exists twice: in a
  withdrawable wallet and in engine insurance. Invariant 1 (`custody_vaults == sum of engine vaults`) is broken
  after every paid liquidation and the gap only grows.
- Honest impact note: today the gap is masked, because the same atoms sit in unbudgeted insurance that nobody
  can fully withdraw (see ACC-3). It becomes a real shortfall (last withdrawers hit the `custody` panic) as soon
  as ACC-3 is fixed or liquidation fees outgrow the stranded surplus. Fix ACC-1 before or together with ACC-3.
- PoC: crates/neap/tests/poc_accounting_liquidation_custody.rs (currently fails).
- Fix: after `exchange::liquidate`, call `exchange::withdraw_insurance_surplus(&mut g, caller_share)` before
  `store_group`, and credit only the amount actually withdrawn.

## ACC-2 (high, BUGFIX) LP pool profit is never converted, so LPs cannot redeem it

- Where: contract.rs:1394-1420 (`lp_redeem` prices at nav_lower, which is capital only when pnl > 0);
  contract.rs:1338-1348 (`convert_pnl` goes through `Self::user()`, which rejects the contract account that owns
  the pool); policy.rs:129-147 (`lp_nav_bounds`). The spec's `convert_pnl_for(market, account)` is not implemented.
- Mechanism: pool losses are settled against pool capital immediately, pool gains (trader losses, spread) stay as
  engine `pnl` forever. Nothing in the contract can convert the pool's PnL. `keeper_step` on the pool only refreshes.
- Scenario A: a trader loses ~300 USDC to the pool and closes. Pool: capital 1_000_000 USDC, pnl +300.0099 USDC.
  The sole LP redeems all shares 100+ blocks later and receives exactly 1_000_000_000_000. Shares are now 0,
  nav_lower 0, nav_upper 300_009_900: the profit is orphaned.
- Scenario B: the next LP deposits 1_000 USDC. Shares are priced at nav_upper (orphaned PnL), redemption at
  nav_lower. An immediate round trip returns 769_224_894, a 23% loss to nobody. With remaining LPs the same
  effect is a permanent, growing gap between deposit price and redeem price.
- PoC: crates/neap/tests/poc_accounting_lp_profit.rs, two tests (both currently fail).
- Fix: in `lp_deposit`, `lp_redeem` (after the refresh) and ideally `crank`, call `exchange::convert_pnl` on the
  pool portfolio and ignore a "nothing to convert" error. I checked at exchange level that the engine accepts this
  for the pool: `convert_pnl` on the flat pool returned Ok(300_009_000) and moved it to capital. Not checked:
  behaviour while the pool still has open exposure (the engine may refuse or convert only the matured part), and
  whether unconverted pool profit can lose its backing over time (spec gap F-1).

## ACC-3 (high, DESIGN) LP, insurance and liquidation-insurance fee shares are stranded

- Where: contract.rs:1717-1740 (`accrue_fee_split`), 1219-1235 (`claim_fees` rejects Lp and Insurance),
  exchange.rs:532-559 (`withdraw_insurance_surplus`).
- Mechanism: every trading and liquidation fee lands in the engine's unbudgeted insurance. Only the Lister and
  Protocol ledgers (60% of trading fees) can ever be claimed. The Lp (30%) and Insurance (10%) ledgers only grow;
  no code path moves those atoms to pool capital or anywhere else. The comment "LP and Insurance fees stay in the
  pool/insurance fund" is wrong for the LP share: it never reaches the pool.
- Second consequence: the engine covers bad debt only from domain-budgeted insurance
  (`available_domain_insurance = min(global, budget_remaining)`), and the only thing that creates budget is
  `insurance_deposit`. So the fee-funded "insurance" (10% of trading fees, 40% of liquidation fees) does not
  absorb bad debt. With no donations the effective insurance fund is zero and losses go straight to the haircut.
- Scenario: one 100-unit round trip at $100 accrues Lp = 6_000_000 atoms. The sole LP exits with exactly its
  deposit. After lister and protocol claim, 40% of all trading fees plus 100% of liquidation fees remain in the
  engine vault with no owner able to reach them.
- PoC: crates/neap/tests/poc_accounting_fee_shares.rs (currently fails; it also fails because of ACC-2).
- Suggested direction (founder decision): on each fill, move the LP share from insurance surplus into pool
  capital (withdraw surplus, then engine deposit into the pool portfolio), and credit the insurance share to the
  domain budgets with the engine's `credit_domain_insurance_budget(s)_not_atomic`. Decide whether lister and
  protocol claims should rank equally with these when insurance has been consumed.

## ACC-4 (high, DESIGN) Lister bond is not locked; listing is free and the contract pays its storage

- Where: contract.rs:400-407 (min bond; `Tier::Major` => 0, tier chosen by the sender) and 452-464 (bond credited
  to the lister's wallet; comment "For now, hold in wallet").
- Mechanism: `Market.bond` is only a number. The same atoms are in the lister's wallet and can be withdrawn in
  the next transaction. No path ever slashes or returns a bond. An unregistered sender also gets a wallet entry
  for free. Each market adds roughly 10 KB (engine group, ~9.5 KB pool portfolio, registry entry), about
  0.1 NEAR, paid from the contract's own balance because `ft_on_transfer` carries no deposit.
- Scenario: any account sends 1 atom of USDC with `CreateMarket{tier: Major}` in a loop, withdrawing the atom each
  time. Cost to the attacker is gas only; the contract loses ~0.1 NEAR of storage stake per call until
  state-growing calls start failing. Custody itself stays consistent (the bond is counted once, as a wallet balance).
- PoC: crates/neap/tests/poc_accounting_bond.rs, two tests (both currently fail).
- Decision needed: lock the bond in a separate ledger counted in custody, define return and slash rules, set a
  non-zero bond (or owner-only) for Major, and charge creation storage to the lister.

## ACC-5 (medium, DESIGN, suspected) The LP pool can be liquidated like a user

- Where: contract.rs:1521-1548 (`liquidate`) and 1498-1517 (`keeper_step`) accept any `AccountId`, including the
  contract's own id, which is the pool's portfolio key.
- Concern: OI caps default to 0 (uncapped), so the pool can be driven to its initial-margin limit; an adverse move
  can then put it under maintenance. `liquidate(market, <contract id>)` would charge the liquidation fee to LP
  capital and pay 60% of it to the caller (with the ACC-1 defect on top).
- Not tested: I did not build a scenario where the pool is under maintenance. This overlaps the liquidation
  reviewer's area; flagged here because the money comes from LPs.

## ACC-6 (low, BUGFIX) Wallet entries created without a storage deposit

- Where: contract.rs:1900-1901 (`vault_out_to_wallet` inserts for an unregistered `liquidate` caller),
  contract.rs:457-459 (CreateMarket sender). Small per-entry cost, paid by the contract. Fix: require the caller
  to be registered, or park the reward.

---

## Checked and found correct

- `ft_on_transfer` Wallet path: only the quote token; unregistered sender refunded in full; custody and wallet
  move by the same amount. A panic inside CreateMarket reverts and the token contract refunds.
- `withdraw` / `on_withdraw`: debit first, custody moves with it, failed transfer re-credits wallet and custody;
  no engine access in the callback.
- `deposit`, `withdraw_margin`, `lp_deposit`, `lp_redeem`, `insurance_deposit`, `claim_fees`: wallet ledger,
  `custody_wallets` / `custody_vaults` and the engine vault move by the same amount (the existing
  `custody_matches_vaults` checks pass on these paths).
- Fee ledgers cannot exceed what was collected: the engine charges the taker only, `fill.fee` is the amount
  actually charged (capped by capital), the split sums exactly to the fee with dust to protocol, and `claim_fees`
  debits the ledger only by what the engine actually released. Lister and protocol authorization is correct.
- `withdraw_insurance_surplus` cannot touch budgeted (donated) insurance or reserved credit; the fallback
  recomputes the same bound the engine enforces.
- LP share math: mint rounds down at nav_upper, redeem rounds down at nav_lower, virtual offset 1e6:1. There is
  no way to raise pool capital without minting shares, so the classic first-depositor donation attack does not
  apply. Zero-share deposits are rejected. Redemption with open pool exposure goes through the engine's
  initial-margin check and the oracle-lag gate.
- Liquidation fee reported by the engine is the amount actually charged, not a requested amount.
- Arithmetic: release profile has overflow checks on; custody helpers use checked math.

Not re-reported (acknowledged in the prior audit): malformed `CreateMarket` msg becomes a wallet deposit (M-2),
`lp_deposit` has no mode gate (M-5), no 24 h redeem delay (L-3), no `pending_refunds` (H-4).
