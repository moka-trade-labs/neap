# NEAR-platform review of the `neap` contract (async, storage, auth, events)

Scope: `/home/user/neap/crates/neap/src/contract.rs` (plus `model.rs`, `exchange.rs`, `policy.rs` where they are reached from a platform hazard). Engine internals treated as verified. Sandbox (real runtime) was not run; everything "proved" below is in the near-sdk mocked runtime.

PoC tests are new files under `/home/user/neap/crates/neap/tests/`. Each asserts the correct behaviour, so each **fails today** and should pass once fixed. Baseline suites (`contract_unit`, `exchange_flow`, `policy_props`) are still green. Run one with `cargo test -p neap --test <name>`.

Overlap note: other reviewers' PoCs already in the tree cover the same root cause as NEAR-1 (`poc_accounting_liquidation_custody.rs`) and parts of NEAR-2 (`poc_accounting_bond.rs`, `poc_access_listing.rs`). I kept my entries because the storage-staking angle of NEAR-2/3 is specific to this area; dedupe when merging.

---

## NEAR-1 (high, BUGFIX) Liquidation reward is paid out of custody without leaving the engine vault

- Where: `contract.rs:1541-1547` (`liquidate`), helper `vault_out_to_wallet` at `contract.rs:1895-1902`.
- What: the engine charges the liquidation fee into its own insurance (inside the engine vault). The wrapper then credits the caller's wallet with 60% of the fee and decrements `custody_vaults`, but never withdraws that amount from the engine. The engine vault is now larger than the tokens backing it.
- Scenario (proved): bob deposits 1,000 USDC, goes ~9x long, price -8%, keeper liquidates. After the call `custody_vaults = 1,000,990.464152` while the engine vault is `1,001,000.000000`: a 9.535848 USDC hole from one liquidation. The hole grows with every liquidation; the last users to withdraw hit the `custody` panic at `contract.rs:1897-1898` and their funds are stuck. Invariant 1 broken.
- Confidence: proved by test. PoC: `poc_near_liq_reward_custody.rs`.
- Fix: before crediting the caller, take the share out of the engine with `exchange::withdraw_insurance_surplus(&mut g, caller_share)` and pay only what was actually withdrawn (same pattern as `claim_fees`), then store the group.

## NEAR-2 (high, DESIGN) Permissionless listing costs the lister nothing; the contract pays ~12 KB of storage per market, forever

- Where: `contract.rs:391-479` (`ft_on_transfer`, `CreateMarket` arm). Bond floor at `400-403`, bond credited to wallet at `460-463`, comment admitting no storage charge at `466-467`.
- What, three things that combine:
  1. `Tier::Major => 0` bond: any account can list a `Major` market by sending 1 atom of USDC.
  2. The bond is added to the lister's ordinary wallet, so `withdraw` returns it immediately. `Market.bond` is only a number.
  3. No NEAR is collected for storage (there is no attached deposit in `ft_on_transfer`), and there is no market cap and no delete path.
- Scenario (proved): a stranger sends 1 atom with a `CreateMarket`/`Major` msg. A market is created, the transfer is kept, and contract storage grows by 11,945 bytes (about 0.12 NEAR staked from the contract's own balance). Repeat in a loop for gas only. A 50 USDC `LongTail` listing leaves 50 USDC in the lister's wallet, withdrawable at once. When the contract's free NEAR is exhausted, every call that grows state without a deposit fails (`request_order`, a liquidator's first wallet credit, new fee-ledger keys), which halts trading.
- Confidence: proved by test. PoC: `poc_near_storage_grief.rs` (`zero_bond_major_listing_by_a_stranger_is_refused`, `lister_bond_is_not_withdrawable`).
- Fix (founder decides the economics): lock the bond in a separate ledger counted in custody, released only on an explicit close path; require a non-zero bond for every permissionless tier (or make `Major` owner-only); pay storage from a prepaid NEAR storage balance of the lister (NEP-145 style) checked against the measured `storage_usage` delta.

## NEAR-3 (high, BUGFIX) Unbounded `symbol` and `price_source` payload stored at the contract's expense

