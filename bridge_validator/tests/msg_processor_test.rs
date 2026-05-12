// Integration and unit tests for MessageProcessor
// Setup testing environment:
// 1. Setup mock config, refer to event_indexer_test.rs
// 2. Mock get_finalized_block to return a fixed block number
// 3. Test check_block_finality and create_xdai_message

mod common;

use alloy::hex;
use alloy::primitives::{address, Bytes, FixedBytes, U256};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;
use common::setup_test_db;
use tokio::sync::{mpsc, watch};
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};
use worker::config::Config;
use worker::contracts::{AMB_BRIDGE, XDAI_BRIDGE};
use worker::service::msg_processor::{EventLogRow, MessageProcessor, SenderData};

// Helper function to create test config
// Note: RPC URLs are placeholders for testing
fn create_test_config() -> Config {
    Config {
        eth_rpc: vec!["https://eth-rpc".to_string()],
        gc_rpc: vec!["https://gc-rpc".to_string()],
        eth_bc_rpc: vec!["https://eth-bc-rpc".to_string()],
        gc_bc_rpc: vec!["https://gc-bc-rpc".to_string()],
        xdai_validator_private_key: None,
        amb_validator_private_key: None,
        eth_amb_bridge_address: address!("0x4C36d2919e407f0Cc2Ee3c993ccF8ac26d9CE64e"),
        gc_amb_bridge_address: address!("0x75Df5AF045d91108662D8080fD1FEFAd6aA0bb59"),
        eth_xdai_bridge_address: address!("0x4aa42145Aa6Ebf72e164C9bBC74fbD3788045016"),
        gc_xdai_bridge_address: address!("0x7301CFA0e1756B71869E93d4e4Dca5c7d0eb0AA6"),
        xdai_execute_message_on_foreign: "false".to_string(),
        amb_execute_message_on_foreign: "false".to_string(),
        xdai_bridge_helper_address: address!("0xe30269bc61E677cD60aD163a221e464B7022fbf5"),
        amb_bridge_helper_address: address!("0x7d94ece17e81355326e3359115D4B02411825EdD"),
        poll_interval_secs: 10,
        max_retry_count: 5,
    }
}

// Helper function to create test config with private keys for signing tests
fn create_test_config_with_keys() -> Config {
    Config {
        eth_rpc: vec!["https://eth-rpc".to_string()],
        gc_rpc: vec!["https://gc-rpc".to_string()],
        eth_bc_rpc: vec!["https://eth-bc-rpc".to_string()],
        gc_bc_rpc: vec!["https://gc-bc-rpc".to_string()],
        // Test private keys (DO NOT use in production!)
        xdai_validator_private_key: Some(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_string(),
        ),
        amb_validator_private_key: Some(
            "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80".to_string(),
        ),
        eth_amb_bridge_address: address!("0x4C36d2919e407f0Cc2Ee3c993ccF8ac26d9CE64e"),
        gc_amb_bridge_address: address!("0x75Df5AF045d91108662D8080fD1FEFAd6aA0bb59"),
        eth_xdai_bridge_address: address!("0x4aa42145Aa6Ebf72e164C9bBC74fbD3788045016"),
        gc_xdai_bridge_address: address!("0x7301CFA0e1756B71869E93d4e4Dca5c7d0eb0AA6"),
        xdai_execute_message_on_foreign: "false".to_string(),
        amb_execute_message_on_foreign: "false".to_string(),
        xdai_bridge_helper_address: address!("0xe30269bc61E677cD60aD163a221e464B7022fbf5"),
        amb_bridge_helper_address: address!("0x7d94ece17e81355326e3359115D4B02411825EdD"),
        poll_interval_secs: 10,
        max_retry_count: 5,
    }
}

