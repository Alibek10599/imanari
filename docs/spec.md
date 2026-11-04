# Imanari: system language and adaptive runtime

Version: 0.1-draft. Date: 2026-11-02. Status: draft specification. 0.1 is implemented in this repository as a developer preview.

## 1. Product contract

Imanari is a human-readable language for describing an application as a connected system: data, capabilities, user interfaces, workflows, infrastructure, recovery policies, and operational evidence. Agents should implement changes using bounded, versioned context rather than reconstructing the project from conversation history.

The long-term product must support web, iOS, Android, backend services, and infrastructure end to end. It must recover from known operational failures, propose and validate repairs, and retain evidence that improves subsequent decisions. It must work both by changing Imanari code and by controlling systems through declared adapters without changing their implementation.

Security and performance are requirements, not marketing guarantees. Imanari targets Rust-style memory and data-race safety in its native implementation and competitive native performance on comparable workloads. It does not claim that every application is secure or that every workload matches C++. Browser rendering, networks, databases, third-party libraries, and native platform bridges remain part of the actual system.

MUST, SHOULD, and MAY are normative. A milestone is complete only when its acceptance checks pass. Unsupported syntax and features MUST produce diagnostics rather than silently degrade to something else.

## 2. Repository boundary and deliverable

Imanari is a standalone project. It MUST have its own Cargo workspace, lockfile, toolchain pin, tests, and README. This specification covers local implementation and verification. Publishing packages, provisioning paid resources, and deploying externally are outside the 0.1 deliverable.

## 3. Release boundaries

### 0.1: runnable local vertical slice

Implement a real `.nari` parser, semantic checks, canonical intermediate representation (IR), Rust backend generation, a browser frontend, SQLite persistence, local development identity, context extraction, structured diagnostics, bounded process recovery, incident memory, and a reproducible isolated repair demonstration.

The reference application is Fieldwork: two users can create, list, and complete their own tasks. The complete operation checks a record revision to prevent lost updates. A worker can crash and recover without losing committed tasks. A seeded bug can be reproduced, patched in an isolated workspace, validated, and recorded. No live LLM account is required to run the acceptance suite.

0.1 is a developer preview, not a production-ready application platform. Native mobile, arbitrary custom business functions, external effects, multi-step durable workflows, production authentication, cloud provisioning, and autonomous production patch release are later milestones with explicit gates below. The compiler MUST reject their declarations in 0.1. Do not substitute placeholder endpoints for working features or claim mobile delivery from a browser preview.

### Subsequent releases

1. Production web: real identity adapter, PostgreSQL, migration compatibility, a single deployment adapter, telemetry and staged releases.
2. Mobile: generated SwiftUI and Kotlin Compose views, shared generated API contracts, native extension boundaries, compatibility with older clients, and explicit offline policies.
3. Extended language: typed business expressions and trusted native extensions, then durable workflows and external adapters.
4. Adaptive operations: canary deployment, external operational repair, evidence-based recovery selection, and narrowly scoped automatic repair releases.

Do not implement all future releases before making 0.1 usable. Their requirements constrain interfaces; they do not justify speculative frameworks.

## 4. Implementation choices

- Compiler, CLI, supervisor, and backend runtime: stable Rust, pinned at implementation start in the standalone workspace. Use `#![forbid(unsafe_code)]` in all first-party Rust crates and generated Rust application code.
- HTTP and async: Axum and Tokio. Database: SQLx with SQLite for 0.1. Serialization: Serde/serde_json. CLI: clap. Use minimal crate features, compatible maintained versions, and a committed lockfile; do not invent crypto, HTTP, or database engines.
- Application actions compile ahead of time to Rust handlers. Do not interpret the source language, call an LLM, or walk the system graph for every request. Shared runtime helpers are acceptable.
- Web target: generated HTML, CSS, and browser JavaScript using native form controls and DOM APIs. Generate type declarations for the API. A frontend framework is not required for the initial three screens/states.
- Mobile later: native view generation through SwiftUI and Kotlin Compose. Share contracts and behavior definitions, allow explicit target-specific presentation. WASM is optional for measured shared computation, not the default UI renderer.
- Start with one Rust package with a library, CLI binary, and normal modules. Split crates only when an actual compilation or ownership boundary needs it. Generated apps are separate packages depending on the versioned runtime interface.
- Suggested modules: `syntax`, `semantic`, `ir`, `codegen`, `runtime`, `context`, `supervisor`, `repair`. Avoid plugin loaders, distributed schedulers, a custom VM, a new borrow checker, or an LLM-provider framework in 0.1.

