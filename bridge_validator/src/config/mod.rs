use alloy::primitives::Address;
use std::env;

#[derive(Clone, Debug)]
pub struct Config {
    pub eth_rpc: String,
    pub gc_rpc: String,
    pub eth_bc_rpc: String,
    pub gc_bc_rpc: String,
    pub xdai_validator_private_key: Option<String>,
    pub amb_validator_private_key: Option<String>,
    pub eth_amb_bridge_address: Address,
    pub gc_amb_bridge_address: Address,
    pub eth_xdai_bridge_address: Address,
    pub gc_xdai_bridge_address: Address,
    pub poll_interval_secs: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        Ok(Config {
            eth_rpc: env::var("ETH_RPC").map_err(|err| format!("Error reading env: {}", err))?,
            gc_rpc: env::var("GC_RPC").map_err(|err| format!("Error reading env: {}", err))?,
            eth_bc_rpc: env::var("ETH_BC_RPC")
                .map_err(|err| format!("Error reading env: {}", err))?,
            gc_bc_rpc: env::var("GC_BC_RPC")
                .map_err(|err| format!("Error reading env: {}", err))?,
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
            poll_interval_secs: env::var("POLL_INTERVAL_SECS")
                .unwrap_or_else(|_| "10".to_string())
                .parse()
                .unwrap_or(5),
        })
    }
}
