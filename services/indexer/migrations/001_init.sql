-- Neap indexer schema. Rebuildable from chain events.

CREATE TABLE IF NOT EXISTS indexer_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS markets (
    id          INTEGER PRIMARY KEY,
    symbol      TEXT NOT NULL,
    tier        TEXT NOT NULL,
    mode        TEXT NOT NULL,
    lister      TEXT NOT NULL,
    bond        BIGINT NOT NULL DEFAULT 0,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS observations (
    id          BIGSERIAL PRIMARY KEY,
    market_id   INTEGER NOT NULL REFERENCES markets(id),
    price_e6    BIGINT NOT NULL,
    ema_e6      BIGINT,
    liquidity   BIGINT,
    seq         BIGINT NOT NULL,
    block       BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_obs_market_block ON observations(market_id, block);
CREATE UNIQUE INDEX IF NOT EXISTS idx_obs_market_seq ON observations(market_id, seq);

CREATE TABLE IF NOT EXISTS candles (
    market_id   INTEGER NOT NULL REFERENCES markets(id),
    interval_s  INTEGER NOT NULL,  -- 60, 300, 900, 3600, 86400
    open_time   TIMESTAMPTZ NOT NULL,
    open        BIGINT NOT NULL,
    high        BIGINT NOT NULL,
    low         BIGINT NOT NULL,
    close       BIGINT NOT NULL,
    volume      BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (market_id, interval_s, open_time)
);

CREATE TABLE IF NOT EXISTS orders (
    id            BIGINT PRIMARY KEY,
    market_id     INTEGER NOT NULL REFERENCES markets(id),
    owner         TEXT NOT NULL,
    side          TEXT NOT NULL,
    size_q        TEXT NOT NULL,
    limit_price   BIGINT,
    status        TEXT NOT NULL DEFAULT 'pending', -- pending, filled, cancelled, expired
    request_block BIGINT,
    fill_price    BIGINT,
    fee           TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_orders_market_status ON orders(market_id, status);

CREATE TABLE IF NOT EXISTS conditionals (
    id            BIGINT PRIMARY KEY,
    market_id     INTEGER NOT NULL REFERENCES markets(id),
    owner         TEXT NOT NULL,
    kind          TEXT NOT NULL,  -- TakeProfit, StopLoss, Limit
    side          TEXT NOT NULL,
    size_q        TEXT NOT NULL,
    trigger_price BIGINT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',
    fill_price    BIGINT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS fills (
    id          BIGSERIAL PRIMARY KEY,
    market_id   INTEGER NOT NULL REFERENCES markets(id),
    owner       TEXT NOT NULL,
    order_id    BIGINT,
    side        TEXT,
    size_q      TEXT NOT NULL,
    price       BIGINT NOT NULL,
    fee         TEXT NOT NULL,
    block       BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_fills_market ON fills(market_id, block);
CREATE INDEX IF NOT EXISTS idx_fills_owner ON fills(owner);

CREATE TABLE IF NOT EXISTS liquidations (
    id          BIGSERIAL PRIMARY KEY,
    market_id   INTEGER NOT NULL REFERENCES markets(id),
    account     TEXT NOT NULL,
    caller      TEXT NOT NULL,
    closed_q    TEXT NOT NULL,
    fee         TEXT NOT NULL,
    block       BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS fee_claims (
    id          BIGSERIAL PRIMARY KEY,
    market_id   INTEGER NOT NULL REFERENCES markets(id),
    payee       TEXT NOT NULL,
    owner       TEXT NOT NULL,
    amount      TEXT NOT NULL,
    block       BIGINT NOT NULL,
    ts          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Receipts already indexed (block feed and POST /tx can overlap).
CREATE TABLE IF NOT EXISTS seen_receipts (
    receipt_id TEXT PRIMARY KEY,
    seen_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Block ranges the fetcher skipped (rate limits, long downtime). Events in
-- them are missing until a backfill reads these rows.
CREATE TABLE IF NOT EXISTS skipped_ranges (
    id         BIGSERIAL PRIMARY KEY,
    from_block BIGINT NOT NULL,
    to_block   BIGINT NOT NULL,
    at         TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Indexes for the queries the API actually runs.
CREATE INDEX IF NOT EXISTS idx_obs_market_ts ON observations(market_id, ts);
CREATE INDEX IF NOT EXISTS idx_fills_market_ts ON fills(market_id, ts);
CREATE INDEX IF NOT EXISTS idx_orders_owner ON orders(owner);
CREATE INDEX IF NOT EXISTS idx_cond_owner ON conditionals(owner);
CREATE INDEX IF NOT EXISTS idx_cond_market_status ON conditionals(market_id, status);
CREATE INDEX IF NOT EXISTS idx_liq_market ON liquidations(market_id, block);
CREATE INDEX IF NOT EXISTS idx_liq_account ON liquidations(account);