Dependencies and the operating system may contain unsafe code; forbidding it in first-party code does not remove that trusted boundary. Rust extensions introduced later are trusted native code, not a permission sandbox merely because they compile safely.

## 5. Language 0.1

### 5.1 Syntax

`fieldwork.nari` accompanying this document is the normative positive fixture. Earlier conversational examples were sketches and are superseded by this grammar.

```ebnf
document    = "imanari", string, ";", "app", identifier, "{", entry*, "}" ;
entry       = property | block ;
property    = identifier, "=", value, ";" ;
block       = identifier, identifier, "{", entry*, "}" ;
value       = string | integer | boolean | symbol | reference | array | object ;
symbol      = identifier ;
reference   = "@", identifier, (".", identifier)* ;
array       = "[", [value, (",", value)*, [","]], "]" ;
object      = "{", [identifier, ":", value, (",", identifier, ":", value)*, [","]], "}" ;
```

Identifiers are ASCII `[A-Za-z_][A-Za-z0-9_]*`. Strings use JSON string escaping. Integers use JSON integer syntax, no fractions/exponents. Booleans are `true`/`false`; they are reserved and cannot name symbols. Whitespace and `//` line comments are ignored outside strings. No interpolation, imports, macros, shell expansion, or executable strings. Duplicate keys, duplicate names in a scope, unknown keys, and unknown blocks are errors. All AST nodes retain byte spans and file paths.

One app file is sufficient in 0.1. Logical graph nodes, not file boundaries, determine context. The parser must impose limits: 1 MiB source, nesting depth 64, and 10,000 declarations. A configurable trusted limit is allowed later; input cannot raise its own limits.

### 5.2 Schema and typing

The only app property is `targets`, exactly `[web]` in 0.1. Supported top-level blocks are `identity`, `entity`, `action`, `screen`, `runtime`, `recovery`, and `repair`. Exactly one identity, runtime, recovery, and repair block is required; other blocks may repeat with unique names. IDs are `app-name/kind/name`; nested IDs append their kind/name. Names are stable identifiers, not generated line-number IDs. Renames are removals plus additions in 0.1.

Field and input blocks have `type` in `Text | Bool | UUID | Int`. Text permits `min_length` and `max_length`, counted in Unicode scalar values; default bounds are 0..4096. Integers are restricted to the JavaScript-safe inclusive range ±9,007,199,254,740,991 and stored as checked Rust integers. UUIDs have canonical lowercase string representation. No implicit casts, nullable values, floats, arbitrary references, or user-defined pointers. All inputs are required; `default` is allowed on entity fields only and must match the type and constraints. HTTP requests containing unknown input fields are rejected.

Every entity has implicit, reserved `id: UUID` and `revision: Int` fields. The runtime generates IDs and starts revisions at 1. Authors cannot declare or directly assign these fields. An `owner_field` property must name an explicit UUID field. All entity access in 0.1 is authenticated and owner-scoped, enforced on the server. There are no public entities or policy expressions in this release.

Each action has `kind`, `entity`, and `access = signed_in`. Optional repeated `input` blocks define its input shape. Action result and possible errors are inferred from the operation:

| Kind | Required properties | Semantics |
| --- | --- | --- |
| `insert` | `set` object | Insert one owned record, return it; unspecified fields require defaults |
| `list` | none | Return owned records with bounded pagination |
| `update` | `id`, `expected_revision`, `set` | Update one owned record at the expected revision; return updated record |

`set` values may be literal typed values or `@input.field` references. On insert only, the owner field MUST be assigned `@actor.id`; any alternative is a compile error. Updates cannot assign the owner, id, or revision. `id` must reference a UUID input and `expected_revision` an Int input. References must resolve and types/constraints must be validated. Narrower destination constraints are enforced at runtime even if the source type is wider. Extra assignments, empty update sets, and missing required assignments fail compilation.

