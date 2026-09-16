//! End-to-end FCR test: indexer → msg_processor → fcr_checker in one run.
//!
//! The per-component tests (`safe_test.rs`, `event_indexer_test.rs`,
//! `fcr_checker_test.rs`) each pin one stage against hand-seeded state. What
//! they can't show is the property FCR actually trades on: that a log is
//! **signed before it is final**, and that the checker later reaches the right
//! verdict about that already-signed log. That only appears when the three
//! stages run over the same rows, in order, with nothing hand-seeded in
//! between.
//!
//! Each test walks the same pipeline:
//!
//!   1. startup preflight decides whether the chain keeps fcr mode,
//!   2. the indexer resolves its upper bound and stores logs from a `safe`
//!      block that has *not* finalized yet,
//!   3. the message processor picks those rows up and signs them (`is_processed
//!      = 'true'`) while they are still `fcr_status = 'pending'`,
//!   4. time passes, the block finalizes, and the checker adjudicates.
//!
//! Step 3 happening before step 4 is the whole point — if the pipeline ever
//! stopped signing pending rows, FCR would silently degrade to finality
//! latency and every test below would still pass in isolation.

mod common;

use alloy::hex;
use alloy::primitives::{b256, Bytes, FixedBytes};
use alloy::rpc::types::Log;
use alloy::sol_types::SolEvent;
use alloy::transports::mock::Asserter;
use common::{create_mock_provider, setup_test_db};
use serde_json::json;
use sqlx::PgPool;
use tokio::sync::{mpsc, watch};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};
use worker::config::{BlockProcessingMode, Config};
use worker::contracts::AMB_BRIDGE;
use worker::service::event_indexer::EventIndexer;
use worker::service::fcr_checker::FcrChecker;
use worker::service::msg_processor::{MessageProcessor, SenderData};
use worker::service::safe::{get_safe_block_number, run_fcr_preflight};

/// The safe block the indexer will cap at. `0x64` is the form the checker's
/// block lookup asks for.
const SAFE_BLOCK: u64 = 100;
/// Finalized head while the safe block is still fresh — deliberately behind
/// `SAFE_BLOCK`, which is the latency fcr mode exists to skip.
const FINALIZED_AT_INDEX_TIME: u64 = 90;
/// Finalized head once the safe block has aged past finality.
const FINALIZED_LATER: u64 = 200;

/// The hash the indexer records for the safe block.
const SAFE_BLOCK_HASH: &str = "0x00000000000000000000000000000000000000000000000000000000000000aa";
/// A different block occupying the same number after finalization.
const REPLACEMENT_BLOCK_HASH: &str =
    "0x00000000000000000000000000000000000000000000000000000000000000bb";

// ---------------------------------------------------------------------------
// Mock execution layer
// ---------------------------------------------------------------------------

/// The chain as an EL node reports it *while indexing*: `safe` is well ahead of
/// `finalized`. Both tags are served from one server because in production they
/// come from the same RPC array.
async fn chain_at_index_time() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"safe\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": format!("0x{:x}", SAFE_BLOCK),
                "hash": SAFE_BLOCK_HASH,
            }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"finalized\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "number": format!("0x{:x}", FINALIZED_AT_INDEX_TIME) }
        })))
        .mount(&server)
        .await;

    server
}

/// The same chain later, after the safe block has finalized. `canonical_hash`
/// is whatever now occupies `SAFE_BLOCK` — equal to what we stored (the block
/// survived) or different (it was reorged out).
///
/// A separate `MockServer` is how "time passed" is expressed: wiremock stubs
/// are static, and the checker must see a *different* answer for the same
/// question than the indexer did.
async fn chain_after_finalization(canonical_hash: &str) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"finalized\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "number": format!("0x{:x}", FINALIZED_LATER) }
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains(format!("\"0x{:x}\"", SAFE_BLOCK)))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": format!("0x{:x}", SAFE_BLOCK),
                "hash": canonical_hash,
            }
        })))
        .mount(&server)
        .await;

    server
}

