//! Shared "what is the latest finalized block" lookup.
//!
//! Beacon chain is the authoritative source of finality; the execution-layer
//! `finalized` tag is used only as a fallback. Both the event indexer (to bound
//! which blocks it indexes) and the message processor (to gate signing) resolve
//! finality through here so they can never disagree on what "finalized" means.

use crate::error::BridgeValidatorError;
use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
struct BeaconBlockResponse {
    data: BlockData,
}

#[derive(Debug, Deserialize, Serialize)]
struct BlockData {
    message: BlockMessage,
    #[serde(default)]
    signature: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BlockMessage {
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    proposer_index: Option<String>,
    #[serde(default)]
    parent_root: Option<String>,
    #[serde(default)]
    state_root: Option<String>,
    body: BlockBody,
}

#[derive(Debug, Deserialize, Serialize)]
struct BlockBody {
    #[serde(default)]
    randao_reveal: Option<String>,
    #[serde(default)]
    eth1_data: Option<Eth1Data>,
    #[serde(default)]
    graffiti: Option<String>,
    #[serde(default)]
    proposer_slashings: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    attester_slashings: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    attestations: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    deposits: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    voluntary_exits: Option<Vec<serde_json::Value>>,
    execution_payload: ExecutionPayload,
}

#[derive(Debug, Deserialize, Serialize)]
struct ExecutionPayload {
    #[serde(default)]
    parent_hash: Option<String>,
    #[serde(default)]
    fee_recipient: Option<Address>,
    #[serde(default)]
    state_root: Option<String>,
    #[serde(default)]
    receipts_root: Option<String>,
    #[serde(default)]
    logs_bloom: Option<String>,
    #[serde(default)]
    prev_randao: Option<String>,
    block_number: i64,
    #[serde(default)]
    gas_limit: Option<String>,
    #[serde(default)]
    gas_used: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    extra_data: Option<String>,
    #[serde(default)]
    base_fee_per_gas: Option<String>,
    #[serde(default)]
    block_hash: Option<String>,
    #[serde(default)]
    transactions: Option<String>,
    #[serde(default)]
    withdrawals: Option<String>,
    #[serde(default)]
    blob_gas_used: Option<String>,
    #[serde(default)]
    excess_blob_gas: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Eth1Data {
    deposit_root: String,
    deposit_count: String,
    block_hash: String,
}

/// Resolve the latest finalized execution-layer block number.
///
/// Tries the beacon RPC first (if configured), then falls back to each EL RPC
/// in turn. Errors only if every configured endpoint fails.
pub async fn get_finalized_block_number(
    http_client: &reqwest::Client,
    bc_rpc: Option<&str>,
    el_rpcs: &[String],
) -> Result<i64, BridgeValidatorError> {
    if let Some(bc_rpc) = bc_rpc {
        match get_finalized_block_from_beacon(http_client, bc_rpc).await {
            Ok(block_number) => return Ok(block_number),
            Err(e) => {
                tracing::warn!("Beacon chain RPC failed: {}, falling back to EL RPCs", e);
            }
        }
    }

    for (i, el_rpc) in el_rpcs.iter().enumerate() {
        tracing::info!("Trying EL RPC {}/{}: {}", i + 1, el_rpcs.len(), el_rpc);
        match get_finalized_block_from_el(http_client, el_rpc).await {
            Ok(block_number) => {
                tracing::info!(
                    "Last finalized block: {} (from EL RPC {}/{})",
                    block_number,
                    i + 1,
                    el_rpcs.len()
                );
                return Ok(block_number);
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to get finalized block from EL RPC {}/{} ({}): {}",
                    i + 1,
                    el_rpcs.len(),
                    el_rpc,
                    e
                );
            }
        }
    }

    Err(BridgeValidatorError::AllRpcsFailedForFinalizedBlock)
}

async fn get_finalized_block_from_beacon(
    http_client: &reqwest::Client,
    bc_rpc: &str,
) -> Result<i64, BridgeValidatorError> {
    let endpoint = format!("{}/eth/v2/beacon/blocks/finalized", bc_rpc);

    let response = http_client
        .get(&endpoint)
        .header("Accept", "application/json")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(BridgeValidatorError::BeaconHttpStatus(response.status()));
    }

    let block_response = response.json::<BeaconBlockResponse>().await?;
    Ok(block_response
        .data
        .message
        .body
        .execution_payload
        .block_number)
}

async fn get_finalized_block_from_el(
    http_client: &reqwest::Client,
    el_rpc: &str,
) -> Result<i64, BridgeValidatorError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": ["finalized", false]
    });

    let response = http_client.post(el_rpc).json(&payload).send().await?;

    if !response.status().is_success() {
        return Err(BridgeValidatorError::ElHttpStatus(response.status()));
    }

    let body: serde_json::Value = response.json().await?;

    let hex_block_number = body["result"]["number"]
        .as_str()
        .ok_or(BridgeValidatorError::EmptyElResponse)?;

    let block_number = i64::from_str_radix(hex_block_number.trim_start_matches("0x"), 16)?;
    Ok(block_number)
}