For list actions, `limit` and `after` are reserved transport inputs and cannot be author-defined: limit defaults to 50 and is 1..100; after is an optional canonical UUID. Results sort by id ascending, filter strictly after the cursor, and return `{items, next_cursor}`. The cursor is a position, never permission to see another user's rows. Concurrent inserts may require a refresh to observe; pagination is not a snapshot guarantee.

0.1 has no arbitrary user Rust or JavaScript function bodies. These small operations are the first executable language subset, not the final limit of Imanari. Later native extensions must declare effects; an in-process Rust declaration alone cannot enforce capabilities against malicious code.

### 5.3 Screens

A screen has `route`, `title`, and `query` referencing a list action. Routes must be unique absolute paths without `..`, query strings, or fragments. It may contain named `form` blocks and `list` blocks.

A form has `submit` referencing an insert action and `input` blocks whose names match its inputs. Form input properties: `label` and optional `placeholder`; type and bounds derive from the action. Required controls and associated labels are generated. No owner or identity control is rendered.

A list has `source = @query.items`, `fields` naming entity fields, and optional `button` blocks. A button has `label`, `action` referencing an update, and `arguments` binding every action input to `@item.field` or a typed literal. The server rechecks all input and ownership. `@query` and `@item` references are valid only in their respective UI scopes. `@actor` is never a browser-supplied server identity.

Generate loading, empty, error, and success states; prevent double submission while a request is outstanding; keep failed form input; announce errors accessibly; refetch after successful mutations. Support pagination. On stale revision show a conflict and refresh; do not silently overwrite newer data. Render record content with text nodes, never raw HTML. No scripts, template expressions, or arbitrary HTML in language values.

### 5.4 Runtime and policy declarations

`identity NAME { mode = local_dev; }` is the only 0.1 identity mode. `runtime` properties are `bind` (must be 127.0.0.1), `port` (1024..65535), `database` (relative path under `.nari/`), `request_limit_bytes` (1..65536), and `request_timeout_ms` (100..30000). Reject symlink or traversal escapes for generated files and database paths. Persistent files use restrictive permissions where supported.

`recovery` declares `target = backend`, `max_restarts`, `window_seconds`, `backoff_ms`, and `after_exhaustion = stop`. Bounds: restarts 0..10, window 1..3600 seconds, backoffs 0..60000 ms. Array length must equal max_restarts. Restart budgets are persisted and cannot reset merely by restarting the supervisor.

`repair` declares `mode = isolated`, `max_attempts` (0..3), `editable_actions` (action symbols), and `automatic_release = false`. Only `set` bindings on listed actions are editable in the 0.1 repair demonstration. Policy itself, permissions, schemas, UI, runtime configuration, acceptance tests, and dependency files are protected. Normal developer changes remain possible through the normal checked build workflow.

## 6. Compiler, graph, and artifacts

Pipeline: parse → validate schema/types/references → construct typed IR → compute dependency graph → emit native backend and web files → compile → package manifest. Diagnostics must identify the originating Imanari span even when Rust compilation fails; include raw generated compiler output as additional detail.

The canonical IR includes schema/compiler versions, stable IDs, typed properties, inferred contracts/effects, resolved references, source mappings, and forward/reverse dependency edges. JSON encoding must be canonical: sorted object keys, deterministic declaration ordering, no timestamps or absolute host paths in semantic hashes. Record source byte hashes separately from semantic hashes. Hash with SHA-256. Changes to comments need not change generated behavior but must invalidate a source-based patch precondition.

Edges must cover action→entity, screen→action, identity→protected operations, runtime→affected artifacts, and recovery/repair→their targets. Store dependency roles, so changes to authentication invalidate all affected actions even if their own text is unchanged. A new or unknown edge class requires conservative validation, not optimistic skipping.

Artifacts live under `.nari/`: `ir.json`, `graph.json`, `generated/`, `build/`, `context/`, `incidents/`, and runtime databases. This directory is ignored by version control except explicitly exported evidence. Generated files are disposable, carry source/version provenance, and must never be edited as the canonical application source. Dependency locks and the source manifest remain tracked outside generated output.

