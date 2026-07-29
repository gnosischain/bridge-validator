# FCR integration (bridge-validator)

Implementation plan for porting **FCR (Fast Confirmation Rule)**. Conceptual background lives in
[`fcr-agent-reference.md`](./fcr-agent-reference.md); this doc is the concrete engineering plan.

> Status: **implemented** (phases 1–4). `block-finality` stays the default so existing
> deployments are unaffected. See [Implementation notes](#implementation-notes) for where each
> piece landed.

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
- `CREATE TABLE IF NOT EXISTS fcr_false_positives ( id SERIAL PRIMARY KEY, chain TEXT NOT NULL,
block_number BIGINT NOT NULL, stored_block_hash TEXT NOT NULL, canonical_block_hash TEXT,
transaction_hash TEXT, log_index BIGINT, event_log_id INT, detected_at_finalized BIGINT,
created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP );`
- Two lookup indexes on the audit table: `idx_fcr_false_positives_chain(chain)` and
  `idx_fcr_false_positives_block_number(block_number)`.

### 2. `config/mod.rs`

- Add `enum BlockProcessingMode { Fcr, BlockFinality }` (default `BlockFinality`).
- Add fields `eth_block_processing_mode` / `gc_block_processing_mode`, parsed from
  `ETH_BLOCK_PROCESSING_MODE` / `GC_BLOCK_PROCESSING_MODE`. An unrecognised value is a **hard
  startup error**, never a silent default — a typo'd mode var must not quietly hand back the
  conservative mode an operator believes they turned off.
- Helpers: `mode_for_chain(&self, chain: &str) -> BlockProcessingMode` and
  `fcr_chains(&self) -> Vec<(&str /*chain*/, &[String] /*el_rpcs*/)>`, plus
  `set_mode_for_chain` (how the §3a preflight applies a downgrade),
  `finality_rpcs_for_chain` (beacon + EL fallbacks for a chain) and
  `bridge_modes_for_chain` (the `event_logs.bridge_mode` values a chain owns).

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
had to make the distinction because `finalized` is universal; `safe` is not.

On startup, for **each chain configured in `fcr` mode**, probe its EL RPC array once with
`eth_getBlockByNumber("safe", false)` and classify the outcome per provider:

- **Valid block object returned** → `SafeProbe::Block`: provider supports `safe`. Good.
- **JSON-RPC `error` object / `safe` tag rejected** (e.g. `-32602 invalid argument`, unsupported-tag
  messages) → `SafeProbe::Unsupported`: this provider **cannot serve `safe`**. A **misconfiguration**,
  not a transient miss — log at `error!`. If the RPC array has other providers that _do_ support
  `safe`, the array can still run; flag the bad provider so a later silent fallback isn't mistaken
  for "chain caught up".
- **`result: null`** (tag accepted, no safe block available yet) → `SafeProbe::Empty`: **legitimate
  empty**, treat as `None` and let the fresh-start guard fall back to finalized. Distinguish this
  from the error case above by the _presence of a JSON-RPC error object_, not by null-ness alone.
- **HTTP error / connection failure / unparseable body** → `SafeProbe::Unreachable`. An HTTP-level
  rejection carries no JSON-RPC error object, so it says **nothing** about `safe` support and must
  not be counted as "unsupported".

Per-chain verdict (`ChainSafeSupport::keeps_fcr`): downgrade the chain to `block-finality` — loudly,
at `error!` — only when providers **answered** and none of them would serve `safe`. If nothing was
reachable at all, keep fcr enabled and log at `error!`: an RPC outage at boot is transient, and the
per-cycle §4 fallback already covers it. Pinning a chain to finality for the whole process lifetime
because the network blipped during startup is the worse failure.

Rationale: without this preflight, an fcr-configured chain pointed at a `safe`-incapable RPC would
silently run on `finalized` forever (the §4 fallback), giving operators finality latency while they
believe they have ~12s confirmation. The check surfaces that at boot instead of never.

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
AND fcr_status = 'pending' AND block_number IS NOT NULL AND block_hash IS NOT NULL
AND block_number <= finalized`,
  3. one `getBlock(number)` per distinct block, then compare:
     - **match** → `UPDATE ... SET fcr_status = 'confirmed'` for that `(number, hash)`.
     - **mismatch** → insert affected rows into `fcr_false_positives`, `tracing::error!(...)`, then
       `UPDATE ... SET fcr_status = 'reverted'`. Both go in **one transaction**, so an alert can
       never be lost while the rows are quietly marked resolved.
     - **`getBlock` null** → skip, retry next cycle (never prune — dropping would read as verified).
- Pending rows with a NULL `block_hash` are **never adjudicated** — hence the `IS NOT NULL` guards
  above. They can't be compared against anything, so the checker counts them separately and logs at
  `error!`. The indexer always records a hash in fcr mode, so this state is an indexer bug rather
  than a normal one: confirming such a row would be a fabricated verdict, reverting it a fabricated
  alert.
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

`block_finality` (underscore) is accepted as a spelling of `block-finality`, and an empty value means
"unset". Anything else fails startup rather than defaulting.

## Tests (existing `tests/` + mock-provider harness)

- **safe resolver:** `get_safe_block_number` parses a result and returns `None`/falls back correctly.
- **safe-support preflight (§3a):** with a mock provider, assert all four probe classes are
  distinguished — a valid `safe` block; a JSON-RPC error / rejected tag ⇒ `Unsupported`; a
  `result: null` ⇒ legitimate-empty (`None` → finalized fallback), keyed on the presence of a
  JSON-RPC error object rather than null-ness; an HTTP error ⇒ `Unreachable`, **not** `Unsupported`.
  Then the per-chain verdict: a safe-incapable chain is downgraded (loud `error!`, never silent), a
  chain nothing could reach keeps fcr, and one bad provider in an otherwise good array is flagged
  without downgrading.
- **event_indexer:** fcr mode sets `fcr_status='pending'` + `block_hash`, block-finality leaves the
  status `NULL`, and each indexer follows its own chain's mode. The bound itself (`resolve_upper_bound`
  caps at `safe`, and falls back to `finalized` when `safe` is a legitimate `None`) is covered in the
  end-to-end test below, where a mock EL can serve both tags.
- **fcr_checker:** confirmed path; false-positive path (mismatch → row in `fcr_false_positives` +
  status `reverted`); null-`getBlock` retry; per-chain scoping; blocks above finalized left pending;
  block-finality (NULL-status) rows ignored; NULL-`block_hash` pending rows never confirmed.
- **end-to-end (`tests/fcr_e2e_test.rs`):** the three stages over the same rows, nothing hand-seeded
  in between — preflight → indexer → msg_processor → checker. The component tests above can't observe
  the property fcr actually trades on, that a row is **signed while still `pending`**
  (`is_processed = 'true'` and `fcr_status = 'pending'` at once). Covers all three closures of that
  window: block survives ⇒ `confirmed`; block reorged out ⇒ `reverted` plus an audit row carrying the
  same `event_log_id` the sender received; block-finality mode unaffected (caps at `finalized` even
  when the same RPC serves a fresher `safe`, NULL status, checker exits). Plus the §4 fresh-start
  guard: a node with no safe block yet falls back to `finalized` and keeps indexing, and those rows
  are still `pending` — `fcr_status` follows the chain's mode, not the bound a given cycle happened
  to use. "Time passing" is two wiremock servers for one chain — the checker must get a different
  answer than the indexer did to the same `eth_getBlockByNumber`.
- **live RPC (`tests/live_rpc_test.rs`):** everything above runs on mocks, which prove the logic but
  not that the wire format we parse is the one a real client emits. These close that gap — `safe`
  leads `finalized`, the probe reports support, the preflight keeps fcr on, canonical hashes match
  the node, and the checker confirms real blocks while flagging forged ones. Both `#[ignore]`d **and**
  gated on `LIVE_EL_RPC`, so neither CI nor a plain `cargo test` ever depends on a network endpoint:
  `LIVE_EL_RPC=http://host:port cargo test --test live_rpc_test -- --ignored`.

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
- [x] End-to-end test across indexer → msg_processor → fcr_checker in one run

## Implementation notes

Where each piece landed:

| Plan item                 | Landed in                                                                                                               |
| ------------------------- | ----------------------------------------------------------------------------------------------------------------------- |
| Migration                 | `bridge_validator/migrations/004_add_fcr_tracking.sql`                                                                  |
| Mode config               | `src/config/mod.rs` — `BlockProcessingMode`, `mode_for_chain`, `fcr_chains`                                             |
| Safe resolver + preflight | `src/service/safe.rs` (sibling of `finality.rs`, per §3)                                                                |
| Indexer mode switch       | `src/service/event_indexer.rs` — `chain()`, `mode()`, `resolve_upper_bound()`                                           |
| Revalidation task         | `src/service/fcr_checker.rs`                                                                                            |
| Wiring                    | `src/main.rs` — preflight before service construction, checker in `tokio::join!`                                        |
| Tests                     | `tests/safe_test.rs`, `tests/fcr_checker_test.rs`, additions to `tests/event_indexer_test.rs` and `src/config/tests.rs` |
| End-to-end test           | `tests/fcr_e2e_test.rs` — preflight → indexer → msg_processor → checker over the same rows                              |
| Live-RPC smoke tests      | `tests/live_rpc_test.rs` — `#[ignore]`d, gated on `LIVE_EL_RPC`                                                         |

Two supporting details worth knowing when reading the code:

- `FcrChecker::check_chain(chain, el_rpcs)` takes the EL array as a parameter while resolving
  finality through `config.finality_rpcs_for_chain(chain)`. In production both come from the same
  config; splitting them lets tests drive the finality source and the block-lookup source
  independently.
- The checker uses runtime `sqlx::query` rather than the `query!` macros, so the new statements
  need no `.sqlx` offline data and `cargo sqlx prepare --check` stays green.
