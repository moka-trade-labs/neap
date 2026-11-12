# Neap Testnet Deployment

**Contract:** `neap-trade.testnet`
**Network:** NEAR Testnet
**Deployed:** 2026-11-12

## Accounts

| Account | Role | Notes |
|---------|------|-------|
| `neap-trade.testnet` | Contract + Owner + Oracle | Holds the deployed contract, pushes prices |
| `ops.neap-trade.testnet` | LP provider, trader | Seeded the pool with 50 USDC |
| `usdc.fakes.testnet` | Quote token (USDC) | Standard NEAR testnet fake USDC, 6 decimals |

## Access for Amalnath

Amal's full-access key on `neap-trade.testnet`:

- **Public key:** `ed25519:DvT51sFiedZNjz1tRP4Ve2L6n8C27bYMcRwnAzjwysjn`
- **Private key:** `ed25519:5PWRmzv543jV4wN7wr3QLEqC7Eonc5npiFpznbex5dV2iFexXFYAVhvwa8tZPbNDZdtkWgJ4bxv9kgq7Bk9hRPfe`
- **Seed phrase:** `hungry glory swallow fence wheat victory skill economy fatigue shift trigger myth`

To import into CLI:

```bash
near account import-account using-private-key ed25519:5PWRmzv543jV4wN7wr3QLEqC7Eonc5npiFpznbex5dV2iFexXFYAVhvwa8tZPbNDZdtkWgJ4bxv9kgq7Bk9hRPfe network-config testnet
```

This key has **full access** to the contract account (owner powers: create markets, push prices, set admin roles). To revoke later:

```bash
near account delete-key neap-trade.testnet ed25519:DvT51sFiedZNjz1tRP4Ve2L6n8C27bYMcRwnAzjwysjn \
  network-config testnet sign-with-keychain send
```

## Market: NEAR-PERP (market_id = 0)

| Param | Value |
|-------|-------|
| Symbol | NEAR-PERP |
| Price ($) | 5.00 (price_e6 = 5000000) |
| Mode | Active |
| Price source | Pushed (owner oracle) |
| Max leverage | 10x (IM 10%) |
| Maintenance margin | 8% |
| Trading fee | 5 bps (0.05%) |
| Liquidation fee | 5% of closed notional (cap 50 USDC) |
| Max price move | 10 bps/block x 20 blocks |
| LP seed | 50 USDC |
| Spread | 5 bps |
| OI caps | Uncapped (set with `set_oi_caps`) |

## Deployment transactions (verify on explorer)

### Step 1: Deploy + init
```
TX: B4VQxs5ygNKg9ySEsLBmWWJgLSGJYRrWd1rQznDs3Na2  (failed - insufficient balance)
TX: riWbqMdswGezV1DWRkNQSqnkxbVexf42pxWr4itskoJ   (failed - insufficient balance)
TX: 5AN9qzBJrZFgDoNrnX9kupYpWiJYpweTkueygLazAFwM   (SUCCESS)
```
Method: `new(owner: "neap-trade.testnet", quote_token: "usdc.fakes.testnet", oracle: "neap-trade.testnet")`

### Step 2: Register contract on USDC
```
TX: 9HvoyBEtqD9DB2FSBLaErb1YQBnzgbpdTsQdNXyJCKgY
```
Method: `usdc.fakes.testnet::storage_deposit(account_id: "neap-trade.testnet")`

### Step 3: Mint 100 testnet USDC
```
TX: DEhMqjhEt2no5SZvZ3dAVWmPanhqn8nQSTe8sRyYjHPz
```
Method: `usdc.fakes.testnet::mint(account_id: "neap-trade.testnet", amount: "100000000")`

### Step 4: Create NEAR-PERP market
```
TX: Af1vSkvLYVorhkcS7fjJnMZ1ZeKgomgAcKthJwmJkUwe
```
Returned market_id: `0`

### Step 5: Seed LP pool (via ops.neap-trade.testnet)
```
ops account created, USDC minted and registered, then:
ft_transfer_call  → 50 USDC deposited to neap wallet
lp_deposit        → 50 USDC seeded into market 0 LP pool
```

### Step 6: Push initial price
```
push_price(market: 0, price: "5000000", publish_ns: "1794479656370028032")
```