Compile-time work should eliminate repeated schema parsing and graph traversal from the request path. Retain necessary input validation, authorization, bounds checks, and transaction checks. Optimization must never remove those checks to satisfy a benchmark.

## 7. Runtime correctness and security

### Identity and web boundary

Development identity is explicitly local-only. Seed two stable user UUIDs. Generate separate cryptographically random session tokens and a single-use bootstrap token expiring after five minutes on startup; never store them in source, context bundles, incident records, or ordinary logs. The CLI may print the local bootstrap URL with its token in the fragment. The page exchanges the token through a same-origin POST, removes the fragment, and obtains an HttpOnly, SameSite=Strict development session cookie. The bootstrap session can select either seeded user through a CSRF-protected development endpoint. Other sessions cannot forge an actor ID. Tests may use a private test harness to obtain both sessions. Session lifetime is eight hours or process lifetime, whichever ends first; an application restart may require bootstrapping again and must not erase user records.

Bind only to loopback, validate Host and Origin, reject cross-origin state changes, and protect cookie-authenticated mutations with a CSRF token. Omitted Origin is permitted only for explicit authenticated CLI/test requests with the required anti-CSRF header. The production planner must reject `local_dev` entirely. Use a strict CSP, no inline executable scripts, and no permissive CORS. Authenticated responses and bootstrap responses use `Cache-Control: no-store`.

This is a local development convenience, not protection from a hostile user or malware on the same host. Production authentication is a separate release gate using a maintained provider/library, not a homegrown password implementation.

### Request and data semantics

All actions use `POST /api/actions/<action-name>` with a JSON input object. Mutation requests carry an `Idempotency-Key` header. List requests are read-only even though their transport is POST. Responses are `{data, request_id}` or `{error: {code, message, field?}, request_id}`. Never include database errors, secrets, or stack traces in public error messages.

Error mapping: malformed/invalid input→400; missing or invalid session→401; absent or inaccessible record→404; revision or idempotency conflict→409; limit exceeded→429; temporary unavailable→503; internal failure→500. Ownership failures and missing records share the same public result. Do not report operation success until the transaction commits.

Generate parameterized SQL. Author identifiers must pass lexical validation and still be quoted by the backend adapter. Ownership filters must appear inside the actual select/update predicate; client filtering is insufficient. Updates atomically check id, owner, and expected_revision, then increment revision; overflow is an error. Use transactions to combine the operation, revision change, and successful idempotency receipt.

On first startup, initialize a new database and store its schema fingerprint and schema shape. When the compiled schema differs from the stored one, the runtime applies only additive changes, automatically and in one transaction before serving: new entities, new fields that declare a default, and widened text bounds. Each applied migration is recorded in `_nari_schema_history`. Any other difference (a removed entity or field, a changed owner field or type, narrowed bounds, or a new field without a default) must stop startup with `MIGRATION_REQUIRED` and name the change; do not drop, recreate, or silently reinterpret stored data. A database without a stored shape only accepts an identical fingerprint. Non-schema application changes may reuse the database. A developer can explicitly choose a different database path for experiments. Migrations that remove, rename or rewrite stored data belong to the production-web milestone and need separate authority.

Idempotency receipts are scoped to actor, action, and key, with a hash of validated canonical input and a stored response. Repeated identical requests return the committed response; different input with the same key returns 409. Concurrent duplicates must converge through a uniqueness constraint and transaction handling. A committed operation followed by a lost HTTP response must not duplicate data on retry. Keep receipts for the life of the 0.1 development database; bounded retention is a later explicit compatibility policy. Failed transactions do not store success receipts.

Apply a default limit of 100 concurrent requests and 60 mutations per actor per minute; bound request size and query results. Return 429 on saturation. Classify dependency timeouts separately from invalid inputs. An HTTP timeout must not be presented as proof that a write did not occur: clients retry using the same idempotency key or inspect its receipt. No external side effects exist in 0.1.

### Trust boundaries

The compiler/runtime, toolchain, dependencies, identity provider, and release authority are trusted components. Imanari source, HTTP inputs, telemetry strings, repair proposals, and retrieved incident content are untrusted data at their boundaries. Log text can never instruct a repair tool to run arbitrary commands. Generated Rust string literals must be escaped structurally, not interpolated as code fragments.

