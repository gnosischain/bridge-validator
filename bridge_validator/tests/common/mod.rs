use alloy::primitives::{address, b256, Address, Bytes, FixedBytes, LogData};
use alloy::rpc::types::Log;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::{Arc, Mutex, MutexGuard};
use testcontainers::{core::WaitFor, runners::AsyncRunner, ContainerAsync, Image};
use tokio::sync::OnceCell;

// Serializes DB-touching tests so they don't trample each other's state
// (the test_db schema is shared across all tests).
static DB_LOCK: Mutex<()> = Mutex::new(());

// Custom Postgres image definition for testcontainers 0.23
#[derive(Debug, Default, Clone)]
struct PostgresImage {}

impl Image for PostgresImage {
    fn name(&self) -> &str {
        "postgres"
    }

    fn tag(&self) -> &str {
        "18-alpine"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr(
            "database system is ready to accept connections",
        )]
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<
        Item = (
            impl Into<std::borrow::Cow<'_, str>>,
            impl Into<std::borrow::Cow<'_, str>>,
        ),
    > {
        vec![
            ("POSTGRES_DB", "test_db"),
            ("POSTGRES_USER", "postgres"),
            ("POSTGRES_PASSWORD", "postgres"),
        ]
    }

    fn expose_ports(&self) -> &[testcontainers::core::ContainerPort] {
        &[testcontainers::core::ContainerPort::Tcp(5432)]
    }
}

// Struct to hold both container and database URL
struct TestDatabase {
    container: Mutex<Option<ContainerAsync<PostgresImage>>>,
    database_url: String,
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let container = self.container.lock().unwrap();
        if container.is_some() {
            eprintln!("Cleaning up PostgreSQL test container...");
            // The container will be automatically stopped and removed when dropped
            // testcontainers handles this cleanup automatically
        }
    }
}

// Global shared test database (one container for all tests)
// The container will be automatically cleaned up when the program exits
static TEST_DB: OnceCell<Arc<TestDatabase>> = OnceCell::const_new();

/// Initialize the test database container (called once for all tests)
async fn init_test_database() -> Arc<TestDatabase> {
    TEST_DB
        .get_or_init(|| async {
            // Check if DATABASE_URL is set (for CI/CD)
            if let Ok(database_url) = std::env::var("DATABASE_URL") {
                eprintln!(
                    "Using external database from DATABASE_URL: {}",
                    database_url
                );

                // No container needed for external database
                return Arc::new(TestDatabase {
                    container: Mutex::new(None),
                    database_url,
                });
            }

            eprintln!("Setting up PostgreSQL container with testcontainers...");

            let postgres_image = PostgresImage::default();

            // Start the container
            let container = postgres_image
                .start()
                .await
                .expect("Failed to start PostgreSQL container");

            // Get connection details
            let host = container.get_host().await.expect("Failed to get host");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("Failed to get port");

            let database_url = format!("postgres://postgres:postgres@{}:{}/test_db", host, port);

            eprintln!("PostgreSQL container started on {}:{}", host, port);
            eprintln!("Connection string: {}", database_url);

            // Wait for PostgreSQL to be fully ready
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

            // Test connection with retries
            let mut retries = 10;
            loop {
                match PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&database_url)
                    .await
                {
                    Ok(test_pool) => {
                        eprintln!("Successfully connected to PostgreSQL container");
                        test_pool.close().await;
                        break;
                    }
                    Err(e) if retries > 0 => {
                        eprintln!("Failed to connect, retrying... ({} attempts left)", retries);
                        eprintln!("Error: {}", e);
                        retries -= 1;
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                    }
                    Err(e) => panic!("Failed to connect after retries: {}", e),
                }
            }

            eprintln!("PostgreSQL container is ready");
            eprintln!("Note: Container will be automatically cleaned up when tests complete");

            Arc::new(TestDatabase {
                container: Mutex::new(Some(container)),
                database_url,
            })
        })
        .await
        .clone()
}

/// Manually stops and removes the test database container
/// Note: This is optional - testcontainers will automatically clean up on program exit
/// This function explicitly removes the container, which can be useful in CI/CD
/// or when you want to free resources before the test process exits
#[allow(dead_code)]
pub async fn shutdown_test_db() {
    if let Some(test_db) = TEST_DB.get() {
        let mut container = test_db.container.lock().unwrap();
        if container.is_some() {
            eprintln!("Explicitly stopping and removing test database container...");
            // Take the container out and drop it to trigger cleanup
            let removed_container = container.take();
            drop(removed_container);
            eprintln!("Test database container removed successfully");
        } else {
            eprintln!("No container to clean up (using external database or already cleaned up)");
        }
    }
}

