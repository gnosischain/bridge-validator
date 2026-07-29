#[cfg(test)]
mod tests;

use alloy::primitives::Address;
use alloy_primitives::Log;

use std::env;

/// Which block a chain's indexers treat as the safe upper bound to index up to.
///
/// `BlockFinality` is the historical (and default) behaviour: only finalized
/// blocks are indexed, so a stored log can never be reorged out. `Fcr` caps at
/// the execution layer's `safe` tag instead — roughly an order of magnitude
/// faster (~12s vs ~12.8m) at the cost of a reorg window that
/// `service::fcr_checker` closes after the fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockProcessingMode {
    Fcr,
    #[default]
    BlockFinality,
}

impl BlockProcessingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockProcessingMode::Fcr => "fcr",
            BlockProcessingMode::BlockFinality => "block-finality",
        }
    }

    pub fn is_fcr(&self) -> bool {
        matches!(self, BlockProcessingMode::Fcr)
    }

    /// Parse a mode from its env-var spelling. Unknown values are rejected
    /// rather than silently defaulted: a typo'd `ETH_BLOCK_PROCESSING_MODE`
    /// must not quietly hand back the conservative mode an operator believes
    /// they turned off.
    fn parse(var: &str, value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            // An empty value is the same as not setting the var at all.
            "" => Ok(Self::default()),
            "fcr" => Ok(BlockProcessingMode::Fcr),
            "block-finality" | "block_finality" => Ok(BlockProcessingMode::BlockFinality),
            other => Err(format!(
                "Invalid {}: '{}' (expected 'fcr' or 'block-finality')",
                var, other
            )),
        }
    }

    fn from_env(var: &str) -> Result<Self, String> {
        match env::var(var) {
            Ok(value) => Self::parse(var, &value),
            Err(_) => Ok(Self::default()),
        }
    }
}

impl std::fmt::Display for BlockProcessingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub eth_rpc: Vec<String>,
    pub gc_rpc: Vec<String>,
    pub eth_bc_rpc: Vec<String>,
    pub gc_bc_rpc: Vec<String>,
    pub xdai_validator_private_key: Option<String>,
    pub amb_validator_private_key: Option<String>,
    pub eth_amb_bridge_address: Address,
    pub gc_amb_bridge_address: Address,
    pub eth_xdai_bridge_address: Address,
    pub gc_xdai_bridge_address: Address,
    pub xdai_execute_message_on_foreign: String,
    pub amb_execute_message_on_foreign: String,
    pub xdai_bridge_helper_address: Address,
    pub amb_bridge_helper_address: Address,
    pub poll_interval_secs: u64,
    pub max_retry_count: u64,
    pub eth_block_processing_mode: BlockProcessingMode,
    pub gc_block_processing_mode: BlockProcessingMode,
}

impl Config {
    /// Parse comma-separated RPC URLs from environment variable
    /// Example: "https://rpc1.com,https://rpc2.com,https://rpc3.com"
    /// Returns: vec!["https://rpc1.com", "https://rpc2.com", "https://rpc3.com"]
    fn parse_rpc_urls(value: String) -> Vec<String> {
        value
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }

    /// Get the primary ETH RPC URL (first in the list)
    pub fn get_eth_rpc(&self) -> &str {
        &self.eth_rpc[0]
    }

    /// Get the primary GC RPC URL (first in the list)
    pub fn get_gc_rpc(&self) -> &str {
        &self.gc_rpc[0]
    }

    /// Get the primary ETH Beacon Chain RPC URL (first in the list), if set
    pub fn get_eth_bc_rpc(&self) -> Option<&str> {
        self.eth_bc_rpc.first().map(|s| s.as_str())
    }

    /// Get the primary GC Beacon Chain RPC URL (first in the list), if set
    pub fn get_gc_bc_rpc(&self) -> Option<&str> {
        self.gc_bc_rpc.first().map(|s| s.as_str())
    }

    /// Block processing mode for a chain (`"eth"` / `"gc"`). All indexers on a
    /// chain share its mode, so both bridges on that side move together.
    /// An unrecognised chain falls back to the conservative default.
    pub fn mode_for_chain(&self, chain: &str) -> BlockProcessingMode {
        match chain {
            "eth" => self.eth_block_processing_mode,
            "gc" => self.gc_block_processing_mode,
            _ => BlockProcessingMode::default(),
        }
    }

    /// Set the mode for a chain. Used by the startup preflight to downgrade a
    /// chain to `block-finality` when no configured RPC can serve `safe`.
    pub fn set_mode_for_chain(&mut self, chain: &str, mode: BlockProcessingMode) {
        match chain {
            "eth" => self.eth_block_processing_mode = mode,
            "gc" => self.gc_block_processing_mode = mode,
            _ => tracing::warn!(
                "Ignoring block processing mode for unknown chain '{}'",
                chain
            ),
        }
    }

    /// Chains currently running in fcr mode, with their EL RPC arrays.
    /// Empty when every chain is on `block-finality` — the fcr checker uses
    /// this to decide whether it needs to run at all.
    pub fn fcr_chains(&self) -> Vec<(&'static str, &[String])> {
        let mut chains = Vec::new();
        if self.eth_block_processing_mode.is_fcr() {
            chains.push(("eth", self.eth_rpc.as_slice()));
        }
        if self.gc_block_processing_mode.is_fcr() {
            chains.push(("gc", self.gc_rpc.as_slice()));
        }
        chains
    }

    /// Beacon RPC (if configured) + EL RPC fallbacks for a chain, in the order
    /// the finality resolver should try them.
    pub fn finality_rpcs_for_chain(&self, chain: &str) -> (Option<&str>, &[String]) {
        match chain {
            "eth" => (self.get_eth_bc_rpc(), self.eth_rpc.as_slice()),
            _ => (self.get_gc_bc_rpc(), self.gc_rpc.as_slice()),
        }
    }

