-- FCR (Fast Confirmation Rule) tracking.
--
-- In block-finality mode every stored row is finalized by construction, so a
-- log can never leave the canonical chain. FCR mode caps indexing at the `safe`
-- block instead (~12s vs ~12.8m), which CAN be reorged out — so rows indexed in
-- that mode carry a lifecycle that the fcr_checker resolves once the block
-- finalizes. Nothing is undone on-chain; a mismatch is recorded as a false
-- positive for alerting.

-- Dedicated column for the execution-layer block hash. It already exists inside
-- log_data JSON, but the revalidation query groups and compares on it, so it
-- needs to be a real (indexable) column.
ALTER TABLE event_logs ADD COLUMN block_hash TEXT;

-- NULL      = row was indexed in block-finality mode (nothing to revalidate)
-- 'pending' = indexed from a `safe` block, awaiting finalization
-- 'confirmed' = finalized canonical hash matched the stored hash
-- 'reverted'  = finalized canonical hash differed -> false positive recorded
ALTER TABLE event_logs ADD COLUMN fcr_status TEXT;

-- The checker's hot query: pending rows at or below the finalized block.
-- Partial index so block-finality deployments pay nothing for it.
CREATE INDEX idx_fcr_pending ON event_logs(block_number) WHERE fcr_status = 'pending';

-- Durable audit trail of safe-block confirmations that did not survive
-- finalization. One row per affected event log, so an alert can be traced back
-- to the exact signed message.
CREATE TABLE IF NOT EXISTS fcr_false_positives (
    id SERIAL PRIMARY KEY,
    chain TEXT NOT NULL,
    block_number BIGINT NOT NULL,
    stored_block_hash TEXT NOT NULL,
    canonical_block_hash TEXT,
    transaction_hash TEXT,
    log_index BIGINT,
    event_log_id INT,
    detected_at_finalized BIGINT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX idx_fcr_false_positives_chain ON fcr_false_positives(chain);
CREATE INDEX idx_fcr_false_positives_block_number ON fcr_false_positives(block_number);
