# Off-chain review, 2026-11-18

Scope: everything in this repo that is not the contract: keeper and price pusher, liquidation scan, faucet,
indexer and API, database, deploy config, CI, and the pieces the architecture calls for that do not exist yet.
Three reviewers read the code in parallel (indexer and API; keeper and prices; deploy and gaps); every finding
below was then checked against the code before it was fixed or written down. Contract, frontend and product
reviews are in their own threads.

Short version: the keeper and indexer were built to make a hackathon demo work, and they did. The gap to
production is mostly three things: the price feed trusted one HTTP answer, the indexer could lose events
for good, and nothing told anyone when either stopped. This PR fixes what could be fixed in code without
changing the design. The rest needs decisions from jason and is in section 3.

## 1. Fixed in this PR

### Keeper and prices

| # | Problem | Fix |
|---|---------|-----|
| K1 | The pushed price was Coinbase alone; Kraken was only used if Coinbase errored. One wrong or cached answer became the price everyone trades and is liquidated on. | Both sources are read together. Push only if they agree within 1% (midpoint). With one source down, the other is accepted only within 5% of the last on-chain price. Otherwise nothing is pushed and the contract's stale ladder takes over. Both limits are env vars. |
| K2 | `NEAP_PUSH_PRICE` was pushed to every Pushed market whenever the live feeds failed. If set, a rate limit from the exchanges would have walked every market to one fixed price. | The fixed price is dev mode only (`NEAP_LIVE_PRICES=false`), never a fallback, and both settings are refused when the contract is not on testnet. |
| K3 | The periodic access-key refresh overwrote the local nonce with the finalized one, which is behind transactions still in flight. That reuses nonces and rejects the next sends in a cascade. | The nonce only ever moves forward. |
| K4 | The transaction hash was read from the RPC response, which need not carry it at `INCLUDED`; the fallback was the string "unknown", which was then reported to the indexer. | The hash is computed locally. |
| K5 | A pending order that cannot fill (limit not met, margin short) got an `execute_order` every second until it expired: about thirty transactions per order, paid by the keeper. Anyone could drain its gas. | Retries back off: 0, 2, 6, 14, 30 s. Five sends at most. |
| K6 | Conditionals shared a loop with two-step orders. Their id scan grows with the oldest resting conditional (they live up to 30 days) and delayed orders that expire in 30 s. | Conditionals run in their own task. A zero mark (unreadable view) no longer counts as a price. |
| K7 | Nothing noticed a dead price feed. With `INCLUDED` a reverted push still logs "pushed price". | Watchdog: `ALERT` log when an Active market's observation stops advancing for 30 s or the signer balance is under 1 NEAR. `GET /status` on the internal server, surfaced through the indexer's `/health`. |
| K8 | Faucet: the rate limit was checked before the mint and recorded after, so parallel requests all passed; maps grew forever; RPC error text (which can hold provider URLs) went back to the caller; it would have run on mainnet with the oracle key. | Slot reserved before sending and released on failure; maps pruned; generic errors; testnet only. |
| K9 | `Config` derived `Debug` with the signing key inside: one `{:?}` away from a log leak. | Manual `Debug` that redacts it, with a test. |
| K10 | A failed indexer read during the liquidation scan was logged at debug level, so "no liquidations are happening" was invisible. | Logged as an error. |

### Indexer and API

