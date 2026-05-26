// Integration tests for EventIndexer
// These tests use testcontainers to spin up a temporary PostgreSQL database
// For provider tests, use mock providers from common::create_mock_provider()
// The database container is automatically cleaned up when tests complete
// You can also explicitly call common::shutdown_test_db() to clean up manually
// TODO: database is not shut down auto, should use tokio OneCell for setting up db at once, and testcontainer Ryuk for dropping container at the end
mod common;

use alloy::primitives::address;
use alloy::transports::mock::Asserter;
use common::{create_mock_provider, create_test_log_with_address_and_topic, setup_test_db};
use tokio::sync::watch;
use worker::config::Config;
use worker::service::event_indexer::EventIndexer;

// Helper function to create test config
// Note: RPC URLs are placeholders - use create_mock_provider() for actual provider tests
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

// Unit tests for check_bridge_mode function
// These don't require a provider or database

#[test]
fn test_check_bridge_mode_amb_eth() {
    let config = create_test_config();

    // Test AMB_ETH
    let bridge_mode = EventIndexer::<()>::check_bridge_mode(config.eth_amb_bridge_address, &config);
    assert_eq!(bridge_mode, "AMB_ETH");
}

#[test]
fn test_check_bridge_mode_amb_gc() {
    let config = create_test_config();

    // Test AMB_GC
    let bridge_mode = EventIndexer::<()>::check_bridge_mode(config.gc_amb_bridge_address, &config);
    assert_eq!(bridge_mode, "AMB_GC");
}

#[test]
fn test_check_bridge_mode_xdai_eth() {
    let config = create_test_config();

    // Test XDAI_ETH
    let bridge_mode =
        EventIndexer::<()>::check_bridge_mode(config.eth_xdai_bridge_address, &config);
    assert_eq!(bridge_mode, "XDAI_ETH");
}

#[test]
fn test_check_bridge_mode_xdai_gc() {
    let config = create_test_config();

    // Test XDAI_GC
    let bridge_mode = EventIndexer::<()>::check_bridge_mode(config.gc_xdai_bridge_address, &config);
    assert_eq!(bridge_mode, "XDAI_GC");
}

#[test]
fn test_check_bridge_mode_unknown() {
    let config = create_test_config();

    // Test unknown address
    let unknown_address = address!("0x0000000000000000000000000000000000000000");
    let bridge_mode = EventIndexer::<()>::check_bridge_mode(unknown_address, &config);
    assert_eq!(bridge_mode, "UNKNOWN");
}

// ============================================================================
// Tests with Mock Provider for Event Detection
// ============================================================================