### Step 7: Add Amal's access key
```
TX: 7EXwSpodRmK1PrKFy5tYCTLLzSBbtwCkTWUqEz92yyY5
```
Added `ed25519:DvT51sFiedZNjz1tRP4Ve2L6n8C27bYMcRwnAzjwysjn` as full-access key.

### Step 8: Keeper verified on testnet
The keeper service (`services/keeper/`) connected to testnet and ran one full tick:

1. Connected to testnet RPC (`https://rpc.testnet.near.org`)
2. Pushed price `5000000` ($5.00) → TX `7ihpS9PGsC3LuB43RYWmKmoy6F4ysUa5GfpShHaChX2T`
3. Cranked market 0 → TX `BxZ3kvR8yux9zZjQjXSSAVNJgmefjV2oKriEHe5q7YcT`

Market state after: `mode: Active`, `seq: 2`, `effective_price: 5000000`, `slot_last: 277352697`.

### Step 9: Security fixes + contract redeployment (2026-11-12)
Redeployed with 5 security fixes (see `docs/security-audit-2026-11-12.md`):
```
TX: CLtndaabh7diB3s9t9AEBtSQTFbcwUzzJw2EZkUe9Nb3
WASM SHA-256: 980c6dfb0a1a247939b0fe4fb6722a7784e86c2d2698a6266f02df167575eeed
Size: 699.5 KB
```
Fixes: C-1 (mode gates), C-2 (atomicity), H-2 (staleness on liquidate), H-3 (ReduceOnly enforcement), M-3 (expired order storage leak).

### Step 10: Full retest (2026-11-12)

**Unit tests:** 48 passed (5 suites)
**Sandbox tests:** 2 passed (full flow + NearPool refresh_price)
**Builds:** neap WASM (0 errors), keeper (0 errors), indexer (0 errors)

**Testnet verification (all succeeded):**

| Test | Result | TX |
|------|--------|-----|
| `get_market(0)` | Active, NEAR-PERP, $5.10, seq=4 | (view) |
| `get_markets_count` | 1 | (view) |
| `get_lp_shares(0, ops)` | 50000000000000 | (view) |
| `get_next_order_id` | 2 | (view) |
| `push_price($5.10)` | Succeeded | `3RqFz8eRDjDbjjmCRF2LHGDnkkcXYWuvbhpqdxkfrDhs` |
| `crank(0)` | Succeeded | `DNQj51JntiKFUg4VnDEvpQtWkFAHu4xR7WcMoF4WFVJd` |
| `quote(Long, 1 unit)` | $5.1025, 5bps spread | (view) |
| `get_fee_ledger(Lister)` | 2550 atoms | (view) |
| `get_fee_ledger(Protocol)` | 513 atoms | (view) |

**Full trade flow tested on testnet:**

| Step | Account | TX status |
|------|---------|-----------|
| Create `trader.neap-trade.testnet` | — | Succeeded |
| Register on USDC + mint 10 USDC | trader | Succeeded |
| Register on neap | trader | Succeeded |
| Deposit 5 USDC via ft_transfer_call | trader | Succeeded |
| open_account(market=0) | trader | Succeeded |
| deposit(market=0, 5 USDC) | trader | Succeeded |
| request_order(Long, 1 unit) | trader | order_id=0, Succeeded |
| push_price($5.10) | oracle | Succeeded |
| execute_order(0) | oracle (keeper) | Fill price $5.10, Succeeded |
| get_account → position=1000000 (1 unit) | — | Verified |
| request_order(Short, 1 unit, reduce_only) | trader | order_id=1, Succeeded |
| push_price($5.10) | oracle | Succeeded |
| execute_order(1) | oracle (keeper) | Closed, Succeeded |
| get_account → position=0 (flat) | — | Verified |
| withdraw_margin(4994899) | trader | Succeeded |
| withdraw(4994899) | trader | Succeeded |
| ft_balance_of(trader) | — | 9994899 (started 10M, lost ~5101 in fees) |

## How to verify

```bash
# Check contract exists and has code
near account view-account-summary neap-trade.testnet network-config testnet now

# Check the market is live
near contract call-function as-read-only neap-trade.testnet get_market \
  json-args '{"market": 0}' network-config testnet now

# Check LP NAV
near contract call-function as-read-only neap-trade.testnet get_lp_shares \
  json-args '{"market": 0, "account": "ops.neap-trade.testnet"}' network-config testnet now

# Check wallet balance
near contract call-function as-read-only neap-trade.testnet get_wallet \
  json-args '{"account": "ops.neap-trade.testnet"}' network-config testnet now
```