- Where: `contract.rs:100-107` (`FtMsg::CreateMarket`), `model.rs:91` (`Tee { pools: Vec<String> }`), stored at `contract.rs:427-451`. Owner path `create_market` (`contract.rs:254`) has the same unbounded `symbol`.
- Scenario (proved): one `ft_transfer_call` with a `Tee` source carrying 400 strings of 1,000 bytes makes the contract store 413,529 bytes (about 4.1 NEAR staked) for a withdrawable 50 USDC. The 4 MB argument limit and prepaid gas are the only bounds, so a few calls lock tens of NEAR. `symbol` is bounded only by accident (the 16 KB log limit in the `market_created` event); `price_source` is not logged, so it is not bounded at all. The oversized `Market` is then cloned on every read of that market.
- Confidence: proved by test. PoC: `poc_near_storage_grief.rs` (`listing_payload_size_is_bounded`).
- Fix: cap `symbol` (for example 32 bytes, restricted charset), cap `pools.len()` and each entry's length, and reject `Tee` until phase 2 exists.

## NEAR-4 (medium, DESIGN) `request_order` stores orders with no storage charge and no per-account limit

- Where: `contract.rs:566-625`. Compare `place_conditional` (`contract.rs:994, 1035`), which does charge.
- Scenario (proved): one account places 300 orders with 1 yocto each; storage grows 34,500 bytes (115 bytes per order, about 0.00115 NEAR each) paid by the contract. Orders live until someone spends gas on `execute_order` after expiry, or the owner cancels. Besides the NEAR lock, `next_order_id` runs ahead and the keeper's `scan_order_ids` range fills with junk it must pay gas to clear.
- Confidence: proved by test. PoC: `poc_near_misc.rs` (`pending_orders_do_not_grow_contract_paid_storage`).
- Fix: either charge storage per order and refund on removal, or (simpler for UX) cap open orders per account (for example 8) and count them in a per-account field paid for at `open_account`. The spec also says `request_order` "escrows margin", which is not implemented and would add a real cost to spam.

## NEAR-5 (medium, BUGFIX) Admin methods do not require 1 yoctoNEAR

- Where: `set_oracle` `contract.rs:314`, `set_oi_caps` `1281`, `set_mode` `1298`, `set_pauser` `1325`, `set_owner` `1331`.
- Scenario (proved): a function-call access key on the owner account (what a NEAR wallet creates when the owner "signs in" to the web app, stored in browser local storage) can call `set_owner` or `set_oracle` with zero deposit. A leaked or XSS-stolen dapp key hands over ownership or the price feed. AGENTS.md and the audit both state the 1-yocto rule; these methods miss it. `set_owner` is also single-step, so a typo is permanent.
- Confidence: proved by test. PoC: `poc_near_admin_yocto.rs`.
- Fix: mark them `#[payable]` with `assert_one_yocto()`; make ownership transfer two-step (propose, accept).

## NEAR-6 (medium, DESIGN) Pool-read oracle: EMA advances once per callback, and the `dex` account is whatever the lister typed

- Where: `refresh_price` `contract.rs:796-813`, `on_price_read` `818-909` (EMA step at `883`), `dex` taken from the msg at `contract.rs:103` / `model.rs:86`.
- Scenario (proved for the EMA part): `refresh_price` is permissionless and each callback applies one 5% EMA step. After one refresh at +15% spot the target is 1.0075; after 61 refreshes in the same block it is 1.1434. The smoothing that the staircase target relies on is therefore a matter of how many refresh transactions someone sends, not of time.
- Scenario (traced by reading): there is no allow-list for `dex`. A lister can point a market at their own contract, return any `amounts` (including a fake 10,000 USDC depth to pass `activate_market`), and then steer the price in steps of up to 20% per callback. Anyone who LPs into that market is the counterparty.
- Smaller points in the same callback: `env::promise_result` (deprecated in near-sdk 5.30 in favour of `promise_result_checked`) reads a result of any size from that untrusted contract; `usdc_atoms * 1_000_000` at `contract.rs:851` can overflow. Both only make the callback fail, which writes nothing, so they are harmless today.
- Confidence: EMA proved by test, `dex` traced by reading. PoC: `poc_near_misc.rs` (`ema_moves_at_most_one_step_per_block`).
- Fix: accept at most one observation per block per market (or weight the EMA by elapsed time); owner-managed allow-list of DEX accounts; use `promise_result_checked(0, 4096)` and checked math.

