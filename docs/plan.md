# Neap MVP build plan

For: Subh (contracts, keeper, oracle agent, indexer). The frontend is jason's (§6).
Window: 2026-11-11 → submission ~2026-11-14. Read `one-pager.md`, `architecture.md`, `contract.md` and `decisions.md` first.

## 0. Ground rules

- **Spec first, then test, then code.** If behaviour changes, update `docs/contract.md` in the same PR.
- **Never rewrite engine math.** `engine/` is the verified Percolator v16 (vendored with provenance). If it truly
  needs a change, write it as a spec diff plus Kani proof, in its own PR.
- **Integer math only.** Prices `u64` scaled 1e6 at the boundary; `u128` inside. Every division rounds against the user.
- **One receipt per value-bearing transition.** See the atomicity rule in `contract.md` §1.
- **Each PR:** `cargo test`, Kani for touched kernels, the security prompts in `docs/security-checks.md`, and the
  `near-contract-audit` skill. Record the results in the PR body.
- **Mainnet with caps.** Every market has a seed cap of 50 USDC and a global contract TVL cap during the hackathon.

## 1. Day 0: set up (≈2 h)

1. Create the `neap` repo. Copy in the starter-kit files and `moka-research/percolator/` (engine + near).
   Pick your own folder layout; keep the engine as its own crate.
2. Toolchain: Rust stable, `wasm32-unknown-unknown`, near-cli-rs, cargo-near (if it won't install, build with
   `RUSTFLAGS='--cfg near' cargo build --release --target wasm32-unknown-unknown`, as in the port).
3. Run the port's tests: `cargo test` in the engine and in near, plus the sandbox gas test (needs the
   RLIMIT_NOFILE shim noted in `percolator/README.md`). **Done when** everything passes on your machine.
4. Create NEAR accounts: `neap-dev.<you>.testnet` for fast iteration, `neap.<team>.near` for the demo. Fund the mainnet
   one with a few NEAR and ~200 USDC.
5. Install skills (`AGENTS.md`).

## 2. Phase 1: everything on NEAR (Days 1–2)

Each step is one PR with tests. Order matters.

| # | Step | Done when |
| --- | --- | --- |
| 1.1 | **Rename and reshape.** `percolator-near` → `neap`. Introduce `Owner` enum (NEAR only for now), a `Market` struct with `mode`, `tier`, `price_source`, `observation`, `lister`, `bond`. Events → `standard: "neap"`. | Old tests pass under the new names |
| 1.2 | **Observation + oracle rules.** One `Observation` type. Store it per market, plus EMA and the staircase that already exists. Stale ladder: >10 s no opens, >60 s ReduceOnly, >1 h PriceUncertain. | Unit tests for every ladder edge; no fill without a fresh observation |
| 1.3 | **NEAR pool reader.** `refresh_price(market)` → `Promise` `get_pool(pool_id)` on `v2.ref-finance.near` (and the quote pool if not USDC-quoted, joined with `.and()`) → `#[private] on_price_read` computes `price = reserve_usdc / reserve_token` with decimals, liquidity in USD, writes the observation only. Reject zero reserves, liquidity below the tier floor, or a move beyond the band. Verify Rhea DCL pools (`dclv2.ref-labs.near`) the same way if a launch token needs one. | Sandbox test with a mock pool contract; mainnet view call against a real pool returns a sane price |
| 1.4 | **Permissionless create_market.** `ft_transfer_call(bond, msg = CreateMarket{symbol, price_source, tier})` → `Proposed`; `activate_market` after the first good observation and listing checks (depth floor, token decimals via `ft_metadata`). Params come from the tier, bounded. Envelope check enforced. | A stranger account lists a market; a bad pool, low depth or a bad tier is refused with the bond refunded |
| 1.5 | **Pricing.** Skew and size spread, leverage ratchet, OI caps per side and per account, `quote()` view. Keep it in `policy.rs` (pure, Kani-provable). | Property tests: price monotone in size; skew-reducing trades are cheaper; caps hold |
| 1.6 | **Two-step orders.** `request_order` (escrow, `request_block`, expiry ≤ 30 s), `execute_order` (needs a newer observation), `cancel_order`, expiry cleanup. Remove the public direct `trade` (keep it internal). | Tests: same-block execute refused; expired order releases escrow; limit respected |
| 1.7 | **Conditional orders.** TP, SL, resting limit. Trigger on mark (engine price). Storage per order charged to the user; max 30 days; reduce-only for TP/SL. | Keeper test: a price path triggers exactly once |
| 1.8 | **Fees.** Engine trading fee 5 bps; wrapper ledgers lister 50 / LP 30 / insurance 10 / protocol 10, paid out with the engine's insurance-surplus withdrawal; `claim_fees`. Liquidation: 5% of closed notional, capped, 60% to the caller in the same call, 40% insurance. | Conservation test: Σ claimed ≤ Σ accrued; custody invariant holds |
| 1.9 | **Admin roles.** Pauser can only tighten (mode, caps, pause opens). Owner sets tier bounds and roles. No admin path to balances. | Test that every admin method leaves balances byte-identical |
| 1.10 | **Keeper (off-chain).** One loop per market: `refresh_price` → `execute_order` for pending ids → conditional triggers → `liquidate` near-MM accounts → `convert_pnl_for` after closes (F-1) → `crank`. Idempotent, restart-safe. Reads pending work from the indexer, falls back to contract views. | Run it 1 h on testnet with 2 bots trading; no stuck orders |
| 1.11 | **Indexer + API.** NEAR events from neardata or free RPC → Postgres (Neon/Supabase free). Tables: markets, observations (candles built from these), orders, fills, positions snapshot, liquidations, fees. REST + one WebSocket channel. Add the display feeds in §4. | The frontend can draw a chart, positions and history from the API alone |
| 1.12 | **Security gate + mainnet deploy.** Run the security prompts, fix findings, deploy to mainnet with caps, list 1 NEAR token market (Rhea pool) and seed it. | A real trade, close and withdraw on mainnet; findings recorded |