/// Creates a test database pool with migrations applied.
///
/// Returns (pool, guard). The guard serializes DB-touching tests via a
/// process-wide mutex so the shared `event_logs` table isn't trampled.
/// Hold the guard for the duration of the test by binding it as `_lock`
/// (or any non-underscore name); dropping it releases the lock for the
/// next DB test.
///
/// Also wipes `event_logs` before returning, so each test starts clean —
/// safe to do because the guard prevents concurrent DB tests.
pub async fn setup_test_db() -> (PgPool, MutexGuard<'static, ()>) {
    // Acquire the cross-test lock FIRST so we don't race with another
    // test's setup/teardown. Ignore poisoning — if a prior test panicked,
    // the DB state is uncertain anyway and the DELETE below cleans up.
    let guard = DB_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Get or initialize the test database
    let test_db = init_test_database().await;
    let database_url = &test_db.database_url;

    eprintln!("Connecting to test database...");

    // Create a new pool for this test
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(std::time::Duration::from_secs(30))
        .test_before_acquire(true)
        .connect(database_url)
        .await
        .expect("Failed to connect to test database");

    eprintln!("Connected successfully");

    // Run migrations (idempotent - safe to run multiple times)
    eprintln!("Running migrations...");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    eprintln!("Migrations completed");

    // Clean slate per test. Safe under the DB_LOCK held above.
    sqlx::query("DELETE FROM event_logs")
        .execute(&pool)
        .await
        .expect("Failed to clean event_logs table");
    sqlx::query("DELETE FROM fcr_false_positives")
        .execute(&pool)
        .await
        .expect("Failed to clean fcr_false_positives table");

    (pool, guard)
}

/// Creates a mock log for testing
pub fn create_test_log(block_number: u64, tx_hash: &str) -> Log {
    Log {
        inner: alloy_primitives::Log {
            address: address!("0x4c36d2919e407f0cc2ee3c993ccf8ac26d9ce64e"),
            data: LogData::new_unchecked(
                vec![b256!(
                    "0x482515ce3d9494a37ce83f18b72b363449458435fafdd7a53ddea7460fe01b58"
                )],
                Bytes::from(vec![0u8; 32]),
            ),
        },
        block_hash: Some(b256!(
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        )),
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: Some(tx_hash.parse().expect("Invalid tx_hash")),
        transaction_index: Some(0),
        log_index: Some(0),
        removed: false,
    }
}

/// Creates a mock log with custom address and topic for testing different bridge modes
pub fn create_test_log_with_address_and_topic(
    block_number: u64,
    tx_hash: &str,
    contract_address: Address,
    topic: [u8; 32],
    log_index: u64,
) -> Log {
    Log {
        inner: alloy_primitives::Log {
            address: contract_address,
            data: LogData::new_unchecked(
                vec![FixedBytes::from(topic)],
                Bytes::from(vec![0u8; 64]), // Some mock data
            ),
        },
        block_hash: Some(b256!(
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        )),
        block_number: Some(block_number),
        block_timestamp: None,
        transaction_hash: Some(tx_hash.parse().expect("Invalid tx_hash")),
        transaction_index: Some(0),
        log_index: Some(log_index),
        removed: false,
    }
}

/// Cleans up test data from database
pub async fn cleanup_test_db(pool: &PgPool) {
    sqlx::query("DELETE FROM event_logs")
        .execute(pool)
        .await
        .expect("Failed to cleanup test database");
}

/// Creates a test configuration for tests without requiring environment variables
pub fn create_test_config() -> worker::config::Config {
    worker::config::Config {
        eth_rpc: vec!["http://localhost:8545".to_string()],
        gc_rpc: vec!["http://localhost:8546".to_string()],
        eth_bc_rpc: vec!["http://localhost:8547".to_string()],
        gc_bc_rpc: vec!["http://localhost:8548".to_string()],
        xdai_validator_private_key: None,
        amb_validator_private_key: None,
        eth_amb_bridge_address: address!("4C36d2919e407f0Cc2Ee3c993ccF8ac26d9CE64e"),
        gc_amb_bridge_address: address!("75Df5AF045d91108662D8080fD1FEFAd6aA0bb59"),
        eth_xdai_bridge_address: address!("4aa42145Aa6Ebf72e164C9bBC74fbD3788045016"),
        gc_xdai_bridge_address: address!("7301CFA0e1756B71869E93d4e4Dca5c7d0eb0AA6"),
        xdai_execute_message_on_foreign: "false".to_string(),
        amb_execute_message_on_foreign: "false".to_string(),
        xdai_bridge_helper_address: address!("e30269bc61E677cD60aD163a221e464B7022fbf5"),
        amb_bridge_helper_address: address!("7d94ece17e81355326e3359115D4B02411825EdD"),
        poll_interval_secs: 1,
        fcr_check_interval_secs: 1,
        max_retry_count: 5,
        eth_block_processing_mode: worker::config::BlockProcessingMode::BlockFinality,
        gc_block_processing_mode: worker::config::BlockProcessingMode::BlockFinality,
    }
}

pub mod mock_provider;
pub use mock_provider::create_mock_provider;
