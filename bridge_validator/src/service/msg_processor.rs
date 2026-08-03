use crate::config::Config;
use crate::error::BridgeValidatorError;
use alloy::primitives::{Address, Bytes};
use alloy::sol_types::SolEvent;
use alloy_primitives::Log;
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
        let log: Log = serde_json::from_value(event_log.log_data.clone())?;

        match event_log.bridge_mode.as_str() {
            "AMB_ETH" => {
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
                        stage: event_log
                            .stage
                            .clone()
                            .unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            "AMB_GC" => {
                let decoded = AMB_BRIDGE::UserRequestForSignature::decode_log(&log)?;

                let priv_key_str = self
                    .config
                    .amb_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("AMB_VALIDATOR_PRIV_KEY"))?;
                let pk_signer: PrivateKeySigner = priv_key_str.parse().map_err(
                    |e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    },
                )?;

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
                        stage: event_log
                            .stage
                            .clone()
                            .unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            "XDAI_ETH" => {
                let decoded = XDAI_BRIDGE::UserRequestForAffirmation::decode_log(&log)?;

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
                        stage: event_log
                            .stage
                            .clone()
                            .unwrap_or_else(|| "home".to_string()),
                    })
                    .await
                    .map_err(|e| BridgeValidatorError::ChannelSend(e.to_string()))?;
            }
            "XDAI_GC" => {
                let decoded = XDAI_BRIDGE::UserRequestForSignature::decode_log(&log)?;
                // message: recipient + value + nonce + bridge_address(foreign_xdai_bridge) + token_address(depends)
                let xdai_message = self.create_xdai_message(
                    decoded.recipient.clone(),
                    decoded.value.clone(),
                    decoded.nonce.clone(),
                    decoded.token.clone(),
                )?;

                let priv_key_str = self
                    .config
                    .xdai_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("XDAI_VALIDATOR_PRIV_KEY"))?;

                let pk_signer: PrivateKeySigner = priv_key_str.parse().map_err(
                    |e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    },
                )?;

                // Decode hex string (strip 0x prefix) to bytes for signing
                let message_bytes = hex::decode(&xdai_message[2..])?;

                let signature: alloy_primitives::Signature = pk_signer
                    .sign_message(&message_bytes)
                    .await
                    .map_err(|e| BridgeValidatorError::Sign(e.to_string()))?;
                tracing::debug!("Signature: 0x{}", hex::encode(&signature.as_bytes()));

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
                        stage: event_log
                            .stage
                            .clone()
                            .unwrap_or_else(|| "home".to_string()),
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

        // `retry_count` is an INT column, so the ceiling is compared as i32.
        // `max_retry_count` is operator-supplied and unbounded, so saturate
        // rather than wrap: a ceiling above i32::MAX means "never give up",
        // which is what clamping to i32::MAX yields in practice.
        let max_retry_count = i32::try_from(self.config.max_retry_count).unwrap_or(i32::MAX);

        // FOR UPDATE SKIP LOCKED is what lets both MessageProcessor instances
        // share this queue: FOR UPDATE row-locks the claimed row until the
        // transaction commits, and SKIP LOCKED makes the other processor step
        // over it and take the next row instead of blocking on the lock.
        let row = sqlx::query_as!(
            EventLogRow,
            r#"
            SELECT id, topic_key, bridge_mode, log_data, block_number, transaction_hash, is_processed, retry_count, stage
            FROM event_logs
            WHERE is_processed = 'false' AND retry_count < $1
            ORDER BY block_number ASC, log_index ASC
            LIMIT 1
            FOR UPDATE SKIP LOCKED
            "#,
            max_retry_count
        )
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(ref log_row) = row {
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

        tx.commit().await?;

        Ok(row)
    }
}
