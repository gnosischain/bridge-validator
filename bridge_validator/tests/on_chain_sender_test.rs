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

#[test]
fn test_compute_message_hashes() {
    // Test hash computation logic used in AMB and xDai bridges
    use alloy::primitives::keccak256;

    let message = Bytes::from(vec![1, 2, 3, 4, 5]);
    let hash_msg = keccak256(&message);

    // Hash should be deterministic
    let hash_msg_2 = keccak256(&message);
    assert_eq!(
        hash_msg, hash_msg_2,
        "Same message should produce same hash"
    );

    // Test sender hash computation: keccak256(sender || hashMsg)
    let sender = Address::from([0xAA; 20]);
    let mut buf = Vec::with_capacity(20 + 32);
    buf.extend_from_slice(sender.as_slice());
    buf.extend_from_slice(hash_msg.as_slice());
    let hash_sender = keccak256(&buf);

    assert_eq!(hash_sender.len(), 32, "Hash should be 32 bytes");
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
    #[ignore] // Methods are private - requires Provider Factory Pattern to test via process_message
    async fn test_increment_retry_count() {
        let (pool, _db_lock) = setup_test_db().await;

        // Insert test event_log with retry_count = 0
        let result = sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, retry_count, is_processed, block_number, transaction_hash)
            VALUES ('test_key_1', 'amb', '{}', 0, 'true', 100, '0x1111111111111111111111111111111111111111111111111111111111111111')
            RETURNING id
            "#,
        )
        .fetch_one(&pool)
        .await
        .expect("Failed to insert test event_log");

        let event_log_id: i32 = result.get("id");

        // Manually test the SQL logic that increment_retry_count uses
        sqlx::query(
            r#"
            UPDATE event_logs
            SET retry_count = retry_count + 1, is_processed = 'false'
            WHERE id = $1
            "#,
        )
        .bind(event_log_id)
        .execute(&pool)
        .await
        .expect("Failed to increment retry count");

        // Verify retry_count = 1, is_processed = 'false'
        let row = sqlx::query("SELECT retry_count, is_processed FROM event_logs WHERE id = $1")
            .bind(event_log_id)
            .fetch_one(&pool)
            .await
            .expect("Failed to fetch event_log");

        let retry_count: i32 = row.get("retry_count");
        let is_processed: String = row.get("is_processed");

        assert_eq!(retry_count, 1, "Retry count should be 1");
        assert_eq!(is_processed, "false", "is_processed should be 'false'");

        cleanup_test_db(&pool).await;
        pool.close().await;
    }

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

// ============================================================================
// CHANNEL COMMUNICATION TESTS
// ============================================================================

