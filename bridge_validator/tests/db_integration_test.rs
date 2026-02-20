// Database integration tests
// For provider tests, use mock providers from common::create_mock_provider()
// The database container is automatically cleaned up when tests complete
// You can also explicitly call common::shutdown_test_db() to clean up manually

mod common;

use common::{cleanup_test_db, create_mock_provider, setup_test_db};
use serde_json::json;

#[tokio::test]
async fn test_insert_and_retrieve_event_log() {
    let pool = setup_test_db().await;

    // Insert a test event log
    let topic_key = "0xtest123456";
    let bridge_mode = "AMB_ETH";
    let log_data = json!({
        "address": "0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e",
        "topics": [topic_key]
    });

    let result = sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
        VALUES ($1, $2, $3, $4, $5, $6)
        RETURNING id
        "#
    )
    .bind(topic_key)
    .bind(bridge_mode)
    .bind(&log_data)
    .bind(12345i64)
    .bind("0xtxhash123")
    .bind("false")
    .fetch_one(&pool)
    .await;

    assert!(result.is_ok(), "Failed to insert event log");

    // Retrieve the inserted log
    let row: (String, String, i64) = sqlx::query_as(
        r#"
        SELECT topic_key, bridge_mode, block_number
        FROM event_logs
        WHERE topic_key = $1
        "#,
    )
    .bind(topic_key)
    .fetch_one(&pool)
    .await
    .expect("Failed to retrieve event log");

    assert_eq!(row.0, topic_key);
    assert_eq!(row.1, bridge_mode);
    assert_eq!(row.2, 12345);

    cleanup_test_db(&pool).await;
}

#[tokio::test]
async fn test_duplicate_event_log_prevention() {
    let pool = setup_test_db().await;

    let topic_key = "0xduplicate_test";
    let tx_hash = "0xtxhash_dup";
    let log_data = json!({
        "address": "0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e",
        "topics": [topic_key]
    });

    // Insert first time
    let result1 = sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (topic_key, transaction_hash) DO NOTHING
        "#
    )
    .bind(topic_key)
    .bind("AMB_ETH")
    .bind(&log_data)
    .bind(100i64)
    .bind(tx_hash)
    .bind("false")
    .execute(&pool)
    .await;

    assert!(result1.is_ok());
    assert_eq!(result1.unwrap().rows_affected(), 1);

    // Try to insert duplicate (should be ignored)
    let result2 = sqlx::query(
        r#"
        INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (topic_key, transaction_hash) DO NOTHING
        "#
    )
    .bind(topic_key)
    .bind("AMB_ETH")
    .bind(&log_data)
    .bind(100i64)
    .bind(tx_hash)
    .bind("false")
    .execute(&pool)
    .await;

    assert!(result2.is_ok());
    assert_eq!(result2.unwrap().rows_affected(), 0); // Should not insert

    // Verify only one row exists
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM event_logs WHERE topic_key = $1")
        .bind(topic_key)
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(count.0, 1);

    cleanup_test_db(&pool).await;
}

#[tokio::test]
async fn test_concurrent_message_processing_with_skip_locked() {
    let pool = setup_test_db().await;

    // Insert 5 test logs
    for i in 0..5 {
        let log_data = json!({
            "address": "0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e",
            "topics": [format!("0xtest{}", i)]
        });

        sqlx::query(
            r#"
            INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#
        )
        .bind(format!("0xtest{}", i))
        .bind("AMB_ETH")
        .bind(&log_data)
        .bind(i as i64)
        .bind(format!("0xtxhash{}", i))
        .bind("false")
        .execute(&pool)
        .await
        .unwrap();
    }

    // Simulate concurrent processing with 3 workers
    let mut handles = vec![];

    for worker_id in 0..3 {
        let pool_clone = pool.clone();
        handles.push(tokio::spawn(async move {
            let mut processed_ids = vec![];

            // Each worker tries to process 2 messages
            for _ in 0..2 {
                let mut tx = pool_clone.begin().await.unwrap();

                let row: Option<(i32,)> = sqlx::query_as(
                    r#"
                    SELECT id
                    FROM event_logs
                    WHERE is_processed = 'false'
                    ORDER BY block_number ASC
                    LIMIT 1
                    FOR UPDATE SKIP LOCKED
                    "#,
                )
                .fetch_optional(&mut *tx)
                .await
                .unwrap();

                if let Some((id,)) = row {
                    // Mark as processed
                    sqlx::query("UPDATE event_logs SET is_processed = 'true' WHERE id = $1")
                        .bind(id)
                        .execute(&mut *tx)
                        .await
                        .unwrap();

                    processed_ids.push(id);
                }

                tx.commit().await.unwrap();
            }

            (worker_id, processed_ids)
        }));
    }

    let results = futures::future::join_all(handles).await;

    // Collect all processed IDs
    let mut all_ids = vec![];
    for result in results {
        let (worker_id, ids) = result.unwrap();
        println!("Worker {} processed: {:?}", worker_id, ids);
        all_ids.extend(ids);
    }

    // Verify no duplicate processing
    let unique_ids: std::collections::HashSet<_> = all_ids.iter().collect();
    assert_eq!(
        all_ids.len(),
        unique_ids.len(),
        "Duplicate processing detected!"
    );

    // Verify all 5 logs were processed
    assert_eq!(all_ids.len(), 5);

    cleanup_test_db(&pool).await;
}
