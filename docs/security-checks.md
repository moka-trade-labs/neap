# Security check prompts

> Copied from moka-research. References to `architecture.md` §9 and `docs/committee/` point at that repo; in neap use `docs/architecture.md` and `docs/contract.md` §6 as the threat model.

Source: the thread by @0xAsm0d3uls (https://x.com/0xAsm0d3uls/status/2119071106992873592), pasted into the
project thread by jason on 2026-11-09. Credit to the author; the base prompts below are theirs, verbatim in
meaning. The NEAR/perp mutations and the process section are ours.

## Process (from the author, adapted)

1. Take the base prompts below.
2. Run them through a **mutator** that knows our threat model (`architecture.md` §9, `docs/committee/*/D-red-team.md`),
   producing goal-bounded variants for each component.
3. Spawn a **swarm of cheap agents**, one variant each, with a bounded goal and the relevant files only.
4. Any new signal (suspicious path, failing invariant) goes back into the mutator as a new seed.
5. Repeat until paths are exhausted or signals stop.
6. Use a **frontier model to triage**: confirm, write a failing test or Kani harness, and hunt variants.

Bounded goals over open-ended exploration: cheap models do much better with "find X in Y" than "find bugs".

## Base prompts

1. Find places where the app is supposed to enforce something, but doesn't actually enforce it.
2. Find security rules that are enforced in one part of the app, but just assumed to be true in another.
3. Find security assumptions the app relies on, but never actually checks or enforces.
4. Find places where the app depends on undocumented behavior, and what happens if that behavior changes.
5. Find operations that are secure on their own, but become vulnerable when you combine or chain them together.
6. Find cases where the same object can be accessed in different ways, but each way ends up having different permissions.
7. Find objects whose identity can be changed or rebound to another user, without going through the right checks.
8. Find requests that get rejected or fail, but still end up changing something or triggering a side effect.
9. Find security checks that only work if certain events happen in a specific order, and what happens if that order changes.
10. Find operations where the transaction fails or rolls back, but some of the side effects still happen.
11. Find things you can still do with an object even after it has been deleted, or access to it has been revoked.
12. Find state changes that can be reversed or undone by someone who shouldn't have permission to do so.
13. Find objects that can end up in two contradictory states at the same time, and what operations become possible because of that.

## Seed mutations for our threat model

Use these as the first mutator output; generate more per PR.

| Base | Mutation for `perp` on NEAR |
| --- | --- |
| 1 | Find entrypoints where an OI cap, leverage ratchet, market status (NoNewOpens/ReduceOnly) or listing rule is specified but not checked on that path (e.g. `execute_order` vs liquidation vs deposit-and-open). |
| 2 | Find checks done in `execute_order` (price age, `source_ts ≥ request_ts + δ`, band) that `crank`, `liquidate` or `ft_on_transfer`-embedded orders assume but skip. |
| 3 | Find where we assume `predecessor_account_id` is a whitelisted token contract, that `intents.near` delivered the stated amount, or that the oracle key cache is current. |
| 4 | Find dependence on NEAR receipt ordering, `ft_transfer_call` refund semantics, 1Click status semantics, or engine `*_not_atomic` behaviour that is not in a spec. |
| 5 | Chain: deposit-and-open + cancel + withdraw; LP deposit + price push + LP withdraw; liquidate + backing top-up in the same block. |
| 6 | Find a position, LP share or margin balance reachable via both a session key and the full-access key, or via intents-signed and NEAR-signed paths, with different checks. |
| 7 | Find whether an order request, LP share, pending refund or session key can be re-pointed to another account (`account_id` in args vs predecessor). |
| 8 | Find rejected orders/expired requests that still move fees, update `seq`, consume a nonce, or change the EMA/skew. |
| 9 | Find checks that depend on `push_price` happening before `execute_order`, or the withdraw callback happening before the next deposit. |
| 10 | **Async**: find any path where a `Promise` fires after engine state is written, and the callback failure leaves state inconsistent (pending refunds, double credit). Find any engine call that straddles a promise. |
| 11 | Find actions possible on a delisted/resolved market, a revoked agent key, an expired order, or a deleted session key. |
| 12 | Find state (market status, oracle key, pending refund, LP lock) that a non-owner can roll back, e.g. by pushing an older but still-valid signed price. |
| 13 | Find a market that is both ReduceOnly and Live for different code paths; an order both Filled and Cancelled; an LP both withdrawing and backing. |

## Baseline gate for every contract PR (in addition to the swarm)

1. **Value conservation**: list debits and credits per entrypoint; rounding always against the user.
2. **Async boundaries**: every `Promise`, what is committed when it fires, what the callback does on failure.
3. **Auth**: `predecessor_account_id` vs `signer_account_id`; `#[private]` callbacks.
4. **Oracle**: registered key, fresh, monotone `seq`/`ts`, inside band; stale handling.
5. **Storage**: NEP-145 enforced; nobody can make the contract pay storage.
6. **Bounded work**: no unbounded iteration.
7. **Admin**: what the owner can change, timelocks, no path to user funds.
8. **Engine invariants**: Kani harnesses for touched kernels; stock reconciliation.
9. `near-contract-audit` skill on the diff.