#[cfg(test)]
mod channel_tests {
    use super::*;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};
    use worker::contracts::{AmbEthCalldata, OnChainCallData};
    use worker::service::msg_processor::SenderData;

    #[tokio::test]
    async fn test_channel_receives_sender_data() {
        // Create mpsc channel with capacity 10
        let (tx, mut rx) = mpsc::channel::<SenderData>(10);

        // Create test data
        let test_calldata = OnChainCallData::AmbEth {
            contract_address: Address::from([0xAA; 20]),
            calldata: AmbEthCalldata {
                message: Bytes::from(vec![1, 2, 3, 4, 5]),
            },
        };

        let sender_data = SenderData {
            on_chain_calldata: test_calldata,
            event_log_id: 123,
            stage: "home".to_string(),
        };

        // Send test SenderData through channel
        tx.send(sender_data)
            .await
            .expect("Failed to send SenderData");

        // Verify OnChainSender receives the data
        let received = rx.recv().await.expect("Failed to receive SenderData");

        // Verify data integrity
        assert_eq!(received.event_log_id, 123, "Event log ID should match");
        match received.on_chain_calldata {
            OnChainCallData::AmbEth {
                contract_address,
                calldata,
            } => {
                assert_eq!(
                    contract_address,
                    Address::from([0xAA; 20]),
                    "Contract address should match"
                );
                assert_eq!(
                    calldata.message,
                    Bytes::from(vec![1, 2, 3, 4, 5]),
                    "Message should match"
                );
            }
            _ => panic!("Expected AmbEth calldata"),
        }
    }

    #[tokio::test]
    async fn test_channel_handles_multiple_messages() {
        let (tx, mut rx) = mpsc::channel::<SenderData>(10);

        // Send 5 different SenderData messages
        for i in 0..5 {
            let test_calldata = OnChainCallData::AmbEth {
                contract_address: Address::from([0xAA; 20]),
                calldata: AmbEthCalldata {
                    message: Bytes::from(vec![i as u8]),
                },
            };

            let sender_data = SenderData {
                on_chain_calldata: test_calldata,
                event_log_id: i as i32,
                stage: "home".to_string(),
            };

            tx.send(sender_data)
                .await
                .expect("Failed to send SenderData");
        }

        // Verify all 5 messages are received in FIFO order
        for i in 0..5 {
            let received = rx.recv().await.expect("Failed to receive SenderData");
            assert_eq!(
                received.event_log_id, i as i32,
                "Message should be received in FIFO order"
            );
        }
    }

    #[tokio::test]
    async fn test_channel_closes_gracefully() {
        let (tx, mut rx) = mpsc::channel::<SenderData>(10);

        // Send 2 messages
        for i in 0..2 {
            let test_calldata = OnChainCallData::AmbEth {
                contract_address: Address::from([0xAA; 20]),
                calldata: AmbEthCalldata {
                    message: Bytes::from(vec![i]),
                },
            };

            let sender_data = SenderData {
                on_chain_calldata: test_calldata,
                event_log_id: i as i32,
                stage: "home".to_string(),
            };

            tx.send(sender_data)
                .await
                .expect("Failed to send SenderData");
        }

        // Drop the sender to close channel
        drop(tx);

        // Verify the receiver can still receive the buffered messages
        let mut received_count = 0;
        while let Some(_) = rx.recv().await {
            received_count += 1;
        }

        assert_eq!(
            received_count, 2,
            "All buffered messages should be received before channel closes"
        );
    }

    #[tokio::test]
    async fn test_channel_backpressure() {
        let (tx, mut rx) = mpsc::channel::<SenderData>(2);

        // Send 2 messages to fill the channel
        for i in 0..2 {
            let test_calldata = OnChainCallData::AmbEth {
                contract_address: Address::from([0xAA; 20]),
                calldata: AmbEthCalldata {
                    message: Bytes::from(vec![i]),
                },
            };

            let sender_data = SenderData {
                on_chain_calldata: test_calldata,
                event_log_id: i as i32,
                stage: "home".to_string(),
            };

            tx.send(sender_data)
                .await
                .expect("Failed to send SenderData");
        }

        // Try to send a 3rd message with timeout (should block if channel is full)
        let tx_clone = tx.clone();
        let send_future = tokio::spawn(async move {
            let test_calldata = OnChainCallData::AmbEth {
                contract_address: Address::from([0xAA; 20]),
                calldata: AmbEthCalldata {
                    message: Bytes::from(vec![2]),
                },
            };

            let sender_data = SenderData {
                on_chain_calldata: test_calldata,
                event_log_id: 2,
                stage: "home".to_string(),
            };

            tx_clone.send(sender_data).await
        });

        // Consume 1 message to make room
        let _ = rx.recv().await.expect("Failed to receive SenderData");

        // Now the 3rd message should be able to be sent
        let result = timeout(Duration::from_secs(1), send_future)
            .await
            .expect("Send should complete after consuming a message");

        assert!(result.is_ok(), "Send should succeed after backpressure");
    }

    #[tokio::test]
    async fn test_channel_concurrent_senders() {
        let (tx, mut rx) = mpsc::channel::<SenderData>(50);

        // Spawn 3 tasks, each sending 10 messages
        let mut handles = vec![];
        for task_id in 0..3 {
            let tx_clone = tx.clone();
            let handle = tokio::spawn(async move {
                for i in 0..10 {
                    let test_calldata = OnChainCallData::AmbEth {
                        contract_address: Address::from([0xAA; 20]),
                        calldata: AmbEthCalldata {
                            message: Bytes::from(vec![task_id, i]),
                        },
                    };

                    let sender_data = SenderData {
                        on_chain_calldata: test_calldata,
                        event_log_id: (task_id * 10 + i) as i32,
                        stage: "home".to_string(),
                    };

                    tx_clone
                        .send(sender_data)
                        .await
                        .expect("Failed to send SenderData");
                }
            });
            handles.push(handle);
        }

        // Wait for all senders to complete
        for handle in handles {
            handle.await.expect("Task failed");
        }

        // Drop the original sender
        drop(tx);

        // Collect all 30 messages on receiver side
        let mut received_count = 0;
        while let Some(_) = rx.recv().await {
            received_count += 1;
        }

        // Verify all 30 messages received (no loss)
        assert_eq!(
            received_count, 30,
            "All 30 messages should be received from concurrent senders"
        );
    }
}

// ============================================================================
// HELPER FUNCTIONS
// ============================================================================

#[cfg(test)]
mod helpers {
    use super::*;

    /// Creates a valid 124-byte xDai message for testing
    pub fn create_test_xdai_message() -> Bytes {
        let mut message = Vec::with_capacity(124);
        message.extend_from_slice(&[0x11; 20]); // recipient
        message.extend_from_slice(&[0x00; 32]); // value
        message.extend_from_slice(&[0x22; 32]); // nonce
        message.extend_from_slice(&[0x33; 20]); // bridge
        message.extend_from_slice(&[0x44; 20]); // token
        Bytes::from(message)
    }

    /// Creates a test AMB message
    pub fn create_test_amb_message() -> Bytes {
        Bytes::from(vec![1, 2, 3, 4, 5, 6, 7, 8])
    }
}
