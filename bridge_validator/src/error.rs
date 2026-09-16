use reqwest::StatusCode;

#[derive(Debug, thiserror::Error)]
pub enum BridgeValidatorError {
    #[error("missing required env var: {0}")]
    MissingEnv(&'static str),
    #[error("config error: {0}")]
    Config(String),

    #[error("invalid chain: {0}")]
    InvalidChain(String),
    #[error("invalid RPC URL: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("no working RPC endpoints")]
    NoWorkingEndpoint,
    #[error("RPC connect failed: {0}")]
    RpcConnect(String),

    #[error("invalid {field} length: expected {expected} bytes, got {actual}")]
    InvalidFieldLength {
        field: &'static str,
        expected: usize,
        actual: usize,
    },

    #[error("failed to parse private key: {0}")]
    KeyParse(String),
    #[error("failed to sign message: {0}")]
    Sign(String),

    #[error("hex decode failed: {0}")]
    HexDecode(#[from] alloy::hex::FromHexError),
    #[error("failed to parse xDai message: {0}")]
    ParseXdaiMessage(String),
    #[error("log decode failed: {0}")]
    LogDecode(#[from] alloy::sol_types::Error),
    #[error("invalid log JSON: {0}")]
    LogJson(#[from] serde_json::Error),

    #[error("contract call failed: {0}")]
    ContractCall(String),
    #[error("transaction submission failed: {0}")]
    TxSubmit(String),
    #[error("transaction receipt failed: {0}")]
    TxReceipt(String),

    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("beacon RPC returned HTTP {0}")]
    BeaconHttpStatus(StatusCode),
    #[error("EL RPC returned HTTP {0}")]
    ElHttpStatus(StatusCode),
    #[error("empty or invalid response from EL RPC")]
    EmptyElResponse,
    #[error("all RPC endpoints failed for finalized block")]
    AllRpcsFailedForFinalizedBlock,
    #[error("all RPC endpoints failed for safe block")]
    AllRpcsFailedForSafeBlock,
    #[error("all RPC endpoints failed to look up block {0}")]
    AllRpcsFailedForBlockLookup(i64),

    #[error("RPC error: {0}")]
    Rpc(String),
    #[error("failed to parse integer: {0}")]
    ParseInt(#[from] std::num::ParseIntError),
    #[error("channel send failed: {0}")]
    ChannelSend(String),

    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("database migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

pub type Result<T> = std::result::Result<T, BridgeValidatorError>;