Do not invoke shells for generated build/run commands; pass executable and argument arrays. A repair agent must not receive production secrets or authority to rewrite its own guardrails. Safe Rust prevents classes of memory misuse, not SQL injection, broken authorization, resource exhaustion, supply-chain compromise, or business mistakes. Explicit checks above remain mandatory.

## 8. Context as a build artifact

`nari context <stable-id>` must emit a readable `context.md` and machine-readable `manifest.json`. Include:

1. Target source spans and inferred contract/effects.
2. Required entity definitions, applicable permissions and system constraints.
3. Direct dependency contracts and reverse-dependent contracts affected by changes.
4. Relevant tests, diagnostic evidence, and explicitly linked decisions/incidents.
5. Source, semantic, dependency-lock, toolchain, and policy hashes.
6. Included and omitted node IDs, why each was included, and how to request expansion.

Do not automatically include every dependency implementation or chat history. Default limit: 32 KiB for readable content. If required contracts/constraints exceed that limit, fail with `CONTEXT_BUDGET_EXCEEDED` and emit a manifest of required expansion; never silently truncate required facts. Optional incidents may be omitted with an explicit reason. Byte count is exact; any displayed token estimate must be labeled an estimate.

The bundle is a snapshot, not a claim of complete understanding. A patch bundle contains the base source hash, graph hash, affected-node IDs, allowed scope, and candidate diff. Before applying, revalidate preconditions. In 0.1 any source change since capture rejects the patch as stale; this conservative rule can later become dependency-aware. Graph changes require recomputing affected checks. Unknown dynamic dependencies require a full check.

Context records must distinguish observed facts, inferred hypotheses, requirements, and historical outcomes. Incidents cannot become executable policies merely because retrieval matched text. Record progress outside conversation: task ID, base revision, completed work, outstanding checks, and evidence paths. Resuming a task rechecks those paths and hashes.

## 9. Recovery, repair, and learning

### Recovery without code changes

The supervisor is a separate process from the application. It owns process lifecycle and a separate durable control database. On unexpected exit, persist the incident and restart attempt before launching a replacement. Use the declared backoff sequence and sliding-window restart budget. Exhaustion transitions to `StoppedNeedsAttention`; do not loop forever. A deliberate shutdown does not trigger a restart. Serialize lifecycle decisions so duplicate exit notifications cannot launch two workers.

Persist states `Starting`, `Healthy`, `Backoff`, `StoppedNeedsAttention`, and `Stopped`. After supervisor restart reconcile the managed process identity before starting another; process IDs alone are insufficient because they are reused. Use an ownership token/lock and verified child endpoint or stop with an explicit reconciliation error. A process launch/readiness timeout counts as a failed restart. Mark readiness only after the database and schema compatibility checks pass.

0.1 demonstrates crashes/restarts. Later adapters may support queue pausing, replica failover, bounded scaling, or rolling back an application release. Each must declare preconditions, authority, expected effects, verification, and whether compensation exists. Never assume restarting repairs a deterministic bug or that rollback restores externally changed data.

### Repair by changing code

0.1 repair is local and isolated. A deterministic test proposer is required; an optional live proposer may use the same interface. Do not require credentials to demonstrate the pipeline. The proposer receives an incident/context bundle and returns a structured patch proposal; it gets no direct access to the running app, protected tests, or control policy.

State machine:

`Detected → Reproduced → Proposed → Validating → Validated | Rejected | Exhausted`

`Validated` means eligible for explicit local application, not released to production. Each transition persists the incident ID, candidate hash, evidence, policy hash, and attempt count. Failed reproduction ends with an inconclusive result; do not fabricate a cause. A new independent incident does not automatically reset a budget for the same unresolved failure signature.

Validation procedure:

1. Capture source and immutable acceptance-test snapshots plus their hashes.
2. Reproduce the reported behavior in a disposable database/workspace.
3. Parse the proposed patch as data; enforce file paths, source preconditions, and semantic edit scope. For 0.1 only allowed action `set` bindings may change.
4. Recheck the whole application and regenerate artifacts. Compile candidate code separately.
5. Run the unchanged reproduction and protected regression suite against the candidate; verify that they failed/passed as expected before/after.
6. Compare permissions, schemas, effects, dependencies, and protected invariants. Any widening or weakening rejects the candidate.
7. Store the report and candidate without changing the running application.

