//! Integration tests for the FCR revalidation task.
//!
//! The checker adjudicates rows that were indexed from a `safe` block once
//! that block finalizes. The properties under test are the ones that decide
//! whether a false confirmation is caught or silently swallowed: a surviving
//! block confirms, a replaced block is recorded as a false positive, a block
//! that can't be fetched is retried (never pruned — a dropped row would read
//! as verified), and one chain's checker never touches the other chain's rows.

mod common;

use common::{create_test_config, setup_test_db};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::watch;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use worker::config::{BlockProcessingMode, Config};
use worker::service::fcr_checker::FcrChecker;

/// EL provider that reports a given finalized block number.
async fn finality_provider(finalized_block: u64) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "number": format!("0x{:x}", finalized_block) }
        })))
        .mount(&server)
        .await;
    server
}

/// EL provider that answers every block lookup with the same hash — i.e. the
/// canonical chain as the checker sees it.
async fn block_provider(canonical_hash: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "number": "0x64", "hash": canonical_hash }
        })))
        .mount(&server)
        .await;
    server
}

/// EL provider that has no block at the requested number yet.
async fn empty_block_provider() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": serde_json::Value::Null
        })))
        .mount(&server)
        .await;
    server
}

/// Config whose finality lookups hit `finality_uri`. Block lookups are passed
/// to `check_chain` separately, so the two can be controlled independently.
fn fcr_config(finality_uri: String) -> Config {
    let mut config = create_test_config();
    // No beacon RPC: force the EL fallback so the mock server is the finality
    // source.
    config.eth_bc_rpc = vec![];
    config.gc_bc_rpc = vec![];
    config.eth_rpc = vec![finality_uri.clone()];
    config.gc_rpc = vec![finality_uri];
    config.eth_block_processing_mode = BlockProcessingMode::Fcr;
    config.gc_block_processing_mode = BlockProcessingMode::Fcr;
    config
}

fn checker(config: Config, pool: &PgPool) -> FcrChecker {
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    FcrChecker::new(config, pool.clone(), shutdown_rx)
}

#[allow(clippy::too_many_arguments)]
async fn insert_pending_row(
    pool: &PgPool,
    bridge_mode: &str,
    block_number: i64,
    block_hash: Option<&str>,
    tx_hash: &str,
    log_index: i64,
    fcr_status: Option<&str>,
) {
    sqlx::query(
        r#"
        INSERT INTO event_logs
            (topic_key, bridge_mode, log_data, block_number, block_hash,
             transaction_hash, log_index, is_processed, fcr_status)
        VALUES ($1, $2, $3, $4, $5, $6, $7, 'false', $8)
        "#,
    )
    .bind("0x11")
    .bind(bridge_mode)
    .bind(json!({}))
    .bind(block_number)
    .bind(block_hash)
    .bind(tx_hash)
    .bind(log_index)
    .bind(fcr_status)
    .execute(pool)
    .await
    .expect("failed to insert test row");
}

async fn statuses(pool: &PgPool) -> Vec<(String, Option<String>)> {
    sqlx::query_as("SELECT transaction_hash, fcr_status FROM event_logs ORDER BY transaction_hash")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn false_positive_count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM fcr_false_positives")
        .fetch_one(pool)
        .await
        .unwrap()
}

// A safe block whose hash still occupies that number after finalization is
// exactly what FCR promised — every row from it becomes `confirmed`.
#[tokio::test]
async fn test_surviving_block_confirms_every_row() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = block_provider("0xaaa").await;

    insert_pending_row(
        &pool,
        "AMB_ETH",
        100,
        Some("0xaaa"),
        "0xt1",
        0,
        Some("pending"),
    )
    .await;
    insert_pending_row(
        &pool,
        "XDAI_ETH",
        100,
        Some("0xaaa"),
        "0xt2",
        1,
        Some("pending"),
    )
    .await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    let rows = statuses(&pool).await;
    assert_eq!(rows.len(), 2);
    for (tx, status) in &rows {
        assert_eq!(status.as_deref(), Some("confirmed"), "row {}", tx);
    }
    assert_eq!(false_positive_count(&pool).await, 0);
}

