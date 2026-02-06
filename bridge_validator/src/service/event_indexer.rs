use crate::config::Config;
use crate::contracts::{AMB_BRIDGE, XDAI_BRIDGE};
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
use tokio::time::{sleep, Duration};
use tracing;

pub struct EventIndexer<P> {
    config: Config,
    provider: P,
    provider_name: String,
    eventName: String,
    contract_address: Address,
    db_pool: PgPool,
}

impl<P: Provider> EventIndexer<P> {
    pub fn new(
        config: Config,
        provider: P,
        provider_name: String,
        eventName: String,
        contract_address: Address,
        db_pool: PgPool,
    ) -> Self {
        Self {
            config,
            provider,
            provider_name,
            eventName,
            contract_address,
            db_pool,
        }
    }

    pub async fn start(self) {
        tracing::info!(
            "[{}-{}] Starting event listener...",
            self.provider_name,
            self.eventName
        );
        let mut last_processed_block = 0;
        loop {
            match self.poll_events(last_processed_block).await {
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
            sleep(Duration::from_secs(self.config.poll_interval_secs)).await;
        }
    }

    fn check_bridge_mode(contract_address: Address, config: &Config) -> String {
        if (contract_address == config.eth_amb_bridge_address) {
            "AMB_ETH".to_string()
        } else if (contract_address == config.gc_amb_bridge_address) {
            "AMB_GC".to_string()
        } else if (contract_address == config.eth_xdai_bridge_address) {
            "XDAI_ETH".to_string()
        } else if (contract_address == config.gc_xdai_bridge_address) {
            "XDAI_GC".to_string()
        } else {
            "UNKNOWN".to_string()
        }
    }

    async fn poll_events(
        &self,
        last_processed_block: u64,
    ) -> Result<u64, Box<dyn std::error::Error>> {
        tracing::debug!(
            "[{}-{}] Polling events...",
            self.provider_name,
            self.eventName
        );
        let latest_block = self.provider.get_block_number().await?;
        tracing::debug!(
            "[{}-{}] Latest block: {}",
            self.provider_name,
            self.eventName,
            latest_block
        );
        if latest_block <= last_processed_block {
            tracing::debug!(
                "[{}-{}] Latest block is already processed",
                self.provider_name,
                self.eventName
            );
            return Ok(last_processed_block);
        }
        let start_block = if last_processed_block == 0 {
            latest_block // Or config.start_block
        } else {
            last_processed_block + 1
        };

        let filter = Filter::new()
            .address(self.contract_address)
            .event(&self.eventName)
            .from_block(start_block);

        let logs = self.provider.get_logs(&filter).await?;
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

            // Extract the event signature (topics[0])
            if let Some(topic_key) = log.topics().get(0) {
                let topic_key_str = format!("{:?}", topic_key);

                // Serialize the entire log object to JSON
                let log_json = serde_json::to_value(&log)?;

                let bridge_mode = Self::check_bridge_mode(self.contract_address, &self.config);

                // Insert into database
                match sqlx::query(
                    r#"
                    INSERT INTO event_logs (topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed)
                    VALUES ($1, $2, $3, $4, $5, $6)
                    ON CONFLICT (topic_key, transaction_hash) DO NOTHING
                    "#
                )
                .bind(&topic_key_str)
                .bind(&bridge_mode)
                .bind(&log_json)
                .bind(log.block_number.map(|n| n as i64))
                .bind(log.transaction_hash.map(|h| format!("{:?}", h)))
                .bind("false")
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

            // Example Log output
            // Log found: Log { inner: Log { address: 0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e, data: LogData { topics: [0x482515ce3d9494a37ce83f18b72b363449458435fafdd7a53ddea7460fe01b58, 0x000500004ac82b41bd819dd871590b510316f2385cb196fb000000000002d8e6], data: 0x000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000b5000500004ac82b41bd819dd871590b510316f2385cb196fb000000000002d8e688ad09518695c6c3712ac10a214be5109a655671f6a78083ca3e2a662d6dd1703c939c8ace2e268d001e84800101000164125e4cfb0000000000000000000000006810e776880c02933d47db1b9fc05908e5386b9600000000000000000000000036c2879f055519593c28b56317950239c6ecd58b0000000000000000000000000000000000000000000000000de0b6b3a76400000000000000000000000000 } }, block_hash: Some(0x223181b0230ef914af338eb648ed05c46743c985e9651b3fb2341c587e0b5f46), block_number: Some(24226354), block_timestamp: None, transaction_hash: Some(0x3108ac7fc0101b236fd43dbacac908e87f85035a65338ce9e6851773f9574706), transaction_index: Some(0), log_index: Some(1), removed: false }
        }

        // Return the latest processed block
        Ok(latest_block)
    }
}