## NEAR-7 (medium, BUGFIX) Events the spec requires are not emitted

- Where: spec `docs/contract.md` section 8. Missing emits in `deposit` (`contract.rs:529`), `withdraw_margin` (`546`), `withdraw` / `on_withdraw` (`485`, `510`), wallet credit in `ft_on_transfer` (`380-390`), `lp_deposit` (`1354`), `lp_redeem` (`1394`), `insurance_deposit` (`1424`), `convert_pnl` (`1338`), `accrue_fee_split` (`1717`, no `fees_accrued`), `register` / `open_account`, owner `create_market` (`254`, no `market_created`), `set_oi_caps`, `set_owner`, `set_oracle`, `set_pauser`.
- Scenario (proved for three): margin deposit, wallet withdraw and LP deposit produce no event. The indexer cannot rebuild balances or LP shares from logs, which the spec says it must. A failed withdrawal (`on_withdraw` returns `false`, the transaction still "succeeds") is invisible to the UI: no `withdraw`, no `refund_parked`.
- Confidence: proved by test (three events), the rest by grep of every `emit(` call. PoC: `poc_near_events.rs`.
- Fix: add the emits; emit `withdraw` in the callback with a `success` flag.

## NEAR-8 (medium, BUGFIX) `size_q as i128` wraps: a "Short" order can buy, skipping the long-side OI cap

- Where: `contract.rs:685-689` (`execute_order`), same cast at `1133-1137` (`execute_conditional`); no upper bound on `size_q` at `request_order` (`577`) or `place_conditional` (`996`). `overflow-checks = true` does not cover `as` casts.
- Scenario (proved): bob requests `side = Short`, `size_q = 2^128 - 10e6`. The cast gives -10 units, the Short branch negates it, and the fill **buys** 10 units (position after: +10,000,000). The OI-cap check at `contract.rs:701-719` looked only at the short side, so with a long cap set and the short cap uncapped (0) this opens long exposure past the long cap. The event says `Short`, and the spread is computed for the wrong side.
- Confidence: proved by test. PoC: `poc_near_misc.rs` (`oversized_size_q_is_rejected_at_request`).
- Fix: `require!(size_q.0 <= i128::MAX as u128)` (better: a sane market maximum) in both request methods, and use `i128::try_from` at the cast sites.

## NEAR-9 (low, BUGFIX) Storage deposits are kept: no refund on removal, and extra NEAR on `lp_deposit` is swallowed

- Where: `cancel_conditional` `contract.rs:1191-1211`, expiry and fill paths `1064-1072`, `1171`; `charge_storage_after_yocto` `1783-1789`.
- Scenario: (a) proved: a user pays storage for a conditional; cancelling, expiring or filling it frees the bytes but refunds nothing, so the NEAR stays with the contract. (b) traced: on a second `lp_deposit` no storage is added, the function returns early, and whatever was attached above 1 yocto is kept. The repo's own tests attach 0.2 NEAR to every `lp_deposit`; a frontend doing the same loses the user 0.2 NEAR per deposit.
- Confidence: (a) proved by test, (b) traced by reading. PoC: `poc_near_misc.rs` (`cancelling_a_conditional_refunds_its_storage`).
- Fix: record the amount paid per conditional and refund it to the owner on removal; in `charge_storage_after_yocto` refund `attached - 1 yocto` when nothing was stored. Longer term, NEP-145 `storage_deposit` / `storage_withdraw` (spec section 4) with a per-account balance.

## NEAR-10 (low, DESIGN) No way to ever release account storage; no NEP-145

