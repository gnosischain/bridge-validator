# FCR integration (bridge-validator)

Implementation plan for porting **FCR (Fast Confirmation Rule)**. Conceptual background lives in
[`fcr-agent-reference.md`](./fcr-agent-reference.md); this doc is the concrete engineering plan.

> Status: **implemented** (phases 1–4). `block-finality` stays the default so existing
> deployments are unaffected. See [Implementation notes](#implementation-notes) for where each
> piece landed and the two places the code deviates from the plan below.

## Scope

Enable `safe`-block processing (fast confirmation, ~12s) as an alternative to the current
`finalized`-only processing (~12.8m), selectable **per chain**. Because `safe` blocks can be
reorged out and a signature cannot be un-signed, a new revalidation task re-checks every
safe-processed block once it finalizes and records a false positive on mismatch. Nothing is
undone on-chain.

## Design anchor

FCR is **purely additive**. bridge-validator already implements `block-finality` mode:
`event_indexer.rs` resolves the finalized block via `finality.rs` (beacon-first, EL fallback)
and only indexes `(last_processed, finalized]`. FCR adds:

1. a faster `safe` upper bound,
2. per-chain mode selection, and
3. a revalidation task that closes the reorg gap FCR intentionally opens.

The `msg_processor` / `on_chain_sender` pipeline needs **no logic change** — it signs whatever
is `pending`; FCR only changes _when_ a row becomes pending and adds a post-hoc check.

### Architecture

| Concern                | bridge-validator (Rust)                                                   |
| ---------------------- | ------------------------------------------------------------------------- |
| State store            | PostgreSQL (`sqlx`) — extend `event_logs` + one audit table               |
| Concurrency            | `tokio` tasks in `tokio::join!`                                           |
| Chain identity         | `eth` / `gc` (4 indexers, one per direction)                              |
| Safe-block pending set | `event_logs.fcr_status = 'pending'` (dedup by grouping on `block_number`) |
| Per-event attribution  | the `event_logs` row itself (`tx_hash`, `log_index`)                      |
| Revalidation worker    | `service/fcr_checker.rs` tokio task                                       |
| False-positive record  | `fcr_false_positives` table + `tracing::error!`                           |

## Approved design decisions

- **State storage:** extend `event_logs` (add `block_hash`, `fcr_status`), plus a dedicated
  `fcr_false_positives` audit table for durable alert history.
- **Mode scope:** per chain — `ETH_BLOCK_PROCESSING_MODE` / `GC_BLOCK_PROCESSING_MODE`
  (`fcr` | `block-finality`, default `block-finality`). All indexers on a chain share the mode.
- **Alerting:** DB record (`fcr_false_positives`) **and** `tracing::error!`.
- **Revert check:** anchor on block **number**, compare hash — match ⇒ confirmed, differing hash
  ⇒ reverted, null `getBlock` ⇒ retry next cycle (never prune).

### EL-layer note on the revert check

bridge-validator works entirely in **EL block numbers** (`finality.rs` returns the execution
`block_number`; `event_logs` stores the EL number and EL hash). EL numbers are contiguous, so the
`fcr-agent-reference.md` §5 "empty slot / 404" case manifests at the EL as **a different block
occupying the same number** — caught directly by anchor-on-number-then-compare-hash against
`getBlock(number)`. Never compare against a beacon block root.

## Changes by file

### 1. `migrations/004_add_fcr_tracking.sql` (new)

- `ALTER TABLE event_logs ADD COLUMN block_hash TEXT;` — dedicated column (currently only inside
  `log_data` JSON) for indexed revalidation grouping.
- `ALTER TABLE event_logs ADD COLUMN fcr_status TEXT;` — `NULL` for block-finality rows;
  `'pending'` | `'confirmed'` | `'reverted'` for fcr rows.
- `CREATE INDEX idx_fcr_pending ON event_logs(block_number) WHERE fcr_status = 'pending';`
  — partial index for the checker's hot query.
- `CREATE TABLE fcr_false_positives ( id SERIAL PRIMARY KEY, chain TEXT NOT NULL,
block_number BIGINT NOT NULL, stored_block_hash TEXT NOT NULL, canonical_block_hash TEXT,
transaction_hash TEXT, log_index BIGINT, event_log_id INT, detected_at_finalized BIGINT,
created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP );`

### 2. `config/mod.rs`

- Add `enum BlockProcessingMode { Fcr, BlockFinality }` (default `BlockFinality`).
- Add fields `eth_block_processing_mode` / `gc_block_processing_mode`, parsed from
  `ETH_BLOCK_PROCESSING_MODE` / `GC_BLOCK_PROCESSING_MODE`.
- Helpers: `mode_for_chain(&self, chain: &str) -> BlockProcessingMode` and
  `fcr_chains(&self) -> Vec<(&str /*chain*/, &[String] /*el_rpcs*/)>`.

### 3. `service/finality.rs` (or sibling `safe.rs`)

- Add `get_safe_block_number(http_client, el_rpcs) -> Result<Option<i64>>` using
  `eth_getBlockByNumber("safe", false)`. **EL-only** — per `fcr-agent-reference.md` §4b there is no
  Beacon `safe` endpoint. Returns `None` when the node doesn't return `safe` (drives the fresh-start
  fallback).