/// A node that *accepts* the `safe` tag but has no safe block yet — FCR off at
/// the client, still syncing, or pre-merge. `result: null` with no JSON-RPC
/// error object is the legitimate-empty case, not a misconfiguration.
async fn chain_without_a_safe_block_yet() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"safe\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": serde_json::Value::Null
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"finalized\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "number": format!("0x{:x}", FINALIZED_AT_INDEX_TIME) }
        })))
        .mount(&server)
        .await;

    server
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Config pointed at one EL server, with no beacon RPC so finality resolves
/// through the EL fallback and the mock is the single source of truth.
fn config_for(el_uri: String, mode: BlockProcessingMode) -> Config {
    let mut config = common::create_test_config();
    config.eth_bc_rpc = vec![];
    config.gc_bc_rpc = vec![];
    config.eth_rpc = vec![el_uri];
    config.eth_block_processing_mode = mode;
    config
}

/// An AMB ETH-side event — the bridge mode the message processor can decode
/// end-to-end without a validator key, so the test exercises the real
/// `read_from_db` → `process_message_or_skip` path rather than a stub.
fn amb_eth_log(block_number: u64, block_hash: FixedBytes<32>, tx_hash: [u8; 32]) -> Log {
    let event = AMB_BRIDGE::UserRequestForAffirmation {
        messageId: FixedBytes::<32>::from([7u8; 32]),
        encodedData: Bytes::from(vec![1, 2, 3, 4, 5]),
    };

    Log {
        inner: alloy::primitives::Log {
            address: common::create_test_config().eth_amb_bridge_address,
            data: event.encode_log_data(),
        },
        block_hash: Some(block_hash),
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: Some(FixedBytes::from(tx_hash)),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    }
}

/// Run the indexer over one cycle exactly as `start()` would: resolve the
/// chain's upper bound from its mode, then index up to it. Returns the bound so
/// the caller can assert *which* head the mode picked.
async fn index_one_cycle(config: &Config, pool: &PgPool, log: Log) -> i64 {
    let (provider, asserter): (_, Asserter) = create_mock_provider();
    asserter.push_success(&vec![log]);

    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let indexer = EventIndexer::new(
        config.clone(),
        provider,
        "eth".to_string(),
        "UserRequestForAffirmation".to_string(),
        config.eth_amb_bridge_address,
        pool.clone(),
        shutdown_rx,
    );

    let upper_bound = indexer
        .resolve_upper_bound()
        .await
        .expect("upper bound should resolve");

    indexer
        .poll_events(0, upper_bound as u64)
        .await
        .expect("indexing should succeed");

    upper_bound
}

/// Drain every unprocessed row through the message processor, which is what
/// `start()` does between sleeps. Returns what reached the on-chain sender.
async fn drain_message_processor(config: &Config, pool: &PgPool) -> Vec<SenderData> {
    let (tx, mut rx) = mpsc::channel::<SenderData>(100);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let processor = MessageProcessor::new(config.clone(), pool.clone(), tx, shutdown_rx);

    while let Some(event_log) = processor
        .read_from_db()
        .await
        .expect("reading unprocessed rows should succeed")
    {
        processor
            .process_message_or_skip(&event_log)
            .await
            .expect("processing should succeed");
    }

    let mut sent = Vec::new();
    while let Ok(data) = rx.try_recv() {
        sent.push(data);
    }
    sent
}

