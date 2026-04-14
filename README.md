# Bridge Validator

A Rust-based validator service for the Gnosis Chain bridge infrastructure. It monitors bridge events on both Ethereum and Gnosis Chain, validates block finality, signs messages, and submits transactions to relay cross-chain messages for both the **AMB (Arbitrary Message Bridge)** and **xDai** bridges.

## Table of Contents

- [Architecture Overview](#architecture-overview)
- [Components](#components)
- [Prerequisites](#prerequisites)
- [Configuration](#configuration)
- [Setup and Run](#setup-and-run)
- [Database Schema](#database-schema)
- [Bridge Modes](#bridge-modes)
- [Retry and Failure Handling](#retry-and-failure-handling)
- [Graceful Shutdown](#graceful-shutdown)

## Architecture Overview

The validator operates as a pipeline of three service layers that communicate through a PostgreSQL database and an in-memory channel:

```mermaid
flowchart TB
    subgraph Blockchains
        ETH[Ethereum]
        GC[Gnosis Chain]
    end

    subgraph EventIndexers["Event Indexers (4 instances)"]
        IE1["ETH AMB Indexer<br/>UserRequestForAffirmation(bytes32,bytes)"]
        IE2["ETH xDai Indexer<br/>UserRequestForAffirmation(address,uint256,bytes32)"]
        IG1["GC AMB Indexer<br/>UserRequestForSignature(bytes32,bytes)"]
        IG2["GC xDai Indexer<br/>UserRequestForSignature(address,uint256,bytes32,address)"]
    end

    subgraph DB["PostgreSQL"]
        EL[(event_logs table)]
    end

    subgraph MsgProcessors["Message Processors (2 instances)"]
        MP["Read unprocessed events<br/>Check block finality<br/>Decode & sign messages"]
    end

    subgraph Sender["On-Chain Sender (1 instance)"]
        OCS["Pre-flight duplicate checks<br/>Submit transactions<br/>Handle home/foreign stages"]
    end

    ETH -->|poll logs| IE1
    ETH -->|poll logs| IE2
    GC -->|poll logs| IG1
    GC -->|poll logs| IG2

    IE1 -->|insert| EL
    IE2 -->|insert| EL
    IG1 -->|insert| EL
    IG2 -->|insert| EL

    EL -->|"SELECT ... FOR UPDATE SKIP LOCKED"| MP
    MP -->|"mpsc channel (cap 32)"| OCS

    OCS -->|executeAffirmation / submitSignature| GC
    OCS -->|executeSignatures| ETH
    OCS -->|"delete / retry"| EL
```

### Data Flow

1. **Event Indexers** poll bridge contracts for new events at a configurable interval and persist raw event logs to PostgreSQL.
2. **Message Processors** atomically claim unprocessed rows (using `FOR UPDATE SKIP LOCKED` to avoid contention), verify block finality via the beacon chain or execution-layer RPC, decode the event, sign it if required, and forward the result through an in-memory channel.
3. **On-Chain Sender** receives signed messages, performs pre-flight duplicate checks against the bridge contract, and submits the transaction. On success, the event row is deleted; on failure, the retry count is incremented.

## Components

### Event Indexer (`service/event_indexer.rs`)

Polls a specific bridge contract on a specific chain for a specific event type. Four instances run concurrently:

| Instance | Chain | Bridge | Event |
|----------|-------|--------|-------|
| `ETHAmb` | Ethereum | AMB | `UserRequestForAffirmation(bytes32,bytes)` |
| `GCAmb` | Gnosis Chain | AMB | `UserRequestForSignature(bytes32,bytes)` |
| `ETHXdai` | Ethereum | xDai | `UserRequestForAffirmation(address,uint256,bytes32)` |
| `GCXdai` | Gnosis Chain | xDai | `UserRequestForSignature(address,uint256,bytes32,address)` |

Each indexer tracks its `last_processed_block` in memory and only queries subsequent blocks on each poll cycle.

### Message Processor (`service/msg_processor.rs`)

Two concurrent instances process events from the database:

- **Finality check**: Queries the beacon chain RPC (`/eth/v1/beacon/blocks/finalized`) for the finalized block number. Falls back to the execution-layer RPC (`eth_getBlockByNumber("finalized")`) if beacon RPC is unavailable. Events from non-finalized blocks are skipped and retried later.
- **Message signing**: For `GC -> ETH` flows (`AMB_GC`, `XDAI_GC`), the processor signs the message with the corresponding validator private key.
- **Concurrency safety**: Uses a SQL transaction with `FOR UPDATE SKIP LOCKED` to ensure two processors never claim the same row.

### On-Chain Sender (`service/on_chain_sender.rs`)

Single instance that receives messages via a `tokio::mpsc` channel (capacity 32):

- **Pre-flight checks**: Reads `affirmationsSigned`, `numAffirmationsSigned`, `numMessagesSigned`, and `messagesSigned` from the bridge contract to avoid duplicate submissions.
- **Two-stage processing**: For GC-to-ETH flows where foreign execution is enabled, the sender first submits the signature on the home chain (stage `home`), then collects all validator signatures and executes on the foreign chain (stage `foreign`).

### Contract Bindings (`contracts/bridge.rs`)

Solidity ABI bindings generated via `alloy::sol!` for four contracts:

| Contract | Purpose |
|----------|---------|
| `AMB_BRIDGE` | AMB bridge on both ETH and GC — event listening, signature submission, affirmation execution |
| `XDAI_BRIDGE` | xDai bridge on both ETH and GC — event listening, signature submission, affirmation execution |
| `AMB_BRIDGE_HELPER` | Helper to collect aggregated signatures for AMB foreign execution |
| `XDAI_BRIDGE_HELPER` | Helper to compute message hashes and collect signatures for xDai foreign execution |

### RPC Provider (`rpc_provider/provider.rs`)

Supports multiple RPC endpoints per chain with automatic failover:

- **Single RPC**: Direct connection via `alloy::ProviderBuilder`.
- **Multiple RPCs**: Builds a `FallbackLayer` (via `tower`) that cycles through providers on failure. Active transport count is capped at `min(3, num_rpcs)`.

### Config (`config/mod.rs`)

Loads all configuration from environment variables. Supports comma-separated RPC URLs for fallback. See [Configuration](#configuration) for details.

## Prerequisites

- **Rust** >= 1.93.0 (or use Docker)
- **PostgreSQL** >= 14
- **SQLx CLI** (for offline query metadata): `cargo install sqlx-cli`
- RPC endpoints for Ethereum and Gnosis Chain (execution layer; beacon chain optional but recommended)
- Validator private keys (for signing bridge messages)

## Configuration

Copy `.env.example` to `.env` and fill in the values:

```bash
cp .env.example .env
```

### Environment Variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `ETH_RPC` | Yes | -- | Ethereum execution-layer RPC URL(s). Comma-separated for fallback. |
| `GC_RPC` | Yes | -- | Gnosis Chain execution-layer RPC URL(s). Comma-separated for fallback. |
| `ETH_BC_RPC` | No | Falls back to `ETH_RPC` | Ethereum beacon chain RPC URL(s) for finality checks. |
| `GC_BC_RPC` | No | Falls back to `GC_RPC` | Gnosis Chain beacon chain RPC URL(s) for finality checks. |
| `DATABASE_URL` | Yes | -- | PostgreSQL connection string. e.g. `postgres://user:pass@host:5432/db` |
| `XDAI_VALIDATOR_PRIV_KEY` | No | -- | Hex-encoded private key for signing xDai bridge messages. |
| `AMB_VALIDATOR_PRIV_KEY` | No | -- | Hex-encoded private key for signing AMB bridge messages. |
| `ETH_AMB_BRIDGE_ADDRESS` | No | `0x4C36d2919e407f0Cc2Ee3c993ccF8ac26d9CE64e` | AMB bridge contract on Ethereum. |
| `GC_AMB_BRIDGE_ADDRESS` | No | `0x75Df5AF045d91108662D8080fD1FEFAd6aA0bb59` | AMB bridge contract on Gnosis Chain. |
| `ETH_XDAI_BRIDGE_ADDRESS` | No | `0x4aa42145Aa6Ebf72e164C9bBC74fbD3788045016` | xDai bridge contract on Ethereum. |
| `GC_XDAI_BRIDGE_ADDRESS` | No | `0x7301CFA0e1756B71869E93d4e4Dca5c7d0eb0AA6` | xDai bridge contract on Gnosis Chain. |
| `XDAI_BRIDGE_HELPER_ADDRESS` | No | `0xe30269bc61E677cD60aD163a221e464B7022fbf5` | xDai bridge helper for signature aggregation. |
| `AMB_BRIDGE_HELPER_ADDRESS` | No | `0x7d94ece17e81355326e3359115D4B02411825EdD` | AMB bridge helper for signature aggregation. |
| `POLL_INTERVAL_SECS` | No | `10` | Seconds between each event-indexer poll cycle. |
| `MAX_RETRY_COUNT` | No | `5` | Maximum retry attempts before an event is dropped. |
| `XDAI_EXECUTE_MESSAGE_ON_FOREIGN` | No | `false` | Set to `true` to also execute xDai messages on the foreign chain (ETH) after submitting the signature on the home chain (GC). |
| `AMB_EXECUTE_MESSAGE_ON_FOREIGN` | No | `false` | Set to `true` to also execute AMB messages on the foreign chain (ETH) after submitting the signature on the home chain (GC). |
| `RUST_LOG` | No | `info` | Log level filter. Options: `error`, `warn`, `info`, `debug`, `trace`. |

### Fallback RPC Example

Provide multiple RPC endpoints for resilience:

```env
ETH_RPC=https://eth-mainnet.alchemyapi.io/v2/KEY,https://mainnet.infura.io/v3/KEY,https://rpc.ankr.com/eth
GC_RPC=https://rpc.gnosischain.com,https://gnosis-rpc.publicnode.com
```

The provider automatically cycles through URLs on failure using a `FallbackLayer`.

## Setup and Run

### Option 1: Docker Compose (recommended for deployment)

This starts both PostgreSQL and the validator worker:

```bash
# 1. Configure environment
cp .env.example .env
# Edit .env with your RPC URLs, private keys, etc.

# 2. Start services
docker compose up -d

# 3. View logs
docker compose logs -f worker

# 4. Stop
docker compose down
```

The worker container waits for PostgreSQL to be healthy before starting. Migrations run automatically on startup.

### Option 2: Local Development

```bash
# 1. Start PostgreSQL (or use an existing instance)
docker run -d --name bridge-postgres \
  -e POSTGRES_USER=bridge \
  -e POSTGRES_PASSWORD=bridge_password \
  -e POSTGRES_DB=bridge_validator \
  -p 5432:5432 \
  postgres:16-alpine

# 2. Configure environment
cp .env.example .env
# Edit .env — set DATABASE_URL=postgres://bridge:bridge_password@localhost:5432/bridge_validator

# 3. Prepare SQLx offline metadata (needed for Dockerfile builds)
cargo install sqlx-cli
cd bridge_validator
cargo sqlx prepare

# 4. Build and run
cargo run --release
```

### Option 3: Build Docker Image Manually

```bash
# Build from repository root
docker build -f bridge_validator/Dockerfile -t bridge-validator .

# Run
docker run --env-file .env bridge-validator
```

The Dockerfile uses a multi-stage build:
- **Build stage**: `rustlang/rust:1.93.0-slim-trixie` — compiles the release binary with `SQLX_OFFLINE=true`.
- **Runtime stage**: `debian:sid-slim` — minimal image with only `ca-certificates` and `libssl3`.

## Database Schema

Migrations run automatically on startup via `sqlx::migrate!`. The database has a single table:

### `event_logs`

| Column | Type | Description |
|--------|------|-------------|
| `id` | `SERIAL PRIMARY KEY` | Auto-increment row ID. |
| `topic_key` | `TEXT NOT NULL` | Event signature hash (identifies the event type). |
| `bridge_mode` | `TEXT NOT NULL` | One of: `AMB_ETH`, `AMB_GC`, `XDAI_ETH`, `XDAI_GC`. |
| `log_data` | `JSONB NOT NULL` | Full serialized `alloy::Log` object. |
| `block_number` | `BIGINT` | Block number where the event was emitted. |
| `transaction_hash` | `TEXT` | Transaction hash of the event. |
| `is_processed` | `TEXT` | `"true"` or `"false"` — whether a processor has claimed this row. |
| `retry_count` | `INT DEFAULT 0` | Number of failed processing attempts. |
| `stage` | `TEXT DEFAULT 'home'` | Processing phase: `home` (submit signature) or `foreign` (execute on foreign chain). |
| `created_at` | `TIMESTAMP` | Row creation time. |

**Unique constraint**: `(topic_key, transaction_hash)` prevents duplicate event insertion.

**Indexes**: `topic_key`, `bridge_mode`, `block_number`, `transaction_hash`.

## Bridge Modes

The validator handles four directional flows across two bridge types:

```mermaid
flowchart LR
    subgraph AMB["AMB Bridge"]
        direction LR
        AMB_ETH["ETH -> GC<br/>(AMB_ETH)<br/>executeAffirmation on GC"]
        AMB_GC["GC -> ETH<br/>(AMB_GC)<br/>submitSignature on GC<br/>+ optional executeSignatures on ETH"]
    end

    subgraph XDAI["xDai Bridge"]
        direction LR
        XDAI_ETH["ETH -> GC<br/>(XDAI_ETH)<br/>executeAffirmation on GC"]
        XDAI_GC["GC -> ETH<br/>(XDAI_GC)<br/>submitSignature on GC<br/>+ optional executeSignatures on ETH"]
    end
```

### AMB_ETH (Ethereum to Gnosis Chain, AMB)

1. Indexer detects `UserRequestForAffirmation` on the ETH AMB bridge.
2. Processor verifies ETH block finality and decodes the message.
3. Sender calls `executeAffirmation(message)` on the GC AMB bridge.

### AMB_GC (Gnosis Chain to Ethereum, AMB)

1. Indexer detects `UserRequestForSignature` on the GC AMB bridge.
2. Processor verifies GC block finality, decodes the message, and signs it with `AMB_VALIDATOR_PRIV_KEY`.
3. Sender calls `submitSignature(signature, message)` on the GC AMB bridge.
4. If `AMB_EXECUTE_MESSAGE_ON_FOREIGN=true`: sender collects all validator signatures via `AMB_BRIDGE_HELPER.getSignatures()`, then calls `safeExecuteSignaturesWithAutoGasLimit(data, signatures)` on the ETH AMB bridge.

### XDAI_ETH (Ethereum to Gnosis Chain, xDai)

1. Indexer detects `UserRequestForAffirmation` on the ETH xDai bridge.
2. Processor verifies ETH block finality and decodes the recipient, value, and nonce.
3. Sender calls `executeAffirmation(recipient, value, nonce)` on the GC xDai bridge.

### XDAI_GC (Gnosis Chain to Ethereum, xDai)

1. Indexer detects `UserRequestForSignature` on the GC xDai bridge.
2. Processor verifies GC block finality, constructs the message (`recipient + value + nonce + bridge_address + token_address`, 256-bit aligned), and signs it with `XDAI_VALIDATOR_PRIV_KEY`.
3. Sender calls `submitSignature(signature, message)` on the GC xDai bridge.
4. If `XDAI_EXECUTE_MESSAGE_ON_FOREIGN=true`: sender gets the message hash via `XDAI_BRIDGE_HELPER.getMessageHash()`, collects signatures via `XDAI_BRIDGE_HELPER.getSignatures()`, then calls `executeSignatures(message, signatures)` on the ETH xDai bridge.

## Retry and Failure Handling

```mermaid
stateDiagram-v2
    [*] --> Unprocessed: Event indexed
    Unprocessed --> Processing: Processor claims row<br/>(FOR UPDATE SKIP LOCKED)
    Processing --> FinalityCheck: Decode event

    FinalityCheck --> Unprocessed: Block not finalized<br/>(reset is_processed=false)
    FinalityCheck --> Signing: Block finalized

    Signing --> OnChainSender: Send via channel

    OnChainSender --> Deleted: Transaction succeeds
    OnChainSender --> StageUpdate: submitSignature succeeds<br/>& execute_foreign=true
    StageUpdate --> OnChainSender: Re-process as stage=foreign

    OnChainSender --> RetryIncrement: Transaction fails
    RetryIncrement --> Unprocessed: retry_count < MAX_RETRY_COUNT
    RetryIncrement --> Dropped: retry_count >= MAX_RETRY_COUNT

    Deleted --> [*]
    Dropped --> [*]
```

- **Finality not reached**: Event is set back to `is_processed='false'` and retried on the next processor cycle.
- **Transaction failure**: `retry_count` is incremented and `is_processed` is reset to `'false'`. The event will be retried until `retry_count` reaches `MAX_RETRY_COUNT` (default: 5).
- **Two-stage flow**: When foreign execution is enabled, a successful `submitSignature` on the home chain updates the row's `stage` to `'foreign'` rather than deleting it, so the foreign execution is retried independently.
- **Duplicate prevention**: Pre-flight contract calls check if the validator has already signed/affirmed, and whether the required signature threshold has already been met.

## Graceful Shutdown

The validator handles `SIGTERM` and `SIGINT` (Ctrl+C):

1. A `tokio::watch` channel broadcasts the shutdown signal to all services.
2. Event indexers and message processors break from their loops after completing the current cycle.
3. The `mpsc::Sender` is dropped when processors stop, closing the channel.
4. The on-chain sender drains any remaining messages in the channel, then exits.
5. `tokio::join!` waits for all services to complete before the process exits.
