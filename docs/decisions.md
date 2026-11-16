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

## D17 Telemetry is an in-process aggregate behind a bounded queue

§12 asks for batched or queued telemetry with explicit capacity and dropped-event counters, and nothing in 0.1 consumed per-request data. Each action response now pushes one event (action name, HTTP status, elapsed microseconds) into a 1,024-slot queue with `try_send`, so a full queue drops the event and increments a counter instead of slowing the request. One task drains the queue in batches of up to 256 into per-action aggregates: request count, count per status, a fixed latency histogram (100 µs to 30 s) and the maximum. Action names not in the spec share a single `(unknown)` bucket, so attacker-chosen paths cannot grow the map. Events carry no actor, input, idempotency key or request ID. The aggregate is per instance and lost on restart. `GET /_nari/metrics` returns it (`nari-metrics/0.1`) only with the owner token, and `nari status` shows it after checking the worker's owner hash, the same check reconciliation uses before it sends the token. Exporters (OTLP, Prometheus) and persisting aggregates for canary comparison wait for the deployment adapter and staged releases that need them.

Acceptance impact: closes the §12 telemetry gap from 0.1. The benchmark baseline runs on the same runtime routes, so both sides pay the same per-request cost.

## D18 PostgreSQL is chosen at deploy time, and generated code no longer calls SQLx

A `postgres://` URL in `NARI_DATABASE_URL` makes the same binary run on PostgreSQL. The URL is a secret, so it lives in the environment, never in source, logs or error messages, and `runtime.database` stays the development SQLite path. To keep one artifact for both, generated handlers call the runtime (`Pool::fetch_all`, `Conn::fetch_one`, `Conn::fetch_optional`) with typed arguments and column lists, and the runtime binds and decodes for whichever backend is open. SQL uses `$N` placeholders, which both drivers accept. DDL is no longer compiled into the app: the stored schema shape keeps each field's type and default, and the runtime derives `CREATE TABLE` and `ADD COLUMN` statements for its dialect. On PostgreSQL integers are `BIGINT`, booleans stay `0`/`1` integers with a `CHECK` so both backends hold the same values, and `id` uses `COLLATE "C"` so cursor pages keep byte order. The SQLite statements are unchanged, so schema fingerprints, and existing databases, carry over.

SQLite serializes writers in-process (D14). PostgreSQL can have several instances on one database, so each mutation takes `pg_advisory_xact_lock` on its (actor, action, key) before reading the receipt, and revision checks rely on row locks re-evaluating the `WHERE` clause. Startup runs the whole schema check and migration inside one transaction under a fixed advisory lock, so two instances starting together cannot both migrate. A PostgreSQL database that already has tables but no `_nari_meta` is refused, as on SQLite. TLS is not compiled in: `sslmode=require` fails at startup instead of silently connecting in plaintext. TLS comes with the deployment adapter, which decides the certificate roots.

Acceptance impact: none for 0.1. `tests/postgres.rs` runs isolation, replay, cross-instance idempotency, the revision race, restart, migrations and refusals against a throwaway cluster, and CI requires it.

## D19 One deployment adapter: Docker, preview only until production identity exists

The production-web roadmap asks for a single deployment adapter. Docker is the one: an OCI image runs on any Docker engine, local or remote through `DOCKER_HOST`, and it is the artifact most hosts accept. `nari plan` writes the generated app to `.nari/generated`, builds it in `rust:<pinned toolchain>-slim-bookworm`, and copies the binary into `gcr.io/distroless/cc-debian12:nonroot`. Image labels carry the source and semantic hashes, the schema fingerprint and the schema shape, so the next plan reads the deployed schema from the running container instead of from local state. The plan records the endpoint and server version, the image digest, each resource with the action it will take, the authentication mode, the database engine and secret reference, the schema migration computed by the same planner the runtime uses, the readiness signal, the rollback rule, the observed target state, and a cost estimate with basis and date. It is written as canonical JSON under its SHA-256.

`nari release HASH` applies exactly that plan. It refuses a file that no longer matches its hash, a source that changed, a missing image, a different Docker endpoint, a target whose containers or volume changed since the plan (drift), and an unset secret variable. All of these are checked before anything is stopped. Readiness is the `NARI_READY` line with the planned source hash in the container log, which the runtime prints only after the database and schema checks pass and which also works on a remote engine. If the new container is not ready, the previous container is restarted, unless the release migrated the schema: the old binary cannot open the newer schema, so it stays stopped as `<name>-prev`, and the next plan refuses to proceed until someone resolves it. Each attempt is recorded in `.nari/deploy/releases/` with its outcome and a log tail that drops the bootstrap line.

Spec §10 requires production plans to reject development identity. Only `local_dev` exists, so `--production` is refused with `E_DEV_IDENTITY`, and preview plans publish on `127.0.0.1` only. Inside the container the app listens on `0.0.0.0` through the new `NARI_BIND` override, and the Host allow-list is unchanged because the host and container ports match. Secrets are passed by name to `docker run`, so they stay out of argv, plans and records. Docker still stores container environment in its configuration, which is readable by anyone who can control the engine. The runtime now compiles sqlx with rustls and webpki roots, so `sslmode=require` and `verify-full` work against managed PostgreSQL, which closes the TLS gap in D18. Staged releases, telemetry-based promotion and a production identity adapter come next.

