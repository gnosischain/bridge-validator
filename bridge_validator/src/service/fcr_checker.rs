//! Revalidation of safe-processed blocks once they finalize.
//!
//! FCR mode deliberately opens a reorg window: the indexer stores (and the
//! message processor signs) logs from `safe` blocks, which are fast-confirmed
//! but not finalized. A signature cannot be un-signed, so this task
//! does not try to undo anything — it closes the loop by *detecting* the one
//! real FCR failure mode, a **false confirmation**: a block marked safe that
//! later turns out not to be on the canonical chain.
//!
//! The check anchors on the block **number** and compares hashes, which is the
//! execution-layer form of the "was this slot orphaned?" question. bridge
//! validator works entirely in EL block numbers, and EL numbers are contiguous,
//! so an orphaned block shows up as *a different block occupying the same
//! number* rather than as a gap. Never compare a stored EL hash against a
//! beacon block root.

use crate::config::Config;
use crate::error::BridgeValidatorError;
use crate::service::{finality, safe};
use sqlx::PgPool;
use tokio::sync::watch;
use tokio::time::{sleep, Duration};

/// Warn when a chain has more than this many rows still awaiting
/// revalidation. A growing backlog means the checker is not keeping up with
/// the indexer (or finality has stalled), and unverified signed messages are
/// piling up.
const PENDING_BACKLOG_WARN_THRESHOLD: i64 = 100;

// How often a revalidation cycle runs is `Config::fcr_check_interval_secs`
// (`FCR_CHECK_INTERVAL_SECS`, default 30s). Finality advances about every 6.4
// minutes, so polling much faster than that only burns RPC calls on blocks that
// cannot possibly have finalized yet — but the cadence is configurable because
// test harnesses drive finality on demand and would otherwise spend most of
// their wall-clock waiting out a production interval.

/// One safe-processed block awaiting revalidation.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingBlock {
    block_number: i64,
    stored_block_hash: String,
}

/// What revalidating a single pending block concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockVerdict {
    /// Canonical hash at that number matches what we stored.
    Confirmed,
    /// A different block occupies that number — the safe block was reorged out.
    Reverted { canonical_block_hash: String },
    /// No provider could produce the block yet. Retry next cycle; never prune,
    /// because dropping a row would read as "verified".
    Unavailable,
}

pub struct FcrChecker {
    config: Config,
    db_pool: PgPool,
    shutdown: watch::Receiver<bool>,
    http_client: reqwest::Client,
}

