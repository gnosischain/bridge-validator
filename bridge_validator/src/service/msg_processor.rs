use crate::config::Config;
use crate::error::BridgeValidatorError;
use alloy::primitives::{Address, Bytes};
use alloy::sol_types::SolEvent;
use alloy_primitives::Log;
use reqwest;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::mpsc::Sender;
use tokio::sync::watch;
use tracing;

use crate::contracts::{
    AmbEthCalldata, AmbGcCalldata, OnChainCallData, XdaiEthCalldata, XdaiGcCalldata, AMB_BRIDGE,
    XDAI_BRIDGE,
};
use alloy::primitives::{FixedBytes, U256};
use alloy::{
    hex,
    signers::{local::PrivateKeySigner, Signer},
};

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

#[derive(Debug, Deserialize, Serialize)]
pub struct EventLogRow {
    pub id: i32,
    pub topic_key: String,
    pub bridge_mode: String,
    pub log_data: serde_json::Value,
    pub block_number: Option<i64>,
    pub transaction_hash: Option<String>,
    pub is_processed: Option<String>,
    pub retry_count: Option<i32>,
    pub stage: Option<String>,
}

pub struct SenderData {
    pub on_chain_calldata: OnChainCallData,
    pub event_log_id: i32,
    pub stage: String,
}
pub struct MessageProcessor {
    config: Config,
    db_pool: PgPool,
    tokio_sender: Sender<SenderData>,
    http_client: reqwest::Client,
    shutdown: watch::Receiver<bool>,
}

impl MessageProcessor {
    pub fn new(
        config: Config,
        db_pool: PgPool,
        tokio_sender: Sender<SenderData>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        Self {
            config,
            db_pool,
            tokio_sender,
            http_client: reqwest::Client::new(),
            shutdown,
        }
    }

