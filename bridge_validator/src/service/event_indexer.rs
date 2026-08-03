use crate::config::{BlockProcessingMode, Config};
use crate::contracts::{AMB_BRIDGE, XDAI_BRIDGE};
use crate::error::BridgeValidatorError;
use alloy::{
    hex,
    primitives::{address, b256, utils::format_ether, Address, Bytes, FixedBytes},
    providers::{Provider, ProviderBuilder},
    rpc::types::{BlockNumberOrTag, Filter, TransactionReceipt},
    signers::{local::PrivateKeySigner, Signer},
    sol,
    sol_types::SolEvent,
};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::{sleep, Duration};
use tracing;

pub struct EventIndexer<P> {
    config: Config,
    provider: P,
    provider_name: String,
    eventName: String,
    contract_address: Address,
    db_pool: PgPool,
    shutdown: watch::Receiver<bool>,
    http_client: reqwest::Client,
}

impl<P: Provider> EventIndexer<P> {
    pub fn new(
        config: Config,
        provider: P,
        provider_name: String,
        eventName: String,
        contract_address: Address,
        db_pool: PgPool,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self {
            config,
            provider,
            provider_name,
            eventName,
            contract_address,
            db_pool,
            shutdown,
            http_client: reqwest::Client::new(),
        }
    }