- Where: no `unregister`, `close_account`, `storage_withdraw` anywhere in `contract.rs`. `open_account` (`337`) costs about 0.1 NEAR per market (9.5 KB portfolio) and `register` a little more, permanently.
- Impact: a real cost for every trader per market, with no exit; listed in the spec as "to build". Also wallets are created for free for unregistered callers by `liquidate` (`1546`), `claim_fees` (`1259`) and `CreateMarket` (`457-459`), a small contract-paid leak.
- Confidence: traced by reading.
- Fix: `close_account` for a flat, empty portfolio with refund of the measured bytes.

## NEAR-11 (info) Deployment and upgrade hygiene

- `new` (`contract.rs:229-230`) is not restricted to the contract account. If deploy and init are not in one batch transaction, anyone can initialise it with their own owner/oracle. Fix: `#[private]` on `new`, or always batch.
- No state version or `migrate` method; `Market`, `Order` and the engine byte layouts are stored raw. Any field change bricks deserialisation of existing state. Plan a versioned enum before mainnet.
- Order and conditional expiry are in blocks (`MAX_ORDER_EXPIRY_BLOCKS = 50`, "about 30 s at 600 ms"), staleness is in nanoseconds, engine funding and price-move caps are per block. A protocol change in block time changes the economics of all block-denominated values.
- `liquidate(market, <contract account>)` and `keeper_step(market, <contract account>)` accept the LP pool's own portfolio as the target (`contract.rs:1498, 1521`). I tested both on a healthy pool: `keeper_step` just refreshes it, `liquidate` fails with `NonProgress`. If the pool ever falls under maintenance, the pool would be liquidated like a user and the caller paid a reward from it. That may be intended; it should be an explicit decision.

---

## Checked and found correct

- `withdraw` / `on_withdraw` (`contract.rs:485-523`): wallet and custody are debited before the promise; the callback is `#[private]`, re-credits exactly once on failure (tested with a mocked `PromiseResult::Failed`: wallet and both custody buckets return to their starting values), and does not touch the engine. Static gas is attached to both legs, so a caller cannot starve the callback: too little prepaid gas fails the whole receipt before anything is committed. A receiver not registered on USDC just fails the transfer and is re-credited.
- No engine mutation in any callback; `on_price_read` writes only the market registry entry. No engine call straddles a promise (invariant 5 holds for the paths that exist).
- `ft_on_transfer`: only the quote token may call; unregistered senders get the full amount back; return values follow NEP-141 (unused amount); every panic path happens before any cross-receipt effect, so the FT contract refunds in full and nothing is half-written.
- No raw `env::storage_write`; all state goes through `near_sdk::store` collections, flushed before storage is measured in `register`, `open_account`, `create_market`, `lp_deposit`, `place_conditional`. Those five measure the real `storage_usage` delta and refund the excess.
- Collection prefixes: eight distinct one-byte borsh prefixes (`Key` enum, `contract.rs:69-78`), no nested collections, no collisions.
- 1 yocto is enforced on every user value-moving call (`withdraw`, `deposit`, `withdraw_margin`, `request_order`, `cancel_order`, `cancel_conditional`, `claim_fees`, `convert_pnl`, `lp_redeem`, `insurance_deposit`; `lp_deposit` and `place_conditional` require at least 1 yocto, which still needs a full-access key).
- The contract account cannot act as a user (`Self::user()`, `contract.rs:1770-1774`).
- Per-call state size: group about 2.4 KB plus a 9.5 KB portfolio; a fill reads and writes three such values. Far from the 4 MB and 300 TGas limits (README sandbox numbers: under 7 TGas per call). No change method iterates an unbounded collection; `scan_order_ids` is a view (acknowledged L-1).
- Wasm profile: `overflow-checks = true`, `panic = "abort"` at the workspace root, which is the profile the documented build command uses. (Casts are the gap: NEAR-8.)
- User-supplied `AccountId`s are validated by near-sdk on JSON deserialisation.

## Not done

- Sandbox (real-runtime) tests were not run, so real gas for the failure path of `on_withdraw` (10 TGas budget) and for `on_price_read` (50 TGas) is estimated from the README figures, not measured.
