# neap-keeper

Automated off-chain service that keeps the Neap perps contract running (plan step 1.10).

## What it does

One loop per market, every N seconds:

1. **Push price** (`push_price` for Pushed markets, `refresh_price` for NearPool)
2. **Crank** the oracle staircase to the current block
3. **Execute pending orders** (scan `0..next_order_id`, call `execute_order`)
4. **Trigger conditionals** (scan, call `execute_conditional`)
5. **Post-fill crank** (settle funding after fills)

Holds no funds. The contract re-checks every call. Idempotent — safe to restart at any time.

## Run

```bash
NEAP_CONTRACT=neap-trade.testnet \
NEAP_SIGNER=neap-trade.testnet \
NEAP_SIGNER_KEY=ed25519:... \
NEAP_MARKETS=0 \
NEAP_PUSH_PRICE=5000000 \
NEAP_LOOP_MS=5000 \
RUST_LOG=info \
cargo run -p neap-keeper
```

## Environment variables

| Var | Required | Default | Description |
|-----|----------|---------|-------------|
| `NEAP_CONTRACT` | Yes | — | Contract account ID |
| `NEAP_SIGNER` | Yes | — | Signer (oracle/keeper) account |
| `NEAP_SIGNER_KEY` | Yes | — | Private key (`ed25519:...`) |
| `NEAP_MARKETS` | No | `0` | Comma-separated market IDs |
| `NEAP_PUSH_PRICE` | No | `0` | Fixed price_e6 to push (0 = use refresh_price) |
| `NEAP_LOOP_MS` | No | `3000` | Loop interval in ms |
| `NEAR_RPC_URL` | No | `https://rpc.testnet.near.org` | RPC endpoint |
| `RUST_LOG` | No | `info` | Log level |

## Verified on testnet

Pushed price and cranked market 0 on `neap-trade.testnet` — TX `7ihpS9PGsC3LuB43RYWmKmoy6F4ysUa5GfpShHaChX2T`.