    pub async fn start(mut self) {
        tracing::info!(
            "[{}-{}] Starting event listener...",
            self.provider_name,
            self.eventName
        );
        let mut last_processed_block = 0;
        loop {
            // Resolve this cycle's upper bound. In block-finality mode that is
            // the latest finalized block, which can't be reorged out, so a log
            // we store is guaranteed to remain part of the canonical chain. In
            // fcr mode it is the (much fresher) `safe` block, which can be
            // reorged out — `service::fcr_checker` re-checks those rows once
            // they finalize.
            match self.resolve_upper_bound().await {
                Ok(upper_bound_block) => {
                    // block numbers are non-negative; clamp defensively.
                    let upper_bound_block = upper_bound_block.max(0) as u64;
                    match self
                        .poll_events(last_processed_block, upper_bound_block)
                        .await
                    {
                        Ok(new_block) => {
                            last_processed_block = new_block;
                        }
                        Err(e) => {
                            tracing::error!(
                                "[{}-{}] Error polling events: {}",
                                self.provider_name,
                                self.eventName,
                                e
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "[{}-{}] Could not resolve the upper bound block, skipping this round: {}",
                        self.provider_name,
                        self.eventName,
                        e
                    );
                }
            }
            tokio::select! {
                _ = sleep(Duration::from_secs(self.config.poll_interval_secs)) => {}
                _ = self.shutdown.changed() => {
                    tracing::info!(
                        "[{}-{}] Shutdown signal received, stopping event indexer",
                        self.provider_name,
                        self.eventName
                    );
                    break;
                }
            }
        }
    }

    /// Index logs in the range `(last_processed_block, upper_bound_block]`.
    ///
    /// `upper_bound_block` is resolved by the caller from the chain's mode:
    /// the latest finalized block in block-finality mode, the latest `safe`
    /// block in fcr mode. The cursor advances to that bound (never to the chain
    /// tip), so blocks between it and the tip are revisited on a later round
    /// rather than being skipped.
    ///
    /// The range is split into consecutive chunks of at most
    /// `config.max_block_range` blocks, one `eth_getLogs` per chunk. A single
    /// call spanning an arbitrarily wide range is what providers reject once
    /// the validator falls far enough behind, and because a failed cycle does
    /// not advance the cursor while the upper bound keeps rising, an unchunked
    /// query only ever gets wider — the stall is permanent. Chunking also makes
    /// catch-up incremental: the cursor is returned as of the last chunk that
    /// actually succeeded, so a failure partway through keeps everything
    /// indexed before it instead of replaying the whole range next cycle.
    pub async fn poll_events(
        &self,
        last_processed_block: u64,
        upper_bound_block: u64,
    ) -> Result<u64, BridgeValidatorError> {
        tracing::debug!(
            "[{}-{}] Polling events...",
            self.provider_name,
            self.eventName
        );
        tracing::debug!(
            "[{}-{}] Upper bound block ({} mode): {}",
            self.provider_name,
            self.eventName,
            self.mode(),
            upper_bound_block
        );
        if upper_bound_block <= last_processed_block {
            tracing::debug!(
                "[{}-{}] No new blocks to process",
                self.provider_name,
                self.eventName
            );
            return Ok(last_processed_block);
        }
        let start_block = if last_processed_block == 0 {
            upper_bound_block // TODO: Or config.start_block
        } else {
            last_processed_block + 1
        };

        // `positive_u64_from_env` rejects zero, so this is always >= 1 and the
        // cursor below always advances.
        let max_block_range = self.config.max_block_range;
        let total_blocks = upper_bound_block - start_block + 1;
        if total_blocks > max_block_range {
            tracing::info!(
                "[{}-{}] Catching up on {} blocks ({}..={}) in chunks of {}",
                self.provider_name,
                self.eventName,
                total_blocks,
                start_block,
                upper_bound_block,
                max_block_range
            );
        }

        // Last block confirmed indexed. Stays below `start_block` until the
        // first chunk lands, which is how a total failure is told apart from a
        // partial one.
        let mut indexed_through = start_block.saturating_sub(1);
        let mut chunk_start = start_block;

        while chunk_start <= upper_bound_block {
            let chunk_end = chunk_start
                .saturating_add(max_block_range - 1)
                .min(upper_bound_block);

            if let Err(e) = self.index_block_range(chunk_start, chunk_end).await {
                if indexed_through >= start_block {
                    // Earlier chunks landed. Keep them: the caller advances its
                    // cursor to `indexed_through`, so the next cycle resumes at
                    // the chunk that failed rather than re-querying from the
                    // start of the range.
                    tracing::error!(
                        "[{}-{}] Failed to index blocks {}..={} ({}); keeping progress through block {}",
                        self.provider_name,
                        self.eventName,
                        chunk_start,
                        chunk_end,
                        e,
                        indexed_through
                    );
                    return Ok(indexed_through);
                }
                return Err(e);
            }

            indexed_through = chunk_end;
            if chunk_end == u64::MAX {
                break;
            }
            chunk_start = chunk_end + 1;
        }

        Ok(indexed_through)
    }

    /// Fetch and persist every matching log in the inclusive range
    /// `[from_block, to_block]`. One `eth_getLogs` call; the caller is
    /// responsible for keeping the span within `config.max_block_range`.
    async fn index_block_range(
        &self,
        from_block: u64,
        to_block: u64,
    ) -> Result<(), BridgeValidatorError> {
        tracing::debug!(
            "[{}-{}] Fetching logs for blocks {}..={}",
            self.provider_name,
            self.eventName,
            from_block,
            to_block
        );

        let filter = Filter::new()
            .address(self.contract_address)
            .event(&self.eventName)
            .from_block(from_block)
            .to_block(to_block);

        let logs = self
            .provider
            .get_logs(&filter)
            .await
            .map_err(|e| BridgeValidatorError::Rpc(e.to_string()))?;
        for log in logs.iter() {
            tracing::debug!(
                "[{}-{}] Log found: {log:?}",
                self.provider_name,
                self.eventName
            );
            tracing::info!(
                "[{}-{}] Log found in tx: {:?}",
                self.provider_name,
                self.eventName,
                log.transaction_hash
            );

            // log_index uniquely identifies a log within a block. Mined logs
            // always carry one; a missing value means the log isn't finalized,
            // so skip it rather than insert a NULL that would defeat the
            // (transaction_hash, log_index) unique constraint.
            let log_index = match log.log_index {
                Some(idx) => idx as i64,
                None => {
                    tracing::warn!(
                        "[{}-{}] Log has no log_index, skipping database storage: tx {:?}",
                        self.provider_name,
                        self.eventName,
                        log.transaction_hash
                    );
                    continue;
                }
            };

            if let Some(topic_key) = log.topics().get(0) {
                let topic_key_str = format!("{:?}", topic_key);

                let log_json = serde_json::to_value(&log)?;

                let bridge_mode = Self::check_bridge_mode(self.contract_address, &self.config);

                // In fcr mode the row is indexed from a `safe` block that can
                // still be reorged out, so it enters the revalidation
                // lifecycle. block-finality rows leave fcr_status NULL —
                // they're final by construction and there is nothing to check.
                let fcr_status = match self.mode() {
                    BlockProcessingMode::Fcr => Some("pending"),
                    BlockProcessingMode::BlockFinality => None,
                };

                match sqlx::query(
                    r#"
                    INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, block_hash, transaction_hash, log_index, is_processed, fcr_status)
                    VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                    ON CONFLICT (transaction_hash, log_index) DO NOTHING
                    "#
                )
                .bind(&topic_key_str)
                .bind(&bridge_mode)
                .bind(&log_json)
                .bind(log.block_number.map(|n| n as i64))
                .bind(log.block_hash.map(|h| format!("{:?}", h)))
                .bind(log.transaction_hash.map(|h| format!("{:?}", h)))
                .bind(log_index)
                .bind("false")
                .bind(fcr_status)
                .execute(&self.db_pool)
                .await {
                    Ok(_) => tracing::debug!("[{}-{}] Stored log with topic key: {} ", self.provider_name, self.eventName, topic_key_str),
                    Err(e) => tracing::error!("[{}-{}] Failed to store log: {}", self.provider_name, self.eventName, e),
                }
            } else {
                tracing::warn!(
                    "[{}-{}] Log has no topics[0], skipping database storage",
                    self.provider_name,
                    self.eventName
                );
            }
        }

        Ok(())
    }

    /// Pick the finality source (beacon RPC + EL RPC fallbacks) for the chain
    /// this indexer watches, derived from the contract's bridge mode. ETH-side
    /// bridges finalize against the Ethereum endpoints; GC-side against Gnosis.
    fn finality_rpcs(&self) -> (Option<&str>, &[String]) {
        self.config.finality_rpcs_for_chain(self.chain())
    }

    /// Resolve the highest block this cycle may index up to, per the chain's
    /// configured mode.
    ///
    /// In fcr mode a missing `safe` block is **not** fatal: it falls back to
    /// `finalized` (the fresh-start guard) so the indexer keeps making progress
    /// conservatively. That fallback is logged loudly — a chain configured for
    /// fcr that silently runs on finality would give operators ~12.8m latency
    /// while they believe they have ~12s.
    pub async fn resolve_upper_bound(&self) -> Result<i64, BridgeValidatorError> {
        let (bc_rpc, el_rpcs) = self.finality_rpcs();

        if self.mode().is_fcr() {
            match crate::service::safe::get_safe_block_number(&self.http_client, el_rpcs).await {
                Ok(Some(safe_block)) => return Ok(safe_block),
                Ok(None) => tracing::warn!(
                    "[{}-{}] Chain '{}' is in fcr mode but no safe block is available yet; \
                     falling back to the finalized block this cycle",
                    self.provider_name,
                    self.eventName,
                    self.chain()
                ),
                Err(e) => tracing::error!(
                    "[{}-{}] Chain '{}' is in fcr mode but the safe block could not be resolved ({}); \
                     falling back to the finalized block this cycle",
                    self.provider_name,
                    self.eventName,
                    self.chain(),
                    e
                ),
            }
        }

        crate::service::finality::get_finalized_block_number(&self.http_client, bc_rpc, el_rpcs)
            .await
    }
}

// Outside the Provider-bound impl so these can be tested without a provider.
impl<P> EventIndexer<P> {
    /// Which chain this indexer watches (`"eth"` / `"gc"`), derived from the
    /// contract's bridge mode. Both bridges on a side share the chain's
    /// block processing mode.
    pub fn chain(&self) -> &'static str {
        match Self::check_bridge_mode(self.contract_address, &self.config).as_str() {
            "AMB_ETH" | "XDAI_ETH" => "eth",
            _ => "gc",
        }
    }

    /// The configured block processing mode for this indexer's chain.
    pub fn mode(&self) -> BlockProcessingMode {
        self.config.mode_for_chain(self.chain())
    }

    pub fn check_bridge_mode(contract_address: Address, config: &Config) -> String {
        if contract_address == config.eth_amb_bridge_address {
            "AMB_ETH".to_string()
        } else if contract_address == config.gc_amb_bridge_address {
            "AMB_GC".to_string()
        } else if contract_address == config.eth_xdai_bridge_address {
            "XDAI_ETH".to_string()
        } else if contract_address == config.gc_xdai_bridge_address {
            "XDAI_GC".to_string()
        } else {
            "UNKNOWN".to_string()
        }
    }
}