// Helper to create a test log
fn create_test_log(block_number: u64, tx_hash: &str) -> Log {
    use alloy::primitives::FixedBytes;

    // Parse the tx_hash string to FixedBytes<32>
    let tx_hash_bytes = tx_hash.trim_start_matches("0x");
    let mut bytes = [0u8; 32];
    hex::decode_to_slice(tx_hash_bytes, &mut bytes).expect("Invalid tx_hash hex");

    Log {
        inner: alloy::primitives::Log {
            address: address!("0x4aa42145Aa6Ebf72e164C9bBC74fbD3788045016"),
            data: alloy::primitives::LogData::new_unchecked(
                vec![],
                alloy::primitives::Bytes::from(vec![1, 2, 3, 4]),
            ),
        },
        block_hash: Some(alloy::primitives::b256!(
            "0x1234567890123456789012345678901234567890123456789012345678901234"
        )),
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from(bytes)),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    }
}

// Unit tests for create_xdai_message

#[tokio::test]
async fn test_create_xdai_message_basic() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config.clone(), pool, tx, shutdown_rx.clone());

    // Test data
    let recipient = address!("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0");
    let value = U256::from(1000000000000000000u64); // 1 ETH in wei
    let nonce = FixedBytes::<32>::from([1u8; 32]);
    let token_address = address!("0x0000000000000000000000000000000000000000");

    let message = processor
        .create_xdai_message(recipient, value, nonce, token_address)
        .unwrap();

    // Verify message format
    assert!(message.starts_with("0x"), "Message should start with 0x");
    assert_eq!(
        message.len(),
        250,
        "Message should be 250 characters (0x + 248 hex chars)"
    );

    // Verify recipient is in the message (first 20 bytes after 0x)
    let recipient_in_msg = &message[2..42];
    assert_eq!(recipient_in_msg, "742d35cc6634c0532925a3b844bc9e7595f0beb0");

    // Verify bridge address is in the message (position 170..210)
    let bridge_in_msg = &message[170..210];
    assert_eq!(bridge_in_msg, "4aa42145aa6ebf72e164c9bbc74fbd3788045016");

    // Verify token address is at the end (last 20 bytes)
    let token_in_msg = &message[210..250];
    assert_eq!(token_in_msg, "0000000000000000000000000000000000000000");
}

#[tokio::test]
async fn test_create_xdai_message_zero_value() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool, tx, shutdown_rx.clone());

    let recipient = address!("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0");
    let value = U256::ZERO;
    let nonce = FixedBytes::<32>::from([0u8; 32]);
    let token_address = address!("0x0000000000000000000000000000000000000000");

    let message = processor
        .create_xdai_message(recipient, value, nonce, token_address)
        .unwrap();

    // Verify message is valid
    assert_eq!(message.len(), 250);
    assert!(message.starts_with("0x"));

    // Value should be all zeros (64 hex chars)
    let value_in_msg = &message[42..106];
    assert_eq!(
        value_in_msg,
        "0000000000000000000000000000000000000000000000000000000000000000"
    );
}

#[tokio::test]
async fn test_create_xdai_message_max_value() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool, tx, shutdown_rx.clone());

    let recipient = address!("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0");
    let value = U256::MAX;
    let nonce = FixedBytes::<32>::from([255u8; 32]);
    let token_address = address!("0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF");

    let message = processor
        .create_xdai_message(recipient, value, nonce, token_address)
        .unwrap();

    // Verify message is valid
    assert_eq!(message.len(), 250);
    assert!(message.starts_with("0x"));

    // Value should be all f's (64 hex chars)
    let value_in_msg = &message[42..106];
    assert_eq!(
        value_in_msg,
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
    );

    // Nonce should be all f's (64 hex chars)
    let nonce_in_msg = &message[106..170];
    assert_eq!(
        nonce_in_msg,
        "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff"
    );

    // Token address should be all f's (40 hex chars)
    let token_in_msg = &message[210..250];
    assert_eq!(token_in_msg, "ffffffffffffffffffffffffffffffffffffffff");
}

