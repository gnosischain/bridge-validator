use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::sol;
sol! {
    #[sol(rpc)]
    contract AMB_BRIDGE {
        event UserRequestForAffirmation(bytes32 indexed messageId, bytes encodedData);
        event UserRequestForSignature(bytes32 indexed messageId, bytes encodedData);
        function executeAffirmation(bytes message);
        function submitSignature(bytes signature, bytes message);
        function safeExecuteSignaturesWithAutoGasLimit(bytes _data, bytes _signatures) public;
        // Signature / affirmation tracking
        function affirmationsSigned(bytes32 _message) public view returns (bool);
        function numAffirmationsSigned(bytes32 _message) public view returns (uint256);
        function numMessagesSigned(bytes32 _message) public view returns (uint256);
        function messagesSigned(bytes32 _message) public view returns (bool);
        function requiredSignatures() public view returns (uint256);

    }

    #[sol(rpc)]
    contract XDAI_BRIDGE{
        event UserRequestForAffirmation(address recipient, uint256 value, bytes32 nonce);
        event UserRequestForSignature(address recipient, uint256 value, bytes32 nonce, address token);
        function executeAffirmation(address recipient, uint256 value, bytes32 nonce);
        function submitSignature(bytes signature, bytes message);
        function executeSignatures(bytes message, bytes signatures);
        // Signature / affirmation tracking
        function affirmationsSigned(bytes32 _message) public view returns (bool);
        function numAffirmationsSigned(bytes32 _message) public view returns (uint256);
        function numMessagesSigned(bytes32 _message) public view returns (uint256);
        function messagesSigned(bytes32 _message) public view returns (bool);
        function requiredSignatures() public view returns (uint256);
    }


    #[sol(rpc)]
    contract AMB_BRIDGE_HELPER{

    function getSignatures(bytes calldata _message) external view returns(bytes memory);

    }
    #[sol(rpc)]
    contract XDAI_BRIDGE_HELPER{
        function getMessage(bytes32 _msgHash) external view returns (bytes memory result);

        function getMessageHash(address _recipient, uint256 _value, bytes32 _nonce, address _token) external view returns (bytes32);

        function getSignatures(bytes32 _msgHash) external view returns (bytes memory);

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
