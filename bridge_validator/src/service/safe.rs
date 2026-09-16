//! Execution-layer `safe`-block lookups for FCR mode.
//!
//! The `safe` tag is the only portable handle on a fast-confirmed block: there
//! is no Beacon API `safe` block_id on any client, so unlike
//! [`crate::service::finality`] this resolver is **EL-only** — never
//! beacon-first, and never compared against a beacon block root.
//!
//! It follows the same resolver shape as [`crate::service::finality`]: walk the
//! configured RPC array in order, validate the response before trusting it, and
//! move to the next provider on failure. The one thing it must do that the
//! finalized resolver never had to is **tell two kinds of "no block" apart**:
//!
//! * a provider that *rejects* the `safe` tag (JSON-RPC error) is a
//!   misconfiguration — the operator believes they have ~12s confirmation and
//!   would silently get ~12.8m finality instead, and
//! * a provider that *accepts* the tag and answers `result: null` simply has no
//!   safe block yet (FCR off, node syncing, pre-merge) — a legitimate empty that
//!   should quietly fall back to finalized.
//!
//! The discriminator is the presence of a JSON-RPC `error` object, never
//! null-ness of `result` alone.

use crate::config::{BlockProcessingMode, Config};
use crate::error::BridgeValidatorError;

/// Outcome of asking a single EL provider for the `safe` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeProbe {
    /// Provider returned a block: it supports `safe` and has one.
    Block(i64),
    /// Provider accepted the tag but has no safe block yet (`result: null`).
    /// Legitimate empty — not a misconfiguration.
    Empty,
    /// Provider rejected the `safe` tag with a JSON-RPC error. This provider
    /// can never serve fcr mode.
    Unsupported(String),
    /// Provider could not be reached, or answered with something unparseable.
    /// Transient as far as we can tell — says nothing about `safe` support.
    Unreachable(String),
}

/// Ask one EL provider for the `safe` block and classify the answer.
pub async fn probe_safe_block(http_client: &reqwest::Client, el_rpc: &str) -> SafeProbe {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": ["safe", false]
    });

    let response = match http_client.post(el_rpc).json(&payload).send().await {
        Ok(response) => response,
        Err(e) => return SafeProbe::Unreachable(e.to_string()),
    };

    if !response.status().is_success() {
        // An HTTP-level rejection carries no JSON-RPC error object, so we
        // cannot claim the tag is unsupported — only that this provider did
        // not answer.
        return SafeProbe::Unreachable(format!("HTTP {}", response.status()));
    }

    let body: serde_json::Value = match response.json().await {
        Ok(body) => body,
        Err(e) => return SafeProbe::Unreachable(format!("invalid JSON body: {}", e)),
    };

    // A JSON-RPC error object is the *only* signal that the tag itself was
    // refused (e.g. -32602 invalid argument, unsupported block tag).
    match body.get("error") {
        Some(error) if !error.is_null() => {
            return SafeProbe::Unsupported(error.to_string());
        }
        _ => {}
    }

    let result = &body["result"];
    if result.is_null() {
        return SafeProbe::Empty;
    }

    let hex_block_number = match result["number"].as_str() {
        Some(number) => number,
        None => {
            return SafeProbe::Unreachable("safe block response has no 'number' field".to_string())
        }
    };

    match i64::from_str_radix(hex_block_number.trim_start_matches("0x"), 16) {
        Ok(block_number) => SafeProbe::Block(block_number),
        Err(e) => SafeProbe::Unreachable(format!("unparseable block number: {}", e)),
    }
}