#[tokio::test]
async fn test_create_xdai_message_message_structure() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config.clone(), pool, tx, shutdown_rx.clone());

    let recipient = address!("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let value = U256::from(123456789u64);
    let nonce = FixedBytes::<32>::from([0x42u8; 32]);
    let token_address = address!("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    let message = processor
        .create_xdai_message(recipient, value, nonce, token_address)
        .unwrap();

    // Parse the message and verify structure
    // Format: 0x + recipient(20B) + value(32B) + nonce(32B) + bridge(20B) + token(20B)
    // Total: 2 + 40 + 64 + 64 + 40 + 40 = 250 chars

    // Extract components
    let recipient_hex = &message[2..42];
    let value_hex = &message[42..106];
    let nonce_hex = &message[106..170];
    let bridge_hex = &message[170..210];
    let token_hex = &message[210..250];

    // Verify lengths
    assert_eq!(recipient_hex.len(), 40); // 20 bytes
    assert_eq!(value_hex.len(), 64); // 32 bytes
    assert_eq!(nonce_hex.len(), 64); // 32 bytes
    assert_eq!(bridge_hex.len(), 40); // 20 bytes
    assert_eq!(token_hex.len(), 40); // 20 bytes

    // Verify specific values
    assert_eq!(recipient_hex, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    assert_eq!(
        nonce_hex,
        "4242424242424242424242424242424242424242424242424242424242424242"
    );
    assert_eq!(token_hex, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");

    // Verify bridge address matches config
    assert_eq!(bridge_hex, "4aa42145aa6ebf72e164c9bbc74fbd3788045016");
}

#[tokio::test]
async fn test_create_xdai_message_value_encoding() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool, tx, shutdown_rx.clone());

    let recipient = address!("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0");
    let token_address = address!("0x0000000000000000000000000000000000000000");
    let nonce = FixedBytes::<32>::from([0u8; 32]);

    // Test various values to ensure proper encoding
    let test_cases = vec![
        (
            U256::from(0u64),
            "0000000000000000000000000000000000000000000000000000000000000000",
        ),
        (
            U256::from(1u64),
            "0000000000000000000000000000000000000000000000000000000000000001",
        ),
        (
            U256::from(255u64),
            "00000000000000000000000000000000000000000000000000000000000000ff",
        ),
        (
            U256::from(256u64),
            "0000000000000000000000000000000000000000000000000000000000000100",
        ),
        (
            U256::from(1000000000000000000u64),
            "0000000000000000000000000000000000000000000000000de0b6b3a7640000",
        ), // 1 ETH
    ];

    for (value, expected_hex) in test_cases {
        let message = processor
            .create_xdai_message(recipient, value, nonce, token_address)
            .unwrap();
        let value_in_msg = &message[42..106];
        assert_eq!(
            value_in_msg, expected_hex,
            "Value encoding mismatch for {:?}",
            value
        );
    }
}

// Database integration tests for read_from_db

#[tokio::test]
async fn test_read_from_db_no_entries() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool.clone(), tx, shutdown_rx.clone());

    // Clean the database to ensure no entries
    sqlx::query("DELETE FROM event_logs")
        .execute(&pool)
        .await
        .unwrap();

    // Read from empty database
    let result = processor.read_from_db().await.unwrap();

    // Should return None when no entries exist
    assert!(result.is_none());
}

#[tokio::test]
async fn test_read_from_db_single_entry() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool.clone(), tx, shutdown_rx.clone());

    // Insert a test event log entry with unique tx hash
    let test_log = create_test_log(
        100,
        "0xa234567890123456789012345678901234567890123456789012345678901234",
    );
    let log_json = serde_json::to_value(&test_log).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind("UserRequestForSignature")
    .bind("XDAI_GC")
    .bind(&log_json)
    .bind(100i64)
    .bind("0xa234567890123456789012345678901234567890123456789012345678901234")
    .bind("false")
    .bind(0i32)
    .execute(&pool)
    .await
    .unwrap();

    // Read from database
    let result = processor.read_from_db().await.unwrap();

    // Should return the entry
    assert!(result.is_some());
    let event_log = result.unwrap();
    assert_eq!(event_log.topic_key, "UserRequestForSignature");
    assert_eq!(event_log.bridge_mode, "XDAI_GC");
    assert_eq!(event_log.block_number, Some(100));
    assert_eq!(event_log.is_processed, Some("false".to_string())); // Returned object has old value
    assert_eq!(event_log.retry_count, Some(0));
}

