//! Tests for the `safe`-block resolver and the startup safe-support preflight.
//!
//! The behaviour that matters here is the three-way classification: a provider
//! that serves `safe`, a provider that *rejects* the tag (misconfiguration —
//! must be loud, never a silent downgrade to finality), and a provider that
//! accepts the tag but has no safe block yet (legitimate empty — quiet
//! fallback). The discriminator is the presence of a JSON-RPC error object,
//! not null-ness of `result`.

mod common;

use common::create_test_config;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};
use worker::config::BlockProcessingMode;
use worker::service::safe::{
    get_canonical_block_hash, get_safe_block_number, preflight_safe_support, probe_safe_block,
    run_fcr_preflight, SafeProbe,
};

/// A provider that answers with a block object.
async fn provider_with_block(block_number: u64, hash: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "number": format!("0x{:x}", block_number),
                "hash": hash,
            }
        })))
        .mount(&server)
        .await;
    server
}

/// A provider that rejects the `safe` tag with a JSON-RPC error.
async fn provider_rejecting_safe() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": { "code": -32602, "message": "invalid argument 0: unsupported block tag" }
        })))
        .mount(&server)
        .await;
    server
}

/// A provider that accepts the tag but has no safe block yet.
async fn provider_with_null_result() -> MockServer {
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

async fn provider_returning_500() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    server
}

// ============================================================================
// probe_safe_block — the three-way classification
// ============================================================================

#[tokio::test]
async fn test_probe_classifies_a_served_safe_block() {
    let server = provider_with_block(0x1234, "0xabc").await;
    let client = reqwest::Client::new();

    let probe = probe_safe_block(&client, &server.uri()).await;

    assert_eq!(probe, SafeProbe::Block(0x1234));
}

// A rejected tag is a misconfiguration, not a transient miss — it must be
// distinguishable from "no safe block yet".
#[tokio::test]
async fn test_probe_classifies_a_rejected_tag_as_unsupported() {
    let server = provider_rejecting_safe().await;
    let client = reqwest::Client::new();

    let probe = probe_safe_block(&client, &server.uri()).await;

    match probe {
        SafeProbe::Unsupported(reason) => {
            assert!(
                reason.contains("-32602"),
                "reason should carry the JSON-RPC error: {}",
                reason
            )
        }
        other => panic!("expected Unsupported, got {:?}", other),
    }
}

// `result: null` with no error object is a legitimate empty (FCR off, node
// syncing, pre-merge) — keyed on the absence of an error object, not on
// null-ness alone.
#[tokio::test]
async fn test_probe_classifies_null_result_as_empty() {
    let server = provider_with_null_result().await;
    let client = reqwest::Client::new();

    let probe = probe_safe_block(&client, &server.uri()).await;

    assert_eq!(probe, SafeProbe::Empty);
}

// An HTTP-level failure carries no JSON-RPC error object, so it says nothing
// about safe support and must not be reported as unsupported.
#[tokio::test]
async fn test_probe_classifies_http_error_as_unreachable() {
    let server = provider_returning_500().await;
    let client = reqwest::Client::new();

    let probe = probe_safe_block(&client, &server.uri()).await;

    match probe {
        SafeProbe::Unreachable(reason) => assert!(reason.contains("500"), "got: {}", reason),
        other => panic!("expected Unreachable, got {:?}", other),
    }
}

// ============================================================================
// get_safe_block_number — RPC-array walk
// ============================================================================

#[tokio::test]
async fn test_safe_resolver_returns_the_first_served_block() {
    let good = provider_with_block(500, "0xaaa").await;
    let unused = provider_with_block(999, "0xbbb").await;
    let client = reqwest::Client::new();

    let result = get_safe_block_number(&client, &[good.uri(), unused.uri()])
        .await
        .unwrap();

    assert_eq!(result, Some(500));
}

// Continue past a provider that can't answer rather than giving up on the
// whole array.
#[tokio::test]
async fn test_safe_resolver_falls_through_to_the_next_provider() {
    let bad = provider_rejecting_safe().await;
    let good = provider_with_block(777, "0xccc").await;
    let client = reqwest::Client::new();

    let result = get_safe_block_number(&client, &[bad.uri(), good.uri()])
        .await
        .unwrap();

    assert_eq!(result, Some(777));
}

// Legitimate empty => Ok(None), which the indexer turns into a quiet fallback
// to finalized rather than an error.
#[tokio::test]
async fn test_safe_resolver_returns_none_for_legitimate_empty() {
    let empty = provider_with_null_result().await;
    let client = reqwest::Client::new();

    let result = get_safe_block_number(&client, &[empty.uri()])
        .await
        .unwrap();

    assert_eq!(result, None);
}

// Nothing usable anywhere is an error, not a `None` — `None` specifically
// means "a provider accepted the tag and had nothing".
#[tokio::test]
async fn test_safe_resolver_errors_when_no_provider_can_serve_safe() {
    let bad = provider_rejecting_safe().await;
    let down = provider_returning_500().await;
    let client = reqwest::Client::new();

    let result = get_safe_block_number(&client, &[bad.uri(), down.uri()]).await;

    assert!(result.is_err());
}

