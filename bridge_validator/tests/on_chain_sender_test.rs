use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use sqlx::Row;
use worker::service::on_chain_sender::OnChainSender;

mod common;

// ============================================================================
//  MESSAGE PARSING TESTS
// ============================================================================

#[test]
fn test_parse_xdai_message_valid() {
    // Test data: valid 124-byte xDai message
    let recipient = Address::from([0x11; 20]);
    let value = U256::from(1000000000000000000u64); // 1 ETH in wei
    let nonce = FixedBytes::<32>::from([0x22; 32]);
    let bridge = Address::from([0x33; 20]);
    let token = Address::from([0x44; 20]);

    // Construct message in expected format
    let mut message_bytes = Vec::with_capacity(124);
    message_bytes.extend_from_slice(recipient.as_slice()); // 20 bytes
    message_bytes.extend_from_slice(&value.to_be_bytes::<32>()); // 32 bytes
    message_bytes.extend_from_slice(nonce.as_slice()); // 32 bytes
    message_bytes.extend_from_slice(bridge.as_slice()); // 20 bytes
    message_bytes.extend_from_slice(token.as_slice()); // 20 bytes

    let message = Bytes::from(message_bytes);

    assert_eq!(message.len(), 124, "Message should be exactly 124 bytes");

    // Now test the actual parsing
    let result = OnChainSender::parse_xdai_message(&message);
    assert!(result.is_ok(), "Valid message should parse successfully");
}

#[test]
fn test_parse_xdai_message_invalid_length_short() {
    // Test message too short (100 bytes instead of 124)
    let message = Bytes::from(vec![0u8; 100]);

    let result = OnChainSender::parse_xdai_message(&message);
    assert!(result.is_err(), "Short message should be rejected");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unexpected length: 100"),
        "Error message should indicate wrong length, got: {}",
        err_msg
    );
}

#[test]
fn test_parse_xdai_message_invalid_length_long() {
    // Test message too long (150 bytes instead of 124)
    let message = Bytes::from(vec![0u8; 150]);

    let result = OnChainSender::parse_xdai_message(&message);
    assert!(result.is_err(), "Long message should be rejected");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("unexpected length: 150"),
        "Error message should indicate wrong length, got: {}",
        err_msg
    );
}

// ============================================================================
//  DATABASE OPERATIONS TESTS
// ============================================================================

#[cfg(test)]
mod database_tests {
    use super::*;
    use common::{cleanup_test_db, create_test_config, setup_test_db};
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_delete_event_log() {
        let (pool, _db_lock) = setup_test_db().await;

        // Insert test event_log
        let result = sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, retry_count, is_processed, block_number, transaction_hash)
            VALUES ('test_key_2', 'amb', '{}', 0, 'true', 100, '0x2222222222222222222222222222222222222222222222222222222222222222')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test event_log");

        let event_log_id: i32 = result.get("id");

        // Verify event_log exists
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_logs WHERE id = $1")
            .bind(event_log_id)
            .fetch_one(&pool)
            .await
            .expect("Failed to count event_logs");
        assert_eq!(count, 1, "Event log should exist");

        let config = create_test_config();
        let (_tx, rx) = mpsc::channel(100);
        let sender = OnChainSender::new(config, pool.clone(), rx);

        // Call delete_event_log
        sender
            .delete_event_log(event_log_id)
            .await
            .expect("Failed to delete event_log");

        // Verify event_log no longer exists
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM event_logs WHERE id = $1")
            .bind(event_log_id)
            .fetch_one(&pool)
            .await
            .expect("Failed to count event_logs");
        assert_eq!(count, 0, "Event log should be deleted");

        // Call delete_event_log again (idempotency test)
        let result = sender.delete_event_log(event_log_id).await;
        assert!(
            result.is_ok(),
            "Deleting non-existent event_log should not error"
        );

        cleanup_test_db(&pool).await;
        pool.close().await;
    }

    #[tokio::test]
    async fn test_increment_retry_count_isolation() {
        let (pool, _db_lock) = setup_test_db().await;

        // Insert two event_logs with different IDs
        let result1 = sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, retry_count, is_processed, block_number, transaction_hash)
            VALUES ('test_key_3', 'amb', '{}', 0, 'true', 100, '0x3333333333333333333333333333333333333333333333333333333333333333')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert first event_log");

        let event_log_id1: i32 = result1.get("id");

        let result2 = sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, retry_count, is_processed, block_number, transaction_hash)
            VALUES ('test_key_4', 'amb', '{}', 0, 'true', 101, '0x4444444444444444444444444444444444444444444444444444444444444444')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert second event_log");

        let event_log_id2: i32 = result2.get("id");

        // Create OnChainSender instance
        let config = create_test_config();
        let (_tx, rx) = mpsc::channel(100);
        let sender = OnChainSender::new(config, pool.clone(), rx);

        // Call increment_retry_count on first event_log
        sender
            .increment_retry_count(event_log_id1)
            .await
            .expect("Failed to increment retry count");

        // Verify only first event_log's retry_count incremented
        let row1 = sqlx::query("SELECT retry_count FROM event_logs WHERE id = $1")
            .bind(event_log_id1)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch first event_log");

        let retry_count1: i32 = row1.get("retry_count");
        assert_eq!(retry_count1, 1, "First event_log retry_count should be 1");

        // Verify second event_log remains unchanged
        let row2 = sqlx::query("SELECT retry_count FROM event_logs WHERE id = $1")
            .bind(event_log_id2)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch second event_log");

        let retry_count2: i32 = row2.get("retry_count");
        assert_eq!(
            retry_count2, 0,
            "Second event_log retry_count should remain 0"
        );

        cleanup_test_db(&pool).await;
        pool.close().await;
    }

    #[tokio::test]
    async fn test_database_operations_with_invalid_id() {
        let (pool, _db_lock) = setup_test_db().await;

        // Create OnChainSender instance
        let config = create_test_config();
        let (_tx, rx) = mpsc::channel(100);
        let sender = OnChainSender::new(config, pool.clone(), rx);

        // Call increment_retry_count with non-existent ID
        let result = sender.increment_retry_count(99999).await;
        assert!(
            result.is_ok(),
            "increment_retry_count with non-existent ID should not error"
        );

        // Call delete_event_log with non-existent ID
        let result = sender.delete_event_log(99999).await;
        assert!(
            result.is_ok(),
            "delete_event_log with non-existent ID should not error"
        );

        cleanup_test_db(&pool).await;
        pool.close().await;
    }
}