#[tokio::test]
async fn test_read_from_db_multiple_entries_orders_by_block_number() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool.clone(), tx, shutdown_rx.clone());

    // Insert multiple test event log entries with different block numbers
    let test_log1 = create_test_log(
        300,
        "0xb333333333333333333333333333333333333333333333333333333333333333",
    );
    let test_log2 = create_test_log(
        100,
        "0xb111111111111111111111111111111111111111111111111111111111111111",
    );
    let test_log3 = create_test_log(
        200,
        "0xb222222222222222222222222222222222222222222222222222222222222222",
    );

    let log_json1 = serde_json::to_value(&test_log1).unwrap();
    let log_json2 = serde_json::to_value(&test_log2).unwrap();
    let log_json3 = serde_json::to_value(&test_log3).unwrap();

    // Insert in non-sequential order
    for (log_json, block_num, tx_hash) in [
        (
            &log_json1,
            300i64,
            "0xb333333333333333333333333333333333333333333333333333333333333333",
        ),
        (
            &log_json2,
            100i64,
            "0xb111111111111111111111111111111111111111111111111111111111111111",
        ),
        (
            &log_json3,
            200i64,
            "0xb222222222222222222222222222222222222222222222222222222222222222",
        ),
    ] {
        sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            "#,
        )
        .bind("UserRequestForSignature")
        .bind("AMB_GC")
        .bind(log_json)
        .bind(block_num)
        .bind(tx_hash)
        .bind("false")
        .bind(0i32)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Read from database - should return entry with lowest block number first
    let result = processor.read_from_db().await.unwrap();

    assert!(result.is_some());
    let event_log = result.unwrap();
    assert_eq!(event_log.block_number, Some(100)); // Lowest block number
    assert_eq!(event_log.is_processed, Some("false".to_string())); // Returned object has old value
}

#[tokio::test]
async fn test_read_from_db_skips_high_retry_count() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool.clone(), tx, shutdown_rx.clone());

    // Insert entry with retry_count >= 5 (should be skipped)
    let test_log1 = create_test_log(
        100,
        "0xc111111111111111111111111111111111111111111111111111111111111111",
    );
    let log_json1 = serde_json::to_value(&test_log1).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind("UserRequestForSignature")
    .bind("XDAI_GC")
    .bind(&log_json1)
    .bind(100i64)
    .bind("0xc111111111111111111111111111111111111111111111111111111111111111")
    .bind("false")
    .bind(5i32) // retry_count = 5, should be skipped
    .execute(&pool)
    .await
    .unwrap();

    // Insert entry with retry_count < 5 (should be returned)
    let test_log2 = create_test_log(
        200,
        "0xc222222222222222222222222222222222222222222222222222222222222222",
    );
    let log_json2 = serde_json::to_value(&test_log2).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind("UserRequestForSignature")
    .bind("XDAI_GC")
    .bind(&log_json2)
    .bind(200i64)
    .bind("0xc222222222222222222222222222222222222222222222222222222222222222")
    .bind("false")
    .bind(2i32) // retry_count = 2
    .execute(&pool)
    .await
    .unwrap();

    // Read from database - should skip the first entry and return the second
    let result = processor.read_from_db().await.unwrap();

    assert!(result.is_some());
    let event_log = result.unwrap();
    assert_eq!(event_log.block_number, Some(200)); // Second entry
    assert_eq!(event_log.retry_count, Some(2));
}

