use crate::config::Config;
use alloy::primitives::{Address, Bytes};
use alloy::sol_types::SolEvent;
use alloy_primitives::Log;
use reqwest;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::error::Error;
use tokio::sync::mpsc::Sender;
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
}

pub struct SenderData {
    pub on_chain_calldata: OnChainCallData,
    pub event_log_id: i32,
}
pub struct MessageProcessor {
    config: Config,
    db_pool: PgPool,
    tokio_sender: Sender<SenderData>,
}

impl MessageProcessor {
    pub fn new(config: Config, db_pool: PgPool, tokio_sender: Sender<SenderData>) -> Self {
        Self {
            config,
            db_pool,
            tokio_sender,
        }
    }

    pub async fn start(self) {
        tracing::info!("starting message sender");

        loop {
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
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
                Err(e) => {
                    tracing::error!("Error reading database: {}", e);
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    pub async fn process_message_or_skip(
        &self,
        event_log: &EventLogRow,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Deserialize the log_data JSON back into a Log object
        let log: Log = serde_json::from_value(event_log.log_data.clone())?;

        match event_log.bridge_mode.as_str() {
            "AMB_ETH" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_eth_bc_rpc().to_string())
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
                    })
                    .await?;
            }
            "AMB_GC" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_gc_bc_rpc().to_string())
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
                    .ok_or("AMB_VALIDATOR_PRIV_KEY must be set in .env")?;
                let pk_signer: PrivateKeySigner = priv_key_str
                    .parse()
                    .map_err(|e| format!("Failed to parse AMB private key: {}", e))?;

                let signature = pk_signer
                    .sign_message(&decoded.encodedData.clone())
                    .await
                    .map_err(|e| format!("Failed to sign AMB message: {}", e))?;

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
                    })
                    .await?;
            }
            "XDAI_ETH" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_eth_bc_rpc().to_string())
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
                    })
                    .await?;
            }
            "XDAI_GC" => {
                if let Some(block_num) = event_log.block_number {
                    if !self
                        .check_block_finality(block_num, self.config.get_gc_bc_rpc().to_string())
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
                );

                // Sign the xdai_message
                let priv_key_str = self
                    .config
                    .xdai_validator_private_key
                    .as_ref()
                    .ok_or("XDAI_VALIDATOR_PRIV_KEY must be set in .env")?;

                let pk_signer: PrivateKeySigner = priv_key_str
                    .parse()
                    .map_err(|e| format!("Failed to parse XDAI private key: {}", e))?;

                // Decode hex string (strip 0x prefix) to bytes for signing
                let message_bytes = hex::decode(&xdai_message[2..])
                    .map_err(|e| format!("Failed to decode xdai message hex: {}", e))?;

                let signature: alloy_primitives::Signature = pk_signer
                    .sign_message(&message_bytes)
                    .await
                    .map_err(|e| format!("Failed to sign XDAI message: {}", e))?;
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
                    })
                    .await?;
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
        bc_rpc: String,
    ) -> Result<bool, Box<dyn Error>> {
        let last_finalized_block = Self::get_finalized_block(bc_rpc).await?;
        // Only process finalized block
        if block_number
            <= last_finalized_block
                .data
                .message
                .body
                .execution_payload
                .block_number
        {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn create_xdai_message(
        &self,
        recipient: Address,
        value: U256,
        nonce: FixedBytes<32>,
        token_address: Address,
    ) -> String {
        // Get bridge address (the foreign bridge contract address)
        let bridge_address = self.config.eth_xdai_bridge_address;

        // Convert each component to hex string (without 0x prefix)
        let recipient_hex = hex::encode(recipient.as_slice());
        assert_eq!(recipient_hex.len(), 40, "Recipient should be 20 bytes");

        // Convert U256 to 32-byte array (big-endian)
        let value_bytes = value.to_be_bytes::<32>();
        let value_hex = hex::encode(value_bytes);
        assert_eq!(value_hex.len(), 64, "Value should be 32 bytes");

        let nonce_hex = hex::encode(nonce.as_slice());
        assert_eq!(nonce_hex.len(), 64, "Nonce should be 32 bytes");

        let bridge_address_hex = hex::encode(bridge_address.as_slice());
        assert_eq!(
            bridge_address_hex.len(),
            40,
            "Bridge address should be 20 bytes"
        );

        let token_address_hex = hex::encode(token_address.as_slice());
        assert_eq!(
            token_address_hex.len(),
            40,
            "Token address should be 20 bytes"
        );

        // Concatenate all parts with 0x prefix
        let message = format!(
            "0x{}{}{}{}{}",
            recipient_hex, value_hex, nonce_hex, bridge_address_hex, token_address_hex
        );

        // Expected length: 2 (0x) + 2 * (20 + 32 + 32 + 20 + 20) = 2 + 248 = 250
        assert_eq!(
            message.len(),
            250,
            "Message should be 124 bytes (248 hex chars + 0x)"
        );

        message
    }
    pub async fn read_from_db(&self) -> Result<Option<EventLogRow>, Box<dyn std::error::Error>> {
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
            SELECT id, topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count
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

    pub async fn write_is_processed_to_false(&self, id: i32) -> Result<(), Box<dyn Error>> {
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

    async fn get_finalized_block(bc_rpc: String) -> Result<BeaconBlockResponse, Box<dyn Error>> {
        // TODO: Client should not be created for every request
        let endpoint = format!("{}/eth/v1/beacon/blocks/finalized", bc_rpc);

        let client = reqwest::Client::new();
        let response = client
            .get(&endpoint)
            .header("Accept", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(format!("HTTP error: {}", response.status()).into());
        }

        let block_response = response.json::<BeaconBlockResponse>().await?;
        Ok(block_response)
    }
}
