# Production Readiness Checklist

Audit performed on 2026-04-10. Tracks all identified issues and their resolution status.

---

## P0 — Critical

- [x] **Private keys in `.env` not committed to git**
  `.env` is in `.gitignore`. Added `.env.*` pattern to also cover `.env.test` and variants.

- [x] **Panicking `.expect()`/`.unwrap()` in transaction paths**
  Replaced 8 locations across `on_chain_sender.rs` and `msg_processor.rs` with proper `?` error propagation. The validator no longer crashes on missing/malformed private keys — errors are logged and the event is retried or skipped.
  - `on_chain_sender.rs`: 4 private key parsing locations (AmbEth, AmbGc, XdaiEth, XdaiGc)
  - `msg_processor.rs`: 4 locations (AMB_GC signing, XDAI_GC signing + hex decode)

## P1 — Important

- [ ] **No CI/CD pipeline**
  No `.github/workflows/` or equivalent. Need automated build, `cargo test`, `cargo audit`, and Docker image build.

- [x] **No graceful shutdown**
  Added `tokio::signal` handling for SIGTERM and SIGINT. All services receive a `watch::channel` shutdown signal: `EventIndexer` and `MessageProcessor` finish their current iteration and break from their loops; `OnChainSender` drains remaining channel messages (completing any in-flight transaction) then exits. The original `mpsc::Sender` is dropped before `tokio::join!` so the channel closes naturally once both `MessageProcessor` instances stop.

- [x] **No retry/reprocess strategy for failed foreign chain executions**
  Added `stage` column to `event_logs` (`'home'` default, `'foreign'` after `submitSignature` succeeds). On foreign execution failure, `increment_retry_count` resets `is_processed='false'`; on retry the `stage='foreign'` flag tells `OnChainSender` to skip pre-flight checks and `submitSignature` (which would revert) and jump straight to foreign execution. Migration: `002_add_stage_column.sql`.

- [ ] **Last processed block not persisted — events missed on restart**
  `event_indexer.rs` stores `last_processed_block` in a local variable (starts at `0`). On restart, it jumps to `latest_block`, skipping all events that occurred while the validator was down. Need to persist per-indexer block cursors in the database.

- [ ] **Missing configurable start block**
  `event_indexer.rs:94` has `// TODO: Or config.start_block`. Without this, there's no way to backfill historical events or recover from a gap.

## P2 — Should Fix

- [x] **HTTP client created per request in `msg_processor.rs`**
  Added `http_client: reqwest::Client` field to `MessageProcessor`. Created once in `new()`, reused for all beacon/EL RPC calls. `reqwest::Client` internally pools connections, so this avoids repeated DNS lookups and TLS handshakes.

- [x] **Beacon chain RPC required at startup — no fallback**
  `ETH_BC_RPC`/`GC_BC_RPC` are now optional. If unset, `get_finalized_block_number` skips the beacon call and falls back to EL RPCs using `eth_getBlockByNumber("finalized", false)`. Logs a warning at startup when BC_RPC is not configured.

- [ ] **No health check / liveness endpoint**
  No HTTP endpoint for Kubernetes probes or monitoring to verify the validator is alive and processing.

- [ ] **No metrics / alerting**
  No Prometheus metrics. Need visibility into: block lag, processing latency, retry counts, transaction success/failure rates.

- [ ] **No RPC call timeouts**
  Provider setup has no explicit timeout or rate-limiting. A stuck RPC call blocks processing indefinitely.

- [ ] **No rate limiting / backpressure**
  If the database fills with unprocessed events, there's no mechanism to slow down indexing or alert operators.

## P3 — Nice to Have

- [ ] **Secure memory handling for private keys**
  Private keys stored as plain `String` in `Config`. Consider `zeroize` crate to clear keys from memory after use.

- [ ] **Reduce `tokio` features**
  `tokio = { version = "1.0", features = ["full"] }` includes everything. Specify only needed features to reduce binary size.

- [ ] **Add `cargo audit` to dependency management**
  No vulnerability scanning configured for dependencies.

- [ ] **Document helper bridge addresses in `.env.example`**
  `XDAI_BRIDGE_HELPER_ADDRESS` and `AMB_BRIDGE_HELPER_ADDRESS` have hardcoded defaults in `config/mod.rs` but are not documented in `.env.example`.

- [ ] **Config test parallelism issue**
  Config unit tests pollute each other via `std::env::set_var` when run in parallel. `test_config_custom_poll_interval` fails intermittently. Tests need isolation (e.g., `serial_test` crate or restructuring to avoid shared env state).

---

## Files Modified

| File | Changes |
|------|---------|
| `.gitignore` | Added `.env.*` pattern |
| `bridge_validator/src/service/on_chain_sender.rs` | Replaced 4x `.expect()` with `?` propagation |
| `bridge_validator/src/service/msg_processor.rs` | Replaced 4x `.expect()`/`.unwrap()` with `?`; added `http_client` field; added EL RPC fallback for finality checks (`get_finalized_block_from_el`) |
| `bridge_validator/src/config/mod.rs` | Made `ETH_BC_RPC`/`GC_BC_RPC` optional; `get_eth_bc_rpc()`/`get_gc_bc_rpc()` now return `Option<&str>` |
| `bridge_validator/tests/msg_processor_test.rs` | Updated `check_block_finality` call sites for new signature |
