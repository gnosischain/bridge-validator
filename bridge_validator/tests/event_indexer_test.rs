// Integration tests for EventIndexer
// These tests use testcontainers to spin up a temporary PostgreSQL database
// For provider tests, use mock providers from common::create_mock_provider()
// The database container is automatically cleaned up when tests complete
// You can also explicitly call common::shutdown_test_db() to clean up manually
// TODO: database is not shut down auto, should use tokio OneCell for setting up db at once, and testcontainer Ryuk for dropping container at the end
mod common;

use alloy::primitives::{address, Address};
use alloy::rpc::types::Log;
use common::{
    cleanup_test_db, create_mock_provider, create_test_log_with_address_and_topic, setup_test_db,
    shutdown_test_db,
};
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

#[test]
fn test_check_bridge_mode_all_addresses_are_unique() {
    let config = create_test_config();

    // Ensure all bridge addresses are different
    assert_ne!(config.eth_amb_bridge_address, config.gc_amb_bridge_address);
    assert_ne!(
        config.eth_amb_bridge_address,
        config.eth_xdai_bridge_address
    );
    assert_ne!(config.eth_amb_bridge_address, config.gc_xdai_bridge_address);
    assert_ne!(config.gc_amb_bridge_address, config.eth_xdai_bridge_address);
    assert_ne!(config.gc_amb_bridge_address, config.gc_xdai_bridge_address);
    assert_ne!(
        config.eth_xdai_bridge_address,
        config.gc_xdai_bridge_address
    );

    // Verify each address maps to the correct bridge mode
    assert_eq!(
        EventIndexer::<()>::check_bridge_mode(config.eth_amb_bridge_address, &config),
        "AMB_ETH"
    );
    assert_eq!(
        EventIndexer::<()>::check_bridge_mode(config.gc_amb_bridge_address, &config),
        "AMB_GC"
    );
    assert_eq!(
        EventIndexer::<()>::check_bridge_mode(config.eth_xdai_bridge_address, &config),
        "XDAI_ETH"
    );
    assert_eq!(
        EventIndexer::<()>::check_bridge_mode(config.gc_xdai_bridge_address, &config),
        "XDAI_GC"
    );
}

// Database integration tests
// These require a running PostgreSQL database (via testcontainers)