/// Resolve the latest `safe` execution-layer block number.
///
/// Returns `Ok(None)` when a provider accepted the tag but has no safe block
/// yet — the caller is expected to fall back to `finalized` (the fresh-start
/// guard). Errors only when no provider produced either a block or a
/// legitimate empty.
pub async fn get_safe_block_number(
    http_client: &reqwest::Client,
    el_rpcs: &[String],
) -> Result<Option<i64>, BridgeValidatorError> {
    let mut saw_legitimate_empty = false;

    for (i, el_rpc) in el_rpcs.iter().enumerate() {
        match probe_safe_block(http_client, el_rpc).await {
            SafeProbe::Block(block_number) => {
                tracing::debug!(
                    "Latest safe block: {} (from EL RPC {}/{})",
                    block_number,
                    i + 1,
                    el_rpcs.len()
                );
                return Ok(Some(block_number));
            }
            SafeProbe::Empty => {
                tracing::warn!(
                    "EL RPC {}/{} ({}) accepted the 'safe' tag but has no safe block yet",
                    i + 1,
                    el_rpcs.len(),
                    el_rpc
                );
                saw_legitimate_empty = true;
            }
            SafeProbe::Unsupported(reason) => {
                tracing::error!(
                    "EL RPC {}/{} ({}) rejected the 'safe' block tag: {} — this provider cannot serve fcr mode",
                    i + 1,
                    el_rpcs.len(),
                    el_rpc,
                    reason
                );
            }
            SafeProbe::Unreachable(reason) => {
                tracing::warn!(
                    "Failed to get safe block from EL RPC {}/{} ({}): {}",
                    i + 1,
                    el_rpcs.len(),
                    el_rpc,
                    reason
                );
            }
        }
    }

    if saw_legitimate_empty {
        Ok(None)
    } else {
        Err(BridgeValidatorError::AllRpcsFailedForSafeBlock)
    }
}

/// Look up the canonical block hash at a given block number.
///
/// Used by the fcr checker to re-check a safe-processed block once it has
/// finalized. Anchoring on the block **number** (not the stored hash) is what
/// makes the check meaningful: a different block occupying the same number is
/// exactly how an orphaned block manifests at the execution layer.
///
/// `Ok(None)` means every reachable provider answered `result: null` — the
/// caller must retry later rather than treat it as a verdict.
pub async fn get_canonical_block_hash(
    http_client: &reqwest::Client,
    el_rpcs: &[String],
    block_number: i64,
) -> Result<Option<String>, BridgeValidatorError> {
    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": [format!("0x{:x}", block_number), false]
    });

    let mut any_provider_answered = false;

    for (i, el_rpc) in el_rpcs.iter().enumerate() {
        let response = match http_client.post(el_rpc).json(&payload).send().await {
            Ok(response) => response,
            Err(e) => {
                tracing::warn!(
                    "Failed to get block {} from EL RPC {}/{} ({}): {}",
                    block_number,
                    i + 1,
                    el_rpcs.len(),
                    el_rpc,
                    e
                );
                continue;
            }
        };

        if !response.status().is_success() {
            tracing::warn!(
                "EL RPC {}/{} ({}) returned HTTP {} for block {}",
                i + 1,
                el_rpcs.len(),
                el_rpc,
                response.status(),
                block_number
            );
            continue;
        }

        let body: serde_json::Value = match response.json().await {
            Ok(body) => body,
            Err(e) => {
                tracing::warn!(
                    "EL RPC {}/{} ({}) returned an invalid JSON body for block {}: {}",
                    i + 1,
                    el_rpcs.len(),
                    el_rpc,
                    block_number,
                    e
                );
                continue;
            }
        };

        if let Some(error) = body.get("error") {
            if !error.is_null() {
                tracing::warn!(
                    "EL RPC {}/{} ({}) errored for block {}: {}",
                    i + 1,
                    el_rpcs.len(),
                    el_rpc,
                    block_number,
                    error
                );
                continue;
            }
        }

        // The provider answered. A null result means it doesn't have the block
        // — try the next provider before concluding "not found".
        any_provider_answered = true;
        if let Some(hash) = body["result"]["hash"].as_str() {
            return Ok(Some(hash.to_ascii_lowercase()));
        }
    }

    if any_provider_answered {
        Ok(None)
    } else {
        Err(BridgeValidatorError::AllRpcsFailedForBlockLookup(
            block_number,
        ))
    }
}

/// Per-chain result of the startup `safe`-support preflight.
#[derive(Debug, Clone)]
pub struct ChainSafeSupport {
    pub chain: String,
    /// At least one provider accepted the `safe` tag (block or legitimate empty).
    pub supported: bool,
    /// At least one provider answered at all (so `supported == false` is a real
    /// verdict rather than "the network was down at boot").
    pub reachable: bool,
    /// Providers that actively rejected the `safe` tag.
    pub unsupported_providers: Vec<String>,
}

