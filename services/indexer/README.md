# neap-indexer

NEAR events → Postgres + REST/WebSocket API (plan step 1.11).

## What it does

1. **Fetcher**: Polls NEAR RPC for new blocks, extracts NEP-297 neap events, writes to Postgres.
2. **REST API**: Markets, candles/observations, orders, fills, liquidations, fee claims.
3. **WebSocket**: Real-time event stream at `/ws`.

All data is rebuildable from chain — the indexer is display-only.

## Run

```bash
# Start Postgres (e.g. Neon, Supabase, or local)
# The schema is auto-created on first run.

NEAP_CONTRACT=neap-trade.testnet \
DATABASE_URL=postgres://user:pass@host/neap \
API_PORT=3000 \
RUST_LOG=info \
cargo run -p neap-indexer
```

## Environment variables

| Var | Required | Default | Description |
|-----|----------|---------|-------------|
| `NEAP_CONTRACT` | Yes | — | Contract account to index |
| `DATABASE_URL` | Yes | — | Postgres connection string |
| `API_PORT` | No | `3000` | HTTP/WS listen port |
| `NEAR_RPC_URL` | No | `https://rpc.testnet.near.org` | RPC endpoint |
| `START_BLOCK` | No | `0` (latest) | Block height to start from |
| `POLL_INTERVAL_MS` | No | `1000` | Block polling interval |
| `RUST_LOG` | No | `info` | Log level |

## API endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/markets` | List all markets |
| GET | `/markets/:id` | Single market |
| GET | `/markets/:id/candles` | Price observations (candles) |
| GET | `/markets/:id/fills` | Trade fills |
| GET | `/markets/:id/orders` | Orders (pending, filled, cancelled) |
| GET | `/markets/:id/liquidations` | Liquidation events |
| GET | `/account/:owner/fills` | Fills for a specific account |
| WS | `/ws` | Real-time event stream (JSON) |

Query params: `?limit=50&offset=0` for pagination, `?interval=60` for candles.

## Database tables

`markets`, `observations`, `candles`, `orders`, `conditionals`, `fills`, `liquidations`, `fee_claims`, `indexer_state`.

Schema: `migrations/001_init.sql`.