/// Test Case 1: Multiple events from different bridge addresses within the same block
/// This test verifies that when multiple bridge events occur in the same block,
/// each event is correctly detected and stored with the appropriate bridge mode
#[tokio::test]
async fn test_multiple_events_same_block_different_bridges() {
    let (pool, _db_lock) = setup_test_db().await;
    let config = create_test_config();

    // Block number where all events occur (use unique block to avoid conflicts)
    let block_number = 99912345u64;

    // Define different topics for each bridge mode to simulate different events
    let amb_eth_topic = [0x11u8; 32]; // AMB ETH event topic
    let amb_gc_topic = [0x22u8; 32]; // AMB GC event topic
    let xdai_eth_topic = [0x33u8; 32]; // XDAI ETH event topic

    // Create logs for different bridges at the same block with unique tx hashes (64 hex chars)
    let amb_eth_log = create_test_log_with_address_and_topic(
        block_number,
        "0xa111111111111111111111111111111111111111111111111111111111111111",
        config.eth_amb_bridge_address,
        amb_eth_topic,
        0,
    );

    let amb_gc_log = create_test_log_with_address_and_topic(
        block_number,
        "0xa222222222222222222222222222222222222222222222222222222222222222",
        config.gc_amb_bridge_address,
        amb_gc_topic,
        1,
    );

    let xdai_eth_log = create_test_log_with_address_and_topic(
        block_number,
        "0xa333333333333333333333333333333333333333333333333333333333333333",
        config.eth_xdai_bridge_address,
        xdai_eth_topic,
        2,
    );

    // Test AMB_ETH indexer
    {
        let (provider, asserter): (_, Asserter) = create_mock_provider();

        // Mock provider responses
        asserter.push_success(&block_number); // get_block_number()
        asserter.push_success(&vec![amb_eth_log.clone()]); // get_logs()

        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let indexer = EventIndexer::new(
            config.clone(),
            provider,
            "eth".to_string(),
            "UserRequestForSignature".to_string(),
            config.eth_amb_bridge_address,
            pool.clone(),
            shutdown_rx,
        );

        let result = indexer.poll_events(0).await;
        assert!(result.is_ok(), "AMB_ETH polling should succeed");
    }

    // Test AMB_GC indexer
    {
        let (provider, asserter): (_, Asserter) = create_mock_provider();

        asserter.push_success(&block_number);
        asserter.push_success(&vec![amb_gc_log.clone()]);

        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let indexer = EventIndexer::new(
            config.clone(),
            provider,
            "gc".to_string(),
            "UserRequestForAffirmation".to_string(),
            config.gc_amb_bridge_address,
            pool.clone(),
            shutdown_rx,
        );

        let result = indexer.poll_events(0).await;
        assert!(result.is_ok(), "AMB_GC polling should succeed");
    }

    // Test XDAI_ETH indexer
    {
        let (provider, asserter): (_, Asserter) = create_mock_provider();

        asserter.push_success(&block_number);
        asserter.push_success(&vec![xdai_eth_log.clone()]);

        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let indexer = EventIndexer::new(
            config.clone(),
            provider,
            "eth".to_string(),
            "UserRequestForSignature".to_string(),
            config.eth_xdai_bridge_address,
            pool.clone(),
            shutdown_rx,
        );

        let result = indexer.poll_events(0).await;
        assert!(result.is_ok(), "XDAI_ETH polling should succeed");
    }

    // Verify all events are stored in the database with correct bridge modes
    // Filter by this test's block number to avoid interference from parallel tests
    let all_events: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT bridge_mode, topic_key, block_number FROM event_logs WHERE block_number = $1 ORDER BY bridge_mode"
    )
    .bind(block_number as i64)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(all_events.len(), 3, "Should have 3 events stored");

    // Verify AMB_ETH event
    assert_eq!(all_events[0].0, "AMB_ETH");
    assert_eq!(all_events[0].2, block_number as i64);
    assert!(all_events[0].1.contains("0x11"));

    // Verify AMB_GC event
    assert_eq!(all_events[1].0, "AMB_GC");
    assert_eq!(all_events[1].2, block_number as i64);
    assert!(all_events[1].1.contains("0x22"));

    // Verify XDAI_ETH event
    assert_eq!(all_events[2].0, "XDAI_ETH");
    assert_eq!(all_events[2].2, block_number as i64);
    assert!(all_events[2].1.contains("0x33"));

    // Verify log_data JSON contains correct addresses
    let log_data_check: Vec<(serde_json::Value,)> = sqlx::query_as(
        "SELECT log_data FROM event_logs WHERE bridge_mode = 'AMB_ETH' AND block_number = $1",
    )
    .bind(block_number as i64)
    .fetch_all(&pool)
    .await
    .unwrap();

    let log_json = &log_data_check[0].0;

    // The address might be nested in different ways, let's check the actual structure
    let stored_address = if let Some(addr) = log_json["inner"]["address"].as_str() {
        addr.to_string()
    } else if let Some(addr) = log_json["address"].as_str() {
        addr.to_string()
    } else {
        // If it's an object, convert it to string representation
        log_json
            .get("inner")
            .and_then(|inner| inner.get("address"))
            .unwrap_or(&log_json["address"])
            .to_string()
            .trim_matches('"')
            .to_string()
    };

    let expected_address = format!("{:?}", config.eth_amb_bridge_address).to_lowercase();
    assert!(
        stored_address
            .to_lowercase()
            .contains(&expected_address[2..]), // Skip "0x" prefix
        "Stored address {} should contain expected address {}",
        stored_address,
        expected_address
    );

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE block_number = $1")
        .bind(block_number as i64)
        .execute(&pool)
        .await
        .unwrap();
}

/// Test Case 2: Multiple events of the SAME type within the SAME transaction.
///
/// Several logs of the same event (same topics[0]) can be emitted in a single
/// transaction, distinguished only by log_index. Each must be stored as its
/// own row.
#[tokio::test]
async fn test_multiple_events_same_tx_same_topic() {
    let (pool, _db_lock) = setup_test_db().await;
    let config = create_test_config();

    let block_number = 99987654u64;
    // Same event signature (topic_key) for every log...
    let topic = [0x44u8; 32];
    // ...and the SAME transaction hash. Only log_index distinguishes them.
    let tx_hash = "0xb444444444444444444444444444444444444444444444444444444444444444";

    // Three logs of the same event, in the same tx, at log_index 0, 1, 2.
    let logs: Vec<_> = (0..3u64)
        .map(|log_index| {
            create_test_log_with_address_and_topic(
                block_number,
                tx_hash,
                config.eth_amb_bridge_address,
                topic,
                log_index,
            )
        })
        .collect();

    let (provider, asserter): (_, Asserter) = create_mock_provider();
    asserter.push_success(&block_number); // get_block_number()
    asserter.push_success(&logs); // get_logs() returns all three at once

    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let indexer = EventIndexer::new(
        config.clone(),
        provider,
        "eth".to_string(),
        "UserRequestForSignature".to_string(),
        config.eth_amb_bridge_address,
        pool.clone(),
        shutdown_rx,
    );

    let result = indexer.poll_events(0).await;
    assert!(result.is_ok(), "polling should succeed");

    // All three logs must be persisted — one row per log_index, not collapsed.
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT log_index, transaction_hash, topic_key FROM event_logs \
         WHERE block_number = $1 ORDER BY log_index ASC",
    )
    .bind(block_number as i64)
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(
        rows.len(),
        3,
        "all 3 same-event logs in one tx should be stored, not collapsed to 1"
    );

    // log_index values are distinct and preserved.
    assert_eq!(rows[0].0, 0);
    assert_eq!(rows[1].0, 1);
    assert_eq!(rows[2].0, 2);

    // They genuinely share the same transaction hash and event signature,
    // proving (transaction_hash, log_index) — not topic_key — is what keeps
    // them apart.
    let expected_tx = tx_hash.to_lowercase();
    for row in &rows {
        assert_eq!(row.1.to_lowercase(), expected_tx, "tx hash should match");
        assert!(row.2.contains("0x44"), "topic_key should match");
    }

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE block_number = $1")
        .bind(block_number as i64)
        .execute(&pool)
        .await
        .unwrap();
}

