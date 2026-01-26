use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::sol;
sol! {
    #[sol(rpc)]
    contract AMB_BRIDGE {
        event UserRequestForAffirmation(bytes32 indexed messageId, bytes encodedData);
        event UserRequestForSignature(bytes32 indexed messageId, bytes encodedData);
        function executeAffirmation(bytes message);
        function submitSignature(bytes signature, bytes message);
    }
    #[sol(rpc)]
    contract XDAI_BRIDGE{
        event UserRequestForAffirmation(address recipient, uint256 value, bytes32 nonce);
        event UserRequestForSignature(address recipient, uint256 value, bytes32 nonce, address token);
        function executeAffirmation(address recipient, uint256 value, bytes32 nonce);
        function submitSignature(bytes signature, bytes message);
    }
}

#[derive(Debug)]
pub enum OnChainCallData {
    AmbEth {
        contract_address: Address,
        calldata: AmbEthCalldata,
    },
    AmbGc {
        contract_address: Address,
        calldata: AmbGcCalldata,
    },
    XdaiEth {
        contract_address: Address,
        calldata: XdaiEthCalldata,
    },
    XdaiGc {
        contract_address: Address,
        calldata: XdaiGcCalldata,
    },
}

#[derive(Debug)]
pub struct AmbEthCalldata {
    pub message: Bytes,
}

#[derive(Debug)]
pub struct AmbGcCalldata {
    pub signature: Bytes,
    pub message: Bytes,
}
#[derive(Debug)]
pub struct XdaiEthCalldata {
    pub recipient: Address,
    pub value: U256,
    pub nonce: FixedBytes<32>,
}
#[derive(Debug)]
pub struct XdaiGcCalldata {
    pub signature: Bytes,
    pub message: Bytes,
}
