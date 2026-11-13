# neap-indexer

NEAR events → Postgres, plus a REST + WebSocket API for the frontend (plan step 1.11).

## What it does

1. **Fetcher.** Streams finalized blocks from neardata.xyz (one request per block) and keeps every
   successful receipt executed by the contract, including ones that arrive through `ft_transfer_call`
   on the USDC contract. Writes NEP-297 `neap` events to Postgres and pushes them to `/ws`.
2. **Live poller.** Every 3 s reads `get_market` for every market, caches it, upserts the `markets`
   table and broadcasts a `market_state` WS message.
3. **API.** REST + WebSocket. All amounts are strings or integers in atoms: USDC has 6 decimals and
   prices are `price_e6` (USD × 10^6).

All data is rebuildable from chain. The indexer is display-only; the contract is the source of truth.

## Run

```bash
NEAP_CONTRACT=neap-trade.testnet \
DATABASE_URL=postgres://user:pass@host/neap \
API_PORT=3000 \
cargo run -p neap-indexer
```

Deployment (indexer + keeper in one container): `deploy/Dockerfile`, `deploy/start.sh`, `render.yaml`.

| Var | Default | Description |
|-----|---------|-------------|
| `NEAP_CONTRACT` | required | Contract account to index |
| `DATABASE_URL` | required | Postgres connection string |
| `API_PORT` / `PORT` | `3000` | HTTP/WS port |
| `NEAR_RPC_URL` | `https://test.rpc.fastnear.com` | RPC for view calls |
| `NEARDATA_URL` | `https://testnet.neardata.xyz` | Block stream |
| `START_BLOCK` | latest | First block to index on an empty DB |
| `POLL_INTERVAL_MS` | `700` | Head poll interval |
| `LIVE_REFRESH_MS` | `3000` | Live market refresh interval |
| `KEEPER_INTERNAL_URL` | unset | Keeper faucet address; enables `POST /faucet` |
| `KEEPALIVE_URL` / `RENDER_EXTERNAL_URL` | unset | Self-ping every 10 min so free hosts don't sleep |

## API

| Method | Path | Returns |
|--------|------|---------|
| GET | `/health` | `{ok, contract, last_indexed_block, live_markets, faucet}` |
| GET | `/config` | `{network, contract, quote_token, quote_decimals, price_decimals, faucet}` |
| GET | `/markets` | Live `get_market` view per market + `id` + `stats_24h {open, high, low, last, change_bps, trades}` |
| GET | `/markets/:id` | Same, one market |
| GET | `/markets/:id/candles?interval=60&limit=300` | `{market, interval, candles: [{time, open, high, low, close, samples}]}`, oldest first, `time` in unix seconds |
| GET | `/markets/:id/observations` | Raw price observations, newest first |
| GET | `/markets/:id/fills` | Fills, newest first |
| GET | `/markets/:id/orders?status=pending` | Two-step orders (`pending`, `filled`, `cancelled`, `expired`) |
| GET | `/markets/:id/conditionals?status=pending` | TP/SL/limit orders (`pending`, `triggered`, `cancelled`, `expired`) |
| GET | `/markets/:id/liquidations` | Liquidations |
| GET | `/markets/:id/quote?side=Long&size_q=…` | `{exec_price, spread_bps, fee}` from the contract's `quote` view |
| GET | `/account/:owner` | `{owner, wallet, markets: [{market, account: {capital, pnl, reserved_pnl, fee_credits, position_q}, lp_shares}]}` (live) |
| GET | `/account/:owner/fills` · `/orders` · `/conditionals` · `/liquidations` | History for one account |
| GET | `/accounts` | `[{owner, market}]` seen trading (used by the keeper's liquidation scan) |
| GET | `/view/:method?args={json}` | Read-only proxy for contract views (allow-listed) |
| POST | `/faucet` body `{"account_id": "you.testnet"}` | Registers the account on test USDC and mints 1,000 USDC. One per account per hour |
| POST | `/tx` body `{"hash": "<tx hash>", "signer_id": "you.testnet"}` | Report a transaction you just sent to the contract so its events (order, fill, deposit, LP, market) are indexed within seconds. Returns 202. Call it after every wallet transaction; the block feed is only a backup |
| WS | `/ws` | JSON messages, any origin |

Pagination: `?limit=50&offset=0` (max 500).

### WebSocket messages

```jsonc
{"event": "snapshot", "data": [/* every market, same shape as GET /markets without stats */]}   // on connect
{"event": "market_state", "data": {/* one market */}}                                            // every ~3 s
{"event": "price_observed" | "order_requested" | "fill" | "order_cancelled" | "order_expired" |
          "conditional_placed" | "conditional_triggered" | "liquidation" | "market_created" |
          "market_activated" | "market_mode_changed" | "fees_claimed",
 "data": {/* contract event payload */}, "block": 277548408, "ts": "2026-11-13T18:43:34Z", "receipt_id": "…"}
```

## Database tables

`markets`, `observations`, `candles`, `orders`, `conditionals`, `fills`, `liquidations`, `fee_claims`,
`indexer_state`. Schema: `migrations/001_init.sql`.