#[tokio::test]
async fn test_read_from_db_only_reads_unprocessed() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool.clone(), tx, shutdown_rx.clone());

    // Insert a processed entry (should be skipped)
    let test_log1 = create_test_log(
        100,
        "0xd111111111111111111111111111111111111111111111111111111111111111",
    );
    let log_json1 = serde_json::to_value(&test_log1).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind("UserRequestForSignature")
    .bind("XDAI_GC")
    .bind(&log_json1)
    .bind(100i64)
    .bind("0xd111111111111111111111111111111111111111111111111111111111111111")
    .bind("true") // Already processed
    .bind(0i32)
    .execute(&pool)
    .await
    .unwrap();

    // Insert an unprocessed entry
    let test_log2 = create_test_log(
        200,
        "0xd222222222222222222222222222222222222222222222222222222222222222",
    );
    let log_json2 = serde_json::to_value(&test_log2).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind("UserRequestForSignature")
    .bind("XDAI_GC")
    .bind(&log_json2)
    .bind(200i64)
    .bind("0xd222222222222222222222222222222222222222222222222222222222222222")
    .bind("false")
    .bind(0i32)
    .execute(&pool)
    .await
    .unwrap();

    // Read from database - should skip the first entry and return the second
    let result = processor.read_from_db().await.unwrap();

    assert!(result.is_some());
    let event_log = result.unwrap();
    assert_eq!(event_log.block_number, Some(200));
}

#[tokio::test]
async fn test_read_from_db_marks_as_processed() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config, pool.clone(), tx, shutdown_rx.clone());

    // Clean the database to ensure no entries
    sqlx::query("DELETE FROM event_logs")
        .execute(&pool)
        .await
        .unwrap();

    // Insert a test entry
    let test_log = create_test_log(
        100,
        "0xe234567890123456789012345678901234567890123456789012345678901234",
    );
    let log_json = serde_json::to_value(&test_log).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind("UserRequestForSignature")
    .bind("XDAI_GC")
    .bind(&log_json)
    .bind(100i64)
    .bind("0xe234567890123456789012345678901234567890123456789012345678901234")
    .bind("false")
    .bind(0i32)
    .execute(&pool)
    .await
    .unwrap();

    // Read from database
    let result = processor.read_from_db().await.unwrap();
    assert!(result.is_some());
    let event_log = result.unwrap();
    let entry_id = event_log.id;

    // Note: The returned object still has the old is_processed value ('false')
    // because it was fetched before the UPDATE. The database row is updated after.
    assert_eq!(event_log.is_processed, Some("false".to_string()));

    // Verify the entry is marked as processed in the database
    let db_row: (String,) = sqlx::query_as(
        r#"
        SELECT is_processed
        FROM event_logs
        WHERE id = $1
        "#,
    )
    .bind(entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(db_row.0, "true");

    // Reading again should return None (no unprocessed entries)
    let result2 = processor.read_from_db().await.unwrap();
    assert!(result2.is_none());
}

// Test for block finality check with wiremock

#[tokio::test]
async fn test_check_block_finality_block_is_finalized() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Start wiremock server
    let mock_server = MockServer::start().await;

    // Mock beacon chain response with finalized block at 200
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 200
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    let processor = MessageProcessor::new(config, pool, tx, shutdown_rx.clone());

    // Test with block 100 (should be finalized since finalized is 200)
    let is_finalized = processor
        .check_block_finality(100, Some(&mock_server.uri()), &[])
        .await
        .unwrap();

    assert!(is_finalized, "Block 100 should be finalized");
}

#[tokio::test]
async fn test_check_block_finality_block_is_not_finalized() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, _rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Start wiremock server
    let mock_server = MockServer::start().await;

    // Mock beacon chain response with finalized block at 200
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 200
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    let processor = MessageProcessor::new(config, pool, tx, shutdown_rx.clone());

    // Test with block 300 (should NOT be finalized since finalized is 200)
    let is_finalized = processor
        .check_block_finality(300, Some(&mock_server.uri()), &[])
        .await
        .unwrap();

    assert!(!is_finalized, "Block 300 should NOT be finalized");
}

// Tests for process_message_or_skip with different bridge modes

