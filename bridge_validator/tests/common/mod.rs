use alloy::primitives::{address, b256, Address, Bytes, FixedBytes, LogData};
use alloy::rpc::types::Log;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::sync::{Arc, Mutex};
use testcontainers::{core::WaitFor, runners::AsyncRunner, ContainerAsync, Image};
use tokio::sync::OnceCell;

// Custom Postgres image definition for testcontainers 0.23
#[derive(Debug, Default, Clone)]
struct PostgresImage {}

impl Image for PostgresImage {
    fn name(&self) -> &str {
        "postgres"
    }

    fn tag(&self) -> &str {
        "16-alpine"
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
                eprintln!("Using external database from DATABASE_URL: {}", database_url);

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
                        eprintln!(
                            "Failed to connect, retrying... ({} attempts left)",
                            retries
                        );
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

/// Creates a test database pool with migrations applied
/// Automatically spins up a PostgreSQL container using testcontainers (once for all tests)
/// The container will be automatically cleaned up when the test process exits
pub async fn setup_test_db() -> PgPool {
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

    // Note: We don't delete all data here to allow parallel test execution
    // Each test should use unique identifiers and clean up its own data

    pool
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

pub mod mock_provider;
pub use mock_provider::create_mock_provider;