## 3. Phase 2: Solana (Days 3–4)

Start the spikes (2.0) on Day 1 in parallel if there's any slack. They're the riskiest part.

| # | Step | Done when |
| --- | --- | --- |
| 2.0a | **Spike: 1Click.** Quote Solana USDC → native NEAR USDC with the recipient = the `neap` contract and a `msg` (or recipient = an intents account, then `ft_withdraw` with msg). Measure p50/p95 latency. Record the result in `docs/spikes/1click.md`. | We know which of the two paths carries a msg into `ft_on_transfer` |
| 2.0b | **Spike: Shade Agent.** Deploy the template on Phala, register it in the agent registry on mainnet, sign `{market, price, liquidity, ts, seq}` for a Raydium pool. | A signature verifies with `env::ed25519_verify` in a sandbox test |
| 2.0c | **Spike: Phantom signing.** Phantom `signMessage` over borsh bytes of a signed action; verify in the contract. | Round trip works in a local page |
| 2.1 | **TEE price source.** `Tee{chain: Solana, pools}`; `submit_price(signed)` checks registry key, seq, ts, band; `sync_tee_keys`. Listing certificate (depth, pool age, mint and freeze authority = none) signed by the agent for `activate_market`. | A Solana-token market activates and trades on mainnet |
| 2.2 | **Oracle agent.** Reads each listed pool via ≥2 RPC providers (Helius free + the public endpoint), refuses if they disagree by > 0.5%, liquidity-weighted median across pools, 60 s EMA, SOL-quoted pools converted with the SOL/USDC pool price, pushes every ~3 s or on demand. | 1 h run without gaps; prices match DexScreener within 0.5% |
| 2.3 | **Solana identity.** `Owner::Sol(pk)`; `execute_signed` with nonce and deadline; a relayer that just forwards (it pays gas, holds no funds). | A Phantom user opens and closes with no NEAR account |
| 2.4 | **Deposit and open from Solana.** `ft_on_transfer` msg `For{owner: Sol(pk), then: DepositAndOpen{…}}` arriving from 1Click/intents. | Demo flow: Phantom USDC → position in < 90 s |
| 2.5 | **Close and withdraw to Solana.** `withdraw(to = Intents{account})` → `ft_transfer_call` to intents.near → 1Click withdrawal to the Solana address; failure → pending refund. | Round trip back to Phantom |
| 2.6 | **Create market from Solana.** The bond comes in with `CreateMarket` inside the `For{…}` msg. | A Phantom user lists a Solana memecoin market |

## 4. Free-tier data plan

- **NEAR:** FastNEAR `free.rpc.fastnear.com` for views and txs; neardata (free, ~180 req/min) for the block stream.
- **Solana:** Helius free (1M credits/mo, 10 rps, 5 WebSockets, WS metered per MB). Subscribe only to listed pools.
  The agent's second RPC: the public mainnet endpoint.
- **Discovery feed** (trending coins, pool lists, pre-listing charts) for both chains: GeckoTerminal and DexScreener public
  APIs, cached server-side 30–60 s. Never called from browsers.
- **Hosting:** keeper, indexer and relayer as one small Rust service on a free tier (Render/Fly/Railway free
  credits) or Cloudflare Workers cron. Postgres on Neon or Supabase free.

## 5. Cross-chain conversion: 1Click vs Rhea SDK

Decision: **1Click (NEAR Intents) for anything cross-chain; Rhea only client-side for NEAR-native tokens.**
1Click solvers already deliver *any supported origin asset* as native USDC on NEAR, so the contract only ever sees USDC.
Rhea's cross-chain aggregation SDK (`@rhea-finance/cross-chain-aggregation-dex`) wraps intents plus NEAR DEX routing. It's
useful in the frontend when a user deposits a NEAR token that isn't USDC (swap on Rhea first, then deposit), and as an
alternative router if 1Click lacks a pair. It adds no contract work. Verify the current package name in the spike: the npm
package and the `@rhea/sdk` named on rhea.finance differ.

## 6. Frontend (jason, plan later)

Lives in the same repo or its own; it talks only to the contract (via wallet) and the API. Screens: market list
(price, 24 h change, OI vs cap, funding, max leverage) · trade (chart from our candles, long/short, size, leverage,
the exact `quote()` preview, TP/SL, slippage) · positions (uPnL, liq price, margin, funding paid, close/partial/TP/SL)
· account (wallet vs margin vs intents balance, deposits/withdrawals with 1Click status) · LP (deposit, NAV, fees earned,
principal at risk) · create market (token picker from the discovery feed, price-source preview, tier, bond, expected
fee share) · market status banners (catching up, reduce-only, price uncertain, haircut %). Wallets: NEAR wallet selector +
Phantom. A function-call access key or the relayer gives one-click trading.

## 7. Demo script (Day 4)

1. NEAR wallet: list a NEAR token market (bond), seed it, trade it.
2. Phantom: deposit Solana USDC via intents → open a 5x long on the Solana memecoin market.
3. Price moves (live), PnL updates, set an SL, partial close.
4. Close and withdraw back to Phantom.
5. Show the haircut indicator and reduce-only banner with a forced price shock on a test market.

## 8. Cut list if time runs out (in this order)

Resting limit orders → Solana market creation (2.6) → close-and-withdraw to Solana (2.5, withdraw to NEAR instead) →
TEE listing certificate (protocol lists Solana markets manually) → conditional orders. **Never cut:** two-step orders,
oracle checks, OI caps, custody invariant tests.
