-- P&L tracking table for profit/loss accounting

CREATE TABLE IF NOT EXISTS pnl_records (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    fill_id UUID NOT NULL REFERENCES fills(id),
    chain_id BIGINT NOT NULL,
    token_address BYTEA NOT NULL,
    amount_in NUMERIC(78, 0) NOT NULL,
    amount_out NUMERIC(78, 0) NOT NULL,
    gas_cost NUMERIC(78, 0) NOT NULL DEFAULT 0,
    net_profit NUMERIC(78, 0) NOT NULL,
    profit_usd NUMERIC(20, 6),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_pnl_fill_id ON pnl_records(fill_id);
CREATE INDEX idx_pnl_chain_id ON pnl_records(chain_id);
CREATE INDEX idx_pnl_recorded_at ON pnl_records(recorded_at);