/// One row's full lifecycle state: was it signed, and how was it adjudicated?
async fn row_state(pool: &PgPool) -> Vec<(i32, Option<String>, Option<String>, Option<String>)> {
    sqlx::query_as("SELECT id, is_processed, fcr_status, block_hash FROM event_logs ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The path fcr mode exists for: index at `safe`, sign immediately, and have
/// the block still be there when it finalizes.
///
/// The assertion that matters is the ordering — `is_processed = 'true'` while
/// `fcr_status` is still `'pending'`. That is the reorg window, open on
/// purpose, and it must be observable here.
#[tokio::test]
async fn test_e2e_safe_block_is_signed_before_finality_then_confirmed() {
    let (pool, _db_lock) = setup_test_db().await;

    // --- 1. startup preflight -------------------------------------------
    let el_now = chain_at_index_time().await;
    let mut config = config_for(el_now.uri(), BlockProcessingMode::Fcr);
    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;
    assert_eq!(
        config.mode_for_chain("eth"),
        BlockProcessingMode::Fcr,
        "a safe-capable RPC must not be downgraded at boot"
    );

    // --- 2. index from the safe block ------------------------------------
    let log = amb_eth_log(
        SAFE_BLOCK,
        b256!("0x00000000000000000000000000000000000000000000000000000000000000aa"),
        [0xe1u8; 32],
    );
    let upper_bound = index_one_cycle(&config, &pool, log).await;

    assert_eq!(
        upper_bound, SAFE_BLOCK as i64,
        "fcr mode must cap at safe ({}), not at the finalized head ({})",
        SAFE_BLOCK, FINALIZED_AT_INDEX_TIME
    );

    let after_index = row_state(&pool).await;
    assert_eq!(after_index.len(), 1, "the safe-block log should be stored");
    assert_eq!(after_index[0].1.as_deref(), Some("false"), "not yet signed");
    assert_eq!(after_index[0].2.as_deref(), Some("pending"));
    assert_eq!(after_index[0].3.as_deref(), Some(SAFE_BLOCK_HASH));

    // --- 3. sign it while it is still un-finalized ------------------------
    let sent = drain_message_processor(&config, &pool).await;
    assert_eq!(sent.len(), 1, "the pending row must reach the sender");
    assert_eq!(sent[0].event_log_id, after_index[0].0);

    let after_signing = row_state(&pool).await;
    assert_eq!(
        after_signing[0].1.as_deref(),
        Some("true"),
        "the row is signed..."
    );
    assert_eq!(
        after_signing[0].2.as_deref(),
        Some("pending"),
        "...while still unverified — this is the reorg window fcr opens"
    );

    // --- 4. the block finalizes, and it is the block we indexed ----------
    let el_later = chain_after_finalization(SAFE_BLOCK_HASH).await;
    let later_config = config_for(el_later.uri(), BlockProcessingMode::Fcr);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    FcrChecker::new(later_config, pool.clone(), shutdown_rx)
        .check_chain("eth", &[el_later.uri()])
        .await
        .expect("revalidation should succeed");

    let final_state = row_state(&pool).await;
    assert_eq!(final_state[0].1.as_deref(), Some("true"));
    assert_eq!(
        final_state[0].2.as_deref(),
        Some("confirmed"),
        "the safe block survived finalization"
    );

    let false_positives: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM fcr_false_positives")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(false_positives, 0);
}

/// The failure mode FCR accepts: the safe block is signed, then a different
/// block finalizes at that number.
///
/// Nothing is undone — the signature is out. What the pipeline owes the
/// operator is an audit row that ties the false positive back to the exact
/// event that was already signed, which is only checkable end-to-end because
/// the `event_log_id` linkage spans all three stages.
#[tokio::test]
async fn test_e2e_reorged_safe_block_is_recorded_against_the_signed_event() {
    let (pool, _db_lock) = setup_test_db().await;

    let el_now = chain_at_index_time().await;
    let mut config = config_for(el_now.uri(), BlockProcessingMode::Fcr);
    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;

    let tx_hash = [0xe2u8; 32];
    let log = amb_eth_log(
        SAFE_BLOCK,
        b256!("0x00000000000000000000000000000000000000000000000000000000000000aa"),
        tx_hash,
    );
    index_one_cycle(&config, &pool, log).await;

    let sent = drain_message_processor(&config, &pool).await;
    assert_eq!(sent.len(), 1, "the event is signed before it is verified");
    let signed_event_log_id = sent[0].event_log_id;

    // A *different* block now occupies that number.
    let el_later = chain_after_finalization(REPLACEMENT_BLOCK_HASH).await;
    let later_config = config_for(el_later.uri(), BlockProcessingMode::Fcr);
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    FcrChecker::new(later_config, pool.clone(), shutdown_rx)
        .check_chain("eth", &[el_later.uri()])
        .await
        .expect("revalidation should succeed");

    let final_state = row_state(&pool).await;
    assert_eq!(
        final_state[0].1.as_deref(),
        Some("true"),
        "the signature is not undone by the revert verdict"
    );
    assert_eq!(final_state[0].2.as_deref(), Some("reverted"));

    let records: Vec<(
        String,
        i64,
        String,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT chain, block_number, stored_block_hash, canonical_block_hash, \
             transaction_hash, event_log_id, detected_at_finalized FROM fcr_false_positives",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(records.len(), 1, "one audit row per affected signed event");
    assert_eq!(records[0].0, "eth");
    assert_eq!(records[0].1, SAFE_BLOCK as i64);
    assert_eq!(records[0].2, SAFE_BLOCK_HASH);
    assert_eq!(records[0].3.as_deref(), Some(REPLACEMENT_BLOCK_HASH));
    assert_eq!(
        records[0].4.as_deref(),
        Some(format!("0x{}", hex::encode(tx_hash)).as_str()),
        "the audit row must name the tx that was signed"
    );
    assert_eq!(
        records[0].5,
        Some(signed_event_log_id),
        "the audit row must point at the same event the sender received"
    );
    assert_eq!(records[0].6, Some(FINALIZED_LATER as i64));
}

/// The fresh-start guard: a node that accepts the `safe` tag but has no safe
/// block *yet* must not stall an fcr chain. The cycle falls back to `finalized`
/// and keeps indexing, conservatively.
///
/// The subtle part is what happens to the rows it stores. `fcr_status` follows
/// the chain's configured **mode**, not which bound the cycle happened to use,
/// so a fallback-cycle row is still `pending` and still gets adjudicated later.
/// The alternative — inferring "this came from finalized, so skip the check" —
/// would make the lifecycle depend on a transient RPC condition.
#[tokio::test]
async fn test_e2e_missing_safe_block_falls_back_to_finalized() {
    let (pool, _db_lock) = setup_test_db().await;

    let el_now = chain_without_a_safe_block_yet().await;
    let mut config = config_for(el_now.uri(), BlockProcessingMode::Fcr);

    // Pin the resolver's verdict first. Without this the test would also pass
    // against a provider that never answered at all — an unreachable RPC falls
    // back to finalized too, and the point here is the *legitimate empty*.
    assert_eq!(
        get_safe_block_number(&reqwest::Client::new(), &config.eth_rpc)
            .await
            .expect("a legitimate empty is not an error"),
        None
    );

    // A legitimate empty is not a misconfiguration — the chain keeps fcr mode.
    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;
    assert_eq!(
        config.mode_for_chain("eth"),
        BlockProcessingMode::Fcr,
        "'no safe block yet' must not be mistaken for 'safe unsupported'"
    );

    let log = amb_eth_log(
        FINALIZED_AT_INDEX_TIME,
        b256!("0x00000000000000000000000000000000000000000000000000000000000000dd"),
        [0xe4u8; 32],
    );
    let upper_bound = index_one_cycle(&config, &pool, log).await;

    assert_eq!(
        upper_bound, FINALIZED_AT_INDEX_TIME as i64,
        "with no safe block the cycle must fall back to finalized, not stall"
    );

    let state = row_state(&pool).await;
    assert_eq!(state.len(), 1, "the fallback cycle still indexes");
    assert_eq!(
        state[0].2.as_deref(),
        Some("pending"),
        "fcr_status follows the chain's mode, not the bound this cycle used"
    );

    // And the row still flows on to be signed, exactly as in a normal cycle.
    let sent = drain_message_processor(&config, &pool).await;
    assert_eq!(sent.len(), 1);
}

/// The default mode must be untouched by any of the above: block-finality caps
/// at `finalized`, the rows it stores carry no fcr status, and the checker has
/// nothing to run on — it exits instead of looping.
#[tokio::test]
async fn test_e2e_block_finality_mode_is_unchanged() {
    let (pool, _db_lock) = setup_test_db().await;

    let el_now = chain_at_index_time().await;
    let mut config = config_for(el_now.uri(), BlockProcessingMode::BlockFinality);

    // Preflight is a no-op when nothing is in fcr mode.
    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;
    assert!(config.fcr_chains().is_empty());

    let log = amb_eth_log(
        FINALIZED_AT_INDEX_TIME,
        b256!("0x00000000000000000000000000000000000000000000000000000000000000cc"),
        [0xe3u8; 32],
    );
    let upper_bound = index_one_cycle(&config, &pool, log).await;

    assert_eq!(
        upper_bound, FINALIZED_AT_INDEX_TIME as i64,
        "block-finality mode must ignore the (much fresher) safe block"
    );

    let sent = drain_message_processor(&config, &pool).await;
    assert_eq!(sent.len(), 1);

    let state = row_state(&pool).await;
    assert_eq!(state[0].1.as_deref(), Some("true"), "signed");
    assert_eq!(
        state[0].2, None,
        "block-finality rows are final by construction — nothing to adjudicate"
    );

    // With no fcr chain the checker must return rather than poll forever.
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let checker = FcrChecker::new(config.clone(), pool.clone(), shutdown_rx);
    tokio::time::timeout(std::time::Duration::from_secs(5), checker.start())
        .await
        .expect("the checker should exit immediately when no chain is in fcr mode");
}