- Follow the **existing resolver pattern** in `finality.rs` (`get_finalized_block_number`): iterate
  the configured RPC array in order, validate the response shape before trusting it, `continue` past
  a failed/empty provider to the next, and only give up when every provider is exhausted.
  `get_safe_block_number` is the `safe`-tag analog of that function.

#### 3a. Startup preflight — verify the RPC actually supports `safe`

`safe` is **not universally supported**: some EL providers reject the `safe` tag outright, and even
supporting nodes legitimately return `null` when there is no safe block yet (FCR disabled, node still
syncing, or pre-merge). These two cases must not be conflated — the existing finalized resolver never
had to make this distinction because `finalized` is universal, but `safe` does.

On startup, for **each chain configured in `fcr` mode**, probe its EL RPC array once with
`eth_getBlockByNumber("safe", false)` and classify the outcome per provider:

- **Valid block object returned** → provider supports `safe`. Good.
- **JSON-RPC error / HTTP error / `safe` tag rejected** (e.g. `-32602 invalid argument`, method/param
  errors, unsupported-tag messages) → **provider cannot serve `safe`**. This is a **misconfiguration**,
  not a transient miss. Log at `error!` and fail the chain's fcr preflight (do not silently downgrade —
  see Risks). If the RPC array has other providers that _do_ support `safe`, the array can still run;
  flag the bad provider so a later silent fallback isn't mistaken for "chain caught up".
- **`result: null`** (tag accepted, no safe block available yet) → **legitimate empty**, treat as
  `None` and let the fresh-start guard fall back to finalized. Distinguish
  this from the error case above by the _presence of a JSON-RPC error object_, not by null-ness alone.

Rationale: without this preflight, an fcr-configured chain pointed at a `safe`-incapable RPC would
silently run on `finalized` forever (the §4 fallback), giving operators finality latency while they
believe they have ~12s confirmation. The check surfaces that at boot instead of never.
Fallback to block finality if safe is not supported

### 4. `service/event_indexer.rs`

- Add a `chain()` helper (`"eth"` / `"gc"` from `check_bridge_mode`) and read the chain's mode.
- Replace the fixed finalized upper bound with a mode switch:
  - `fcr` → `get_safe_block_number`; if `None`, fall back to finalized (fresh-start guard) and log
    clearly that a chain configured for fcr can't get `safe`.
  - `block-finality` → unchanged.
- Populate the new `block_hash` column on insert.
- In fcr mode set `fcr_status = 'pending'` on inserted rows; block-finality inserts leave it `NULL`.

### 5. `service/fcr_checker.rs` (new — the revalidation task)

- Struct with a `start()` loop added to `tokio::join!` in `main.rs`. Runs only if ≥1 chain is in
  fcr mode (else logs "not required" and returns).
- Per cycle, per fcr chain:
  1. resolve `finalized` via `finality.rs`,
  2. `SELECT DISTINCT block_number, block_hash FROM event_logs WHERE bridge_mode IN (<chain modes>)
AND fcr_status = 'pending' AND block_number <= finalized`,
  3. one `getBlock(number)` per distinct block, then compare:
     - **match** → `UPDATE ... SET fcr_status = 'confirmed'` for that `(number, hash)`.
     - **mismatch** → insert affected rows into `fcr_false_positives`, `tracing::error!(...)`, then
       `UPDATE ... SET fcr_status = 'reverted'`.
     - **`getBlock` null** → skip, retry next cycle (never prune — dropping would read as verified).
- Backlog warn on the count of `pending` rows (`PENDING_BACKLOG_WARN_THRESHOLD`).

### 6. `main.rs`

- Construct `FcrChecker`, add it to the `tokio::join!(...)`.

### 7. Docs / env

- `README.md`: the "every stored row is finalized by construction" invariant now holds only in
  block-finality mode. Document fcr mode, the revalidation task, and false-positive semantics.
- `.env.example` / `.env.example.*`: add the two mode vars (default `block-finality`).

## New env variables

| Var                         | Values                    | Default          | Purpose                         |
| --------------------------- | ------------------------- | ---------------- | ------------------------------- |
| `ETH_BLOCK_PROCESSING_MODE` | `fcr` \| `block-finality` | `block-finality` | ETH-side indexers' upper bound. |
| `GC_BLOCK_PROCESSING_MODE`  | `fcr` \| `block-finality` | `block-finality` | GC-side indexers' upper bound.  |