#[tokio::test]
async fn test_process_message_amb_eth_finalized_sends_data() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, mut rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Setup wiremock for finalized block
    let mock_server = MockServer::start().await;
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 200
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    // Update config to use mock server
    let mut config_with_mock = config.clone();
    config_with_mock.eth_bc_rpc = vec![mock_server.uri()];

    let processor = MessageProcessor::new(
        config_with_mock.clone(),
        pool.clone(),
        tx,
        shutdown_rx.clone(),
    );

    // Create AMB_ETH event log
    let message_id = FixedBytes::<32>::from([1u8; 32]);
    let encoded_data = Bytes::from(vec![1, 2, 3, 4, 5]);
    let event = AMB_BRIDGE::UserRequestForAffirmation {
        messageId: message_id,
        encodedData: encoded_data.clone(),
    };

    let log = Log {
        inner: alloy::primitives::Log {
            address: config_with_mock.eth_amb_bridge_address,
            data: event.encode_log_data(),
        },
        block_hash: Some(FixedBytes::from([0u8; 32])),
        block_number: Some(100),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from([1u8; 32])),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };

    let event_log = EventLogRow {
        id: 1,
        topic_key: "UserRequestForAffirmation".to_string(),
        bridge_mode: "AMB_ETH".to_string(),
        log_data: serde_json::to_value(&log).unwrap(),
        block_number: Some(100),
        transaction_hash: Some("0x1234".to_string()),
        is_processed: Some("false".to_string()),
        retry_count: Some(0),
        stage: None,
    };

    // Process the message
    processor.process_message_or_skip(&event_log).await.unwrap();

    // Verify data was sent to channel
    let received = rx.try_recv().unwrap();
    assert_eq!(received.event_log_id, 1);

    match received.on_chain_calldata {
        worker::contracts::OnChainCallData::AmbEth {
            contract_address,
            calldata,
        } => {
            assert_eq!(contract_address, config_with_mock.gc_amb_bridge_address);
            assert_eq!(calldata.message, encoded_data);
        }
        _ => panic!("Expected AmbEth calldata"),
    }
}

