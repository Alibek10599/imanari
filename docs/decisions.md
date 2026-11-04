# Decision records

Each record states the decision, why it was made, and its effect on the SPEC §13 acceptance suite. None of them widens scope or weakens a guarantee unless it says so.

## D1 Oversized request bodies return 413, not 429

SPEC §7 maps "limit exceeded" to 429. 0.1 returns 429 for rate and concurrency limits and `413 PAYLOAD_TOO_LARGE` for bodies over `request_limit_bytes`. A retry-later status for a payload that can never succeed would mislead clients that back off and retry. The inferred contracts list both codes.

Acceptance impact: none. Item 4's input-limit check asserts 413, and the rate limit asserts 429.

## D2 Protected tests are JSON scenarios in `acceptance/`

The grammar has no test syntax, and unknown blocks are errors (§5.1). Protected tests are therefore data files outside the `.nari` source: declarative HTTP steps with subset expectations. They are hashed into context, repair snapshots, and candidates, and the repair path policy forbids edits to `acceptance/`.

Acceptance impact: items 8 and 9 use them as the unchanged protected suite.

## D3 SQLx 0.8.6 rather than 0.9.0

0.9.0 had just been released when implementation started, and 0.8.6 is maintained and has a stable `begin_with("BEGIN IMMEDIATE")` API. Versions are locked in `Cargo.lock`.

Acceptance impact: none.

## D4 Generated apps build against a bundled copy of the runtime

The CLI embeds the `imanari-runtime` manifest and source and writes them to `.nari/generated/runtime/`, next to the generated app, which depends on it as `path = "../runtime"`. The generated tree is position-independent, a released `nari` binary needs no source checkout, and the dependency lock embedded from this workspace still pins every crate. The runtime manifest is standalone (no workspace inheritance), so it can be copied verbatim. Benchmarks build the baseline and the kernel the same way, under `.nari/bench/crates/`. Unchanged files are not rewritten, so Cargo does not rebuild them.

Acceptance impact: none. Earlier builds referenced the checkout's `runtime/` by absolute path.

## D5 `@actor.id` is accepted only as the owner assignment of an insert

§5.2 says set values are literals or `@input` references, with the owner on insert required to be `@actor.id`. Any other use of `@actor` is rejected. This is stricter than permitting it and closes an identity-forwarding path.

## D6 List actions cannot declare inputs

0.1 has no filter semantics, so author inputs on a list could not affect behavior. They are rejected with `E_UNSUPPORTED_FEATURE`, and `limit` and `after` with `E_RESERVED_INPUT`.

## D7 Restart budget across supervisor restarts

Restart attempts are rows in the control DB, counted in a sliding window that a new supervisor never resets. A supervisor's first launch is not a restart, but a crash after it is charged against the persisted window. `StoppedNeedsAttention` blocks `nari dev` until the operator passes `--acknowledge`, which records a transition and keeps the history. Deterministic startup refusals (`MIGRATION_REQUIRED`, configuration errors) stop immediately instead of looping.

Acceptance impact: item 6 verifies exhaustion, refusal, acknowledgement, and an unchanged budget.

## D8 Failed reconciliation records `Stopped` and blocks duplicates

When an unverified process answers on the recorded port, the supervisor records `Stopped` with a `RECONCILE_FAILED` reason, so the control DB never claims a worker it does not manage. Every later start probes the recorded port again. Process IDs are never trusted; ownership is proven by an owner token whose SHA-256 the worker reports.

## D9 Repair budget is per unresolved failure signature

`max_attempts` counts attempts for a failure signature since the last applied repair of that signature. A new incident for an unresolved signature does not reset the budget, while a regression after an applied fix starts a new one. Rejected and inconclusive attempts count, stay visible as precedents, and are never reused.

## D10 Dev and test fault injection through `NARI_FAULT`

Crash points before commit, after commit, on request, and on start are compiled into the runtime and enabled only by an environment variable that the trusted operator or test harness sets. They make the crash and lost-response acceptance checks deterministic.

## D11 Benchmark rate-limit override

`NARI_MUTATIONS_PER_MINUTE` lets the trusted operator raise the per-actor limit (1..1,000,000). The check still runs per request, identically in the generated app and the baseline. It is not a benchmark-only bypass.

## D12 Sandbox resource limits on macOS

Seatbelt confines writes to the workspace, keeps protected fixtures read-only, denies network (loopback inbound only while running), and denies reads of credential directories and the control and app databases. The environment is cleared. A self-test must observe denied writes and connections before isolation is claimed. Wall-clock, CPU, file-size and open-file limits are enforced. Memory and process-count rlimits are not enforced by macOS for this use, and the run phase denies `fork`.

Acceptance impact: item 9's sandbox clause holds on macOS, and other platforms report `SANDBOX_UNAVAILABLE`.

## D14 In-process FIFO writer queue for mutations

SQLite allows one writer. With concurrent `BEGIN IMMEDIATE` transactions, the losers sleep in SQLite's busy handler with coarse backoff, which produced p99 latencies of 10–38 ms and run-to-run p95 swings of 3–4×. Mutations now take an async FIFO lock before `BEGIN IMMEDIATE` and hold it until commit or rollback. The transaction still uses `BEGIN IMMEDIATE`, the receipt primary key still backs idempotency convergence, `synchronous` and the other durability settings are unchanged, and waiting counts toward `request_timeout_ms`.

Acceptance impact: item 5's concurrency tests still pass, and item 12's runtime budgets are now met. The miss recorded before this change stays on record in [progress.md](progress.md).

## D13 Semantic and source hashes

The semantic hash covers the typed IR without spans, so comments, formatting and declaration reordering do not change it. The source hash covers the exact bytes, so any edit makes a captured patch stale (§8).

## D15 The project is named Imanari and the command is `nari`

"Frame" was already taken by a state-machine language with a Rust compiler on crates.io (frame-lang/framec), by Polkadot's FRAME runtime framework, and by the `frame` crate. The language is now Imanari, after the BJJ roll into a leg entanglement. Day-to-day names use the short form `nari`: the CLI, the `.nari` source extension, the `.nari/` artifact directory, `NARI_*` environment variables, and the `nari-*/0.1` artifact schemas. Crates keep the full name (`imanari`, `imanari-runtime`), and source files open with `imanari "0.1";`. The language version stays 0.1, and the tool moves to 0.2.0 because the command, extension and header changed.

Acceptance impact: none. Behavior is unchanged; only names moved.

## D16 Additive schema changes migrate automatically; everything else still stops

Before this, any schema change stopped startup with `MIGRATION_REQUIRED`, so adding one field meant deleting the development database. The database now stores the schema shape next to its fingerprint: each entity's owner field, its create statements, and each field's type and, when the field has a default, an `ALTER TABLE ... ADD COLUMN ... DEFAULT` statement. On a fingerprint mismatch the runtime plans a migration from the stored shape to the compiled one. New entities, new fields with a default, and widened text bounds are applied in one `BEGIN IMMEDIATE` transaction that also swaps the fingerprint (compare-and-set) and appends a row to `_nari_schema_history`. Removed entities or fields, owner or type changes, narrowed bounds, and new fields without a default are refused with a message naming each change, because each of them drops or reinterprets stored data. The planner lives in `imanari-runtime` and the compiler links the same function, so `nari diff` reports exactly what the next start will do. Databases created before this change have no stored shape: they keep working while the fingerprint matches, have the shape recorded on that start, and refuse a mismatch as before.

Acceptance impact: none. The 0.1 guarantee that stored data is never dropped, recreated or reinterpreted still holds, and it is now tested through both outcomes.
