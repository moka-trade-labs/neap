# Security pass: percolator engine + NEAR wrapper

Method: the 13 bounded prompts from the 0xAsm0d3uls vuln-research thread
(pasted by jason, 2026-11-09), each applied to the threat model below. Each
finding is pinned by a test or proof where possible. Scope was the NEAR wrapper
(`neap/`, all of it) and the engine surfaces the wrapper calls plus the moka
engine change. The 24k-line engine core was **not** re-audited line by line;
upstream already carries 325 Kani harnesses and a regression suite, which pass
here unchanged.

## Threat model

- **Assets:** quote tokens (NEP-141 USDC) in custody; LP pool capital;
  trader margin and matured profit; insurance.
- **Actors:** any NEAR account (trader, LP, keeper), the oracle account
  (TEE agent, trusted for liveness and freshness, *not* for magnitude), the
  owner (lists markets, rotates oracle), the quote-token contract, and the
  contract account itself (anyone holding a leftover full-access key).
- **Boundaries:** one contract holds everything. The only async boundary is the
  outbound `ft_transfer` on `withdraw`. NEAR reverts a receipt on panic, and
  every engine `Err` panics, so engine `*_not_atomic` partial writes never persist.

## Findings

| ID | Sev | Prompt that found it | Finding | Status |
|---|---|---|---|---|
| W-2 | **Critical** | same object, different access paths | The LP pool portfolio is keyed by the contract's own account id in the same map as users. A caller with the contract account's key could call `withdraw_margin` / `trade` "as the pool" and move LP capital into its own wallet. | **Fixed**: every user method resolves the caller via `user()`, which refuses the contract account. Test `contract_account_cannot_act_as_user_and_drain_the_pool`. Deploy guidance: delete the contract account's full-access keys. |
| W-1 | High | rule enforced in one place, assumed in another | First draft refused to liquidate the pool. The engine's bankruptcy path (A/K socialization) assumes every under-MM account is liquidatable; exempting the pool would let it sit insolvent. | **Fixed**: pool is liquidatable like any account. Also removed redundant pool write-backs that would have clobbered a pool liquidation. |
| W-3 | High | operations safe alone, unsafe chained | `lp_deposit` priced shares off the current (possibly lagged) mark. Chain "oracle push → deposit before catch-up → catch-up" let a depositor buy pending pool gains at the stale NAV, diluting existing LPs. | **Fixed**: `lp_deposit` refuses while the engine price lags the oracle target. Test `lp_deposit_refused_during_oracle_lag`. |
| W-6 | High | checks that depend on event order | Every keeper path ran oracle catch-up first. Outside Live mode the engine refuses catch-up, so `keeper_step` could never reach the Recovery→Resolved transition: a market in Recovery would be stuck forever. | **Fixed**: `keeper_step` skips catch-up when the market is not Live. |
| F-1 | Medium | state that can end up contradictory / stranded | A winner's profit is backed by the loser's crystallized principal only until the backing bucket's freshness horizon `max(max_accrual_dt, h_max, …)`. If the winner does not convert inside that window, the bucket expires, the backing is impaired, and `convert` returns `LockActive`. The PnL is not erased but is stranded. Engine behaviour, inherited from upstream. | **Pinned**: test `finding_f1_unconverted_profit_loses_backing_after_horizon`. Mitigation for now: frontend auto-prompts conversion, keeper bot converts on the owner's behalf only when h = 1. Needs a design decision (see below). |
| E-2 | Medium | spec rule not enforced as written | spec §8.4 allows withdrawal from a nonflat account under withdrawal health; the engine only allowed flat withdraws. Traders could not take out excess margin and the pooled LP could never be redeemed. | **Fixed in engine**: `withdraw_with_open_positions_not_atomic`, Kani-proved margin kernel (3 harnesses), 5 tests. |
| W-4 | Low | requests that fail but still change something | `lp_deposit` created a share-ledger entry whose storage the contract paid for. Many dust deposits from many accounts would slowly drain the contract's storage balance. | **Fixed**: a first LP deposit must attach storage; excess refunded. |
| W-8 | Low | rule enforced in one place, assumed in another | Engine `deposit` is Live-only (#448) but `deposit_domain_insurance` has no mode gate. After resolution the payout snapshot is fixed, so a late insurance donation is stranded. | **Fixed in wrapper**: insurance deposits are Live-only. |
| E-3 | Info | assumption never checked | `accrue_asset_path_to_not_atomic` validates every step except the first step's carried remainder when the target is unchanged; the engine assumes the wrapper carries it honestly. Worth at most < 1 bps of one step. | Wrapper stores the remainder itself and never lets callers supply steps. |
| W-5 | Info | identity rebound to another user | Portfolios and wallets are keyed by `AccountId`. If a NEAR sub-account is deleted and its parent recreates it, the new holder inherits the balance. | Documented. Same as every NEAR contract keyed by account id. |
| W-7 | Info | checks that depend on event order | During oracle catch-up, trades (including closes) are refused per spec §9.7. A position cannot be closed until the staircase reaches the target (2% at 30 bps/slot ≈ 7 blocks ≈ 4 s). | By design. A reduce-only exception needs the spec's dual-price shadow check; backlog. |
| T-1 | Trust | assumption never checked | The oracle bounds speed (staircase) and freshness (monotone, max age), not magnitude. A compromised oracle can walk the price anywhere at the configured cap. | Trust assumption on the NEAR TEE oracle. Backlog: per-update deviation bound, multi-source median. |

Prompts with no finding in scope: delete/revoke (no deletion paths exist yet),
undo by unauthorized party (owner/oracle rotation are owner-only; `on_withdraw`
is `#[private]`), rollback with side effects (wallet is debited before the
promise and re-credited on failure; Kani-proved `proof_custody_failed_withdraw_restores_exactly`).

## Open decision for jason (F-1)

Options: (a) keep engine behaviour and make conversion part of the close flow
(frontend sends `trade` + `convert_pnl` after warmup); (b) let keepers convert
any account's matured, backed profit permissionlessly (value-neutral for the
owner only when h = 1, so it must check h); (c) change the engine so an expired
bucket's backing degrades to the unsecured pool (h-haircut) instead of locking.
(c) is the right long-term fix but touches the proven backing kernels.