    /// Bridge modes (the `event_logs.bridge_mode` values) belonging to a chain.
    pub fn bridge_modes_for_chain(chain: &str) -> [&'static str; 2] {
        match chain {
            "eth" => ["AMB_ETH", "XDAI_ETH"],
            _ => ["AMB_GC", "XDAI_GC"],
        }
    }

    pub fn from_env() -> Result<Self, String> {
        let eth_rpc: Vec<String> = Self::parse_rpc_urls(
            env::var("ETH_RPC").map_err(|err| format!("Error reading ETH_RPC: {}", err))?,
        );
        let gc_rpc: Vec<String> = Self::parse_rpc_urls(
            env::var("GC_RPC").map_err(|err| format!("Error reading GC_RPC: {}", err))?,
        );
        let eth_bc_rpc: Vec<String> = env::var("ETH_BC_RPC")
            .map(Self::parse_rpc_urls)
            .unwrap_or_default();
        let gc_bc_rpc: Vec<String> = env::var("GC_BC_RPC")
            .map(Self::parse_rpc_urls)
            .unwrap_or_default();

        // Validate that at least one RPC URL is provided for each
        if eth_rpc.is_empty() {
            return Err("ETH_RPC must contain at least one valid URL".to_string());
        }
        if gc_rpc.is_empty() {
            return Err("GC_RPC must contain at least one valid URL".to_string());
        }

        if eth_bc_rpc.is_empty() {
            tracing::warn!("ETH_BC_RPC not set, will use ETH_RPC for finality checks");
        }
        if gc_bc_rpc.is_empty() {
            tracing::warn!("GC_BC_RPC not set, will use GC_RPC for finality checks");
        }

        Ok(Config {
            eth_rpc,
            gc_rpc,
            eth_bc_rpc,
            gc_bc_rpc,
            xdai_validator_private_key: env::var("XDAI_VALIDATOR_PRIV_KEY")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .map_err(|err| format!("Error reading env: {}", err))?,
            amb_validator_private_key: env::var("AMB_VALIDATOR_PRIV_KEY")
                .ok()
                .map(|s| s.parse())
                .transpose()
                .map_err(|err| format!("Error reading env: {}", err))?,
            eth_amb_bridge_address: env::var("ETH_AMB_BRIDGE_ADDRESS")
                .unwrap_or_else(|_| "0x4C36d2919e407f0Cc2Ee3c993ccF8ac26d9CE64e".to_string())
                .parse()
                .map_err(|err| format!("Error parsing ETH_AMB_BRIDGE_ADDRESS: {}", err))?,
            gc_amb_bridge_address: env::var("GC_AMB_BRIDGE_ADDRESS")
                .unwrap_or_else(|_| "0x75Df5AF045d91108662D8080fD1FEFAd6aA0bb59".to_string())
                .parse()
                .map_err(|err| format!("Error parsing GC_AMB_BRIDGE_ADDRESS: {}", err))?,
            eth_xdai_bridge_address: env::var("ETH_XDAI_BRIDGE_ADDRESS")
                .unwrap_or_else(|_| "0x4aa42145Aa6Ebf72e164C9bBC74fbD3788045016".to_string())
                .parse()
                .map_err(|err| format!("Error parsing ETH_XDAI_BRIDGE_ADDRESS: {}", err))?,
            gc_xdai_bridge_address: env::var("GC_XDAI_BRIDGE_ADDRESS")
                .unwrap_or_else(|_| "0x7301CFA0e1756B71869E93d4e4Dca5c7d0eb0AA6".to_string())
                .parse()
                .map_err(|err| format!("Error parsing GC_XDAI_BRIDGE_ADDRESS: {}", err))?,
            xdai_execute_message_on_foreign: env::var("XDAI_EXECUTE_MESSAGE_ON_FOREIGN")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .map_err(|err| format!("Error parsing XDAI_EXECUTE_MESSAGE_ON_FOREIGN: {}", err))?,
            amb_execute_message_on_foreign: env::var("AMB_EXECUTE_MESSAGE_ON_FOREIGN")
                .unwrap_or_else(|_| "false".to_string())
                .parse()
                .map_err(|err| format!("Error parsing AMB_EXECUTE_MESSAGE_ON_FOREIGN: {}", err))?,
            xdai_bridge_helper_address: env::var("XDAI_BRIDGE_HELPER_ADDRESS")
                .unwrap_or_else(|_| "0xe30269bc61E677cD60aD163a221e464B7022fbf5".to_string())
                .parse()
                .map_err(|err| format!("Error parsing XDAI_BRIDGE_HELPER_ADDRESS: {}", err))?,
            amb_bridge_helper_address: env::var("AMB_BRIDGE_HELPER_ADDRESS")
                .unwrap_or_else(|_| "0x7d94ece17e81355326e3359115D4B02411825EdD".to_string())
                .parse()
                .map_err(|err| format!("Error parsing AMB_BRIDGE_HELPER_ADDRESS: {}", err))?,
            poll_interval_secs: env::var("POLL_INTERVAL_SECS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(5),
            max_retry_count: env::var("MAX_RETRY_COUNT")
                .unwrap_or_else(|_| "5".to_string())
                .parse()
                .unwrap_or(5),
            eth_block_processing_mode: BlockProcessingMode::from_env("ETH_BLOCK_PROCESSING_MODE")?,
            gc_block_processing_mode: BlockProcessingMode::from_env("GC_BLOCK_PROCESSING_MODE")?,
        })
    }
}