    pub async fn start(mut self) {
        tracing::info!("starting message sender");

        loop {
            if *self.shutdown.borrow() {
                tracing::info!("Shutdown signal received, stopping message processor");
                break;
            }

            match self.read_from_db().await {
                Ok(Some(event_log)) => {
                    // Received event log data that needs to be processed
                    tracing::debug!(
                        "Processing event log ID: {}, Topic: {}, Bridge Mode: {}, Origin Tx: {:?}",
                        event_log.id,
                        event_log.topic_key,
                        event_log.bridge_mode,
                        event_log.transaction_hash,
                    );

                    tracing::debug!(
                        "Processing event, Bridge Mode: {}, Origin Tx: {:?}",
                        event_log.bridge_mode,
                        event_log.transaction_hash,
                    );

                    // Call process_message_or_skip and pass the event log data as function argument
                    if let Err(e) = self.process_message_or_skip(&event_log).await {
                        tracing::error!("Error in process_message_or_skip: {}", e);
                    }
                }
                Ok(None) => {
                    // No unprocessed logs found, wait before checking again
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                        _ = self.shutdown.changed() => {
                            tracing::info!("Shutdown signal received, stopping message processor");
                            break;
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Error reading database: {}", e);
                    tokio::select! {
                        _ = tokio::time::sleep(tokio::time::Duration::from_secs(5)) => {}
                        _ = self.shutdown.changed() => {
                            tracing::info!("Shutdown signal received, stopping message processor");
                            break;
                        }
                    }
                }
            }
        }
    }

    pub async fn process_message_or_skip(
        &self,
        event_log: &EventLogRow,
    ) -> Result<(), BridgeValidatorError> {
        // Deserialize the log_data JSON back into a Log object
        let log: Log = serde_json::from_value(event_log.log_data.clone())?;

        match event_log.bridge_mode.as_str() {
            "AMB_ETH" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_eth_bc_rpc(), &self.config.eth_rpc)
                        .await?
                    {
                        tracing::info!("Block {} not finalized yet, skipping", block_num);
                        self.write_is_processed_to_false(event_log.id).await?;
                        return Ok(());
                    }
                }

                let decoded = AMB_BRIDGE::UserRequestForAffirmation::decode_log(&log)?;

                self.tokio_sender
                    .send(SenderData {
                        on_chain_calldata: OnChainCallData::AmbEth {
                            contract_address: self.config.gc_amb_bridge_address,
                            calldata: AmbEthCalldata {
                                message: decoded.encodedData.clone(),
                            },
                        },
                        event_log_id: event_log.id,
                        stage: event_log.stage.clone().unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            "AMB_GC" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_gc_bc_rpc(), &self.config.gc_rpc)
                        .await?
                    {
                        tracing::info!("Block {} not finalized yet, skipping", block_num);
                        self.write_is_processed_to_false(event_log.id).await?;
                        return Ok(());
                    }
                }

                let decoded = AMB_BRIDGE::UserRequestForSignature::decode_log(&log)?;

                // sign
                let priv_key_str = self
                    .config
                    .amb_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("AMB_VALIDATOR_PRIV_KEY"))?;
                let pk_signer: PrivateKeySigner = priv_key_str
                    .parse()
                    .map_err(|e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    })?;

                let signature = pk_signer
                    .sign_message(&decoded.encodedData.clone())
                    .await
                    .map_err(|e| BridgeValidatorError::Sign(e.to_string()))?;

                self.tokio_sender
                    .send(SenderData {
                        on_chain_calldata: OnChainCallData::AmbGc {
                            contract_address: self.config.gc_amb_bridge_address,
                            calldata: AmbGcCalldata {
                                message: decoded.encodedData.clone(),
                                signature: Bytes::copy_from_slice(&signature.as_bytes()),
                            },
                        },
                        event_log_id: event_log.id,
                        stage: event_log.stage.clone().unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            "XDAI_ETH" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_eth_bc_rpc(), &self.config.eth_rpc)
                        .await?
                    {
                        tracing::info!("Block {} not finalized yet, skipping", block_num);
                        self.write_is_processed_to_false(event_log.id).await?;
                        return Ok(());
                    }
                }

                let decoded = XDAI_BRIDGE::UserRequestForAffirmation::decode_log(&log)?;

                // Call safeExecuteSignaturesWithAutoGasLimit
                self.tokio_sender
                    .send(SenderData {
                        on_chain_calldata: OnChainCallData::XdaiEth {
                            contract_address: self.config.gc_xdai_bridge_address,
                            calldata: XdaiEthCalldata {
                                recipient: decoded.recipient.clone(),
                                value: decoded.value.clone(),
                                nonce: decoded.nonce.clone(),
                            },
                        },
                        event_log_id: event_log.id,
                        stage: event_log.stage.clone().unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            "XDAI_GC" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_gc_bc_rpc(), &self.config.gc_rpc)
                        .await?
                    {
                        tracing::info!("Block {} not finalized yet, skipping", block_num);
                        self.write_is_processed_to_false(event_log.id).await?;
                        return Ok(());
                    }
                }

                let decoded = XDAI_BRIDGE::UserRequestForSignature::decode_log(&log)?;
                // message: recipient + value + nonce + bridge_address(foreign_xdai_bridge) + token_address(depends)
                let xdai_message = self.create_xdai_message(
                    decoded.recipient.clone(),
                    decoded.value.clone(),
                    decoded.nonce.clone(),
                    decoded.token.clone(),
                )?;

                // Sign the xdai_message
                let priv_key_str = self
                    .config
                    .xdai_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("XDAI_VALIDATOR_PRIV_KEY"))?;

                let pk_signer: PrivateKeySigner = priv_key_str
                    .parse()
                    .map_err(|e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    })?;

                // Decode hex string (strip 0x prefix) to bytes for signing
                let message_bytes = hex::decode(&xdai_message[2..])?;

                let signature: alloy_primitives::Signature = pk_signer
                    .sign_message(&message_bytes)
                    .await
                    .map_err(|e| BridgeValidatorError::Sign(e.to_string()))?;
                tracing::debug!("Signature: 0x{}", hex::encode(&signature.as_bytes()));

                // Decode the hex string to actual bytes before sending
                self.tokio_sender
                    .send(SenderData {
                        on_chain_calldata: OnChainCallData::XdaiGc {
                            contract_address: self.config.gc_xdai_bridge_address,
                            calldata: XdaiGcCalldata {
                                message: Bytes::copy_from_slice(&message_bytes),
                                signature: Bytes::copy_from_slice(&signature.as_bytes()),
                            },
                        },
                        event_log_id: event_log.id,
                        stage: event_log.stage.clone().unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            _ => {
                tracing::warn!("Unknown bridge mode: {}", event_log.bridge_mode);
            }
        }

        Ok(())
    }

    pub async fn check_block_finality(
        &self,
        block_number: i64,
        bc_rpc: Option<&str>,
        el_rpcs: &[String],
    ) -> Result<bool, BridgeValidatorError> {
        let finalized_block_number = self.get_finalized_block_number(bc_rpc, el_rpcs).await?;
        Ok(block_number <= finalized_block_number)
    }

    pub fn create_xdai_message(
        &self,
        recipient: Address,
        value: U256,
        nonce: FixedBytes<32>,
        token_address: Address,
    ) -> Result<String, BridgeValidatorError> {
        let bridge_address = self.config.eth_xdai_bridge_address;

        let recipient_hex = hex::encode(recipient.as_slice());
        if recipient_hex.len() != 40 {
            return Err(BridgeValidatorError::InvalidFieldLength {
                field: "recipient",
                expected: 20,
                actual: recipient_hex.len() / 2,
            });
        }

        let value_bytes = value.to_be_bytes::<32>();
        let value_hex = hex::encode(value_bytes);
        if value_hex.len() != 64 {
            return Err(BridgeValidatorError::InvalidFieldLength {
                field: "value",
                expected: 32,
                actual: value_hex.len() / 2,
            });
        }

        let nonce_hex = hex::encode(nonce.as_slice());
        if nonce_hex.len() != 64 {
            return Err(BridgeValidatorError::InvalidFieldLength {
                field: "nonce",
                expected: 32,
                actual: nonce_hex.len() / 2,
            });
        }

        let bridge_address_hex = hex::encode(bridge_address.as_slice());
        if bridge_address_hex.len() != 40 {
            return Err(BridgeValidatorError::InvalidFieldLength {
                field: "bridge_address",
                expected: 20,
                actual: bridge_address_hex.len() / 2,
            });
        }

        let token_address_hex = hex::encode(token_address.as_slice());
        if token_address_hex.len() != 40 {
            return Err(BridgeValidatorError::InvalidFieldLength {
                field: "token_address",
                expected: 20,
                actual: token_address_hex.len() / 2,
            });
        }

        let message = format!(
            "0x{}{}{}{}{}",
            recipient_hex, value_hex, nonce_hex, bridge_address_hex, token_address_hex
        );

        if message.len() != 250 {
            return Err(BridgeValidatorError::InvalidFieldLength {
                field: "message",
                expected: 124,
                actual: (message.len() - 2) / 2,
            });
        }

        Ok(message)
    }
    pub async fn read_from_db(&self) -> Result<Option<EventLogRow>, BridgeValidatorError> {
        // Read the first row from database where is_processed is 'false'
        // Set it to 'true' and return the row data

        // Start a transaction for atomic read and update
        let mut tx = self.db_pool.begin().await?;

        // FOR UPDATE SKIP LOCKED
        // 1. FOR UPDATE - Locks the selected row(s) within the transaction, preventing other transactions from reading or modifying them
        // 2. SKIP LOCKED - If a row is already locked by another transaction, skip it and move to the next available row
        // Query for the first unprocessed log with the smallest block_number
        let row = sqlx::query_as!(
            EventLogRow,
            r#"
            SELECT id, topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count, stage
            FROM event_logs
            WHERE is_processed = 'false' AND retry_count < 5
            ORDER BY block_number ASC
            LIMIT 1
            FOR UPDATE SKIP LOCKED
            "#
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(ref log_row) = row {
            // Mark this row as processed
            sqlx::query(
                r#"
                UPDATE event_logs
                SET is_processed = 'true'
                WHERE id = $1
                "#,
            )
            .bind(log_row.id)
            .execute(&mut *tx)
            .await?;

            // TODO: use transaction_hash_src_chain as unique id
            tracing::debug!("Marked log {} as processed", log_row.id);
        }

        // Commit the transaction
        tx.commit().await?;

        Ok(row)
    }

    pub async fn write_is_processed_to_false(&self, id: i32) -> Result<(), BridgeValidatorError> {
        // write is_processed to false
        sqlx::query(
            r#"
            UPDATE event_logs
            SET is_processed = 'false'
            WHERE id = $1
            "#,
        )
        .bind(id)
        .execute(&self.db_pool)
        .await?;

        tracing::info!("Set is_processed to false for event_log id: {}", id);
        Ok(())
    }

    async fn get_finalized_block_number(
        &self,
        bc_rpc: Option<&str>,
        el_rpcs: &[String],
    ) -> Result<i64, BridgeValidatorError> {
        // Try beacon chain RPC first, if configured
        if let Some(bc_rpc) = bc_rpc {
            match self.get_finalized_block_from_beacon(bc_rpc).await {
                Ok(block_number) => return Ok(block_number),
                Err(e) => {
                    tracing::warn!(
                        "Beacon chain RPC failed: {}, falling back to EL RPCs",
                        e
                    );
                }
            }
        }

        // Fallback: try EL RPCs with eth_getBlockByNumber
        for (i, el_rpc) in el_rpcs.iter().enumerate() {
            tracing::info!(
                "Trying EL RPC {}/{}: {}",
                i + 1,
                el_rpcs.len(),
                el_rpc
            );
            match self.get_finalized_block_from_el(el_rpc).await {
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
        &self,
        bc_rpc: &str,
    ) -> Result<i64, BridgeValidatorError> {
        let endpoint = format!("{}/eth/v1/beacon/blocks/finalized", bc_rpc);

        let response = self
            .http_client
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
        &self,
        el_rpc: &str,
    ) -> Result<i64, BridgeValidatorError> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_getBlockByNumber",
            "params": ["finalized", false]
        });

        let response = self
            .http_client
            .post(el_rpc)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(BridgeValidatorError::ElHttpStatus(response.status()));
        }

        let body: serde_json::Value = response.json().await?;

        let hex_block_number = body["result"]["number"]
            .as_str()
            .ok_or(BridgeValidatorError::EmptyElResponse)?;

        let block_number =
            i64::from_str_radix(hex_block_number.trim_start_matches("0x"), 16)?;
        Ok(block_number)
    }
}