Use an OS/container sandbox with no network, no production credentials, read-only protected fixtures, and time/memory/process limits to execute candidate builds and tests. Separate compilation from execution; compiler plugins and build scripts are also code execution. Prefetch and lock trusted dependencies outside the candidate sandbox, build offline inside it. A temporary directory alone is not a security sandbox. If isolation is unavailable, proposal and static validation may work, but dynamic repair validation must report `SANDBOX_UNAVAILABLE` and cannot claim `Validated`.

A trusted operator can apply a validated local patch with a separate CLI command, which rechecks hashes and protected tests. Repairs to the compiler, runtime, supervisor, tests, or repair policy itself use normal development/review; the repair controller cannot authorize them. Do not automatically generate a passing test from the same candidate and call that independent validation.

### Learning from evidence

Store incident records in the control database and export redacted JSON. Minimum fields: incident ID, target ID, source/runtime/policy versions, failure signature, observed symptoms, hypotheses, reproduction reference, intervention, preconditions, checks, outcome, timestamps, and supersedes/revocation links. Keep credentials and raw user payloads out; use synthetic reproduction fixtures or explicitly sanitized data.

Initially, learning is retrieval and outcome accounting. Exact compatible failure signatures rank validated past interventions ahead of untried ones. Never expand authority based on a success counter. A fix observed once is a candidate precedent, not a universal rule or evidence of causation. Incompatible dependency/schema versions exclude automatic reuse. Failed and inconclusive attempts remain visible. Deterministic lexical/signature retrieval is enough; no vector database or model training is required.

## 10. DevOps, mobile, and later extension requirements

These are roadmap contracts, not 0.1 implementation requirements:

- Production plans must show concrete resources, authentication mode, secret references, connectivity, schema transitions, artifact digests, and estimated costs with the basis/date of estimates. Apply exactly a reviewed plan with drift checks. Reject development identity or unknown required adapters.
- Releases use immutable artifacts and readiness gates. Canary promotion requires a declared observation window, minimum evidence volume, correct-behavior checks, and comparison against baseline, not only fewer errors. Missing evidence prevents promotion.
- Database changes follow expand/migrate/contract where needed. Destructive changes and data repair need separate authority. Compatibility must be checked before reverting a binary; database rollback is not assumed.
- Mobile clients carry contract versions. Backend changes must account for installed older clients. Offline writes require explicit queue, idempotency, conflict-resolution, and retention semantics. Do not label an operation offline-safe without them.
- Native mobile binaries follow platform distribution requirements. Remote repair cannot assume arbitrary native code replacement on already-installed devices; server recovery, declared remote configuration, or a new client release may be required.
- Workflows persist versioned state and effect receipts. Recovery is at-least-once unless an adapter supplies stronger guarantees. External effects require idempotency or reconciliation; exactly-once execution cannot be inferred from a workflow log.
- Third-party services can participate through typed adapters and observability even if their code is unavailable. Healing is limited to the adapter's actual controls. An opaque service cannot be promised arbitrary internal repair.
- Native extensions expose typed owned values, no user raw pointers, and explicit allocation/error behavior. Trusted in-process extensions can exceed declared effects; untrusted extensions require a real isolation boundary. Browser client validation is never the authoritative backend policy.

## 11. CLI contract

All commands accept `--project PATH` and `--json`; default project is the current directory containing exactly one `.nari` file. Human output is concise. JSON diagnostics contain `code`, `severity`, `message`, `node_id`, `file`, `span`, and `details`. Exit codes: 0 success, 1 validation/check failure, 2 usage/unsupported feature, 3 execution/environment failure, 4 stale precondition or policy rejection. Do not leak secrets in JSON mode.