#[tokio::test]
async fn test_event_indexer_database_setup() {
    let pool = setup_test_db().await;

    // Verify the event_logs table exists and has the expected schema
    let result: (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*)
        FROM information_schema.tables
        WHERE table_name = 'event_logs'
        "#,
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(result.0, 1, "event_logs table should exist");

    // Verify table has expected columns
    let columns: Vec<(String,)> = sqlx::query_as(
        r#"
        SELECT column_name
        FROM information_schema.columns
        WHERE table_name = 'event_logs'
        ORDER BY ordinal_position
        "#,
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let column_names: Vec<String> = columns.iter().map(|(name,)| name.clone()).collect();

    // Check for essential columns
    assert!(column_names.contains(&"id".to_string()));
    assert!(column_names.contains(&"topic_key".to_string()));
    assert!(column_names.contains(&"bridge_mode".to_string()));
    assert!(column_names.contains(&"log_data".to_string()));
    assert!(column_names.contains(&"block_number".to_string()));
    assert!(column_names.contains(&"transaction_hash".to_string()));
    assert!(column_names.contains(&"is_processed".to_string()));
    assert!(column_names.contains(&"retry_count".to_string()));

    // No cleanup needed - this test doesn't insert any data
}

#[tokio::test]
async fn test_event_indexer_can_store_different_bridge_modes() {
    let pool = setup_test_db().await;
    let _config = create_test_config();

    // Use unique identifiers to avoid conflicts with parallel tests
    let test_id = "store_modes";
    let bridge_modes = vec!["AMB_ETH", "AMB_GC", "XDAI_ETH", "XDAI_GC"];

    for (idx, mode) in bridge_modes.iter().enumerate() {
        let log_data = serde_json::json!({
            "address": "0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e",
            "topics": [format!("0x{}test{}", test_id, idx)]
        });

        sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(format!("0x{}test{}", test_id, idx))
        .bind(mode)
        .bind(&log_data)
        .bind(idx as i64)
        .bind(format!("0x{}txhash{}", test_id, idx))
        .bind("false")
        .execute(&pool)
        .await
        .expect("Failed to insert event");
    }

    // Verify all bridge modes were stored using unique topic_key prefix
    for (idx, mode) in bridge_modes.iter().enumerate() {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM event_logs WHERE bridge_mode = $1 AND topic_key = $2",
        )
        .bind(mode)
        .bind(format!("0x{}test{}", test_id, idx))
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count.0, 1, "Should have 1 event for bridge mode {}", mode);
    }

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE topic_key LIKE $1")
        .bind(format!("0x{}%", test_id))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_event_indexer_duplicate_constraint() {
    let pool = setup_test_db().await;
    let test_id = "dup_test";

    let topic_key = format!("0x{}duplicate", test_id);
    let tx_hash = format!("0x{}txhash", test_id);
    let log_data = serde_json::json!({
        "address": "0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e",
        "topics": [&topic_key]
    });

    // Insert first time
    let result1 = sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (topic_key, transaction_hash) DO NOTHING
        "#,
    )
    .bind(&topic_key)
    .bind("AMB_ETH")
    .bind(&log_data)
    .bind(100i64)
    .bind(&tx_hash)
    .bind("false")
    .execute(&pool)
    .await;

    assert!(result1.is_ok());
    assert_eq!(result1.unwrap().rows_affected(), 1);

    // Try to insert duplicate (should be ignored due to ON CONFLICT)
    let result2 = sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (topic_key, transaction_hash) DO NOTHING
        "#,
    )
    .bind(&topic_key)
    .bind("AMB_ETH")
    .bind(&log_data)
    .bind(100i64)
    .bind(&tx_hash)
    .bind("false")
    .execute(&pool)
    .await;

    assert!(result2.is_ok());
    assert_eq!(result2.unwrap().rows_affected(), 0); // Should not insert

    // Verify only one row exists
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM event_logs WHERE topic_key = $1")
        .bind(&topic_key)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(count.0, 1);

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE topic_key = $1")
        .bind(&topic_key)
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_event_indexer_query_by_bridge_mode() {
    let pool = setup_test_db().await;
    let test_id = "query_mode";

    // Insert events for different bridge modes
    for i in 0..5 {
        let bridge_mode = if i % 2 == 0 { "AMB_ETH" } else { "XDAI_GC" };
        let log_data = serde_json::json!({"test": i});

        sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(format!("0x{}test{}", test_id, i))
        .bind(bridge_mode)
        .bind(&log_data)
        .bind(i as i64)
        .bind(format!("0x{}txhash{}", test_id, i))
        .bind("false")
        .execute(&pool)
        .await
        .unwrap();
    }

    // Query AMB_ETH events for this test only
    let amb_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM event_logs WHERE bridge_mode = 'AMB_ETH' AND topic_key LIKE $1",
    )
    .bind(format!("0x{}%", test_id))
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(amb_count.0, 3);

    // Query XDAI_GC events for this test only
    let xdai_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM event_logs WHERE bridge_mode = 'XDAI_GC' AND topic_key LIKE $1",
    )
    .bind(format!("0x{}%", test_id))
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(xdai_count.0, 2);

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE topic_key LIKE $1")
        .bind(format!("0x{}%", test_id))
        .execute(&pool)
        .await
        .unwrap();
}

#[tokio::test]
async fn test_event_indexer_query_unprocessed_events() {
    let pool = setup_test_db().await;
    let test_id = "query_unproc";

    // Insert mix of processed and unprocessed events
    for i in 0..10 {
        let is_processed = if i < 5 { "true" } else { "false" };
        let log_data = serde_json::json!({"test": i});

        sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(format!("0x{}test{}", test_id, i))
        .bind("AMB_ETH")
        .bind(&log_data)
        .bind(i as i64)
        .bind(format!("0x{}txhash{}", test_id, i))
        .bind(is_processed)
        .execute(&pool)
        .await
        .unwrap();
    }

    // Query unprocessed events for this test only
    let unprocessed_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM event_logs WHERE is_processed = 'false' AND topic_key LIKE $1",
    )
    .bind(format!("0x{}%", test_id))
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(unprocessed_count.0, 5);

    // Query processed events for this test only
    let processed_count: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM event_logs WHERE is_processed = 'true' AND topic_key LIKE $1",
    )
    .bind(format!("0x{}%", test_id))
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(processed_count.0, 5);

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE topic_key LIKE $1")
        .bind(format!("0x{}%", test_id))
        .execute(&pool)
        .await
        .unwrap();
}

// ============================================================================
// Tests with Mock Provider for Event Detection
// ============================================================================

