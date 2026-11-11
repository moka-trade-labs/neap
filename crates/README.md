# percolator → NEAR

The percolator perp risk engine, hardened, plus a NEAR contract around it.
Research-grade, not audited.

```
crates/
  engine/     vendored dcccrypto/percolator v16 risk engine (no_std, chain-agnostic)
              + moka changes (see "Engine changes"); UPSTREAM.md has provenance
  neap/       neap (was percolator-near): the NEAR contract (near-sdk 5)
    src/policy.rs     pure wrapper decisions, Kani-proved
    src/exchange.rs   engine glue + spec §9 sequencing, no near-sdk
    src/contract.rs   storage, auth, NEP-141 custody, async withdraw
    src/proofs.rs     Kani harnesses for policy
    tests/            exchange_flow.rs (engine-level E2E), contract_unit.rs (mocked runtime)
  SECURITY.md  security pass (13 bounded prompts), findings and fixes
```

## Why "translate to NEAR" is a wrapper, not a rewrite

The engine has zero Solana dependencies: it is `no_std` Rust over plain
byte-layout structs (`bytemuck::Pod`), with no CPI, signer checks or token
transfers. Everything Solana-specific lived in the separate `percolator-prog`
wrapper. So the port keeps the 24k-line, 325-harness verified engine
byte-for-byte and replaces only the wrapper. Rewriting the math would throw away
its proofs.

| Solana wrapper (percolator-prog) | NEAR wrapper (this repo) |
|---|---|
| Slab account (zero-copy) | Market group bytes in one storage key, portfolio bytes per `(market, account)` |
| `Clock::slot` | `env::block_height()` |
| Instruction fails ⇒ tx reverts | Engine `Err` ⇒ `env::panic_str` ⇒ receipt reverts |
| SPL token CPI | NEP-141 `ft_transfer_call` in, `ft_transfer` + callback out |
| Signer check | `predecessor_account_id` + 1 yoctoNEAR on value-moving calls |
| Matcher program CPI | No matcher: the LP pool portfolio is the counterparty (pool-to-peer) |
| Pyth / authority push | `push_price` from the oracle account (NEAR TEE agent) |

## Contract layout

- **One engine market group per listed market.** Risk params (`V16Config`) are
  per group, so this gives per-market leverage/envelope and isolation. A trader
  has one portfolio per market they open. Cross-margin across markets is a later step.
- **LP pool** = an ordinary engine portfolio owned by the contract, taking the
  other side of every fill at `oracle ± spread_bps` (rounded against the taker).
  LP shares are ERC-4626-style with a virtual offset; deposits priced at the upper
  NAV bound, redemptions at the lower bound (unrealized pool profit is never paid out).
- **Funding** comes from pool skew, never from callers: traders net long ⇒ longs pay,
  scaled by `|pool position| / OI`, capped by the market's funding cap.
- **Oracle** keeps the raw target separate from the engine price and walks the
  engine price toward it along the spec's canonical capped staircase
  (`max_price_move_bps_per_slot`). While they differ, trades, withdrawals, profit
  conversion and LP deposits are refused (spec §9.7). A flat market re-anchors instantly.
- **Wallet ledger**: tokens land in a per-user wallet (`ft_on_transfer`), move into
  margin with `deposit`, back with `withdraw_margin`, and out with `withdraw`.
  Custody invariant: `Σ wallets + Σ engine vaults` is tracked and Kani-proved per operation.

### Methods

| Who | Method |
|---|---|
| owner | `new`, `create_market(symbol, init_price, params, spread_bps, max_oracle_age_ns)`, `set_oracle`, `set_owner` |
| user (1 yocto) | `deposit`, `withdraw_margin`, `trade(market, size_q, limit_price)`, `convert_pnl`, `withdraw`, `lp_deposit`, `lp_redeem`, `insurance_deposit` |
| user (storage) | `register`, `open_account(market)` |
| oracle | `push_price(market, price, publish_ns)` |
| anyone | `crank(market)`, `keeper_step(market, account)`, `liquidate(market, account)` |
| views | `get_market`, `get_account`, `get_wallet`, `get_lp_shares`, `get_custody`, `get_markets_count` |

Units: `size_q` is base quantity × `POS_SCALE` (1e6). Prices are quote atoms per
1 base unit (e.g. $100 with 6-decimal USDC = `100_000_000`).

### Choosing envelope params

