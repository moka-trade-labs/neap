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
Output: `crates/neap/target-wasm/wasm32-unknown-unknown/release/neap.wasm` (687 KB)
SHA-256: `99759e2d1cf265691a92d845a7df58dc921c08bc9ffa65229c420d7c0ed8b5ae`