#[tokio::test]
async fn test_process_message_amb_eth_not_finalized_writes_false() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, mut rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Setup wiremock for NOT finalized block
    let mock_server = MockServer::start().await;
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 50
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    let mut config_with_mock = config.clone();
    config_with_mock.eth_bc_rpc = vec![mock_server.uri()];

    let processor = MessageProcessor::new(
        config_with_mock.clone(),
        pool.clone(),
        tx,
        shutdown_rx.clone(),
    );

    // Insert test entry
    let message_id = FixedBytes::<32>::from([1u8; 32]);
    let encoded_data = Bytes::from(vec![1, 2, 3, 4, 5]);
    let event = AMB_BRIDGE::UserRequestForAffirmation {
        messageId: message_id,
        encodedData: encoded_data.clone(),
    };

    let log = Log {
        inner: alloy::primitives::Log {
            address: config_with_mock.eth_amb_bridge_address,
            data: event.encode_log_data(),
        },
        block_hash: Some(FixedBytes::from([0u8; 32])),
        block_number: Some(100),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from([2u8; 32])),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };

    let log_json = serde_json::to_value(&log).unwrap();

    sqlx::query(
        r#"
        INSERT INTO event_logs (id, topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(123i32)
    .bind("UserRequestForAffirmation")
    .bind("AMB_ETH")
    .bind(&log_json)
    .bind(100i64)
    .bind("0xg234567890123456789012345678901234567890123456789012345678901234")
    .bind("true")
    .bind(0i32)
    .execute(&pool)
    .await
    .unwrap();

    let event_log = EventLogRow {
        id: 123,
        topic_key: "UserRequestForAffirmation".to_string(),
        bridge_mode: "AMB_ETH".to_string(),
        log_data: log_json,
        block_number: Some(100),
        transaction_hash: Some(
            "0xg234567890123456789012345678901234567890123456789012345678901234".to_string(),
        ),
        is_processed: Some("true".to_string()),
        retry_count: Some(0),
        stage: None,
    };

    // Process the message (should skip and write false)
    processor.process_message_or_skip(&event_log).await.unwrap();

    // Verify no data was sent to channel
    assert!(
        rx.try_recv().is_err(),
        "No data should be sent when not finalized"
    );

    // Verify is_processed is set to false
    let db_row: (String,) = sqlx::query_as(
        r#"
        SELECT is_processed
        FROM event_logs
        WHERE id = $1
        "#,
    )
    .bind(123i32)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(db_row.0, "false", "is_processed should be set to false");
}

#[tokio::test]
async fn test_process_message_xdai_eth_finalized_sends_data() {
    let config = create_test_config();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, mut rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Setup wiremock for finalized block
    let mock_server = MockServer::start().await;
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 200
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    let mut config_with_mock = config.clone();
    config_with_mock.eth_bc_rpc = vec![mock_server.uri()];

    let processor = MessageProcessor::new(
        config_with_mock.clone(),
        pool.clone(),
        tx,
        shutdown_rx.clone(),
    );

    // Create XDAI_ETH event log
    let recipient = address!("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0");
    let value = U256::from(1000000000000000000u64);
    let nonce = FixedBytes::<32>::from([3u8; 32]);

    let event = XDAI_BRIDGE::UserRequestForAffirmation {
        recipient: recipient.clone(),
        value: value.clone(),
        nonce: nonce.clone(),
    };

    let log = Log {
        inner: alloy::primitives::Log {
            address: config_with_mock.eth_xdai_bridge_address,
            data: event.encode_log_data(),
        },
        block_hash: Some(FixedBytes::from([0u8; 32])),
        block_number: Some(100),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from([3u8; 32])),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };

    let event_log = EventLogRow {
        id: 2,
        topic_key: "UserRequestForAffirmation".to_string(),
        bridge_mode: "XDAI_ETH".to_string(),
        log_data: serde_json::to_value(&log).unwrap(),
        block_number: Some(100),
        transaction_hash: Some("0x2234".to_string()),
        is_processed: Some("false".to_string()),
        retry_count: Some(0),
        stage: None,
    };

    // Process the message
    processor.process_message_or_skip(&event_log).await.unwrap();

    // Verify data was sent to channel
    let received = rx.try_recv().unwrap();
    assert_eq!(received.event_log_id, 2);

    match received.on_chain_calldata {
        worker::contracts::OnChainCallData::XdaiEth {
            contract_address,
            calldata,
        } => {
            assert_eq!(contract_address, config_with_mock.gc_xdai_bridge_address);
            assert_eq!(calldata.recipient, recipient);
            assert_eq!(calldata.value, value);
            assert_eq!(calldata.nonce, nonce);
        }
        _ => panic!("Expected XdaiEth calldata"),
    }
}

#[tokio::test]
async fn test_process_message_amb_gc_finalized_sends_signed_data() {
    let config = create_test_config_with_keys();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, mut rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Setup wiremock for finalized block
    let mock_server = MockServer::start().await;
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 200
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    let mut config_with_mock = config.clone();
    config_with_mock.gc_bc_rpc = vec![mock_server.uri()];

    let processor = MessageProcessor::new(
        config_with_mock.clone(),
        pool.clone(),
        tx,
        shutdown_rx.clone(),
    );

    // Create AMB_GC event log (requires signature)
    let message_id = FixedBytes::<32>::from([4u8; 32]);
    let encoded_data = Bytes::from(vec![1, 2, 3, 4, 5, 6]);
    let event = AMB_BRIDGE::UserRequestForSignature {
        messageId: message_id,
        encodedData: encoded_data.clone(),
    };

    let log = Log {
        inner: alloy::primitives::Log {
            address: config_with_mock.gc_amb_bridge_address,
            data: event.encode_log_data(),
        },
        block_hash: Some(FixedBytes::from([0u8; 32])),
        block_number: Some(100),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from([4u8; 32])),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };

    let event_log = EventLogRow {
        id: 3,
        topic_key: "UserRequestForSignature".to_string(),
        bridge_mode: "AMB_GC".to_string(),
        log_data: serde_json::to_value(&log).unwrap(),
        block_number: Some(100),
        transaction_hash: Some("0x3234".to_string()),
        is_processed: Some("false".to_string()),
        retry_count: Some(0),
        stage: None,
    };

    // Process the message
    processor.process_message_or_skip(&event_log).await.unwrap();

    // Verify data was sent to channel
    let received = rx.try_recv().unwrap();
    assert_eq!(received.event_log_id, 3);

    match received.on_chain_calldata {
        worker::contracts::OnChainCallData::AmbGc {
            contract_address,
            calldata,
        } => {
            assert_eq!(contract_address, config_with_mock.gc_amb_bridge_address);
            assert_eq!(calldata.message, encoded_data);
            // Verify signature is not empty
            assert!(
                !calldata.signature.is_empty(),
                "Signature should not be empty"
            );
            assert_eq!(calldata.signature.len(), 65, "Signature should be 65 bytes");
        }
        _ => panic!("Expected AmbGc calldata"),
    }
}

