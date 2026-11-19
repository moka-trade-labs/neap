# neap-keeper

Automated off-chain service that keeps the Neap perps contract running (plan step 1.10).

## What it does

Independent loops sharing one signer:

1. **Prices** (every `NEAP_LOOP_MS`): for Pushed markets, read Coinbase and Kraken together and call
   `push_price` only when they agree within `NEAP_MAX_SOURCE_SPREAD_BPS` (the midpoint is pushed). With one
   source down, the other is accepted only within `NEAP_SINGLE_SOURCE_MAX_MOVE_BPS` of the last on-chain
   price. Otherwise nothing is pushed and the contract's stale ladder takes over. NearPool markets get
   `refresh_price`. Proposed markets are activated once they have an observation; the staircase is cranked.
2. **Orders** (every second): `execute_order` for pending two-step orders, asking for a fresh observation
   first when needed. Retries back off (0, 2, 6, 14, 30 s after first sight) so an order that cannot fill
   costs five transactions at most, not thirty.
3. **Conditionals** (every 3 s, own task): `execute_conditional` when the mark is within 0.3% of the trigger.
4. **Liquidations**: accounts from the indexer with a non-zero position get `liquidate`.
5. **Watchdog**: transactions are sent with `wait_until: INCLUDED`, so a "sent" log line proves nothing. The
   keeper logs `ALERT` when an Active market's observation stops advancing for `NEAP_STALE_ALERT_SECS` or the
   signer balance drops under `NEAP_MIN_BALANCE_NEAR`.

Internal HTTP server (`FAUCET_ADDR`, loopback): `GET /status` (per-market observation progress, signer
balance, refused prices; the indexer's `/health` includes it) and, on testnet only,
`POST /faucet {"account_id"}`, which registers the account on test USDC and mints it.

Holds no funds. The contract re-checks every call. Idempotent — safe to restart at any time.

## Run

```bash
NEAP_CONTRACT=neap-trade.testnet \
NEAP_SIGNER=neap-trade.testnet \
NEAP_SIGNER_KEY=ed25519:... \
NEAP_MARKETS=0 \
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
| `NEAP_LIVE_PRICES` | No | `true` | Pushed markets get the live USD price (Coinbase and Kraken, cross-checked) |
| `NEAP_PRICE_FEEDS` | No | from symbol | Feed symbol overrides, e.g. `0=NEAR,1=SOL` (default: market symbol minus `-PERP`) |
| `NEAP_PUSH_PRICE` | No | `0` | Fixed price_e6, used only with `NEAP_LIVE_PRICES=false`. Testnet only; refused elsewhere |
| `NEAP_MAX_SOURCE_SPREAD_BPS` | No | `100` | Two sources must agree within this or nothing is pushed |
| `NEAP_SINGLE_SOURCE_MAX_MOVE_BPS` | No | `500` | With one source, furthest it may be from the on-chain price |
| `NEAP_STALE_ALERT_SECS` | No | `30` | Log `ALERT` when an Active market's observation is older |
| `NEAP_MIN_BALANCE_NEAR` | No | `1` | Log `ALERT` when the signer holds less |
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
