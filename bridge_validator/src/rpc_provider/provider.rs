use crate::config::Config;
use crate::error::BridgeValidatorError;
use alloy::{
    providers::{Provider, ProviderBuilder},
    rpc::client::RpcClient,
    transports::{
        http::{reqwest::Url, Http},
        layers::FallbackLayer,
    },
};
use std::num::NonZeroUsize;
use tower::ServiceBuilder;

pub async fn setup_provider(
    config: &Config,
    chain: &str,
) -> Result<impl Provider, BridgeValidatorError> {
    // Select which RPC array to use based on chain
    let rpc_urls = match chain {
        "eth" => &config.eth_rpc,
        "gc" => &config.gc_rpc,
        _ => return Err(BridgeValidatorError::InvalidChain(chain.to_string())),
    };

    // If only one RPC, use simple connection (no fallback needed)
    if rpc_urls.len() == 1 {
        return ProviderBuilder::new()
            .connect(&rpc_urls[0])
            .await
            .map_err(|e| BridgeValidatorError::RpcConnect(e.to_string()));
    }

    let mut transports = Vec::new();
    for url in rpc_urls {
        let parsed_url = Url::parse(url)?;
        transports.push(Http::new(parsed_url));
    }

    // Configure the fallback layer with active transport count (max 3 or less if fewer RPCs)
    // Refer to https://alloy.rs/examples/layers/fallback_layer
    let active_count = transports.len().min(3);
    let fallback_layer = FallbackLayer::default()
        .with_active_transport_count(NonZeroUsize::new(active_count).unwrap());

    let transport = ServiceBuilder::new()
        .layer(fallback_layer)
        .service(transports);

    let client = RpcClient::builder().transport(transport, false);

    // Create provider with the client
    let provider = ProviderBuilder::new().connect_client(client);

    Ok(provider)
}