// The one real FCR failure mode: a different block occupies that number after
// finalization. Nothing is undone on-chain, but every affected (already
// signed) event must be recorded.
#[tokio::test]
async fn test_replaced_block_records_a_false_positive_per_event() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = block_provider("0xbbb").await;

    insert_pending_row(
        &pool,
        "AMB_ETH",
        100,
        Some("0xaaa"),
        "0xt1",
        0,
        Some("pending"),
    )
    .await;
    insert_pending_row(
        &pool,
        "AMB_ETH",
        100,
        Some("0xaaa"),
        "0xt2",
        1,
        Some("pending"),
    )
    .await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    for (tx, status) in statuses(&pool).await {
        assert_eq!(status.as_deref(), Some("reverted"), "row {}", tx);
    }

    let records: Vec<(
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT chain, block_number, stored_block_hash, canonical_block_hash, \
             transaction_hash, detected_at_finalized FROM fcr_false_positives \
             ORDER BY transaction_hash",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(records.len(), 2, "one audit row per affected event");
    assert_eq!(records[0].0, "eth");
    assert_eq!(records[0].1, 100);
    assert_eq!(records[0].2, "0xaaa");
    assert_eq!(records[0].3.as_deref(), Some("0xbbb"));
    assert_eq!(records[0].4.as_deref(), Some("0xt1"));
    assert_eq!(records[0].5, Some(200));
}

// A block the node can't produce yet is not a verdict. Rows stay pending and
// are retried; pruning them would be indistinguishable from verifying them.
#[tokio::test]
async fn test_unavailable_block_is_retried_not_resolved() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = empty_block_provider().await;

    insert_pending_row(
        &pool,
        "AMB_ETH",
        100,
        Some("0xaaa"),
        "0xt1",
        0,
        Some("pending"),
    )
    .await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    let rows = statuses(&pool).await;
    assert_eq!(rows[0].1.as_deref(), Some("pending"));
    assert_eq!(false_positive_count(&pool).await, 0);
}

// Each chain's cycle resolves finality and canonical hashes on that chain
// only; a GC row must not be adjudicated by the ETH cycle.
#[tokio::test]
async fn test_check_chain_is_scoped_to_one_chain() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = block_provider("0xaaa").await;

    insert_pending_row(
        &pool,
        "AMB_ETH",
        100,
        Some("0xaaa"),
        "0xt1",
        0,
        Some("pending"),
    )
    .await;
    insert_pending_row(
        &pool,
        "AMB_GC",
        100,
        Some("0xaaa"),
        "0xt2",
        1,
        Some("pending"),
    )
    .await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    let rows = statuses(&pool).await;
    assert_eq!(rows[0].1.as_deref(), Some("confirmed"), "eth row");
    assert_eq!(rows[1].1.as_deref(), Some("pending"), "gc row untouched");
}

// Blocks above the finalized head have not been adjudicated by anything yet —
// checking them early would compare against a chain that can still change.
#[tokio::test]
async fn test_blocks_above_finalized_are_left_pending() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = block_provider("0xbbb").await;

    insert_pending_row(
        &pool,
        "AMB_ETH",
        300,
        Some("0xaaa"),
        "0xt1",
        0,
        Some("pending"),
    )
    .await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    let rows = statuses(&pool).await;
    assert_eq!(rows[0].1.as_deref(), Some("pending"));
    assert_eq!(false_positive_count(&pool).await, 0);
}

// Rows indexed in block-finality mode carry a NULL status and are final by
// construction — the checker must not touch them even on an fcr chain.
#[tokio::test]
async fn test_block_finality_rows_are_ignored() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = block_provider("0xbbb").await;

    insert_pending_row(&pool, "AMB_ETH", 100, Some("0xaaa"), "0xt1", 0, None).await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    let rows = statuses(&pool).await;
    assert_eq!(rows[0].1, None);
    assert_eq!(false_positive_count(&pool).await, 0);
}

// A pending row with no stored hash can't be compared against anything. It
// must stay pending (and be reported), never be silently confirmed.
#[tokio::test]
async fn test_pending_row_without_block_hash_is_not_confirmed() {
    let (pool, _db_lock) = setup_test_db().await;
    let finality = finality_provider(200).await;
    let blocks = block_provider("0xaaa").await;

    insert_pending_row(&pool, "AMB_ETH", 100, None, "0xt1", 0, Some("pending")).await;

    checker(fcr_config(finality.uri()), &pool)
        .check_chain("eth", &[blocks.uri()])
        .await
        .expect("check_chain should succeed");

    let rows = statuses(&pool).await;
    assert_eq!(rows[0].1.as_deref(), Some("pending"));
    assert_eq!(false_positive_count(&pool).await, 0);
}