The engine refuses any market where one worst-case step can eat maintenance margin:
`max_price_move_bps_per_slot × max_accrual_dt_slots + funding + liquidation fee ≤ MM`
(exact per-notional check at listing). Example used in tests: 10x IM, 5% MM,
30 bps/block, 10-block accrual window, 50 bps liquidation fee. `h_min` (profit
warmup) must be > 0 for public markets (spec §9.2); the wrapper enforces it.

## Real-runtime results (NEAR sandbox 2.14.0)

`neap/sandbox/tests/gas.rs` deploys the contract and a mock NEP-141 USDC into a
local NEAR node and runs: list market → LP seeds $1M → trader deposits via
`ft_transfer_call` → opens 10 units → oracle +1% → keeper cranks the staircase →
withdraws margin while positioned → closes → converts profit → withdraws to the
token (ends +$7.98 after spread and fees) → second trader opens ~9x → oracle −7% →
keeper liquidates (partial, fee to insurance) → custody ledger equals engine vault.

| method | gas (TGas) |
|---|---|
| trade (open) | 5.5 |
| trade (close) | 6.0 |
| withdraw_margin (open position) | 4.7 |
| liquidate | 4.7 |
| keeper_step | 4.3 |
| convert_pnl | 4.4 |
| crank / push_price | 2.9 |
| deposit / lp_deposit | 3.8 / 4.9 |
| withdraw (with ft_transfer + callback) | 6.8 |
| create_market / open_account / register | 3.8 / 3.4 / 2.8 |

Every call is under 7 TGas of NEAR's 300 TGas limit, so batching (e.g. a keeper
liquidating several accounts per tx) has plenty of room. Release wasm is 516 KB.

Build + run (the sandbox needs `RLIMIT_NOFILE` 65535; in containers with a lower
hard limit, preload a shim that clamps `setrlimit`):
```bash
cd crates/neap
RUSTFLAGS="--cfg near -C link-arg=-s -C target-cpu=mvp" CARGO_TARGET_DIR=target-wasm \
  cargo build --target wasm32-unknown-unknown --release
RUSTFLAGS="--cfg near -C link-arg=-s -C target-cpu=mvp" CARGO_TARGET_DIR=target-wasm \
  cargo build --manifest-path sandbox/mock-ft/Cargo.toml --target wasm32-unknown-unknown --release
cd sandbox && cargo test --release -- --nocapture
```

## Engine changes (moka fork)

1. `withdraw_with_open_positions_not_atomic` — spec §8.4 nonflat withdraw.
   Upstream only allowed withdrawing when flat, which blocks "withdraw free margin"
   for traders and makes a pooled LP unredeemable. Gate: full refresh, current
   certificate, no oracle lag, equity measured *without* positive PnL and against
   certified equity must cover IM. Kani: `tests/proofs_moka_withdraw.rs` (3 harnesses).

## Verification status

| Check | Result |
|---|---|
| Engine `cargo test --features fuzz` | 434 passed, 0 failed |
| Engine Kani, moka harnesses (`--harness proof_moka_`) | 3/3 verified, all covers hit |
| NEAR `cargo test` | 22 passed (10 engine-flow, 9 mocked-runtime contract, 3 property tests: LP math, pool exec price (exhaustive on a small domain + 2M random), skew funding) |
| NEAR sandbox (real runtime) | full flow incl. liquidation passes; gas table above |
| NEAR Kani (`cargo kani --no-default-features`) | 9 of 14 verified: custody (3), oracle acceptance, within_limit, skew funding sign/bound, LP NAV bounds, LP excess-redeem, exec price zero-spread (16-bit inputs). The 5 harnesses whose kernels use 128-bit (or symbolic-factor 64-bit) mul/div did not finish in 20 min under CBMC even at 16-bit inputs; their properties are covered by `tests/policy_props.rs` instead |
| wasm32 build | release wasm builds (516 KB) and runs in the sandbox |

Run:
```bash
cargo test -p percolator --features fuzz
cd crates/engine && cargo kani --tests --features fuzz --harness proof_moka_
cargo test -p neap
cd crates/neap && cargo kani --no-default-features
cd crates/neap && cargo near build non-reproducible-wasm   # needs cargo-near
```

## QEDGen evaluation

[QEDGen/qedsvm](https://github.com/QEDGen/qedsvm) is a Lean 4 library that verifies
compiled **Solana sBPF** bytecode path-by-path. It does not read Rust source, wasm,
or NEAR contracts, so it cannot check this port. Verdict: **not useful for the NEAR
build**; it might be useful later if we also ship the Solana build. Kani (already
used upstream and here) is the right tool for the NEAR side.
