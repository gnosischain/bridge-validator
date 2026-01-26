use crate::config::Config;

use crate::contracts::{OnChainCallData, AMB_BRIDGE, XDAI_BRIDGE};

use alloy::{providers::ProviderBuilder, signers::local::PrivateKeySigner};

use alloy::hex;

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::error::Error;
use tokio::sync::mpsc::Receiver;

pub struct OnChainSender {
    config: Config,
    db_pool: PgPool,
    tokio_receiver: Receiver<OnChainCallData>,
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
}

impl OnChainSender {
    pub fn new(config: Config, db_pool: PgPool, tokio_receiver: Receiver<OnChainCallData>) -> Self {
        Self {
            config,
            db_pool,
            tokio_receiver,
        }
    }

    pub async fn start(mut self) {
        println!("On_chain_sender start...");
        while let Some(msg) = self.tokio_receiver.recv().await {
            println!("Received message for on chain call: {:?}", msg);
            if let Err(e) = self.process_message(msg).await {
                eprintln!("Error processing message: {}", e);
            }
        }

        async fn process_message(&self, msg: OnChainCallData) -> Result<(), Box<dyn Error>> {
            match msg {
                OnChainCallData::AmbEth {
                    contract_address,
                    calldata,
                } => {
                    println!("=== AmbEth Call Debug ===");
                    println!("Contract address: {:?}", contract_address);
                    println!("Message length: {} bytes", calldata.message.len());
                    println!("Message hex: 0x{}", hex::encode(&calldata.message));

                    // Parse the private key string into a PrivateKeySigner
                    let pk_signer: PrivateKeySigner = self
                        .config
                        .clone()
                        .amb_validator_private_key
                        .expect("AMB_VALIDATOR_PRIV_KEY must be set in .env")
                        .parse()
                        .expect("Failed to parse private key");

                    let provider = ProviderBuilder::new()
                        .wallet(pk_signer)
                        .connect(&self.config.clone().gc_rpc)
                        .await?;

                    let bridge_instance = AMB_BRIDGE::new(contract_address, provider);
                    let execute_affirmation_tx = bridge_instance
                        .executeAffirmation(calldata.message)
                        .send()
                        .await?;

                    let execute_affirmation_receipt =
                        execute_affirmation_tx.get_receipt().await.unwrap();

                    println!(
                        "executeAffirmation in block {:?}",
                        execute_affirmation_receipt
                    );
                    Ok(())
                }
                OnChainCallData::AmbGc {
                    contract_address,
                    calldata,
                } => {
                    println!("=== AmbGc Call Debug ===");
                    println!("Contract address: {:?}", contract_address);
                    println!("Message length: {} bytes", calldata.message.len());
                    println!("Message hex: 0x{}", hex::encode(&calldata.message));
                    println!("Signature length: {} bytes", calldata.signature.len());
                    println!("Signature hex: 0x{}", hex::encode(&calldata.signature));

                    // Parse the private key string into a PrivateKeySigner
                    let pk_signer: PrivateKeySigner = self
                        .config
                        .clone()
                        .amb_validator_private_key
                        .expect("AMB_VALIDATOR_PRIV_KEY must be set in .env")
                        .parse()
                        .expect("Failed to parse private key");

                    let provider = ProviderBuilder::new()
                        .wallet(pk_signer)
                        .connect(&self.config.clone().gc_rpc)
                        .await?;

                    let bridge_instance = AMB_BRIDGE::new(contract_address, provider);
                    let execute_signature_tx = bridge_instance
                        .submitSignature(calldata.signature, calldata.message)
                        .send()
                        .await?;

                    let execute_signature_receipt =
                        execute_signature_tx.get_receipt().await.unwrap();

                    println!("execute_signature_tx {:?}", execute_signature_receipt);
                    Ok(())
                }
                OnChainCallData::XdaiEth {
                    contract_address,
                    calldata,
                } => {
                    println!("=== XdaiEth Call Debug ===");
                    println!("Contract address: {:?}", contract_address);
                    println!("Recipient: {:?}", calldata.recipient);
                    println!("Value: {:?}", calldata.value);
                    println!("Nonce: 0x{}", hex::encode(calldata.nonce));

                    // Parse the private key string into a PrivateKeySigner
                    let pk_signer: PrivateKeySigner = self
                        .config
                        .clone()
                        .xdai_validator_private_key
                        .expect("XDAI_VALIDATOR_PRIV_KEY must be set in .env")
                        .parse()
                        .expect("Failed to parse private key");

                    let provider = ProviderBuilder::new()
                        .wallet(pk_signer)
                        .connect(&self.config.clone().gc_rpc)
                        .await?;

                    let bridge_instance = XDAI_BRIDGE::new(contract_address, provider);
                    let execute_affirmation_tx = bridge_instance
                        .executeAffirmation(calldata.recipient, calldata.value, calldata.nonce)
                        .send()
                        .await?;

                    let execute_affirmation_receipt =
                        execute_affirmation_tx.get_receipt().await.unwrap();

                    println!(
                        "executeAffirmation in block {:?}",
                        execute_affirmation_receipt
                    );

                    Ok(())
                }
                OnChainCallData::XdaiGc {
                    contract_address,
                    calldata,
                } => {
                    println!("=== XdaiGc Call Debug ===");
                    println!("Contract address: {:?}", contract_address);
                    println!("Message length: {} bytes", calldata.message.len());
                    println!("Message hex: 0x{}", hex::encode(&calldata.message));
                    println!("Signature length: {} bytes", calldata.signature.len());
                    println!("Signature hex: 0x{}", hex::encode(&calldata.signature));

                    // Parse the private key string into a PrivateKeySigner
                    let pk_signer: PrivateKeySigner = self
                        .config
                        .clone()
                        .xdai_validator_private_key
                        .expect("XDAI_VALIDATOR_PRIV_KEY must be set in .env")
                        .parse()
                        .expect("Failed to parse private key");

                    let provider = ProviderBuilder::new()
                        .wallet(pk_signer)
                        .connect(&self.config.clone().gc_rpc)
                        .await?;

                    let bridge_instance = XDAI_BRIDGE::new(contract_address, provider);
                    let execute_signature_tx = bridge_instance
                        .submitSignature(calldata.signature, calldata.message)
                        .send()
                        .await?;

                    let execute_signature_receipt =
                        execute_signature_tx.get_receipt().await.unwrap();

                    println!("execute_signature_tx {:?}", execute_signature_receipt);
                    Ok(())
                }
            }
        }
    }
}
