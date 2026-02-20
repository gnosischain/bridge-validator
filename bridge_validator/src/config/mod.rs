#[cfg(test)]
mod tests;

use alloy::primitives::Address;
use alloy_primitives::Log;

use std::env;

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

    /// Get the primary ETH Beacon Chain RPC URL (first in the list)
    pub fn get_eth_bc_rpc(&self) -> &str {
        &self.eth_bc_rpc[0]
    }

    /// Get the primary GC Beacon Chain RPC URL (first in the list)
    pub fn get_gc_bc_rpc(&self) -> &str {
        &self.gc_bc_rpc[0]
    }

    pub fn from_env() -> Result<Self, String> {
        let eth_rpc: Vec<String> = Self::parse_rpc_urls(
            env::var("ETH_RPC").map_err(|err| format!("Error reading ETH_RPC: {}", err))?,
        );
        let gc_rpc: Vec<String> = Self::parse_rpc_urls(
            env::var("GC_RPC").map_err(|err| format!("Error reading GC_RPC: {}", err))?,
        );
        let eth_bc_rpc: Vec<String> = Self::parse_rpc_urls(
            env::var("ETH_BC_RPC").map_err(|err| format!("Error reading ETH_BC_RPC: {}", err))?,
        );
        let gc_bc_rpc: Vec<String> = Self::parse_rpc_urls(
            env::var("GC_BC_RPC").map_err(|err| format!("Error reading GC_BC_RPC: {}", err))?,
        );

        // Validate that at least one RPC URL is provided for each
        if eth_rpc.is_empty() {
            return Err("ETH_RPC must contain at least one valid URL".to_string());
        }
        if gc_rpc.is_empty() {
            return Err("GC_RPC must contain at least one valid URL".to_string());
        }
        if eth_bc_rpc.is_empty() {
            return Err("ETH_BC_RPC must contain at least one valid URL".to_string());
        }
        if gc_bc_rpc.is_empty() {
            return Err("GC_BC_RPC must contain at least one valid URL".to_string());
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
        })
    }
}