| Command | Required behavior |
| --- | --- |
| `nari init NAME` | Create a non-overwriting standalone example project |
| `nari fmt [--check]` | Deterministic source formatting, preserving comments |
| `nari check` | Parse/type/reference/policy validation, no server or network |
| `nari build` | Emit IR, graph, native app, browser files, and artifact manifest |
| `nari dev` | Build then run supervisor/app; local development bootstrap |
| `nari test` | Run generated contract checks and protected project tests |
| `nari context ID [--max-bytes N]` | Emit versioned context and inclusion manifest |
| `nari diff --base PATH` | Compare two source snapshots by behavior and dependencies |
| `nari status` | Read supervisor state and recent incident summary |
| `nari incidents [--id ID]` | Read redacted records and evidence references |
| `nari repair ID --proposer fixture` | Run isolated repair pipeline, never release |
| `nari apply-repair ID --candidate HASH` | Explicitly apply validated local candidate after rechecks |
| `nari bench` | Reproducible compiler/runtime/context benchmark report |

`plan`, `release`, and mobile build targets return explicit unsupported-feature errors until their milestones exist. `nari check` remains offline after installing the CLI. `build` may need dependency installation initially, and must report that distinctly from source validation failure.

Behavior diff reports added/removed capabilities, input/output changes, access changes, data migrations (split into changes applied automatically and changes that block startup), affected consumers, runtime changes, and recovery/repair authority changes. Any unclassified semantic difference is shown as unknown and triggers full validation; do not call it harmless.

## 12. Performance requirements and evidence

Goals are provisional acceptance budgets, not measured claims. Use release builds on the same machine with recorded CPU, OS, compiler flags, dependency versions, data sizes, warmup, repetitions, concurrency, and raw samples. Report median, p95, throughput, peak memory, and error rate as appropriate. Separate clean compilation, incremental compilation, request handling, database, browser, and model/proposer time.

- Compiler/graph: check a generated 1,000-action fixture in ≤1 second median on a documented developer machine; warm context extraction ≤100 ms median. Include correctness checks and report hardware-dependent misses honestly.
- Native runtime: compare generated Imanari handlers with hand-written Rust using the same HTTP stack, database, auth, validation, idempotency, and semantics. Target ≤15% p95 latency overhead and ≤20% peak-memory overhead under identical load; record throughput too. If measured durations are below timer resolution, batch operations.
- C++ comparison: include a small equivalent bounded validation/transformation kernel in optimized C++ and generated Rust. Match integer/overflow/error semantics and inputs, verify equal outputs, and target within 20% runtime on that kernel. Report the workload and variance. This does not establish whole-stack C++ parity. If a C++ toolchain is absent, mark the comparison not measured rather than passed.
- No LLM call, repair decision, source parsing, or unbounded incident retrieval on the normal request path. Batch or queue telemetry with explicit capacity and dropped-event counters; audit-critical recovery transitions persist before action.
- Agent iteration: script the same representative changes against a baseline hand-written app and Imanari. Report context bytes, bytes of changed source, checks run, elapsed time, failures, and successful fresh-agent continuation. Do not claim model token savings without actual token counts from a named tokenizer/provider.

Use at least five measured runs after warmup. If budgets fail, profile and record the cause; do not weaken security, omit validation from one baseline, or cherry-pick runs. Functional milestones may be demonstrated with documented performance misses, but the performance gate remains open.

## 13. Acceptance suite

1. Parse/check/format the supplied fixture; formatted output parses to the same semantic IR and preserves comments. Canonical outputs match across repeated builds with identical locked inputs, excluding documented binary/toolchain nondeterminism.
2. Reject unresolved actions, duplicate declarations, unknown properties, invalid types/references, owner reassignment, reserved-field changes, oversized/deep syntax, and unsupported targets with useful spans.
3. Create a task from the generated browser, reload it from SQLite, list it, and complete it. Verify loading, errors, empty state, labels, keyboard operation, and escaped malicious text.
4. User B cannot list or update A's records, including by guessing IDs or modifying client requests. Unauthenticated and forged-session requests fail. Test CSRF, origin/host checks, input limits, and unknown fields.
5. Two updates using the same revision yield exactly one success. Idempotent concurrent create attempts yield one record. Simulate a committed write whose HTTP response is lost; the retry returns its receipt.
6. Kill the app child during a request. On restart committed data survives and no success is falsely reported for a rolled-back transaction. Exceed the restart budget and verify a durable stopped state. Restart the supervisor and verify no duplicate worker or reset budget.
7. Extract context for `fieldwork/action/complete_task`. Required Task, auth, relevant consumers, and protected checks are included; unrelated implementations are absent. Oversized mandatory context fails explicitly. Changing source after capture rejects a patch.
8. Seed a bug assigning `completed = false` in `complete_task` while a protected behavioral test expects completion. Reproduce, propose `true`, validate in isolation, and store a candidate. Verify no running-source mutation before explicit apply.
9. Reject patches changing ownership, recovery limits, protected tests, editable scope, paths outside the project, or dependency files. Reject a stale candidate. On missing sandbox, report unavailable rather than successful repair.
10. Record failed/inconclusive/successful attempts and retrieve only version-compatible precedents. Scan exported context/evidence for seeded secret canaries; none may appear.
11. Generate a behavior diff for an input-bound change and an owner-policy change, correctly identify affected nodes, and conservatively check unknown changes.
12. Run the benchmark suite and publish measured results, baseline definitions, and unresolved budget misses.

