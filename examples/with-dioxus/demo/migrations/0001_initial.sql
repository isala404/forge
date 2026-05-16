CREATE TYPE user_role AS ENUM ('admin', 'member', 'guest');

CREATE TABLE users (
    id UUID PRIMARY KEY,
    email VARCHAR(255) NOT NULL,
    name VARCHAR(255) NOT NULL,
    role user_role NOT NULL DEFAULT 'member',
    password_hash TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX idx_users_email ON users(email);

CREATE TABLE iss_location (
    id UUID PRIMARY KEY,
    latitude DOUBLE PRECISION NOT NULL,
    longitude DOUBLE PRECISION NOT NULL,
    api_timestamp TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE trades (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    symbol VARCHAR(20) NOT NULL,
    price DOUBLE PRECISION NOT NULL,
    quantity DOUBLE PRECISION NOT NULL,
    trade_time TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    is_buyer_maker BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_trades_created_at ON trades(created_at DESC);

CREATE TABLE webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    idempotency_key VARCHAR(255) NOT NULL,
    webhook_name VARCHAR(100) NOT NULL,
    payload JSONB,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_webhook_events_processed_at ON webhook_events(processed_at DESC);

SELECT forge_enable_reactivity('users');
SELECT forge_enable_reactivity('iss_location');
SELECT forge_enable_reactivity('trades');
SELECT forge_enable_reactivity('webhook_events');

-- Stats snapshot table for cached query demo
CREATE TABLE demo_stats (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    total_users INTEGER NOT NULL DEFAULT 0,
    total_trades INTEGER NOT NULL DEFAULT 0,
    total_webhooks INTEGER NOT NULL DEFAULT 0,
    computed_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Sample user for demo (password: "password123")
INSERT INTO users (id, email, name, role, password_hash, created_at, updated_at)
VALUES (
    'a1b2c3d4-e5f6-4a5b-8c9d-0e1f2a3b4c5d',
    'demo@example.com',
    'Demo User',
    'member',
    '$argon2id$v=19$m=19456,t=2,p=1$AjozmE60AjazLA3S4LXuvw$v+Jo+M5NZ+Q1K4ro1pDS4Hx0/cnHJ3uvmJC7RiNJkUg',
    NOW(),
    NOW()
);
