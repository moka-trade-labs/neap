# AGENTS.md: operating manual for the neap repo

Neap: perps on any token, from any chain, settled on NEAR. Pool-to-peer markets, Percolator risk engine,
NEAR Intents for cross-chain, TEE oracle for prices NEAR can't read.

## Read first (in order)

1. `docs/one-pager.md`: what we're building.
2. `docs/decisions.md`: what is decided. Don't reverse anything silently; add a dated entry.
3. `docs/architecture.md`: the whole system and its flows.
4. `docs/contract.md`: the contract spec (normative) and the gap list.
5. `docs/plan.md`: the build order and the definition of done for each step.
6. `HANDOFF.md` (create it on the first session): the latest state. Update it before you stop.

## House rules

- **Spec first.** A behaviour change starts as a diff to `docs/contract.md`, then a failing test, then code.
- **Engine is sacred.** Don't edit engine math. If you must, open a separate PR with a spec diff and a Kani proof.
- **Integer math only.** No floats in the contract, keeper pricing or SDK amounts. Round against the user.
- **No engine call straddles a promise.** Callbacks never mutate engine state.
- **Every value-moving user call** requires 1 yoctoNEAR (NEAR) or a signed action with nonce + deadline (Solana).
- **Never weaken a test to make it pass.** Fix the spec (gap) or the code (bug).
- **Secrets** never go in the repo: keys, API keys, `.env`.
- **Security gate before merge** of contract code: `cargo test`, Kani for touched kernels, `docs/security-checks.md`
  prompts, `near-contract-audit`. Put the results in the PR body.

## Loop for every component

1. Write or update the spec section. 2. Write the test or Kani harness that fails if it's false. 3. Implement.
4. Run tests, proofs and security prompts. 5. Feed failures back to step 1 (spec gap) or step 3 (bug).

## Skills

```bash
bunx skills add near/agent-skills                                     # near-smart-contracts, near-intents, near-ai-cloud, near-contract-audit, near-cli-rs, near-api-js, near-dapp
npx skills add https://github.com/solana-foundation/solana-dev-skill  # Solana pool reads, Phantom
npx skillfish add julianromli/opencode-template backend-dev            # keeper, indexer, relayer
npx skillfish add chris-yyau/busdriver latency-critical-systems
npx skillfish add miles990/claude-software-skills realtime-systems
npx skillfish add mwathiben/hush-private-bookmarks solana-security
npx skillfish add quyennguyenvu/grimoire arch-diagram
npx skillfish add affaan-m/ecc strategic-compact
```

Third-party skills are prompts that run with your permissions. Read each SKILL.md before first use.

| Job | Skills |
| --- | --- |
| Contract (near-sdk, storage, promises) | `near-smart-contracts`, `near-cli-rs` |
| Security review | `near-contract-audit`, `docs/security-checks.md`, `solana-security` (agent side) |
| Deposits, withdrawals, 1Click | `near-intents` |
| TEE oracle agent | `near-ai-cloud`, Shade Agent docs |
| Solana pool reads | `solana-dev` |
| Keeper, indexer, relayer | `backend-dev`, `latency-critical-systems`, `realtime-systems` |
| Diagrams | `arch-diagram` (mermaid) |

## Handoff

Before ending a session, append to `HANDOFF.md`: what changed, what's half-done, what's next, and any default you
chose that jason or Subh hasn't confirmed.