/// Test Case 1: Multiple events from different bridge addresses within the same block
/// This test verifies that when multiple bridge events occur in the same block,
/// each event is correctly detected and stored with the appropriate bridge mode
#[tokio::test]
async fn test_multiple_events_same_block_different_bridges() {
    let pool = setup_test_db().await;
    let config = create_test_config();
    let test_id = "multi_same_block";

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
        let (provider, asserter) = create_mock_provider();

        // Mock provider responses
        asserter.push_success(&block_number); // get_block_number()
        asserter.push_success(&vec![amb_eth_log.clone()]); // get_logs()

        let indexer = EventIndexer::new(
            config.clone(),
            provider,
            "eth".to_string(),
            "UserRequestForSignature".to_string(),
            config.eth_amb_bridge_address,
            pool.clone(),
        );

        let result = indexer.poll_events(0).await;
        assert!(result.is_ok(), "AMB_ETH polling should succeed");
    }

    // Test AMB_GC indexer
    {
        let (provider, asserter) = create_mock_provider();

        asserter.push_success(&block_number);
        asserter.push_success(&vec![amb_gc_log.clone()]);

        let indexer = EventIndexer::new(
            config.clone(),
            provider,
            "gc".to_string(),
            "UserRequestForAffirmation".to_string(),
            config.gc_amb_bridge_address,
            pool.clone(),
        );

        let result = indexer.poll_events(0).await;
        assert!(result.is_ok(), "AMB_GC polling should succeed");
    }

    // Test XDAI_ETH indexer
    {
        let (provider, asserter) = create_mock_provider();

        asserter.push_success(&block_number);
        asserter.push_success(&vec![xdai_eth_log.clone()]);

        let indexer = EventIndexer::new(
            config.clone(),
            provider,
            "eth".to_string(),
            "UserRequestForSignature".to_string(),
            config.eth_xdai_bridge_address,
            pool.clone(),
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

/// Test Case 2: All 4 different bridge modes detected in each block
/// This test verifies that multiple blocks can each contain events from all 4 bridge modes
/// and that all events are correctly stored with accurate log data
#[tokio::test]
async fn test_four_bridge_modes_per_block() {
    let pool = setup_test_db().await;
    let config = create_test_config();
    let test_id = "four_modes";

    // Test across 3 different blocks (use unique block numbers to avoid conflicts)
    let blocks = vec![99910000u64, 99910001u64, 99910002u64];

    for (block_idx, block_number) in blocks.iter().enumerate() {
        // Create unique topics for each bridge mode in this block
        let amb_eth_topic = [0x10 + block_idx as u8; 32];
        let amb_gc_topic = [0x20 + block_idx as u8; 32];
        let xdai_eth_topic = [0x30 + block_idx as u8; 32];
        let xdai_gc_topic = [0x40 + block_idx as u8; 32];

        // Create and store events for all 4 bridge modes
        let bridge_configs = vec![
            (
                config.eth_amb_bridge_address,
                "AMB_ETH",
                amb_eth_topic,
                0x1000 + block_idx,
            ),
            (
                config.gc_amb_bridge_address,
                "AMB_GC",
                amb_gc_topic,
                0x2000 + block_idx,
            ),
            (
                config.eth_xdai_bridge_address,
                "XDAI_ETH",
                xdai_eth_topic,
                0x3000 + block_idx,
            ),
            (
                config.gc_xdai_bridge_address,
                "XDAI_GC",
                xdai_gc_topic,
                0x4000 + block_idx,
            ),
        ];

        for (address, bridge_name, topic, tx_suffix) in bridge_configs {
            let tx_hash = format!("0x{:064x}", tx_suffix);

            let log =
                create_test_log_with_address_and_topic(*block_number, &tx_hash, address, topic, 0);

            let (provider, asserter) = create_mock_provider();

            // Mock provider responses
            asserter.push_success(block_number);
            asserter.push_success(&vec![log.clone()]);

            let indexer = EventIndexer::new(
                config.clone(),
                provider,
                bridge_name.to_string(),
                "TestEvent".to_string(),
                address,
                pool.clone(),
            );

            let result = indexer.poll_events(0).await;
            assert!(
                result.is_ok(),
                "Polling should succeed for {} in block {}",
                bridge_name,
                block_number
            );
        }
    }

    // Verify total number of events (3 blocks × 4 bridge modes = 12 events)
    // Filter by this test's block numbers to avoid counting events from other tests
    let total_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM event_logs WHERE block_number IN ($1, $2, $3)")
            .bind(blocks[0] as i64)
            .bind(blocks[1] as i64)
            .bind(blocks[2] as i64)
            .fetch_one(&pool)
            .await
            .unwrap();

    assert_eq!(
        total_count.0, 12,
        "Should have 12 total events (3 blocks × 4 bridges)"
    );

    // Verify each block has all 4 bridge modes
    for block_number in blocks.iter() {
        let events_in_block: Vec<(String,)> = sqlx::query_as(
            "SELECT bridge_mode FROM event_logs WHERE block_number = $1 ORDER BY bridge_mode",
        )
        .bind(*block_number as i64)
        .fetch_all(&pool)
        .await
        .unwrap();

        assert_eq!(
            events_in_block.len(),
            4,
            "Block {} should have 4 events",
            block_number
        );

        let bridge_modes: Vec<String> = events_in_block.iter().map(|r| r.0.clone()).collect();
        assert_eq!(
            bridge_modes,
            vec!["AMB_ETH", "AMB_GC", "XDAI_ETH", "XDAI_GC"]
        );
    }

    // Verify each bridge mode appears exactly 3 times (once per block) within this test's blocks
    for bridge_mode in ["AMB_ETH", "AMB_GC", "XDAI_ETH", "XDAI_GC"].iter() {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM event_logs WHERE bridge_mode = $1 AND block_number IN ($2, $3, $4)"
        )
        .bind(bridge_mode)
        .bind(blocks[0] as i64)
        .bind(blocks[1] as i64)
        .bind(blocks[2] as i64)
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(count.0, 3, "{} should appear 3 times", bridge_mode);
    }

    // Verify log data integrity - check that addresses match bridge modes
    // Filter by this test's blocks to avoid checking events from other tests
    let log_verification: Vec<(String, serde_json::Value, String)> = sqlx::query_as(
        "SELECT bridge_mode, log_data, transaction_hash FROM event_logs WHERE block_number IN ($1, $2, $3) ORDER BY bridge_mode, block_number"
    )
    .bind(blocks[0] as i64)
    .bind(blocks[1] as i64)
    .bind(blocks[2] as i64)
    .fetch_all(&pool)
    .await
    .unwrap();

    for (bridge_mode, log_data, tx_hash) in log_verification {
        // Extract address from log data - it might be in different locations
        let stored_address = if let Some(addr) = log_data["inner"]["address"].as_str() {
            addr.to_string()
        } else if let Some(addr) = log_data["address"].as_str() {
            addr.to_string()
        } else {
            log_data
                .get("inner")
                .and_then(|inner| inner.get("address"))
                .unwrap_or(&log_data["address"])
                .to_string()
                .trim_matches('"')
                .to_string()
        };

        let expected_address = match bridge_mode.as_str() {
            "AMB_ETH" => config.eth_amb_bridge_address,
            "AMB_GC" => config.gc_amb_bridge_address,
            "XDAI_ETH" => config.eth_xdai_bridge_address,
            "XDAI_GC" => config.gc_xdai_bridge_address,
            _ => panic!("Unexpected bridge mode: {}", bridge_mode),
        };

        let expected_addr_str = format!("{:?}", expected_address).to_lowercase();
        assert!(
            stored_address
                .to_lowercase()
                .contains(&expected_addr_str[2..]), // Skip "0x"
            "Address mismatch for bridge mode {} in tx {}: stored={}, expected={}",
            bridge_mode,
            tx_hash,
            stored_address,
            expected_addr_str
        );

        // Verify log data has required fields (checking multiple possible locations)
        let has_block_number =
            log_data.get("block_number").is_some() || log_data.get("blockNumber").is_some();
        let has_tx_hash =
            log_data.get("transaction_hash").is_some() || log_data.get("transactionHash").is_some();
        let has_topics =
            log_data.pointer("/inner/data/topics").is_some() || log_data.get("topics").is_some();

        assert!(
            has_block_number,
            "block_number should be present in log data. Actual data: {:?}",
            log_data
        );
        assert!(
            has_tx_hash,
            "transaction_hash should be present in log data"
        );
        assert!(has_topics, "topics should be present in log data");
    }

    // Verify all events are marked as unprocessed initially (within this test's blocks)
    let unprocessed: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM event_logs WHERE is_processed = 'false' AND block_number IN ($1, $2, $3)"
    )
    .bind(blocks[0] as i64)
    .bind(blocks[1] as i64)
    .bind(blocks[2] as i64)
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(unprocessed.0, 12, "All events should be unprocessed");

    // Clean up only this test's data
    sqlx::query("DELETE FROM event_logs WHERE block_number IN ($1, $2, $3)")
        .bind(blocks[0] as i64)
        .bind(blocks[1] as i64)
        .bind(blocks[2] as i64)
        .execute(&pool)
        .await
        .unwrap();
}