| # | Problem | Fix |
|---|---------|-----|
| I1 | A receipt was marked seen before its events were written, outside any transaction. A failed write, or a failed block lookup in `POST /tx` (HTTP 429 is routine here), lost the fill for good because the receipt would never be retried. | The seen-marker and all of a receipt's events commit in one transaction; the block is read first; the fetcher retries. |
| I2 | A fill, cancel or expiry indexed before its `order_requested` (the two delivery paths are independent) left the order `pending` forever. | Terminal events upsert the order; a later request fills in details without changing the status. |
| I3 | The contract emits no `fill` for a triggered TP/SL/limit, so those trades were missing from history and 24h counts. | `conditional_triggered` also writes a fill. |
| I4 | `START_BLOCK` never worked: the "too far behind" check jumped to head straight after. Skipped ranges left no trace. | `START_BLOCK` is honoured. Every skipped range is recorded in `skipped_ranges` (count shown in `/health`). |
| I5 | `/account/:owner`, `/view/*` and `/quote` each call RPCs, unauthenticated, uncached, unlimited. The API shares its RPC quota with the keeper, so a few polling browsers or one script could stall the price feed. `POST /tx` with junk hashes held all 16 lookup slots for minutes. | Per-client limits (120/min chain reads, 30/min writes, 600/min other), 25 s request deadline, 2 s view cache, one lookup per hash, three tries for unknown hashes, account ids validated. |
| I6 | `/health` returned `ok: true` whatever happened. | `status` is `ok` or `degraded` with reasons: database, age of market state, keeper status, stuck prices. It stays HTTP 200 on purpose (see the code comment); point an uptime monitor at `status`. |
| I7 | One unreadable market froze the cache for all markets, with no timestamp to show it. An unparsable count emptied the list. | Per-market fallback to the last state, `fetched_at_ms` on every market, count must be a number. |
| I8 | Every candle request aggregated the market's whole history; 24h stats ran on every `/markets` call with no index on `ts`. | Candles read only the window asked for; stats cached 5 s; indexes for the time, owner and status queries. |
| I9 | WebSocket: no connection cap, 64 MB inbound messages accepted, a client that stopped reading pinned its task, lagging clients silently missed messages. | 500 connection cap, 4 KB inbound, 10 s send deadline, `{"event":"resync"}` on lag. |
| I10 | Changing `NEAP_CONTRACT` (or a typo in it) truncated every table. | Needs `RESET_ON_CONTRACT_CHANGE=1`. |
| I11 | Transport errors (which can contain RPC URLs and keys embedded in them) were returned to callers. | Generic text out, detail in the log. Contract-level view errors still pass through. |

### Deploy, CI, hygiene

| # | Problem | Fix |
|---|---------|-----|
| D1 | No CI at all. | `.github/workflows/ci.yml`: build and test the services (with Postgres for the database scenarios), contract and engine tests, the Docker image build, and a check that fails on a committed NEAR private key. |
| D2 | The container ran as root while holding a signing key. | Non-root user. |
| D3 | `CONTRACT.md` held a full-access private key and seed phrase for `neap-trade.testnet`. | Removed from the file. **It is still in git history: see P1.** |

Tests added: price decision table, retry schedule, trigger rule, key redaction, rate limiter, client
address, start cursor, and database scenarios for I1, I2, I3 and the candle query. Keeper 11 tests, indexer 10.

## 2. Checked and not a bug

- "The indexer flips the sign of fills" (reported by the trader UX review). The indexer stores `size_q` as the
  contract emits it: negative means the taker sold. The demo account's history has a 100 TIDE short and a 10
  NEAR long; its on-chain positions are +100 TIDE and +110 NEAR, so later buys (+200 TIDE, +100 NEAR) were
  never indexed. That is missing events (I1, O1), not a sign error. The frontend should not derive entry price
  or PnL from fills until O1 is done.
- Events from another contract cannot be injected through `POST /tx`: the executor of each receipt is checked.
- A panic in a background task does not leave a half-dead process: the release profile is `panic = "abort"`,
  so the process exits and is restarted.

## 3. Open: needs a decision from jason

These change the trust model, the architecture or the providers, so they are proposals, not code.

**P1. The leaked key (do first).** The private key and seed phrase for `neap-trade.testnet` are in git history
from the first deployment. Anyone who can read the repo can act as owner and oracle of that contract.
Proposed: delete that key on chain today (the command is in `CONTRACT.md`), and treat the account as
compromised; do not reuse it for mainnet naming. Rewriting git history is optional once the key is dead and
breaks existing clones, so I would skip it.

**P2. Free Postgres is deleted on 2026-12-13.** The indexer exits when it cannot connect, and it is the
container's main process, so the keeper stops with it: prices stop for every market. History cannot be
rebuilt (O1). Proposed: move to a paid or non-expiring database before that date, and split the keeper into
its own Render worker so the price feed does not depend on the API process.

