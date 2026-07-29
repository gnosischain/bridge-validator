mod config;
mod contracts;
mod error;
mod rpc_provider;
mod service;

use crate::contracts::OnChainCallData;
use crate::error::BridgeValidatorError;
use crate::rpc_provider::setup_provider;
use crate::service::event_indexer::EventIndexer;
use crate::service::fcr_checker::FcrChecker;
use crate::service::msg_processor::{MessageProcessor, SenderData};
use crate::service::on_chain_sender::OnChainSender;

use config::Config;
use sqlx::postgres::PgPoolOptions;
use tokio::sync::{mpsc, watch};
use tracing;
use tracing_subscriber::{self, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[tokio::main]
async fn main() -> Result<(), BridgeValidatorError> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer())
        .init();

    dotenv::dotenv().ok();
    let mut config = Config::from_env().map_err(BridgeValidatorError::Config)?;

    // Verify at boot that every chain configured for fcr can actually be
    // served `safe` by its RPC array. A safe-incapable RPC would otherwise
    // degrade that chain to finality for the whole process lifetime while the
    // operator believes they have ~12s confirmation. Chains that fail are
    // downgraded explicitly and loudly here.
    crate::service::safe::run_fcr_preflight(&mut config, &reqwest::Client::new()).await;
    let config = config;

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

    // Shutdown signal: broadcast to all services on SIGTERM/SIGINT
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm =
                signal(SignalKind::terminate()).expect("Failed to create SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Received SIGINT (Ctrl+C), initiating graceful shutdown...");
                }
                _ = sigterm.recv() => {
                    tracing::info!("Received SIGTERM, initiating graceful shutdown...");
                }
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            tracing::info!("Received Ctrl+C, initiating graceful shutdown...");
        }
        let _ = shutdown_tx.send(true);
    });

    // Initiating services
    let indexer_eth_amb = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "eth").await?,
        "ETHAmb".to_string(),
        "UserRequestForAffirmation(bytes32,bytes)".to_string(),
        config.eth_amb_bridge_address,
        pool.clone(),
        shutdown_rx.clone(),
    );

    let indexer_gc_amb = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "gc").await?,
        "GCAmb".to_string(),
        "UserRequestForSignature(bytes32,bytes)".to_string(),
        config.gc_amb_bridge_address,
        pool.clone(),
        shutdown_rx.clone(),
    );

    let indexer_eth_xdai = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "eth").await?,
        "ETHXdai".to_string(),
        "UserRequestForAffirmation(address,uint256,bytes32)".to_string(),
        config.eth_xdai_bridge_address,
        pool.clone(),
        shutdown_rx.clone(),
    );

    let indexer_gc_xdai = EventIndexer::new(
        config.clone(),
        setup_provider(&config, "gc").await?,
        "GCXdai".to_string(),
        "UserRequestForSignature(address,uint256,bytes32,address)".to_string(),
        config.gc_xdai_bridge_address,
        pool.clone(),
        shutdown_rx.clone(),
    );

    let (tx, rx) = mpsc::channel::<SenderData>(32);

    let msg_processor_1 = MessageProcessor::new(
        config.clone(),
        pool.clone(),
        tx.clone(),
        shutdown_rx.clone(),
    );
    let msg_processor_2 = MessageProcessor::new(
        config.clone(),
        pool.clone(),
        tx.clone(),
        shutdown_rx.clone(),
    );

    let on_chain_sender = OnChainSender::new(config.clone(), pool.clone(), rx);

    // Re-checks every safe-processed block once it finalizes. Exits immediately
    // when no chain is in fcr mode.
    let fcr_checker = FcrChecker::new(config.clone(), pool.clone(), shutdown_rx.clone());

    // Drop the original sender so the channel closes once both MessageProcessors stop.
    // OnChainSender will drain remaining messages, then exit.
    drop(tx);

    tokio::join!(
        indexer_eth_amb.start(),
        indexer_eth_xdai.start(),
        indexer_gc_amb.start(),
        indexer_gc_xdai.start(),
        msg_processor_1.start(),
        msg_processor_2.start(),
        on_chain_sender.start(),
        fcr_checker.start()
    );

    tracing::info!("All services stopped, shutting down");
    Ok(())
}