#[tokio::test]
async fn test_process_message_xdai_gc_finalized_sends_signed_data() {
    let config = create_test_config_with_keys();
    let (pool, _db_lock) = setup_test_db().await;
    let (tx, mut rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);

    // Setup wiremock for finalized block
    let mock_server = MockServer::start().await;
    let beacon_response = serde_json::json!({
        "data": {
            "message": {
                "body": {
                    "execution_payload": {
                        "block_number": 200
                    }
                }
            }
        }
    });

    Mock::given(method("GET"))
        .and(path("/eth/v2/beacon/blocks/finalized"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&beacon_response))
        .mount(&mock_server)
        .await;

    let mut config_with_mock = config.clone();
    config_with_mock.gc_bc_rpc = vec![mock_server.uri()];

    let processor = MessageProcessor::new(
        config_with_mock.clone(),
        pool.clone(),
        tx,
        shutdown_rx.clone(),
    );

    // Create XDAI_GC event log (requires signature)
    let recipient = address!("0x742d35Cc6634C0532925a3b844Bc9e7595f0bEb0");
    let value = U256::from(2000000000000000000u64);
    let nonce = FixedBytes::<32>::from([5u8; 32]);
    let token = address!("0x0000000000000000000000000000000000000000");

    let event = XDAI_BRIDGE::UserRequestForSignature {
        recipient: recipient.clone(),
        value: value.clone(),
        nonce: nonce.clone(),
        token: token.clone(),
    };

    let log = Log {
        inner: alloy::primitives::Log {
            address: config_with_mock.gc_xdai_bridge_address,
            data: event.encode_log_data(),
        },
        block_hash: Some(FixedBytes::from([0u8; 32])),
        block_number: Some(100),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from([5u8; 32])),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    };

    let event_log = EventLogRow {
        id: 4,
        topic_key: "UserRequestForSignature".to_string(),
        bridge_mode: "XDAI_GC".to_string(),
        log_data: serde_json::to_value(&log).unwrap(),
        block_number: Some(100),
        transaction_hash: Some("0x4234".to_string()),
        is_processed: Some("false".to_string()),
        retry_count: Some(0),
        stage: None,
    };

    // Process the message
    processor.process_message_or_skip(&event_log).await.unwrap();

    // Verify data was sent to channel
    let received = rx.try_recv().unwrap();
    assert_eq!(received.event_log_id, 4);

    match received.on_chain_calldata {
        worker::contracts::OnChainCallData::XdaiGc {
            contract_address,
            calldata,
        } => {
            assert_eq!(contract_address, config_with_mock.gc_xdai_bridge_address);
            // Verify message is the correct format (124 bytes)
            assert_eq!(calldata.message.len(), 124, "Message should be 124 bytes");
            // Verify signature is not empty
            assert!(
                !calldata.signature.is_empty(),
                "Signature should not be empty"
            );
            assert_eq!(calldata.signature.len(), 65, "Signature should be 65 bytes");
        }
        _ => panic!("Expected XdaiGc calldata"),
    }
}