Acceptance impact: §11's `plan` and `release` exist for the Docker preview environment. Production plans still fail explicitly.

## D20 Production identity is OpenID Connect through the `openidconnect` crate

§5 allowed only `local_dev`, and §10 forbids production plans with development identity. `mode = oidc` with `issuer` and `client_id` works with any compliant provider, so the app does not depend on one vendor. The issuer must be https, except `http://127.0.0.1` and `http://localhost` for local providers. The runtime uses the maintained `openidconnect` crate, behind an `oidc` feature that generated apps enable only for this mode, because it adds about 120 crates (reqwest and RustCrypto) to the build. At startup the app discovers the provider; if discovery fails, the app is not ready. `/_nari/login` creates a PKCE verifier, nonce and state, keeps them for ten minutes in a map capped at 10,000 entries, sets the state in an HttpOnly `SameSite=Lax` cookie scoped to the callback path, and redirects. `/_nari/callback` requires the cookie to match the state, consumes the pending entry so a callback cannot be replayed, exchanges the code with the verifier over an HTTP client that does not follow redirects, and verifies the ID token's signature, issuer, audience, expiry and nonce. A signature failure refreshes the provider's keys once, so signing-key rotation does not lock users out. The actor ID is a version-8 UUID from SHA-256 of the length-prefixed issuer and `sub`; it is stable across sign-ins and cannot collide across issuers. The label shown in the UI is the email claim, or `sub`. Errors returned by the provider are not echoed. Sessions keep the eight-hour lifetime, are capped at 100,000, and `POST /_nari/logout` ends one with CSRF.

`NARI_PUBLIC_URL` adds the public host and origin to the Host and Origin allow-lists and sets the `Secure` cookie flag when it is https. In oidc mode the bootstrap and dev-select routes are not served, and no bootstrap line is printed. `nari plan --production` now accepts oidc apps. It requires an https `--public-url` and takes `--oidc-secret-env` as a secret reference and `--publish-address` for non-loopback publishing; TLS for the public URL is terminated in front of the container.

Acceptance impact: none for 0.1 apps; `local_dev` output, hashes and the graph snapshot are unchanged. `tests/oidc.rs` signs real RS256 tokens with a mock provider. It checks sign-in, isolation by subject, stable actors, rejection of a wrong audience, issuer, expiry, nonce or signature, a forged or replayed callback, key rotation, sign-out, and https cookie flags.

## D21 Blue-green releases behind an nginx proxy, with expand-compatible schemas

D19 replaced the container in place, so every release had a short outage. Releases are now blue-green. An nginx container (`<name>-proxy`, pinned `nginx:1.30.6-alpine`) owns the published port. It routes over a user-defined Docker network to `<name>-blue` or `<name>-green`, which publish nothing. nginx was chosen over a proxy of our own: its graceful reload is a tested zero-downtime switch, and handling HTTP edge cases is not a place for new code. The route uses a variable upstream with Docker's resolver, so nginx starts and reloads even when a color is down. The active color is read back from the proxy's configuration, and it is part of the plan's observed state, so drift checks cover it.

A release runs the new color next to the active one on the same database. It waits for `NARI_READY` with the planned source hash, checks `/_nari/health` from inside the network, and writes the new route. If `nginx -t` passes it reloads; otherwise it restores the previous file. Then it polls through the proxy until the new source hash answers. After a five-second drain, the old color receives SIGTERM, finishes in-flight requests and is removed. A failure before the switch removes the new color, and traffic never moves (`aborted`). A failure after it routes back (`rolled_back`). The first release creates the proxy after the first color is ready. A container from a D19-style release is refused with `E_LEGACY_DEPLOYMENT`, because the proxy needs its port.

Two runtime changes make the overlap safe. First, the expand step: on a fingerprint mismatch where the stored schema only adds to the compiled one (new entities, new fields with defaults, widened text bounds), the runtime now starts against it unchanged instead of refusing. The old color's inserts get the column defaults, and its reads ignore columns it does not know. A plan whose source is behind the deployed schema says `behind: true` and needs no migration, and `nari diff` lists the difference as `database_ahead_by` instead of `blocked`. The runtime cannot tell an old color during a release from a source that removed a field or narrowed a bound, so both run behind: the column and its data stay, unused, and dropping them is a contract step that needs separate authority (spec §10). A change that both builds cannot accept, such as a different type or owner, is still refused. Second, `mode = oidc` sessions are stored in `_nari_sessions`, keyed by the SHA-256 of the token, so a session created on one color is honored by the next. Each instance caches sessions in memory, so a sign-out reaches other instances' caches only when they next miss. `local_dev` sessions stay in memory, so a preview switch asks the developer to open the new bootstrap link.

Acceptance impact: none for 0.1. The D16 guarantee that data is never dropped or reinterpreted still holds. The relaxation only lets an older build run against a strict superset of its schema. `tests/deploy.rs` releases with a migration while a client polls the app through the proxy every 20 ms: no request fails, both versions answer, the old color is retired, and a release that never becomes ready drops nothing and leaves traffic on the old color. The post-switch route-back is implemented but has no automated test.