impl FcrChecker {
    pub fn new(config: Config, db_pool: PgPool, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            config,
            db_pool,
            shutdown,
            http_client: reqwest::Client::new(),
        }
    }

    pub async fn start(mut self) {
        // Own the chain list up front so the loop doesn't hold a borrow of
        // self.config across the shutdown wait.
        let fcr_chains: Vec<(&'static str, Vec<String>)> = self
            .config
            .fcr_chains()
            .into_iter()
            .map(|(chain, el_rpcs)| (chain, el_rpcs.to_vec()))
            .collect();

        if fcr_chains.is_empty() {
            tracing::info!(
                "[fcr-checker] No chain is in fcr mode, revalidation not required; exiting"
            );
            return;
        }

        let check_interval = Duration::from_secs(self.config.fcr_check_interval_secs);

        tracing::info!(
            "[fcr-checker] Starting revalidation for chains: {:?} (every {}s)",
            fcr_chains.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            self.config.fcr_check_interval_secs
        );

        loop {
            for (chain, el_rpcs) in &fcr_chains {
                if let Err(e) = self.check_chain(chain, el_rpcs).await {
                    tracing::error!("[fcr-checker:{}] Revalidation cycle failed: {}", chain, e);
                }
            }

            tokio::select! {
                _ = sleep(check_interval) => {}
                _ = self.shutdown.changed() => {
                    tracing::info!("[fcr-checker] Shutdown signal received, stopping");
                    break;
                }
            }
        }
    }

    /// Revalidate every pending block of one chain that has now finalized.
    pub async fn check_chain(
        &self,
        chain: &str,
        el_rpcs: &[String],
    ) -> Result<(), BridgeValidatorError> {
        let (bc_rpc, finality_el_rpcs) = self.config.finality_rpcs_for_chain(chain);
        let finalized_block =
            finality::get_finalized_block_number(&self.http_client, bc_rpc, finality_el_rpcs)
                .await?;

        self.report_backlog(chain).await?;

        let pending = self.pending_blocks(chain, finalized_block).await?;
        if pending.is_empty() {
            tracing::debug!(
                "[fcr-checker:{}] No pending blocks at or below finalized block {}",
                chain,
                finalized_block
            );
            return Ok(());
        }

        tracing::info!(
            "[fcr-checker:{}] Revalidating {} safe-processed block(s) up to finalized block {}",
            chain,
            pending.len(),
            finalized_block
        );

        for block in pending {
            let verdict = self.verdict_for(el_rpcs, &block).await?;
            match verdict {
                BlockVerdict::Confirmed => {
                    let updated = self.mark_confirmed(chain, &block).await?;
                    tracing::debug!(
                        "[fcr-checker:{}] Block {} ({}) survived finalization; {} row(s) confirmed",
                        chain,
                        block.block_number,
                        block.stored_block_hash,
                        updated
                    );
                }
                BlockVerdict::Reverted {
                    canonical_block_hash,
                } => {
                    self.record_false_positive(
                        chain,
                        &block,
                        Some(canonical_block_hash.as_str()),
                        finalized_block,
                    )
                    .await?;
                }
                BlockVerdict::Unavailable => {
                    // Never prune: a dropped row would be indistinguishable
                    // from a verified one.
                    tracing::warn!(
                        "[fcr-checker:{}] Block {} could not be fetched from any provider; \
                         leaving it pending for the next cycle",
                        chain,
                        block.block_number
                    );
                }
            }
        }

        Ok(())
    }

    /// Compare the canonical chain's block at this number against what we
    /// stored when the block was merely safe.
    async fn verdict_for(
        &self,
        el_rpcs: &[String],
        block: &PendingBlock,
    ) -> Result<BlockVerdict, BridgeValidatorError> {
        let canonical =
            match safe::get_canonical_block_hash(&self.http_client, el_rpcs, block.block_number)
                .await
            {
                Ok(Some(hash)) => hash,
                Ok(None) => return Ok(BlockVerdict::Unavailable),
                Err(e) => {
                    tracing::warn!(
                        "[fcr-checker] Block {} lookup failed on every provider: {}",
                        block.block_number,
                        e
                    );
                    return Ok(BlockVerdict::Unavailable);
                }
            };

        if canonical.eq_ignore_ascii_case(&block.stored_block_hash) {
            Ok(BlockVerdict::Confirmed)
        } else {
            Ok(BlockVerdict::Reverted {
                canonical_block_hash: canonical,
            })
        }
    }

    /// Distinct `(block_number, block_hash)` pairs still awaiting
    /// revalidation on this chain, bounded by the finalized block.
    ///
    /// Grouping means one RPC call per block rather than one per event.
    async fn pending_blocks(
        &self,
        chain: &str,
        finalized_block: i64,
    ) -> Result<Vec<PendingBlock>, BridgeValidatorError> {
        let bridge_modes = Self::bridge_modes(chain);

        let rows: Vec<(i64, String)> = sqlx::query_as(
            r#"
            SELECT DISTINCT block_number, block_hash
            FROM event_logs
            WHERE bridge_mode = ANY($1)
              AND fcr_status = 'pending'
              AND block_number IS NOT NULL
              AND block_hash IS NOT NULL
              AND block_number <= $2
            ORDER BY block_number ASC
            "#,
        )
        .bind(&bridge_modes)
        .bind(finalized_block)
        .fetch_all(&self.db_pool)
        .await?;

        // Pending rows with no stored hash can't be revalidated at all. The
        // indexer always records one in fcr mode, so this is a bug rather than
        // a normal state — say so loudly instead of silently confirming them.
        let unverifiable: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM event_logs
            WHERE bridge_mode = ANY($1)
              AND fcr_status = 'pending'
              AND block_hash IS NULL
              AND block_number <= $2
            "#,
        )
        .bind(&bridge_modes)
        .bind(finalized_block)
        .fetch_one(&self.db_pool)
        .await?;

        if unverifiable > 0 {
            tracing::error!(
                "[fcr-checker:{}] {} finalized pending row(s) have no stored block_hash and cannot \
                 be revalidated — they stay pending; investigate the indexer",
                chain,
                unverifiable
            );
        }

        Ok(rows
            .into_iter()
            .map(|(block_number, stored_block_hash)| PendingBlock {
                block_number,
                stored_block_hash,
            })
            .collect())
    }

    async fn report_backlog(&self, chain: &str) -> Result<(), BridgeValidatorError> {
        let backlog: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*)
            FROM event_logs
            WHERE bridge_mode = ANY($1) AND fcr_status = 'pending'
            "#,
        )
        .bind(&Self::bridge_modes(chain))
        .fetch_one(&self.db_pool)
        .await?;

        if backlog > PENDING_BACKLOG_WARN_THRESHOLD {
            tracing::warn!(
                "[fcr-checker:{}] {} rows are awaiting revalidation (threshold {}) — \
                 signed messages are outrunning finality",
                chain,
                backlog,
                PENDING_BACKLOG_WARN_THRESHOLD
            );
        }

        Ok(())
    }

    async fn mark_confirmed(
        &self,
        chain: &str,
        block: &PendingBlock,
    ) -> Result<u64, BridgeValidatorError> {
        let result = sqlx::query(
            r#"
            UPDATE event_logs
            SET fcr_status = 'confirmed'
            WHERE bridge_mode = ANY($1)
              AND fcr_status = 'pending'
              AND block_number = $2
              AND block_hash = $3
            "#,
        )
        .bind(&Self::bridge_modes(chain))
        .bind(block.block_number)
        .bind(&block.stored_block_hash)
        .execute(&self.db_pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Record a false positive: the safe block we indexed (and signed off) is
    /// not the block that finalized at that number.
    ///
    /// The audit rows and the status update go in one transaction so an alert
    /// can never be lost while the rows are quietly marked resolved.
    async fn record_false_positive(
        &self,
        chain: &str,
        block: &PendingBlock,
        canonical_block_hash: Option<&str>,
        finalized_block: i64,
    ) -> Result<(), BridgeValidatorError> {
        let bridge_modes = Self::bridge_modes(chain);
        let mut tx = self.db_pool.begin().await?;

        let affected: Vec<(i32, Option<String>, Option<i64>)> = sqlx::query_as(
            r#"
            SELECT id, transaction_hash, log_index
            FROM event_logs
            WHERE bridge_mode = ANY($1)
              AND fcr_status = 'pending'
              AND block_number = $2
              AND block_hash = $3
            FOR UPDATE
            "#,
        )
        .bind(&bridge_modes)
        .bind(block.block_number)
        .bind(&block.stored_block_hash)
        .fetch_all(&mut *tx)
        .await?;

        tracing::error!(
            "[fcr-checker:{}] FCR FALSE POSITIVE: block {} was processed as safe with hash {} but \
             the canonical block at that number is {:?} (detected at finalized block {}). \
             {} already-signed event(s) affected — nothing is undone on-chain.",
            chain,
            block.block_number,
            block.stored_block_hash,
            canonical_block_hash,
            finalized_block,
            affected.len()
        );

        for (event_log_id, transaction_hash, log_index) in &affected {
            tracing::error!(
                "[fcr-checker:{}] Affected event log id={} tx={:?} log_index={:?} (block {})",
                chain,
                event_log_id,
                transaction_hash,
                log_index,
                block.block_number
            );

            sqlx::query(
                r#"
                INSERT INTO fcr_false_positives
                    (chain, block_number, stored_block_hash, canonical_block_hash,
                     transaction_hash, log_index, event_log_id, detected_at_finalized)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                "#,
            )
            .bind(chain)
            .bind(block.block_number)
            .bind(&block.stored_block_hash)
            .bind(canonical_block_hash)
            .bind(transaction_hash)
            .bind(log_index)
            .bind(event_log_id)
            .bind(finalized_block)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            r#"
            UPDATE event_logs
            SET fcr_status = 'reverted'
            WHERE bridge_mode = ANY($1)
              AND fcr_status = 'pending'
              AND block_number = $2
              AND block_hash = $3
            "#,
        )
        .bind(&bridge_modes)
        .bind(block.block_number)
        .bind(&block.stored_block_hash)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    fn bridge_modes(chain: &str) -> Vec<String> {
        Config::bridge_modes_for_chain(chain)
            .iter()
            .map(|mode| mode.to_string())
            .collect()
    }
}