## Tests (existing `tests/` + mock-provider harness)

- **safe resolver:** `get_safe_block_number` parses a result and returns `None`/falls back correctly.
- **safe-support preflight (§3a):** with a mock provider, assert the three classes are distinguished —
  a valid `safe` block passes; a JSON-RPC error / rejected-tag response fails the fcr preflight (loud
  `error!`, not silent downgrade); a `result: null` is treated as legitimate-empty (`None` → finalized
  fallback), keyed on the presence of a JSON-RPC error object, not null-ness alone.
- **event_indexer:** fcr mode caps at `safe`, sets `fcr_status='pending'` + `block_hash`; falls back
  to finalized when `safe` is `None`.
- **fcr_checker:** confirmed path; false-positive path (mismatch → row in `fcr_false_positives` +
  status `reverted`); null-`getBlock` retry; per-chain scoping.

## Phasing

1. Migration + config (mode plumbing, no behavior change).
2. `get_safe_block_number` + §3a startup safe-support preflight + indexer mode switch (fcr rows
   start flowing).
3. `fcr_checker` task + `fcr_false_positives` + `main.rs` wiring.
4. Docs + tests + integration.

## Risks / call-outs

- **Reorg window opens by design** in the sign path — the guarantee downgrade must be an explicit,
  documented, per-chain operator choice.
- **Silent fallback**: `get_safe_block_number` returning `None` falls back to finalized — log
  prominently so an fcr-configured chain silently running conservative is visible. The §3a startup
  preflight is the primary guard: a `safe`-incapable RPC is caught at boot instead of degrading
  silently for the process lifetime.
- **`safe` not universally supported**: unlike `finalized`, the `safe` tag is rejected by some EL
  providers. Verify support per RPC before trusting fcr mode (§3a), and separate a hard unsupported-tag
  error (misconfig, fail loud) from a legitimate `null` (no safe block yet, fall back quietly).
- **Default unchanged**: block-finality remains the default everywhere; no existing deployment
  changes behavior without setting a mode var.

## ToDo

- [x] Migration + config
- [x] safe resolver + safe-support startup preflight (§3a) + indexer mode switch
- [x] fcr_checker + false-positive table + wiring
- [x] Docs + env
- [x] Unit tests
- [x] Integration test (per-component, against a real Postgres + mock RPCs)
- [ ] End-to-end test across indexer → msg_processor → fcr_checker in one run

## Implementation notes

Where each piece landed:

| Plan item                | Landed in                                                                 |
| ------------------------ | ------------------------------------------------------------------------- |
| Migration                | `bridge_validator/migrations/004_add_fcr_tracking.sql`                    |
| Mode config              | `src/config/mod.rs` — `BlockProcessingMode`, `mode_for_chain`, `fcr_chains` |
| Safe resolver + preflight | `src/service/safe.rs` (sibling of `finality.rs`, per §3)                  |
| Indexer mode switch      | `src/service/event_indexer.rs` — `chain()`, `mode()`, `resolve_upper_bound()` |
| Revalidation task        | `src/service/fcr_checker.rs`                                              |
| Wiring                   | `src/main.rs` — preflight before service construction, checker in `tokio::join!` |
| Tests                    | `tests/safe_test.rs`, `tests/fcr_checker_test.rs`, additions to `tests/event_indexer_test.rs` and `src/config/tests.rs` |

Two deviations from the plan above, both in the direction of not producing false verdicts:

1. **Preflight failure is scoped to "reachable but incapable".** A chain is downgraded to
   `block-finality` only when providers *answered* and none would serve `safe`. If nothing was
   reachable at boot (an RPC outage), fcr stays on and is logged at `error!` — a transient outage
   should not silently pin a chain to finality for the process lifetime, and the per-cycle fallback
   already covers it. See `ChainSafeSupport::keeps_fcr`.
2. **Pending rows with a NULL `block_hash` are never adjudicated.** The indexer always records a
   hash in fcr mode, so this state is a bug rather than a normal one. The checker excludes them
   from the comparison query, counts them, and logs at `error!` — confirming them would be a
   fabricated verdict, and reverting them would be a fabricated alert.

Two supporting details worth knowing when reading the code:

- `FcrChecker::check_chain(chain, el_rpcs)` takes the EL array as a parameter while resolving
  finality through `config.finality_rpcs_for_chain(chain)`. In production both come from the same
  config; splitting them lets tests drive the finality source and the block-lookup source
  independently.
- The checker uses runtime `sqlx::query` rather than the `query!` macros, so the new statements
  need no `.sqlx` offline data and `cargo sqlx prepare --check` stays green.