Use targeted unit tests for parser/semantics, integration tests for transactional/runtime boundaries, and one browser journey for the reference app. Include bounded parser fuzzing or property-based malformed-input tests. Do not add tests that merely duplicate generated source strings without checking behavior.

## 14. Implementation sequence and completion evidence

**M0 — Bootstrap and language:** standalone Rust project, grammar, formatter, schema validation, stable IR, source spans. Deliver `check`, `fmt`, fixture tests, and deterministic graph snapshots.

**M1 — Executable app:** Rust code generation, SQLite schema, sessions, owned actions, transactional idempotency/revisions, web generator, `build/dev/test`. Deliver the working two-user reference app and security/concurrency checks.

**M2 — Agent workflow:** graph dependencies, context manifests, stale patch rejection, behavior diff, persistent task progress format. Deliver reproducible bundles and a fresh-session handoff example.

**M3 — Recovery:** separate supervisor, persisted budgets, crash injection, reconciliation, redacted incident records. Deliver crash/restart and exhaustion evidence.

**M4 — Isolated repair and learning:** fixture proposer, sandbox runner, protected checks, scope enforcement, candidate/application separation, incident retrieval. Deliver both a successful repair and rejected malicious/stale proposals.

**M5 — Measurement and packaging:** performance harness/baselines, CLI help, clean-install instructions, dependency inventory, limitations, and a completion report mapping every acceptance item to evidence. Mark unavailable environments and unmet budgets honestly.

Complete these milestones in order. If a required external facility is unavailable, finish independent work and report the exact blocker, command, and missing prerequisite. Do not substitute mocks for the actual database, worker lifecycle, ownership checks, or sandbox and label the milestone complete. The deterministic proposer is an intentional test component; production autonomous repair remains a later release.

## 15. Grounding and nonclaims

- Lua offers protected calls, loading, and debugging, not built-in autonomous repair or learning: [Lua reference manual](https://www.lua.org/manual/5.5/manual.html).
- Supervised restart strategies and restart-intensity limits inform recovery: [Erlang/OTP supervision](https://www.erlang.org/doc/system/sup_princ.html).
- Safe Rust informs the native implementation boundary; unsafe code and trusted dependencies still matter: [Rust unsafe-code chapter](https://doc.rust-lang.org/stable/book/ch20-01-unsafe-rust.html).
- Native performance is a design goal to measure: [Rust introduction](https://doc.rust-lang.org/book/ch00-00-introduction.html).
- Declarative constraints and explicit dependencies have precedents: [CUE introduction](https://cuelang.org/docs/introduction/) and [Bazel dependency guidance](https://docs.bazel.build/versions/main/best-practices.html).
- Durable workflow replay has explicit requirements: [Temporal durable execution](https://learn.temporal.io/tutorials/go/background-check/durable-execution/).
- Selected implementation libraries: [Axum](https://docs.rs/axum/latest/axum/) and [SQLx](https://docs.rs/sqlx/latest/sqlx/). Resolve compatible versions when implementing and pin them.

Imanari is a working name, not a trademark or package-name availability claim. The proposed combination is not a claim that no existing system has related ideas. No compiler, self-healing system, universal security guarantee, or C++ performance result has been delivered by this document alone.
