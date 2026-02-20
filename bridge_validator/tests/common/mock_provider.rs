use alloy::primitives::{bytes, Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::transports::mock::Asserter;

/// Creates a mock provider for testing using Alloy's mock transport
/// Returns a tuple of (provider, asserter) where the asserter can be used
/// to push expected responses
pub fn create_mock_provider() -> (impl Provider, Asserter) {
    let asserter = Asserter::new();
    let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());
    (provider, asserter)
}

// https://github.com/alloy-rs/alloy/blob/main/crates/provider/tests/it/mock.rs
#[tokio::test]
async fn mocked_default_provider() {
    let asserter = Asserter::new();
    let provider = ProviderBuilder::new().connect_mocked_client(asserter.clone());

    asserter.push_success(&21965802);
    asserter.push_success(&21965803);
    asserter.push_failure_msg("mock test");

    let response = provider.get_block_number().await.unwrap();
    assert_eq!(response, 21965802);

    let response = provider.get_block_number().await.unwrap();
    assert_eq!(response, 21965803);

    let response = provider.get_block_number().await.unwrap_err();
    assert!(response.to_string().contains("mock test"), "{response}");

    let response = provider.get_block_number().await.unwrap_err();
    assert!(
        response
            .to_string()
            .contains("empty asserter response queue"),
        "{response}"
    );
    assert!(
        response.to_string().contains("eth_blockNumber"),
        "{response}"
    );
    assert!(response.to_string().contains("3"), "{response}");

    let accounts = [Address::with_last_byte(1), Address::with_last_byte(2)];
    asserter.push_success(&accounts);
    let response = provider.get_accounts().await.unwrap();
    assert_eq!(response, accounts);

    let call_resp = bytes!("12345678");
    asserter.push_success(&call_resp);
    let tx = TransactionRequest::default();
    let response = provider.call(tx).await.unwrap();
    assert_eq!(response, call_resp);

    let assert_bal = U256::from(123456780);
    asserter.push_success(&assert_bal);
    let response = provider.get_balance(Address::default()).await.unwrap();
    assert_eq!(response, assert_bal);
}
