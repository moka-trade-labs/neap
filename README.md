# Neap

Perps on any token, from any chain, settled on NEAR.

- **Any token:** create a market by naming its price source, a NEAR DEX pool or a pool on another chain read by a TEE oracle.
- **Any chain:** deposit, open, close and list from Solana (and later EVM) through NEAR Intents.
- **Pool-to-peer:** each market's USDC pool is the counterparty, so no order book is needed. Percolator risk engine, isolated markets.

Start with `docs/one-pager.md`, then `AGENTS.md`.

## Where things live

| Path | Holds |
| --- | --- |
| `Cargo.toml`, `Cargo.lock`, `.cargo/` | Root Cargo workspace: engine, contract and every service. Holds the shared release profile (wasm-size) and `RUST_MIN_STACK`. The sandbox crates are excluded (own roots). |
| `crates/engine/` | Percolator v16 risk engine (margin, PnL, funding, liquidation, haircut). `no_std`. Math is not edited. `UPSTREAM.md` has provenance. |
| `crates/neap/` | The smart contract, compiles to `neap.wasm`. |
| `crates/neap/src/model.rs` | Data types: `Owner`, `Market`, `Tier`, `Mode`, `PriceSource`, `Observation`. |
| `crates/neap/src/policy.rs` | Pure wrapper math (pool pricing, funding, oracle acceptance, LP shares, custody ledger). No engine, no near-sdk. Kani-proved. |
| `crates/neap/src/exchange.rs` | Engine glue and call ordering (oracle catch-up, lag gating, pool-to-peer fills). No near-sdk. |
| `crates/neap/src/contract.rs` | near-sdk layer: storage, auth, NEP-141 custody, entry points, events. Will be split into modules as steps touch it. |
| `crates/neap/src/proofs.rs` | Kani harnesses for `policy`. |
| `crates/neap/tests/` | `exchange_flow.rs` (engine-level), `contract_unit.rs` (mocked runtime), `policy_props.rs` (property tests). |
| `crates/neap/sandbox/` | Real-runtime tests: gas numbers, full flow on a local NEAR node; `mock-ft/` is a fake USDC. |
| `crates/README.md`, `crates/SECURITY.md` | Port notes and gas numbers; past security findings. |
| `services/` | All off-chain services are Rust (members of the root workspace; currently empty skeletons). |
| `services/keeper/` | Off-chain loop: refresh price, execute orders, triggers, liquidate, crank. |
| `services/indexer/` | Events to Postgres, REST and WebSocket API. |
| `services/oracle-agent/` | Phase 2: TEE agent that signs Solana pool prices. |
| `services/relayer/` | Phase 2: forwards Solana-signed actions, pays gas. |
| `services/types/` | Rust crate `neap-types`: types shared by the services (no near-sdk). |
| `web/` | Frontend (jason). |
| `docs/one-pager.md` | The product on one page. |
| `docs/decisions.md` | Founder decisions D1 to D20. |
| `docs/architecture.md` | Whole system, flows, price sources, data plan. |
| `docs/components.md` | Perp-DEX components mapped to Neap, data flow, inputs and outputs. |
| `docs/contract.md` | Contract spec (normative), state, methods, params, invariants, gap list. |
| `docs/plan.md` | Build order with done-criteria. |
| `docs/security-checks.md` | Security review prompts. |
| `docs/diagrams/` | Rendered system, contract and create-market diagrams. |
| `AGENTS.md`, `CLAUDE.md` | Rules and skills for coding agents. |
| `HANDOFF.md` | Latest state; updated before each session ends. |

Status: hackathon MVP in progress (plan step 1.1 done). Not audited.