## How to trade (manual test)

### 1. Get testnet USDC
```bash
near contract call-function as-transaction usdc.fakes.testnet storage_deposit \
  json-args '{"account_id": "<your-account>.testnet"}' \
  prepaid-gas '10 Tgas' attached-deposit '0.01 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send

near contract call-function as-transaction usdc.fakes.testnet mint \
  json-args '{"account_id": "<your-account>.testnet", "amount": "10000000"}' \
  prepaid-gas '10 Tgas' attached-deposit '0 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send
```

### 2. Register + deposit
```bash
near contract call-function as-transaction neap-trade.testnet register \
  json-args '{}' prepaid-gas '10 Tgas' attached-deposit '0.1 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send

near contract call-function as-transaction usdc.fakes.testnet ft_transfer_call \
  json-args '{"receiver_id":"neap-trade.testnet","amount":"5000000","msg":""}' \
  prepaid-gas '50 Tgas' attached-deposit '0.000000000000000000000001 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send

near contract call-function as-transaction neap-trade.testnet open_account \
  json-args '{"market": 0}' prepaid-gas '10 Tgas' attached-deposit '0.2 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send

near contract call-function as-transaction neap-trade.testnet deposit \
  json-args '{"market": 0, "amount": "5000000"}' \
  prepaid-gas '10 Tgas' attached-deposit '0.000000000000000000000001 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send
```

### 3. Open a long (two-step)
```bash
# Request (at block N)
near contract call-function as-transaction neap-trade.testnet request_order \
  json-args '{"market":0,"side":"Long","size_q":"1000000","limit_price":"18446744073709551615","expiry_blocks":30,"reduce_only":false}' \
  prepaid-gas '10 Tgas' attached-deposit '0.000000000000000000000001 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send

# Push fresh price (oracle — needs neap-trade.testnet key)
TS=$(python3 -c "import time; print(int(time.time() * 1e9))")
near contract call-function as-transaction neap-trade.testnet push_price \
  json-args "{\"market\":0,\"price\":\"5000000\",\"publish_ns\":\"$TS\"}" \
  prepaid-gas '10 Tgas' attached-deposit '0 NEAR' \
  sign-as neap-trade.testnet network-config testnet sign-with-keychain send

# Execute (anyone can call — the keeper does this)
near contract call-function as-transaction neap-trade.testnet execute_order \
  json-args '{"order_id": 0}' \
  prepaid-gas '30 Tgas' attached-deposit '0 NEAR' \
  sign-as <your-account>.testnet network-config testnet sign-with-keychain send
```

### 4. Check position
```bash
near contract call-function as-read-only neap-trade.testnet get_account \
  json-args '{"market": 0, "account": "<your-account>.testnet"}' \
  network-config testnet now
```

## Contract methods reference

| Method | Caller | Description |
|--------|--------|-------------|
| `register()` | anyone (1 yocto) | Create wallet ledger |
| `ft_on_transfer(sender, amount, msg)` | USDC only | Deposit USDC or create market |
| `open_account(market)` | anyone | Create margin account |
| `deposit(market, amount)` | owner (1 yocto) | Wallet → margin |
| `withdraw_margin(market, amount)` | owner (1 yocto) | Margin → wallet |
| `withdraw(amount)` | owner (1 yocto) | Wallet → USDC transfer |
| `request_order(...)` | owner (1 yocto) | Place pending order |
| `execute_order(order_id)` | anyone (keeper) | Fill pending order |
| `cancel_order(order_id)` | owner (1 yocto) | Cancel pending order |
| `place_conditional(...)` | owner (storage) | TP/SL/Limit order |
| `execute_conditional(cond_id)` | anyone (keeper) | Trigger conditional |
| `cancel_conditional(cond_id)` | owner (1 yocto) | Cancel conditional |
| `lp_deposit(market, amount)` | anyone (1 yocto+) | Seed LP pool |
| `lp_redeem(market, shares)` | owner (1 yocto) | Redeem LP shares |
| `liquidate(market, account)` | anyone | Liquidate undercollateralized |
| `crank(market)` | anyone | Advance oracle staircase |
| `push_price(market, price, publish_ns)` | oracle only | Push new price |
| `refresh_price(market)` | anyone | NearPool: fetch price |
| `claim_fees(market, payee)` | lister/owner (1 yocto) | Withdraw accrued fees |
| `set_mode(market, mode)` | pauser/owner | Tighten market mode |
| `set_oi_caps(market, ...)` | owner | Set OI caps |
| `set_pauser(account)` | owner | Set pauser role |
| `set_owner(account)` | owner | Transfer ownership |
| `quote(market, side, size_q)` | view | Preview execution price |
| `get_market(market)` | view | Market state |
| `get_account(market, account)` | view | Position/margin state |
| `get_wallet(account)` | view | Idle USDC balance |
| `get_fee_ledger(market, payee)` | view | Accrued fees |

