# HANDOFF

## 2026-11-11: setup + plan step 1.1

Layout: `crates/engine` (vendored Percolator v16, untouched), `crates/neap` (contract, was `percolator-near`),
`crates/README.md` + `SECURITY.md` (port notes, past findings).

Step 1.1 done:
- Crate/lib/wasm renamed `percolator-near` → `neap`, struct `Perp` → `Neap`; sandbox crate `neap-sandbox`.
- `src/model.rs`: `Owner {Near, Sol}`, `Tier`, `Mode`, `PriceSource`, `Observation`, `Market` (replaces `MarketInfo`).
  Wallets, portfolios, LP shares are keyed by `Owner`. `Owner::Sol` exists but nothing constructs it yet (phase 2).
- Events: `standard: "neap"`, `version: "1.0.0"`, `fill` carries `owner` + `block` (spec §8).
- Verified: `cargo test` in `crates/neap` (22 old + 1 new pass), release wasm builds (560 KB).
  Not re-run: Kani, the sandbox gas test (`crates/neap/sandbox`).

Defaults I chose (unconfirmed):
- `PriceSource::Pushed` is a transitional variant so the owner-only `create_market` + `push_price` keep working;
  it is deleted in 1.3 (see `docs/contract.md` §3). Owner-created markets are `Tier::Major`, `Mode::Active`, bond 0.
- Public method args stay `AccountId` (views, `keeper_step`, `liquidate`); they wrap to `Owner::Near`. `on_withdraw`
  takes `Owner`. Switch views to `Owner` when Solana users land (2.3).
- `Mode` and `Tier` are stored and exposed but not enforced yet (1.2, 1.4).

Next: 1.2 observation + oracle rules (stale ladder, EMA, monotone `seq`/`source_ts`).

## 2026-11-11: workspace + Rust-only services

- Root `Cargo.toml` workspace: `crates/engine`, `crates/neap`, `services/*` (keeper, indexer, relayer, oracle-agent, types).
  Service crates are empty skeletons. Single root `Cargo.lock`, `.cargo/config.toml` (`RUST_MIN_STACK`).
- Excluded (own roots): `crates/neap/sandbox`, `crates/neap/sandbox/mock-ft`.
- Profiles moved to the root. `release` is now the contract's wasm profile (opt-level z); the engine's old
  `release` (lto fat, opt 3) is gone. `sbf` kept. Engine's `[workspace.metadata.kani]` became `[package.metadata.kani]`
  (not re-verified with `cargo kani`; check before relying on it).
- D21: all services are Rust, no TypeScript.
- Commands now run from the root: `cargo test -p neap`, `cargo test -p percolator --features fuzz`.
