mod config;
mod contracts;
mod rpc_provider;
mod service;

use crate::contracts::OnChainCallData;
use crate::rpc_provider::setup_provider;
use crate::service::event_indexer::EventIndexer;
use crate::service::msg_processor::{MessageProcessor, SenderData};
use crate::service::on_chain_sender::OnChainSender;

use config::Config;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::mpsc::{self};
use tracing;
use tracing_subscriber::{self, EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    dotenv::dotenv().ok();
    let config = Config::from_env()?;

    // Initialize database connection pool
    let database_url =
        std::env::var("DATABASE_URL").expect("DATABASE_URL must be set in .env file");

    let pool = PgPoolOptions::new()
        .max_connections(10)
        .test_before_acquire(true)
        .connect(&database_url)
        .await?;

    tracing::info!("Database connection established");

    // Run migrations automatically
    tracing::info!("Running database migrations...");
    match sqlx::migrate!("./migrations").run(&pool).await {
        Ok(_) => tracing::info!("Migrations completed successfully"),
        Err(e) => {
            tracing::error!("Migration failed: {:?}", e);
            return Err(e.into());
        }
    }
    // Verify the table exists
    let table_check = sqlx::query("SELECT to_regclass('public.event_logs')")
        .fetch_one(&pool)
        .await?;
    tracing::info!("Verified event_logs table exists");

    // Add a small delay to ensure everything is ready
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    // Initiating services
    let indexer_eth_amb = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "eth").await?,
        "ETHAmb".to_string(),
        "UserRequestForAffirmation(bytes32,bytes)".to_string(),
        config.eth_amb_bridge_address,
        pool.clone(),
    );

    let indexer_gc_amb = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "gc").await?,
        "GCAmb".to_string(),
        "UserRequestForSignature(bytes32,bytes)".to_string(),
        config.gc_amb_bridge_address,
        pool.clone(),
    );

    let indexer_eth_xdai = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "eth").await?,
        "ETHXdai".to_string(),
        "UserRequestForAffirmation(address,uint256,bytes32)".to_string(),
        config.eth_xdai_bridge_address,
        pool.clone(),
    );

    let indexer_gc_xdai = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "gc").await?,
        "GCXdai".to_string(),
        "UserRequestForSignature(address,uint256,bytes32,address)".to_string(),
        config.gc_xdai_bridge_address,
        pool.clone(),
    );

    let (tx, rx) = mpsc::channel::<SenderData>(32);

    let msg_processor_1 = MessageProcessor::new(config.clone(), pool.clone(), tx.clone());
    let msg_processor_2 = MessageProcessor::new(config.clone(), pool.clone(), tx.clone());

    let on_chain_sender = OnChainSender::new(config.clone(), pool.clone(), rx);

    tokio::join!(
        indexer_eth_amb.start(),
        indexer_eth_xdai.start(),
        indexer_gc_amb.start(),
        indexer_gc_xdai.start(),
        msg_processor_1.start(),
        msg_processor_2.start(),
        on_chain_sender.start()
    );

    Ok(())
}
