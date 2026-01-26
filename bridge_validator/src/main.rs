mod config;
mod contracts;
mod service;

use crate::contracts::OnChainCallData;
use crate::service::event_indexer::EventIndexer;
use crate::service::msg_processor::MessageProcessor;
use crate::service::on_chain_sender::OnChainSender;
use alloy::providers::ProviderBuilder;

use config::Config;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::mpsc::{self};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv::dotenv().ok();
    let config = Config::from_env()?;

    // Initialize database connection pool
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set in .env file");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    println!("Database connection established");

    // Run migrations automatically
    println!("Running database migrations...");
    sqlx::migrate!("./migrations").run(&pool).await?;
    println!("Migrations completed successfully");

    // Initiating services
    let indexer_eth_amb = EventIndexer::new(
        config.clone(),
        ProviderBuilder::new()
            .connect(&config.clone().eth_rpc)
            .await?,
        "ETHAmb".to_string(),
        "UserRequestForAffirmation(bytes32,bytes)".to_string(),
        config.eth_amb_bridge_address,
        pool.clone(),
    );

    let indexer_gc_amb = EventIndexer::new(
        config.clone(),
        ProviderBuilder::new()
            .connect(&config.clone().gc_rpc)
            .await?,
        "GCAmb".to_string(),
        "UserRequestForSignature(bytes32,bytes)".to_string(),
        config.gc_amb_bridge_address,
        pool.clone(),
    );

    let indexer_eth_xdai = EventIndexer::new(
        config.clone(),
        ProviderBuilder::new()
            .connect(&config.clone().eth_rpc)
            .await?,
        "ETHXdai".to_string(),
        "UserRequestForAffirmation(address,uint256,bytes32)".to_string(),
        config.eth_xdai_bridge_address,
        pool.clone(),
    );

    let indexer_gc_xdai = EventIndexer::new(
        config.clone(),
        ProviderBuilder::new()
            .connect(&config.clone().gc_rpc)
            .await?,
        "GCXdai".to_string(),
        "UserRequestForSignature(address,uint256,bytes32,address)".to_string(),
        config.gc_xdai_bridge_address,
        pool.clone(),
    );

    let (tx, rx) = mpsc::channel::<OnChainCallData>(32);

    let msg_processor = MessageProcessor::new(config.clone(), pool.clone(), tx.clone());

    let on_chain_sender = OnChainSender::new(config.clone(), pool.clone(), rx);

    tokio::join!(
        indexer_eth_amb.start(),
        indexer_eth_xdai.start(),
        indexer_gc_amb.start(),
        indexer_gc_xdai.start(),
        msg_processor.start(),
        on_chain_sender.start()
    );

    Ok(())
}