## WASM build

```bash
RUSTFLAGS="--cfg near -C link-arg=-s" CARGO_TARGET_DIR=crates/neap/target-wasm \
  cargo build --target wasm32-unknown-unknown --release -p neap
```
Output: `crates/neap/target-wasm/wasm32-unknown-unknown/release/neap.wasm` (699.5 KB)
SHA-256: `980c6dfb0a1a247939b0fe4fb6722a7784e86c2d2698a6266f02df167575eeed`

**Deployed WASM matches local build** (verified via `near account view-account-summary`).

## Run tests yourself

### Prerequisites
```bash
# Rust toolchain
rustup target add wasm32-unknown-unknown

# NEAR CLI
cargo install near-cli-rs

# Postgres (only for indexer — skip if you just want contract tests)
# Use Neon (https://neon.tech) free tier or local: brew install postgresql
```

### 1. Unit tests (no network needed)
```bash
cd neap
cargo test -p neap
# Expected: 48 passed (5 suites)
```

### 2. Build the WASM
```bash
RUSTFLAGS="--cfg near -C link-arg=-s" CARGO_TARGET_DIR=crates/neap/target-wasm \
  cargo build --target wasm32-unknown-unknown --release -p neap
# Output: crates/neap/target-wasm/wasm32-unknown-unknown/release/neap.wasm
```

### 3. Sandbox tests (real NEAR runtime, no testnet needed)
```bash
# Build mock contracts first
cd crates/neap/sandbox/mock-ft
RUSTFLAGS="--cfg near" CARGO_TARGET_DIR=../../target-wasm \
  cargo build --target wasm32-unknown-unknown --release
cd ../mock-ref-pool
RUSTFLAGS="--cfg near" CARGO_TARGET_DIR=../../target-wasm \
  cargo build --target wasm32-unknown-unknown --release
cd ..

# Run sandbox tests
cargo test --release -- --nocapture
# Expected: 2 passed (sandbox_flow_and_gas + sandbox_refresh_price_near_pool)
```

### 4. Build the keeper
```bash
cargo build -p neap-keeper
# Expected: 0 errors
```

### 5. Run the keeper (needs testnet key)
```bash
NEAP_CONTRACT=neap-trade.testnet \
NEAP_SIGNER=neap-trade.testnet \
NEAP_SIGNER_KEY="ed25519:..." \
NEAP_MARKETS=0 \
NEAP_PUSH_PRICE=5100000 \
NEAP_LOOP_MS=10000 \
RUST_LOG=info \
cargo run -p neap-keeper
# You should see: "pushed price" and "cranked" logs
```

### 6. Build the indexer
```bash
cargo build -p neap-indexer
# Expected: 0 errors
```

### 7. Run the indexer (needs Postgres)
```bash
NEAP_CONTRACT=neap-trade.testnet \
DATABASE_URL=postgres://user:pass@localhost/neap \
API_PORT=3000 \
RUST_LOG=info \
cargo run -p neap-indexer
# API at http://localhost:3000/markets
# WebSocket at ws://localhost:3000/ws
```

### 8. Verify testnet contract (no local setup needed)
```bash
# Check the market is live
near contract call-function as-read-only neap-trade.testnet get_market \
  json-args '{"market": 0}' network-config testnet now

# Check the quote
near contract call-function as-read-only neap-trade.testnet quote \
  json-args '{"market":0,"side":"Long","size_q":"1000000"}' \
  network-config testnet now

# Check fee ledger
near contract call-function as-read-only neap-trade.testnet get_fee_ledger \
  json-args '{"market":0,"payee":"Lister"}' network-config testnet now
```
