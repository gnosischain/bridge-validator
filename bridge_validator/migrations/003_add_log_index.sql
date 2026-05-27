-- Add log_index to uniquely identify each log within a transaction.
--
-- The previous UNIQUE(topic_key, transaction_hash) collapsed multiple logs of
-- the same event type emitted in a single transaction: topic_key is only the
-- event signature (topics[0]), so two transfers of the same kind in one tx
-- shared a key and the second was dropped by ON CONFLICT ... DO NOTHING.
--
-- log_index is unique within a block (per the Ethereum JSON-RPC spec), so
-- (transaction_hash, log_index) uniquely identifies a single log.
ALTER TABLE event_logs ADD COLUMN log_index BIGINT;

ALTER TABLE event_logs DROP CONSTRAINT IF EXISTS event_logs_topic_key_transaction_hash_key;

ALTER TABLE event_logs ADD CONSTRAINT event_logs_transaction_hash_log_index_key
    UNIQUE (transaction_hash, log_index);

CREATE INDEX idx_log_index ON event_logs(log_index);
