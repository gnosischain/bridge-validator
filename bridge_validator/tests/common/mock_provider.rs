use alloy::providers::{Provider, ProviderBuilder};
use alloy::transports::mock::Asserter;

/// Creates a mock provider for testing using Alloy's mock transport
/// Returns a tuple of (provider, asserter) where the asserter can be used
/// to push expected responses
pub fn create_mock_provider() -> (impl Provider, Asserter) {
    let asserter: Asserter = Asserter::new();
    let provider: alloy::providers::fillers::FillProvider<
        alloy::providers::fillers::JoinFill<
            alloy::providers::Identity,
            alloy::providers::fillers::JoinFill<
                alloy::providers::fillers::GasFiller,
                alloy::providers::fillers::JoinFill<
                    alloy::providers::fillers::BlobGasFiller,
                    alloy::providers::fillers::JoinFill<
                        alloy::providers::fillers::NonceFiller,
                        alloy::providers::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        alloy::providers::RootProvider,
    > = ProviderBuilder::new().connect_mocked_client(asserter.clone());
    (provider, asserter)
}
