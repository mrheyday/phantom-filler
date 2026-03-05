-- Initial database schema for Phantom Filler

-- Orders table: stores discovered Dutch auction orders
CREATE TABLE IF NOT EXISTS orders (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_hash BYTEA NOT NULL UNIQUE,
    chain_id BIGINT NOT NULL,
    reactor_address BYTEA NOT NULL,
    signer_address BYTEA NOT NULL,
    nonce NUMERIC(78, 0) NOT NULL,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    input_token BYTEA NOT NULL,
    input_amount NUMERIC(78, 0) NOT NULL,
    decay_start_time TIMESTAMPTZ NOT NULL,
    decay_end_time TIMESTAMPTZ NOT NULL,
    deadline TIMESTAMPTZ NOT NULL,
    signature BYTEA NOT NULL,
    raw_order BYTEA,
    discovered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT valid_status CHECK (status IN ('pending', 'active', 'filled', 'expired', 'cancelled'))
);

-- Order outputs table: stores output tokens with decay parameters
CREATE TABLE IF NOT EXISTS order_outputs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
    output_index INT NOT NULL,
    token BYTEA NOT NULL,
    start_amount NUMERIC(78, 0) NOT NULL,
    end_amount NUMERIC(78, 0) NOT NULL,
    recipient BYTEA NOT NULL,

    CONSTRAINT unique_order_output UNIQUE (order_id, output_index)
);

-- Fills table: records fill attempts and results
CREATE TABLE IF NOT EXISTS fills (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    order_id UUID NOT NULL REFERENCES orders(id),
    chain_id BIGINT NOT NULL,
    strategy_name VARCHAR(100) NOT NULL,
    tx_hash BYTEA,
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    estimated_profit NUMERIC(78, 0),
    actual_profit NUMERIC(78, 0),
    gas_used NUMERIC(78, 0),
    gas_price NUMERIC(78, 0),
    fill_amount NUMERIC(78, 0),
    error_message TEXT,
    submitted_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    confirmed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT valid_fill_status CHECK (status IN ('pending', 'submitted', 'confirmed', 'reverted', 'dropped'))
);

-- Balances table: tracks token balances across chains
CREATE TABLE IF NOT EXISTS balances (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    chain_id BIGINT NOT NULL,
    wallet_address BYTEA NOT NULL,
    token_address BYTEA NOT NULL,
    balance NUMERIC(78, 0) NOT NULL DEFAULT 0,
    last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT unique_balance UNIQUE (chain_id, wallet_address, token_address)
);

-- Transactions table: raw transaction log for auditing
CREATE TABLE IF NOT EXISTS transactions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    chain_id BIGINT NOT NULL,
    tx_hash BYTEA NOT NULL,
    from_address BYTEA NOT NULL,
    to_address BYTEA,
    value NUMERIC(78, 0) NOT NULL DEFAULT 0,
    gas_used NUMERIC(78, 0),
    gas_price NUMERIC(78, 0),
    status VARCHAR(20) NOT NULL DEFAULT 'pending',
    block_number BIGINT,
    block_timestamp TIMESTAMPTZ,
    tx_type VARCHAR(50) NOT NULL,
    related_order_id UUID REFERENCES orders(id),
    related_fill_id UUID REFERENCES fills(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    CONSTRAINT valid_tx_status CHECK (status IN ('pending', 'confirmed', 'reverted', 'dropped'))
);

-- Indexes for common query patterns

-- Orders: lookup by status, chain, and deadline
CREATE INDEX idx_orders_status ON orders(status);
CREATE INDEX idx_orders_chain_status ON orders(chain_id, status);
CREATE INDEX idx_orders_deadline ON orders(deadline);
CREATE INDEX idx_orders_signer ON orders(signer_address);
CREATE INDEX idx_orders_discovered_at ON orders(discovered_at);

-- Fills: lookup by order, status, and time
CREATE INDEX idx_fills_order_id ON fills(order_id);
CREATE INDEX idx_fills_status ON fills(status);
CREATE INDEX idx_fills_submitted_at ON fills(submitted_at);
CREATE INDEX idx_fills_chain_status ON fills(chain_id, status);

-- Balances: lookup by chain and wallet
CREATE INDEX idx_balances_chain_wallet ON balances(chain_id, wallet_address);
CREATE INDEX idx_balances_last_updated ON balances(last_updated);

-- Transactions: lookup by hash, chain, and related entities
CREATE INDEX idx_transactions_tx_hash ON transactions(tx_hash);
CREATE INDEX idx_transactions_chain_status ON transactions(chain_id, status);
CREATE INDEX idx_transactions_block_number ON transactions(chain_id, block_number);
CREATE INDEX idx_transactions_related_order ON transactions(related_order_id);
CREATE INDEX idx_transactions_related_fill ON transactions(related_fill_id);

-- Updated_at trigger function
CREATE OR REPLACE FUNCTION update_updated_at_column()
RETURNS TRIGGER AS $$
BEGIN
    NEW.updated_at = NOW();
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- Auto-update updated_at on orders
CREATE TRIGGER trigger_orders_updated_at
    BEFORE UPDATE ON orders
    FOR EACH ROW
    EXECUTE FUNCTION update_updated_at_column();
