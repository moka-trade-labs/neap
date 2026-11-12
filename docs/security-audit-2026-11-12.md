# Security Audit — 2026-11-12

Scope: `crates/neap/src/{contract,model,policy,exchange}.rs`
Method: 13 base prompts + baseline gate from `docs/security-checks.md`

## Fixed (in this session)

| ID | Severity | Issue | Fix |
|----|----------|-------|-----|
| C-1 | CRITICAL | Missing mode checks on `deposit`, `withdraw_margin` (and others) — allowed interaction with Proposed/Closed markets | Added `require!(mode == Active \|\| ReduceOnly)` to `deposit`; `require!(mode != Proposed)` to `withdraw_margin` |
| C-2 | CRITICAL | `on_price_read` callback mutated engine state via `catch_up` — violated atomicity rule (no engine mutation in callbacks) | Removed `catch_up` from callback; now only writes oracle meta. The keeper's `crank` catches up the engine |
| H-2 | HIGH | No staleness check on `liquidate` — could liquidate at hours-old prices | Added `require!(staleness < PriceUncertain)` before liquidation |
| H-3 | HIGH | `execute_order` didn't enforce `ReduceOnly` market mode — position-increasing trades went through in admin-paused markets | Added explicit `if mode == ReduceOnly && !reduce_only` check |
| M-3 | MEDIUM | Expired orders: `panic_str` after `orders.remove()` reverted the removal — storage leak | Changed to `return U64(0)` so removal persists. Same fix for conditional expiry |

## Acknowledged (not fixed — known limitations)

| ID | Severity | Issue | Reason |
|----|----------|-------|--------|
| H-1 | HIGH | Owner `create_market` bypasses tier bounds, bonds, activation ceremony | Transitional admin path for hackathon. Owner trust is disclosed. Will be locked/removed post-hackathon |
| H-4 | HIGH | No `pending_refunds` — tokens in flight between withdraw and callback | The re-credit-on-failure pattern works correctly; the window is one receipt. Spec lists this as acceptable |
| M-1 | MEDIUM | Band breach → ReduceOnly with no recovery path | Needs a `restore_mode` admin method or time-based recovery. Post-hackathon |
| M-2 | MEDIUM | Malformed `CreateMarket` msg silently becomes wallet deposit | Conservative (user doesn't lose funds). Could add explicit error variant later |
| M-4 | MEDIUM | `set_oracle` has no timelock | Owner key is the acknowledged residual risk. Multisig + timelock post-hackathon |
| M-5 | MEDIUM | `lp_deposit` doesn't check market mode | LP deposits in distressed markets are self-harming, not exploitable. Could add mode gate |
| M-6 | MEDIUM | Reduce-only trades charged dynamic spread instead of `spread_min` | Conservative (overcharges, doesn't undercharge). Spec says "reduce skew pays spread_min" — fix post-hackathon |
| L-1 | LOW | `scan_order_ids` unbounded limit param | Gas limit provides natural cap. Could add explicit max |
| L-2 | LOW | `claim_fees` Protocol pays to current owner | By design |
| L-3 | LOW | No 24h LP redeem delay | Known gap, in the plan |
| L-4 | LOW | `on_price_read` assumes 6-decimal tokens | Documented limitation. Band check catches extreme errors |

## Confirmed correct

- **Auth**: All value-moving calls require 1 yoctoNEAR. `ft_on_transfer` checks `predecessor == quote_token`. Callbacks are `#[private]`. Contract account excluded from user methods.
- **Oracle monotonicity**: `publish_ns > last_publish_ns` enforced. Future bounded by 2s.
- **Fee conservation**: Splits sum to 10000 bps. Rounding dust → protocol.
- **No admin path to funds**: `set_mode` is tighten-only. `set_oi_caps` and `set_pauser` don't touch balances (tested: `admin_methods_do_not_change_balances`).
- **Engine invariants**: `V >= C_tot + I` checked in tests after every operation. No engine mutation straddles a promise (C-2 fixed).
