# neap-keeper

Automated off-chain service that keeps the Neap perps contract running (plan step 1.10).

## What it does

One loop per market, every N seconds:

1. **Price**: live USD price via `push_price` for Pushed markets, `refresh_price` for NearPool
2. **Activate** Proposed markets once they have an observation (`activate_market`)
3. **Crank** the oracle staircase to the current block
4. **Execute pending orders** (scan `0..next_order_id`, call `execute_order`)
5. **Trigger conditionals** (scan, call `execute_conditional`)
6. **Liquidate** every few ticks: accounts from the indexer with a non-zero position (`liquidate`)

Optional faucet (`FAUCET_ADDR`): `POST /faucet {"account_id"}` registers the account on test USDC and mints it.

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
| `NEAP_MARKETS` | No | `auto` | `auto` = every market on the contract, or comma-separated IDs |
| `NEAP_LIVE_PRICES` | No | `true` | Pushed markets get the live USD price (Coinbase, Kraken fallback) |
| `NEAP_PRICE_FEEDS` | No | from symbol | Feed symbol overrides, e.g. `0=NEAR,1=SOL` (default: market symbol minus `-PERP`) |
| `NEAP_PUSH_PRICE` | No | `0` | Fixed price_e6 fallback when no live feed answers |
| `NEAP_INDEXER_URL` | No | unset | Indexer base URL; enables the liquidation scan over `/accounts` |
| `NEAP_LIQ_EVERY` | No | `5` | Liquidation scan every N ticks |
| `FAUCET_ADDR` | No | unset | Internal listen address for the testnet faucet (e.g. `127.0.0.1:4001`) |
| `FAUCET_TOKEN` | No | `usdc.fakes.testnet` | Mintable test USDC |
| `FAUCET_AMOUNT` | No | `1000000000` | Atoms per drip (1,000 USDC) |
| `FAUCET_NEAR` | No | `0` | NEAR sent to accounts holding < 0.5 NEAR |
| `NEAP_LOOP_MS` | No | `3000` | Loop interval in ms |
| `NEAR_RPC_URL` | No | `https://rpc.testnet.near.org` | RPC endpoint |
| `RUST_LOG` | No | `info` | Log level |

## Verified on testnet

Pushed price and cranked market 0 on `neap-trade.testnet` — TX `7ihpS9PGsC3LuB43RYWmKmoy6F4ysUa5GfpShHaChX2T`.