**P3. Oracle trust for Pushed markets.** After this PR it is still one hot key pushing a price read from two
exchange HTTP APIs, with the keeper's own clock as the timestamp. That is fine for a testnet demo and not for
real money. Options: (a) restrict mainnet to NearPool markets until the TEE oracle exists; (b) a signed feed
such as Pyth for majors; (c) several independent signers with an on-chain median. I recommend (a) for launch
and (b) for majors after, because both remove the hot key from the price path.

**P4. One key does everything.** The oracle account also pays for order execution, liquidations and the
public faucet. Draining its gas through any of those stops prices. Proposed: separate accounts for oracle,
keeper and faucet, with a function-call-only key for the oracle.

**P5. Liquidations need a contract view.** The scan sends `liquidate` for every open position every 15 s and
relies on the contract to refuse; with `INCLUDED` the refusal is never seen. At 100 positions that is over half
a million failed transactions a day and the scan cannot finish inside its own period. Accounts owned from
Solana (`sol:`) are skipped entirely, and the account list comes from the indexer, which loses events. The
keeper cannot check health itself without copying engine math. Proposed (contract thread): a view that lists
accounts with positions and one that returns margin health, then the keeper sends only for accounts under
maintenance, worst first.

**P6. Permissionless markets and the keeper.** With `NEAP_MARKETS=auto` the keeper serves every market on the
contract: `refresh_price` and `crank` each tick, and `activate_market` each tick for every Proposed market,
forever. Anyone who lists junk markets spends the keeper's gas. Proposed: an allow-list of market ids for the
team keeper, with listers running or paying for their own.

**P7. Where events come from.** Fills are indexed either from a free block feed that rate-limits Render's
shared address, or because a browser called `POST /tx`. Proposed: a FastNEAR key (already supported, was
requested on 2026-11-13) as the minimum; then the backfill in O1.

**P8. Hosting.** "Free tiers only" (decision D18) and "live oracle" do not fit together: a free Render
instance sleeps, restarts, and shares an address that public RPCs throttle. `autoDeploy` also ships every
push on the tracked branch to the process holding the oracle key. Proposed: a paid instance and RPC plan for
anything with real funds, and deploy from tagged releases.

**P9. Not built yet (phase 2).** TEE oracle agent, relayer for Solana-signed actions, 1Click deposit-and-open
and close-and-withdraw with a status table and refund handling, the Solana side of the indexer, auto-convert
profit on close (needs `convert_pnl_for` in the contract), a shared types crate, metrics and alert delivery.
`services/oracle-agent`, `services/relayer` and `services/types` are empty. The open questions that block them:
who funds relayer gas and what the abuse policy is; who bears a stuck or refunded 1Click swap; the TEE
code-hash upgrade policy.

## 4. Open: code work, no decision needed

- **O1. Backfill.** Read `skipped_ranges` and replay those blocks at a low rate; reconcile `pending` orders
  against `get_order`/`get_conditional`. Until then history has holes after any outage.
- **O2. Retention.** `observations` grows by about 29,000 rows per market per day and candles are computed from
  it on request. Materialize the `candles` table (it exists and is unused), then prune raw observations.
- **O3. Observation time base.** Rows written from events use block time; rows from the 3 s poll use the
  oracle's `source_ts`. Use block time for both.
- **O4. Alert delivery.** The `ALERT` log lines and `/health` status need somewhere to go: an external uptime
  check on `/health` and a webhook.
- **O5. Keeper leader lock.** During a redeploy two keepers briefly share a key. K3 makes this self-healing;
  a Postgres advisory lock would make it clean.
- **O6. Push on change.** Prices are pushed every tick even when unchanged (about 0.4 NEAR per hour per market
  on testnet).
- **O7. Proper migrations.** The schema file is re-run on every boot; it can add tables and indexes but never
  alter a column. Move to `sqlx::migrate!`.
- **O8. Separate server profile.** The services are built with the contract's size-optimized profile.
- **O9. CI extras.** `cargo fmt` and `clippy -D warnings` (the tree does not pass either today), `cargo deny`,
  the wasm build and sandbox tests.
- **O10. `TRUSTED_PROXY_HOPS` on Render.** The per-client limits use the leftmost `X-Forwarded-For` entry until
  this is set, and a client can fake that. The right value needs one look at a real request's header on Render.
- **O11. Contract events.** No events exist for deposits, withdrawals or LP actions, so the indexer cannot show
  them (contract thread).
