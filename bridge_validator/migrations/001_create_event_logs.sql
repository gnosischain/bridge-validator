-- Create event_logs table
CREATE TABLE IF NOT EXISTS event_logs (
    id SERIAL PRIMARY KEY,
    topic_key TEXT NOT NULL,
    bridge_mode TEXT NOT NULL,
    log_data JSONB NOT NULL,
    block_number BIGINT,
    transaction_hash TEXT,
    is_processed TEXT,
    retry_count INT DEFAULT 0,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(topic_key, transaction_hash)
);

-- Create indexes for better query performance
CREATE INDEX idx_topic_key ON event_logs(topic_key);
CREATE INDEX idx_bridge_mode ON event_logs(bridge_mode);
CREATE INDEX idx_block_number ON event_logs(block_number);
CREATE INDEX idx_transaction_hash ON event_logs(transaction_hash);