// ============================================================================
// get_canonical_block_hash — the checker's lookup
// ============================================================================

#[tokio::test]
async fn test_canonical_block_hash_is_returned_lowercased() {
    let server = provider_with_block(42, "0xDEADBEEF").await;
    let client = reqwest::Client::new();

    let hash = get_canonical_block_hash(&client, &[server.uri()], 42)
        .await
        .unwrap();

    assert_eq!(hash, Some("0xdeadbeef".to_string()));
}

// A null result means "retry later", not "reverted" — the checker must be able
// to tell the two apart.
#[tokio::test]
async fn test_canonical_block_hash_is_none_when_provider_has_no_block() {
    let server = provider_with_null_result().await;
    let client = reqwest::Client::new();

    let hash = get_canonical_block_hash(&client, &[server.uri()], 42)
        .await
        .unwrap();

    assert_eq!(hash, None);
}

#[tokio::test]
async fn test_canonical_block_hash_errors_when_every_provider_fails() {
    let down = provider_returning_500().await;
    let client = reqwest::Client::new();

    let result = get_canonical_block_hash(&client, &[down.uri()], 42).await;

    assert!(result.is_err());
}

// ============================================================================
// Startup preflight (§3a)
// ============================================================================

#[tokio::test]
async fn test_preflight_accepts_a_provider_that_serves_safe() {
    let good = provider_with_block(10, "0xaaa").await;
    let client = reqwest::Client::new();

    let support = preflight_safe_support(&client, "eth", &[good.uri()]).await;

    assert!(support.supported);
    assert!(support.keeps_fcr());
    assert!(support.unsupported_providers.is_empty());
}

// Tag accepted, no safe block yet: still a supported provider — the fallback
// to finalized is quiet and temporary, not a misconfiguration.
#[tokio::test]
async fn test_preflight_treats_legitimate_empty_as_supported() {
    let empty = provider_with_null_result().await;
    let client = reqwest::Client::new();

    let support = preflight_safe_support(&client, "eth", &[empty.uri()]).await;

    assert!(support.supported);
    assert!(support.keeps_fcr());
}

// A safe-incapable RPC array must fail the chain's preflight instead of
// degrading silently to finality for the process lifetime.
#[tokio::test]
async fn test_preflight_fails_when_every_provider_rejects_safe() {
    let bad = provider_rejecting_safe().await;
    let client = reqwest::Client::new();

    let support = preflight_safe_support(&client, "eth", &[bad.uri()]).await;

    assert!(!support.supported);
    assert!(support.reachable);
    assert!(!support.keeps_fcr());
    assert_eq!(support.unsupported_providers, vec![bad.uri()]);
}

// One good provider keeps the array usable, but the bad one is still flagged
// so a later failover isn't mistaken for "the chain caught up".
#[tokio::test]
async fn test_preflight_flags_a_bad_provider_in_an_otherwise_good_array() {
    let bad = provider_rejecting_safe().await;
    let good = provider_with_block(10, "0xaaa").await;
    let client = reqwest::Client::new();

    let support = preflight_safe_support(&client, "eth", &[bad.uri(), good.uri()]).await;

    assert!(support.supported);
    assert!(support.keeps_fcr());
    assert_eq!(support.unsupported_providers, vec![bad.uri()]);
}

#[tokio::test]
async fn test_run_fcr_preflight_downgrades_a_safe_incapable_chain() {
    let bad = provider_rejecting_safe().await;
    let good = provider_with_block(10, "0xaaa").await;

    let mut config = create_test_config();
    config.eth_rpc = vec![bad.uri()];
    config.gc_rpc = vec![good.uri()];
    config.eth_block_processing_mode = BlockProcessingMode::Fcr;
    config.gc_block_processing_mode = BlockProcessingMode::Fcr;

    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;

    // Only the chain that can't serve `safe` is downgraded.
    assert_eq!(
        config.mode_for_chain("eth"),
        BlockProcessingMode::BlockFinality
    );
    assert_eq!(config.mode_for_chain("gc"), BlockProcessingMode::Fcr);
}

// An RPC outage at boot is transient and must not permanently downgrade the
// chain — the per-cycle fallback already handles it.
#[tokio::test]
async fn test_run_fcr_preflight_keeps_fcr_when_nothing_is_reachable() {
    let down = provider_returning_500().await;

    let mut config = create_test_config();
    config.eth_rpc = vec![down.uri()];
    config.eth_block_processing_mode = BlockProcessingMode::Fcr;
    config.gc_block_processing_mode = BlockProcessingMode::BlockFinality;

    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;

    assert_eq!(config.mode_for_chain("eth"), BlockProcessingMode::Fcr);
}

// block-finality deployments must not make a single probe call.
#[tokio::test]
async fn test_run_fcr_preflight_is_a_no_op_without_fcr_chains() {
    let server = provider_with_block(10, "0xaaa").await;

    let mut config = create_test_config();
    config.eth_rpc = vec![server.uri()];
    config.gc_rpc = vec![server.uri()];

    run_fcr_preflight(&mut config, &reqwest::Client::new()).await;

    assert_eq!(
        config.mode_for_chain("eth"),
        BlockProcessingMode::BlockFinality
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}