impl ChainSafeSupport {
    /// Whether fcr mode should stay enabled for this chain.
    ///
    /// A chain is downgraded only when providers *answered* and none of them
    /// would serve `safe`. If nothing was reachable at boot we keep fcr on: an
    /// RPC outage during startup is transient and the runtime resolver already
    /// falls back to finalized per cycle.
    pub fn keeps_fcr(&self) -> bool {
        self.supported || !self.reachable
    }
}

/// Probe every provider of a chain's EL RPC array once for `safe` support.
pub async fn preflight_safe_support(
    http_client: &reqwest::Client,
    chain: &str,
    el_rpcs: &[String],
) -> ChainSafeSupport {
    let mut support = ChainSafeSupport {
        chain: chain.to_string(),
        supported: false,
        reachable: false,
        unsupported_providers: Vec::new(),
    };

    for el_rpc in el_rpcs {
        match probe_safe_block(http_client, el_rpc).await {
            SafeProbe::Block(block_number) => {
                support.supported = true;
                support.reachable = true;
                tracing::info!(
                    "[fcr-preflight:{}] {} supports the 'safe' tag (safe block {})",
                    chain,
                    el_rpc,
                    block_number
                );
            }
            SafeProbe::Empty => {
                support.supported = true;
                support.reachable = true;
                tracing::warn!(
                    "[fcr-preflight:{}] {} accepts the 'safe' tag but has no safe block yet — \
                     indexing will fall back to finalized until one appears",
                    chain,
                    el_rpc
                );
            }
            SafeProbe::Unsupported(reason) => {
                support.reachable = true;
                support.unsupported_providers.push(el_rpc.clone());
                tracing::error!(
                    "[fcr-preflight:{}] {} rejects the 'safe' block tag ({}) — misconfigured for fcr mode",
                    chain,
                    el_rpc,
                    reason
                );
            }
            SafeProbe::Unreachable(reason) => {
                tracing::warn!(
                    "[fcr-preflight:{}] {} did not answer the 'safe' probe: {}",
                    chain,
                    el_rpc,
                    reason
                );
            }
        }
    }

    support
}

/// Run the startup preflight for every chain configured in fcr mode and
/// downgrade any chain whose RPC array cannot serve `safe`.
///
/// Without this, an fcr-configured chain pointed at a `safe`-incapable RPC
/// would run on `finalized` for the whole process lifetime while the operator
/// believed they had ~12s confirmation. Surfacing it at boot is the point.
pub async fn run_fcr_preflight(config: &mut Config, http_client: &reqwest::Client) {
    let fcr_chains: Vec<(&'static str, Vec<String>)> = config
        .fcr_chains()
        .into_iter()
        .map(|(chain, el_rpcs)| (chain, el_rpcs.to_vec()))
        .collect();

    if fcr_chains.is_empty() {
        tracing::info!(
            "All chains are in '{}' mode; skipping fcr preflight",
            BlockProcessingMode::BlockFinality
        );
        return;
    }

    for (chain, el_rpcs) in fcr_chains {
        let support = preflight_safe_support(http_client, chain, &el_rpcs).await;

        if support.keeps_fcr() {
            if !support.unsupported_providers.is_empty() {
                tracing::error!(
                    "[fcr-preflight:{}] fcr mode stays enabled, but {} of {} providers reject 'safe' ({:?}) — \
                     fix them so a failover doesn't silently degrade to finality",
                    chain,
                    support.unsupported_providers.len(),
                    el_rpcs.len(),
                    support.unsupported_providers
                );
            }
            if !support.reachable {
                tracing::error!(
                    "[fcr-preflight:{}] no configured EL RPC answered the 'safe' probe — keeping fcr mode \
                     (treating this as a transient outage), indexing falls back to finalized until one responds",
                    chain
                );
            }
            tracing::info!(
                "[{}] block processing mode: {}",
                support.chain,
                BlockProcessingMode::Fcr
            );
        } else {
            tracing::error!(
                "[fcr-preflight:{}] no configured EL RPC can serve the 'safe' block tag — \
                 downgrading this chain to '{}'. Point {} at a safe-capable execution client to use fcr.",
                chain,
                BlockProcessingMode::BlockFinality,
                chain.to_ascii_uppercase() + "_RPC"
            );
            config.set_mode_for_chain(chain, BlockProcessingMode::BlockFinality);
        }
    }
}
