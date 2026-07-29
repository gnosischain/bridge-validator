use crate::config::Config;

use crate::contracts::{
    OnChainCallData, AMB_BRIDGE, AMB_BRIDGE_HELPER, XDAI_BRIDGE, XDAI_BRIDGE_HELPER,
};

use crate::error::BridgeValidatorError;
use crate::service::msg_processor::SenderData;

use alloy::{
    hex,
    primitives::{keccak256, Address, Bytes, FixedBytes, U256},
    providers::ProviderBuilder,
    signers::local::PrivateKeySigner,
};

use sqlx::PgPool;
use tokio::sync::mpsc::Receiver;
use tracing;

pub struct OnChainSender {
    config: Config,
    db_pool: PgPool,
    tokio_receiver: Receiver<SenderData>,
}

impl OnChainSender {
    pub fn new(config: Config, db_pool: PgPool, tokio_receiver: Receiver<SenderData>) -> Self {
        Self {
            config,
            db_pool,
            tokio_receiver,
        }
    }

    pub async fn start(mut self) {
        tracing::info!("On_chain_sender start...");
        while let Some(sender_data) = self.tokio_receiver.recv().await {
            tracing::info!(
                "Received message for on chain call: {:?}",
                sender_data.on_chain_calldata
            );
            if let Err(e) = self
                .process_message(
                    sender_data.on_chain_calldata,
                    sender_data.event_log_id,
                    &sender_data.stage,
                )
                .await
            {
                tracing::error!("Error processing message: {}", e);
            }
        }
    }
    async fn process_message(
        &self,
        msg: OnChainCallData,
        event_log_id: i32,
        stage: &str,
    ) -> Result<(), BridgeValidatorError> {
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
                    .amb_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("AMB_VALIDATOR_PRIV_KEY"))?
                    .parse()
                    .map_err(|e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    })?;

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await
                    .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()))?;

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
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;
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
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                // Check 3: signed < requiredSignatures()
                let required: U256 = bridge_instance
                    .requiredSignatures()
                    .call()
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;
                if signed >= required {
                    tracing::info!(
                        "AMB_ETH: message already has {signed} >= required {required} affirmations, skipping"
                    );
                    self.delete_event_log(event_log_id).await?;
                    return Ok(());
                }

                // Execute the affirmation transaction
                match bridge_instance
                    .executeAffirmation(calldata.message)
                    .send()
                    .await
                {
                    Ok(execute_affirmation_tx) => {
                        match execute_affirmation_tx.get_receipt().await {
                            Ok(execute_affirmation_receipt) => {
                                tracing::info!(
                                    "AMB: executeAffirmation called in transaction {:?}",
                                    execute_affirmation_receipt.transaction_hash
                                );
                                self.delete_event_log(event_log_id).await?;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "AMB: Failed to get receipt for executeAffirmation: {}",
                                    e
                                );
                                self.increment_retry_count(event_log_id).await?;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "AMB: Failed to send executeAffirmation transaction: {}",
                            e
                        );
                        self.increment_retry_count(event_log_id).await?;
                    }
                }
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
                    .amb_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("AMB_VALIDATOR_PRIV_KEY"))?
                    .parse()
                    .map_err(|e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    })?;

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await
                    .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()))?;

                let bridge_instance = AMB_BRIDGE::new(contract_address, provider.clone());

                let required: U256 = bridge_instance
                    .requiredSignatures()
                    .call()
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                // Stage "home": run pre-flight checks and submitSignature on home chain
                if stage != "foreign" {
                    // bytes32 hashMsg = keccak256(abi.encodePacked(message));
                    let hash_msg = keccak256(&calldata.message);

                    // bytes32 hashSender = keccak256(abi.encodePacked(msg.sender, hashMsg));
                    let sender_addr = pk_signer.address();
                    let mut buf = Vec::with_capacity(20 + 32);
                    buf.extend_from_slice(sender_addr.as_slice());
                    buf.extend_from_slice(hash_msg.as_slice());
                    let hash_sender = keccak256(&buf);

                    // uint256 signed = bridge_instance.numMessagesSigned(hashMsg);
                    let signed: U256 = bridge_instance
                        .numMessagesSigned(hash_msg)
                        .call()
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                    // Check 1: require(!isAlreadyProcessed(signed));
                    if signed >= required {
                        tracing::info!(
                            "AMB_GC: message already has {signed} >= required {required} signatures, skipping submitSignature"
                        );
                        self.delete_event_log(event_log_id).await?;
                        return Ok(());
                    }

                    // Check 2: require(!bridge_instance.messagesSigned(hashSender));
                    let already_signed_by_validator = bridge_instance
                        .messagesSigned(hash_sender)
                        .call()
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;
                    if already_signed_by_validator {
                        tracing::info!(
                            "AMB_GC: validator {} already signed this message, skipping",
                            sender_addr
                        );
                        self.delete_event_log(event_log_id).await?;
                        return Ok(());
                    }

                    // Submit the signature transaction
                    let submit_result = async {
                        let submit_signature_tx = bridge_instance
                            .submitSignature(calldata.signature, calldata.message.clone())
                            .send()
                            .await
                            .map_err(|e| {
                                tracing::error!(
                                    "Failed to send submitSignature transaction: {}",
                                    e
                                );
                                BridgeValidatorError::TxSubmit(e.to_string())
                            })?;

                        let submit_signature_receipt =
                            submit_signature_tx.get_receipt().await.map_err(|e| {
                                tracing::error!("Failed to get receipt for submitSignature: {}", e);
                                BridgeValidatorError::TxReceipt(e.to_string())
                            })?;

                        tracing::info!(
                            "submit_signature_tx_hash {:?}",
                            submit_signature_receipt.transaction_hash
                        );
                        Ok::<(), BridgeValidatorError>(())
                    }
                    .await;

                    // If submit failed, increment retry count and return
                    if submit_result.is_err() {
                        self.increment_retry_count(event_log_id).await?;
                        return Ok(());
                    }

                    if self.config.amb_execute_message_on_foreign != "true" {
                        // Not executing on foreign chain, delete after submitSignature success
                        self.delete_event_log(event_log_id).await?;
                        return Ok(());
                    }

                    // Mark stage as 'foreign' before attempting foreign execution
                    self.update_stage(event_log_id, "foreign").await?;
                }

                // Stage "foreign": execute on foreign chain
                // (reached either after fresh submitSignature or on retry with stage='foreign')
                if self.config.amb_execute_message_on_foreign == "true" {
                    let bridge_helper_instance =
                        AMB_BRIDGE_HELPER::new(self.config.amb_bridge_helper_address, provider);

                    let signatures = bridge_helper_instance
                        .getSignatures(calldata.message.clone())
                        .call()
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                    if signatures.len() == (2 + 65 * required.to::<usize>() - 1) {
                        let eth_provider = ProviderBuilder::new()
                            .wallet(pk_signer.clone())
                            .connect(self.config.get_eth_rpc())
                            .await
                            .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()))?;

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

                        // AMB uses safeExecuteSignaturesWithGasLimit
                        match foreign_bridge_instance
                            .safeExecuteSignaturesWithAutoGasLimit(
                                calldata.message.clone(),
                                signatures.clone(),
                            )
                            .send()
                            .await
                        {
                            Ok(execute_signature_tx) => {
                                match execute_signature_tx.get_receipt().await {
                                    Ok(execute_signature_receipt) => {
                                        tracing::info!(
                                            "AMB executeSignatures on foreign chain: {:?}",
                                            execute_signature_receipt.transaction_hash
                                        );
                                        self.delete_event_log(event_log_id).await?;
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to get receipt for AMB foreign execution: {}",
                                            e
                                        );
                                        self.increment_retry_count(event_log_id).await?;
                                    }
                                }
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
                                self.increment_retry_count(event_log_id).await?;
                            }
                        }
                    } else {
                        tracing::warn!(
                            "Not enough signatures collected yet: {} bytes, expected: {}",
                            signatures.len(),
                            2 + 65 * required.to::<usize>() - 1
                        );
                        self.delete_event_log(event_log_id).await?;
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
                    .xdai_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("XDAI_VALIDATOR_PRIV_KEY"))?
                    .parse()
                    .map_err(|e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    })?;

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await
                    .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()))?;

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
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;
                if already_affirmed {
                    tracing::info!(
                        "XDAI_ETH: affirmation already signed by validator {}, skipping",
                        sender_addr
                    );
                    // Delete the event log since this validator already signed
                    self.delete_event_log(event_log_id).await?;
                    return Ok(());
                }

                // signed = bridge_instance.numAffirmationsSigned(hashMsg);
                let signed: U256 = bridge_instance
                    .numAffirmationsSigned(hash_msg)
                    .call()
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                // Check 3: signed < requiredSignatures()
                let required: U256 = bridge_instance
                    .requiredSignatures()
                    .call()
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;
                if signed >= required {
                    tracing::info!(
                        "XDAI_ETH: message already has {signed} >= required {required} affirmations, skipping"
                    );
                    // Delete the event log since it's already processed
                    self.delete_event_log(event_log_id).await?;
                    return Ok(());
                }

                // Execute the affirmation transaction
                match bridge_instance
                    .executeAffirmation(calldata.recipient, calldata.value, calldata.nonce)
                    .send()
                    .await
                {
                    Ok(execute_affirmation_tx) => {
                        match execute_affirmation_tx.get_receipt().await {
                            Ok(execute_affirmation_receipt) => {
                                tracing::info!(
                                    "xDAI: executeAffirmation called in transaction {:?}",
                                    execute_affirmation_receipt.transaction_hash
                                );
                                self.delete_event_log(event_log_id).await?;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "xDAI: Failed to get receipt for executeAffirmation: {}",
                                    e
                                );
                                self.increment_retry_count(event_log_id).await?;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "xDAI: Failed to send executeAffirmation transaction: {}",
                            e
                        );
                        self.increment_retry_count(event_log_id).await?;
                    }
                }

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

                let pk_signer: PrivateKeySigner = self
                    .config
                    .xdai_validator_private_key
                    .as_ref()
                    .ok_or(BridgeValidatorError::MissingEnv("XDAI_VALIDATOR_PRIV_KEY"))?
                    .parse()
                    .map_err(|e: alloy::signers::local::LocalSignerError| {
                        BridgeValidatorError::KeyParse(e.to_string())
                    })?;

                let provider = ProviderBuilder::new()
                    .wallet(pk_signer.clone())
                    .connect(self.config.get_gc_rpc())
                    .await
                    .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()))?;

                let bridge_instance = XDAI_BRIDGE::new(contract_address, provider.clone());

                let required: U256 = bridge_instance
                    .requiredSignatures()
                    .call()
                    .await
                    .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                // Stage "home": run pre-flight checks and submitSignature on home chain
                if stage != "foreign" {
                    // Reconstruct hashMsg = keccak256(abi.encodePacked(message));
                    let hash_msg = keccak256(&calldata.message);

                    // bytes32 hashSender = keccak256(abi.encodePacked(msg.sender, hashMsg));
                    let sender_addr = pk_signer.address();
                    let mut buf = Vec::with_capacity(20 + 32);
                    buf.extend_from_slice(sender_addr.as_slice());
                    buf.extend_from_slice(hash_msg.as_slice());
                    let hash_sender = keccak256(&buf);

                    // uint256 signed = bridge_instance.numMessagesSigned(hashMsg);
                    let signed: U256 = bridge_instance
                        .numMessagesSigned(hash_msg)
                        .call()
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                    // Check 1: require(!isAlreadyProcessed(signed));
                    if signed >= required {
                        tracing::info!(
                            "XDAI_GC: message already has {signed} >= required {required} signatures, skipping submitSignature"
                        );
                        self.delete_event_log(event_log_id).await?;
                        return Ok(());
                    }

                    // Check 2: require(!bridge_instance.messagesSigned(hashSender));
                    let already_signed_by_validator = bridge_instance
                        .messagesSigned(hash_sender)
                        .call()
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;
                    if already_signed_by_validator {
                        tracing::info!(
                            "XDAI_GC: validator {} already signed this message, skipping",
                            sender_addr
                        );
                        self.delete_event_log(event_log_id).await?;
                        return Ok(());
                    }

                    // Submit the signature transaction
                    let submit_result = async {
                        let submit_signature_tx = bridge_instance
                            .submitSignature(calldata.signature, calldata.message.clone())
                            .send()
                            .await
                            .map_err(|e| {
                                tracing::error!(
                                    "Failed to send submitSignature transaction: {}",
                                    e
                                );
                                BridgeValidatorError::TxSubmit(e.to_string())
                            })?;

                        let submit_signature_receipt =
                            submit_signature_tx.get_receipt().await.map_err(|e| {
                                tracing::error!("Failed to get receipt for submitSignature: {}", e);
                                BridgeValidatorError::TxReceipt(e.to_string())
                            })?;

                        tracing::info!(
                            "submit_signature_tx_hash {:?}",
                            submit_signature_receipt.transaction_hash
                        );
                        Ok::<(), BridgeValidatorError>(())
                    }
                    .await;

                    // If submit failed, increment retry count and return
                    if submit_result.is_err() {
                        self.increment_retry_count(event_log_id).await?;
                        return Ok(());
                    }

                    if self.config.xdai_execute_message_on_foreign != "true" {
                        // Not executing on foreign chain, delete after submitSignature success
                        self.delete_event_log(event_log_id).await?;
                        return Ok(());
                    }

                    // Mark stage as 'foreign' before attempting foreign execution
                    self.update_stage(event_log_id, "foreign").await?;
                }

                // Stage "foreign": execute on foreign chain
                // (reached either after fresh submitSignature or on retry with stage='foreign')
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
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                    let signatures = bridge_helper_instance
                        .getSignatures(msg_hash)
                        .call()
                        .await
                        .map_err(|e| BridgeValidatorError::ContractCall(e.to_string()))?;

                    if signatures.len() == (2 + 65 * required.to::<usize>() - 1) {
                        // Create provider for Ethereum (foreign chain)
                        let eth_provider = ProviderBuilder::new()
                            .wallet(pk_signer.clone())
                            .connect(self.config.get_eth_rpc())
                            .await
                            .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()))?;

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

                        match foreign_bridge_instance
                            .executeSignatures(calldata.message.clone(), signatures.clone())
                            .send()
                            .await
                        {
                            Ok(execute_signature_tx) => {
                                match execute_signature_tx.get_receipt().await {
                                    Ok(execute_signature_receipt) => {
                                        tracing::info!(
                                            "XDAI executeSignatures on foreign chain: {:?}",
                                            execute_signature_receipt.transaction_hash
                                        );
                                        self.delete_event_log(event_log_id).await?;
                                    }
                                    Err(e) => {
                                        tracing::error!(
                                            "Failed to get receipt for XDAI foreign execution: {}",
                                            e
                                        );
                                        self.increment_retry_count(event_log_id).await?;
                                    }
                                }
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
                                self.increment_retry_count(event_log_id).await?;
                            }
                        }
                    } else {
                        tracing::warn!(
                            "Not enough signatures collected yet: {} bytes, expected: {}",
                            signatures.len(),
                            2 + 65 * required.to::<usize>() - 1
                        );
                        self.delete_event_log(event_log_id).await?;
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
    pub fn parse_xdai_message(message: &Bytes) -> Result<ParsedXdaiMessage, BridgeValidatorError> {
        // 20 + 32 + 32 + 20 + 20 = 124 bytes
        if message.len() != 124 {
            return Err(BridgeValidatorError::ParseXdaiMessage(format!(
                "unexpected length: {}",
                message.len()
            )));
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

    /// Update the processing stage for an event log
    async fn update_stage(
        &self,
        event_log_id: i32,
        stage: &str,
    ) -> Result<(), BridgeValidatorError> {
        sqlx::query(
            r#"
            UPDATE event_logs
            SET stage = $1
            WHERE id = $2
            "#,
        )
        .bind(stage)
        .bind(event_log_id)
        .execute(&self.db_pool)
        .await?;

        tracing::info!(
            "Updated stage to '{}' for event_log id: {}",
            stage,
            event_log_id
        );
        Ok(())
    }

    /// Increment retry count for an event log in the database
    pub async fn increment_retry_count(
        &self,
        event_log_id: i32,
    ) -> Result<(), BridgeValidatorError> {
        sqlx::query(
            r#"
            UPDATE event_logs
            SET retry_count = retry_count + 1, is_processed = 'false'
            WHERE id = $1
            "#,
        )
        .bind(event_log_id)
        .execute(&self.db_pool)
        .await?;

        tracing::info!("Incremented retry_count for event_log id: {}", event_log_id);
        Ok(())
    }

    /// Delete an event log from the database
    pub async fn delete_event_log(&self, event_log_id: i32) -> Result<(), BridgeValidatorError> {
        sqlx::query(
            r#"
            DELETE FROM event_logs
            WHERE id = $1
            "#,
        )
        .bind(event_log_id)
        .execute(&self.db_pool)
        .await?;

        tracing::debug!("Deleted event_log id: {}", event_log_id);
        Ok(())
    }
}

#[derive(Debug)]
pub struct ParsedXdaiMessage {
    recipient: Address,
    value: U256,
    nonce: FixedBytes<32>,
    bridge: Address,
    token: Address,
}
