use crate::config::Config;

use crate::contracts::{
    OnChainCallData, AMB_BRIDGE, AMB_BRIDGE_HELPER, XDAI_BRIDGE, XDAI_BRIDGE_HELPER,
};

use alloy::{
    hex,
    primitives::{keccak256, Address, Bytes, FixedBytes, U256},
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::error::Error;
use tokio::sync::mpsc::Receiver;
use tracing;

pub struct OnChainSender {
    config: Config,
    db_pool: PgPool,
    tokio_receiver: Receiver<OnChainCallData>,
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
        tracing::info!("On_chain_sender start...");
        while let Some(msg) = self.tokio_receiver.recv().await {
            tracing::info!("Received message for on chain call: {:?}", msg);
            if let Err(e) = self.process_message(msg).await {
                tracing::error!("Error processing message: {}", e);
            }
        }
    }
    async fn process_message(&self, msg: OnChainCallData) -> Result<(), Box<dyn Error>> {
        match msg {
            OnChainCallData::AmbEth {
                contract_address,
                calldata,
            } => {
                tracing::debug!("=== AmbEth Call Debug ===");
                tracing::debug!("Contract address: {:?}", contract_address);
                tracing::debug!("Message length: {} bytes", calldata.message.len());
                tracing::debug!("Message hex: 0x{}", hex::encode(&calldata.message));

                // Parse the private key string into a PrivateKeySigner
                let pk_signer: PrivateKeySigner = self
                    .config
                    .clone()
                    .amb_validator_private_key
                    .expect("AMB_VALIDATOR_PRIV_KEY must be set in .env")
                    .parse()
                    .expect("Failed to parse private key");

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await?;

                let bridge_instance = AMB_BRIDGE::new(contract_address, provider.clone());

                // Compute hashMsg = keccak256(abi.encodePacked(message))
                let hash_msg = keccak256(&calldata.message);

                // Compute hashSender = keccak256(abi.encodePacked(msg.sender, hashMsg))
                let sender_addr = pk_signer.address();
                let mut buf = Vec::with_capacity(20 + 32);
                buf.extend_from_slice(sender_addr.as_slice());
                buf.extend_from_slice(hash_msg.as_slice());
                let hash_sender = keccak256(&buf);

                // Check 1: require(!affirmationsSigned(hashSender));
                let already_affirmed = bridge_instance
                    .affirmationsSigned(hash_sender)
                    .call()
                    .await?;
                if already_affirmed {
                    tracing::info!(
                        "AMB_ETH: affirmation already signed by validator {}, skipping",
                        sender_addr
                    );
                    return Ok(());
                }

                // Check 2: signed = numAffirmationsSigned(hashMsg); require(!isAlreadyProcessed(signed));
                let signed: U256 = bridge_instance
                    .numAffirmationsSigned(hash_msg)
                    .call()
                    .await?;

                // Check 3: signed < requiredSignatures()
                let required: U256 = bridge_instance.requiredSignatures().call().await?;
                if signed >= required {
                    tracing::info!(
                        "AMB_ETH: message already has {signed} >= required {required} affirmations, skipping"
                    );
                    return Ok(());
                }

                let execute_affirmation_tx = bridge_instance
                    .executeAffirmation(calldata.message)
                    .send()
                    .await?;

                let execute_affirmation_receipt =
                    execute_affirmation_tx.get_receipt().await.unwrap();

                tracing::info!(
                    "executeAffirmation called in transaction {:?}",
                    execute_affirmation_receipt.transaction_hash
                );
                Ok(())
            }
            OnChainCallData::AmbGc {
                contract_address,
                calldata,
            } => {
                tracing::debug!("=== AmbGc Call Debug ===");
                tracing::debug!("Contract address: {:?}", contract_address);
                tracing::debug!("Message length: {} bytes", calldata.message.len());
                tracing::debug!("Message hex: 0x{}", hex::encode(&calldata.message));
                tracing::debug!("Signature length: {} bytes", calldata.signature.len());
                tracing::debug!("Signature hex: 0x{}", hex::encode(&calldata.signature));

                // Parse the private key string into a PrivateKeySigner
                let pk_signer: PrivateKeySigner = self
                    .config
                    .clone()
                    .amb_validator_private_key
                    .expect("AMB_VALIDATOR_PRIV_KEY must be set in .env")
                    .parse()
                    .expect("Failed to parse private key");

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await?;

                let bridge_instance = AMB_BRIDGE::new(contract_address, provider.clone());

                // bytes32 hashMsg = keccak256(abi.encodePacked(message));
                let hash_msg = keccak256(&calldata.message);

                // bytes32 hashSender = keccak256(abi.encodePacked(msg.sender, hashMsg));
                let sender_addr = pk_signer.address();
                let mut buf = Vec::with_capacity(20 + 32);
                buf.extend_from_slice(sender_addr.as_slice());
                buf.extend_from_slice(hash_msg.as_slice());
                let hash_sender = keccak256(&buf);

                // uint256 signed = bridge_instance.numMessagesSigned(hashMsg);
                let signed: U256 = bridge_instance.numMessagesSigned(hash_msg).call().await?;

                // Check 1: require(!isAlreadyProcessed(signed));
                let required: U256 = bridge_instance.requiredSignatures().call().await?;
                if signed >= required {
                    tracing::info!(
                        "AMB_GC: message already has {signed} >= required {required} signatures, skipping submitSignature"
                    );
                    return Ok(());
                }

                // Check 2: require(!bridge_instance.messagesSigned(hashSender));
                let already_signed_by_validator =
                    bridge_instance.messagesSigned(hash_sender).call().await?;
                if already_signed_by_validator {
                    tracing::info!(
                        "AMB_GC: validator {} already signed this message, skipping",
                        sender_addr
                    );
                    return Ok(());
                }

                let submit_signature_tx = bridge_instance
                    .submitSignature(calldata.signature, calldata.message.clone())
                    .send()
                    .await?;

                let submit_signature_receipt = submit_signature_tx.get_receipt().await.unwrap();

                tracing::info!(
                    "submit_signature_tx {:?}",
                    submit_signature_receipt.transaction_hash
                );

                //  call the bridge helper contract to collect aggregated signatures.
                if self.config.amb_execute_message_on_foreign == "true" {
                    let bridge_helper_instance =
                        AMB_BRIDGE_HELPER::new(self.config.amb_bridge_helper_address, provider);

                    let signatures = bridge_helper_instance
                        .getSignatures(calldata.message.clone())
                        .call()
                        .await?;

                    if signatures.len() == (2 + 65 * required.to::<usize>() - 1) {
                        // Create provider for Ethereum (foreign chain)
                        let eth_provider = ProviderBuilder::new()
                            .wallet(pk_signer.clone())
                            .connect(self.config.get_eth_rpc())
                            .await?;

                        tracing::debug!(
                            "Creating foreign AMB bridge instance at address: {:?}",
                            self.config.eth_amb_bridge_address
                        );

                        let foreign_bridge_instance =
                            AMB_BRIDGE::new(self.config.eth_amb_bridge_address, eth_provider);

                        tracing::debug!(
                            "Calling safeExecuteSignaturesWithGasLimit with message (len={}): 0x{}, signatures (len={}): 0x{}, gas: 1000000",
                            calldata.message.len(),
                            hex::encode(&calldata.message),
                            signatures.len(),
                            hex::encode(&signatures)
                        );

                        // AMB bridge uses safeExecuteSignaturesWithGasLimit
                        match foreign_bridge_instance
                            .safeExecuteSignaturesWithAutoGasLimit(
                                calldata.message.clone(),
                                signatures.clone(),
                            )
                            .send()
                            .await
                        {
                            Ok(execute_signature_tx) => {
                                let execute_signature_receipt =
                                    execute_signature_tx.get_receipt().await?;
                                tracing::info!(
                                    "AMB executeSignatures on foreign chain: {:?}",
                                    execute_signature_receipt.transaction_hash
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to execute AMB signatures on foreign chain at address {:?}: {}",
                                    self.config.eth_amb_bridge_address,
                                    e
                                );
                                tracing::error!(
                                    "Message: 0x{}, Signatures: 0x{}",
                                    hex::encode(&calldata.message),
                                    hex::encode(&signatures)
                                );
                                // TODO:Push it to the reprocess queue
                            }
                        }
                    }
                }

                Ok(())
            }
            OnChainCallData::XdaiEth {
                contract_address,
                calldata,
            } => {
                tracing::debug!("=== XdaiEth Call Debug ===");
                tracing::debug!("Contract address: {:?}", contract_address);
                tracing::debug!("Recipient: {:?}", calldata.recipient);
                tracing::debug!("Value: {:?}", calldata.value);
                tracing::debug!("Nonce: 0x{}", hex::encode(calldata.nonce));

                // Parse the private key string into a PrivateKeySigner
                let pk_signer: PrivateKeySigner = self
                    .config
                    .clone()
                    .xdai_validator_private_key
                    .expect("XDAI_VALIDATOR_PRIV_KEY must be set in .env")
                    .parse()
                    .expect("Failed to parse private key");

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await?;

                let bridge_instance = XDAI_BRIDGE::new(contract_address, provider);

                // bytes32 hashMsg = keccak256(abi.encodePacked(recipient, value, nonce));
                let mut buf = Vec::with_capacity(20 + 32 + 32);
                buf.extend_from_slice(calldata.recipient.as_slice());
                let value_bytes = calldata.value.to_be_bytes::<32>();
                buf.extend_from_slice(&value_bytes);
                buf.extend_from_slice(calldata.nonce.as_slice());
                let hash_msg = keccak256(&buf);

                // bytes32 hashSender = keccak256(abi.encodePacked(msg.sender, hashMsg));
                let sender_addr = pk_signer.address();
                let mut buf2 = Vec::with_capacity(20 + 32);
                buf2.extend_from_slice(sender_addr.as_slice());
                buf2.extend_from_slice(hash_msg.as_slice());
                let hash_sender = keccak256(&buf2);

                // Check 2:   require(!bridge_instance.affirmationsSigned(hashSender));
                let already_affirmed = bridge_instance
                    .affirmationsSigned(hash_sender)
                    .call()
                    .await?;
                if already_affirmed {
                    tracing::info!(
                        "XDAI_ETH: affirmation already signed by validator {}, skipping",
                        sender_addr
                    );
                    return Ok(());
                }

                // signed = bridge_instance.numAffirmationsSigned(hashMsg);
                let signed: U256 = bridge_instance
                    .numAffirmationsSigned(hash_msg)
                    .call()
                    .await?;

                // Check 3: signed < requiredSignatures()
                let required: U256 = bridge_instance.requiredSignatures().call().await?;
                if signed >= required {
                    tracing::info!(
                        "XDAI_ETH: message already has {signed} >= required {required} affirmations, skipping"
                    );
                    return Ok(());
                }

                let execute_affirmation_tx = bridge_instance
                    .executeAffirmation(calldata.recipient, calldata.value, calldata.nonce)
                    .send()
                    .await?;

                let execute_affirmation_receipt =
                    execute_affirmation_tx.get_receipt().await.unwrap();

                tracing::info!(
                    "executeAffirmation called in transaction {:?}",
                    execute_affirmation_receipt.transaction_hash
                );

                Ok(())
            }
            OnChainCallData::XdaiGc {
                contract_address,
                calldata,
            } => {
                tracing::debug!("=== XdaiGc Call Debug ===");
                tracing::debug!("Contract address: {:?}", contract_address);
                tracing::debug!("Message length: {} bytes", calldata.message.len());
                tracing::debug!("Message hex: 0x{}", hex::encode(&calldata.message));
                tracing::debug!("Signature length: {} bytes", calldata.signature.len());
                tracing::debug!("Signature hex: 0x{}", hex::encode(&calldata.signature));

                // Parse the private key string into a PrivateKeySigner
                let pk_signer: PrivateKeySigner = self
                    .config
                    .clone()
                    .xdai_validator_private_key
                    .expect("XDAI_VALIDATOR_PRIV_KEY must be set in .env")
                    .parse()
                    .expect("Failed to parse private key");

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await?;

                let bridge_instance = XDAI_BRIDGE::new(contract_address, provider.clone());

                // Reconstruct hashMsg = keccak256(abi.encodePacked(message));
                let hash_msg = keccak256(&calldata.message);

                // bytes32 hashSender = keccak256(abi.encodePacked(msg.sender, hashMsg));
                let sender_addr = pk_signer.address();
                let mut buf = Vec::with_capacity(20 + 32);
                buf.extend_from_slice(sender_addr.as_slice());
                buf.extend_from_slice(hash_msg.as_slice());
                let hash_sender = keccak256(&buf);

                // uint256 signed = bridge_instance.numMessagesSigned(hashMsg);
                let signed: U256 = bridge_instance.numMessagesSigned(hash_msg).call().await?;

                // Check 1: require(!isAlreadyProcessed(signed));
                let required: U256 = bridge_instance.requiredSignatures().call().await?;
                if signed >= required {
                    tracing::info!(
                        "XDAI_GC: message already has {signed} >= required {required} signatures, skipping submitSignature"
                    );
                    return Ok(());
                }

                // Optional Check 2: require(!bridge_instance.messagesSigned(hashSender));
                let already_signed_by_validator =
                    bridge_instance.messagesSigned(hash_sender).call().await?;
                if already_signed_by_validator {
                    tracing::info!(
                        "XDAI_GC: validator {} already signed this message, skipping",
                        sender_addr
                    );
                    return Ok(());
                }

                let submit_signature_tx = bridge_instance
                    .submitSignature(calldata.signature, calldata.message.clone())
                    .send()
                    .await?;

                let submit_signature_receipt = submit_signature_tx.get_receipt().await.unwrap();

                tracing::info!(
                    "submit_signature_tx {:?}",
                    submit_signature_receipt.transaction_hash
                );

                // Optionally: inspect aggregated signatures on the helper contract and compute
                // the message hash using the same layout as `create_xdai_message`.
                if self.config.xdai_execute_message_on_foreign == "true" {
                    let bridge_helper_instance =
                        XDAI_BRIDGE_HELPER::new(self.config.xdai_bridge_helper_address, provider);

                    let parsed = Self::parse_xdai_message(&calldata.message)?;
                    tracing::debug!(
                        "{:?} , {:?}, {:?}, {:?} ",
                        parsed.recipient,
                        parsed.value,
                        parsed.nonce,
                        parsed.token,
                    );
                    let msg_hash = bridge_helper_instance
                        .getMessageHash(parsed.recipient, parsed.value, parsed.nonce, parsed.token)
                        .call()
                        .await?;

                    let signatures = bridge_helper_instance
                        .getSignatures(msg_hash)
                        .call()
                        .await?;

                    tracing::info!(
                        "xDai helper returned {} signature bytes for msgHash",
                        signatures.len()
                    );

                    tracing::info!("required {:?} ", required.to::<usize>());
                    if signatures.len() == (2 + 65 * required.to::<usize>() - 1) {
                        // Create provider for Ethereum (foreign chain)
                        let eth_provider = ProviderBuilder::new()
                            .wallet(pk_signer.clone())
                            .connect(self.config.get_eth_rpc())
                            .await?;

                        tracing::info!(
                            "Creating foreign bridge instance at address: {:?}",
                            self.config.eth_xdai_bridge_address
                        );

                        let foreign_bridge_instance =
                            XDAI_BRIDGE::new(self.config.eth_xdai_bridge_address, eth_provider);

                        tracing::debug!(
                            "Calling executeSignatures with signatures (len={}): 0x{}, message (len={}): 0x{}",
                            signatures.len(),
                            hex::encode(&signatures),
                            calldata.message.len(),
                            hex::encode(&calldata.message)
                        );

                        // Before calling executeSignatures, check if the message is already processed
                        // by verifying on the foreign bridge
                        match foreign_bridge_instance
                            .executeSignatures(calldata.message.clone(), signatures.clone())
                            .send()
                            .await
                        {
                            Ok(execute_signature_tx) => {
                                let execute_signature_receipt =
                                    execute_signature_tx.get_receipt().await?;
                                tracing::info!(
                                    "XDAI executeSignatures on foreign chain: {:?}",
                                    execute_signature_receipt.transaction_hash
                                );
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to execute signatures on foreign chain at address {:?}: {}",
                                    self.config.eth_xdai_bridge_address,
                                    e
                                );
                                tracing::error!(
                                    "Signatures: 0x{}, Message: 0x{}",
                                    hex::encode(&signatures),
                                    hex::encode(&calldata.message)
                                );
                                // TODO: push to reprocess queue
                            }
                        }
                    }
                }

                Ok(())
            }
        }
    }

    /// Parse the xDai message format created in `MessageProcessor::create_xdai_message`.
    ///
    /// Layout (all big-endian, no 0x prefix):
    /// - 20 bytes: recipient address
    /// - 32 bytes: value (U256)
    /// - 32 bytes: nonce (bytes32)
    /// - 20 bytes: bridge address
    /// - 20 bytes: token address
    fn parse_xdai_message(message: &Bytes) -> Result<ParsedXdaiMessage, Box<dyn Error>> {
        // 20 + 32 + 32 + 20 + 20 = 124 bytes
        if message.len() != 124 {
            return Err(format!("Unexpected xDai message length: {}", message.len()).into());
        }

        let bytes = message.as_ref();

        let recipient = Address::from_slice(&bytes[0..20]);

        let mut value_bytes = [0u8; 32];
        value_bytes.copy_from_slice(&bytes[20..52]);
        let value = U256::from_be_bytes(value_bytes);

        let nonce = FixedBytes::<32>::from_slice(&bytes[52..84]);

        let bridge = Address::from_slice(&bytes[84..104]);
        let token = Address::from_slice(&bytes[104..124]);

        Ok(ParsedXdaiMessage {
            recipient,
            value,
            nonce,
            bridge,
            token,
        })
    }
}

#[derive(Debug)]
struct ParsedXdaiMessage {
    recipient: Address,
    value: U256,
    nonce: FixedBytes<32>,
    bridge: Address,
    token: Address,
}
