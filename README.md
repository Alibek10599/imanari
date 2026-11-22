# Imanari

Imanari is a small, human-readable system language. A `.nari` file declares data, owner-scoped actions, screens, runtime limits, recovery policy, and repair authority. The compiler turns it into a native Rust backend (Axum + SQLx on SQLite or PostgreSQL), a browser UI built from native controls, a typed IR, and a dependency graph. Agents work from bounded, versioned context bundles instead of reconstructing the project from chat history.

Imanari 0.1 is a developer preview. [docs/spec.md](docs/spec.md) is the normative contract and [examples/fieldwork](examples/fieldwork) the reference app. [docs/progress.md](docs/progress.md) maps every acceptance item to its evidence, and [docs/decisions.md](docs/decisions.md) records spec clarifications.

This is a local vertical slice. It is not a production platform, and it makes no universal security or C++-parity claims (see [Non-claims](#limits-and-non-claims)).

## Requirements

- Rust via rustup. Generated apps pin Rust 1.97.1 through `rust-toolchain.toml`, and rustup installs it on first use.
- macOS for isolated repair, which uses Seatbelt (`/usr/bin/sandbox-exec`). On other platforms repair runs proposal and static validation only and reports `SANDBOX_UNAVAILABLE`.
- Network access for the first `nari build`, which downloads crates once. Later builds work offline.
- Optional: Docker (Engine 23+ with BuildKit) for `nari plan` and `nari release`.
- Optional: Node 22+ and a Chromium (Playwright's headless shell or Chrome) for the browser journey test, and `c++` for the C++ kernel benchmark.

## Install

Imanari compiles apps with Cargo, so install Rust with [rustup](https://rustup.rs) first.

Prebuilt binaries for macOS (Apple silicon) and Linux (x86_64 and arm64) are attached to each [release](https://github.com/Alibek10599/imanari/releases/latest). On macOS:

```sh
curl -fsSL https://github.com/Alibek10599/imanari/releases/latest/download/imanari-aarch64-apple-darwin.tar.gz | tar -xz
sudo install imanari-aarch64-apple-darwin/nari /usr/local/bin/nari
```

On Linux, use `imanari-x86_64-unknown-linux-gnu` or `imanari-aarch64-unknown-linux-gnu` instead. Each archive has a `.sha256` file next to it.

To build from source instead:

```sh
cargo install --locked --git https://github.com/Alibek10599/imanari imanari
```

From a checkout, `cargo build --release` puts the CLI at `target/release/nari`.

## Run the reference app

```sh
cd examples/fieldwork
nari check                           # parse, type, reference and policy checks (offline)
nari fmt --check                     # deterministic formatting; comments are preserved
nari build                           # writes .nari/{ir.json,graph.json,generated/,build/,manifest.json}
nari dev                             # builds, starts the supervisor and app, prints a one-time bootstrap URL
```

Open the printed `open: http://127.0.0.1:7331/#bootstrap=…` URL. The page exchanges the token, removes it from the URL, and sets an HttpOnly, SameSite=Strict session. The dev bar switches between the two seeded users (User A and User B). Stop with Ctrl-C.

The first `nari build` compiles Axum, SQLx and Tokio in release mode, which takes about a minute. To share that cache between projects, set `NARI_TARGET_DIR`.

## Commands

| Command | What it does |
| --- | --- |
| `nari init NAME` | Create `NAME/` with an example app and protected tests; never overwrites |
| `nari fmt [--check]` | Format deterministically, preserving comments; refuses output that changes semantics |
| `nari check` | Validate offline, with diagnostics that carry file, line, column, byte span and node id |
| `nari build` | Write IR, graph, Rust backend, web files and the artifact manifest, then compile |
| `nari dev [--acknowledge]` | Run the supervisor and app; `--acknowledge` clears `StoppedNeedsAttention` and keeps the restart history |
| `nari test` | Run generated contract checks plus the protected tests in `acceptance/*.json`; failures become incidents |
| `nari context ID [--max-bytes N]` | Write `.nari/context/<id>/context.md` and `manifest.json` |
| `nari diff --base PATH` | Diff behavior and dependencies against another snapshot |
| `nari status` / `nari incidents [--id ID]` | Show supervisor state, live per-action metrics, and redacted incident records |
| `nari repair ID --proposer fixture\|file:PATH` | Run the isolated repair pipeline; never releases |
| `nari apply-repair ID --candidate HASH` | Explicit local apply after rechecking hashes and protected tests |
| `nari bench [--quick]` | Benchmark report in `.nari/bench/report.json` |
| `nari plan [--name N] [--port P] [--database-url-env VAR]` | Build the app's Docker image and write a reviewable plan to `.nari/deploy/plans/<hash>.json` |
| `nari release HASH` | Apply exactly that plan after hash, source, artifact and drift checks, then wait for readiness |

Every command accepts `--project PATH` and `--json`.

Exit codes:

- 0: success.
- 1: validation or check failure.
- 2: usage error or unsupported feature.
- 3: execution or environment failure.
- 4: stale precondition or policy rejection.

### Seeded-bug repair walkthrough

From the repository root:

```sh
mkdir /tmp/fw && cp -R examples/fieldwork/. /tmp/fw/
sed -i '' 's/set = { completed: true };/set = { completed: false };/' /tmp/fw/fieldwork.nari
nari --project /tmp/fw test                                  # protected test fails; prints the incident id
nari --project /tmp/fw repair <incident> --proposer fixture  # reproduce, propose, validate in Seatbelt; source untouched
nari --project /tmp/fw apply-repair <incident> --candidate <hash>
```

## Protected tests

`acceptance/*.json` files are declarative HTTP scenarios. They are data, not code, and the repair pipeline cannot modify them:

```json
{ "name": "…", "covers": ["fieldwork/action/complete_task"],
  "steps": [ { "as": "a", "action": "create_task", "input": { "title": "x" }, "expect": { "status": 200 }, "save": "task" },
             { "as": "a", "action": "complete_task", "input": { "id": "$task.id", "revision": "$task.revision" },
               "expect": { "status": 200, "data": { "completed": true } } } ] }
```

`expect.data` is a subset match. `$name.field` substitutes a saved response, `as` is `a`, `b` or omitted (unauthenticated), and `key` pins an idempotency key.

## Deploy with Docker

```sh
nari plan --port 8080                # builds the image, prints resources, schema changes, cost and the plan hash
nari release <plan hash>             # applies exactly that plan
```

Releases are blue-green. An nginx proxy (`<name>-proxy`, pinned `nginx:1.30.6-alpine`) owns the published port and routes over a private Docker network to `<name>-blue` or `<name>-green`. A plan names the image digest, the network, the volume or database secret reference, the new color with its restart policy (from the `recovery` block), the proxy route change, the color it retires, the schema migration, and a cost estimate with its basis and date.

`nari release` refuses a plan whose file, source, image or target changed since it was written. It then:

1. Starts the new color next to the running one, on the same database.
2. Waits for the app to report ready, and checks `/_nari/health` from inside the network.
3. Reloads the proxy to route to the new color, and confirms through the proxy that the new version answers.
4. Lets the old color drain for five seconds, then stops it.

If the new color never becomes ready, it is removed and traffic never moves (`aborted`). If it fails after the switch, the proxy routes back (`rolled_back`). Schema changes are additive, so the old color keeps working against the migrated database while both run. Sign-in sessions of `mode = oidc` apps are stored in the database and survive the switch.

A `local_dev` app can only be planned as a preview: the container is published on the host's loopback interface. Production plans (`--production`) need `mode = oidc`, an https `--public-url`, and optionally `--oidc-secret-env VAR` and `--publish-address`; terminate TLS for the public URL in front of the container. With `--database-url-env VAR`, the container uses PostgreSQL and reads the URL from `VAR` at release time. The value never reaches the plan or the release record, but Docker keeps container environment in its own configuration.

## Sign-in with OpenID Connect

```
identity users {
  mode = oidc;
  issuer = "https://accounts.example.com";
  client_id = "fieldwork";
}
```

With `mode = oidc` the app has no seeded users or bootstrap link. `/_nari/login` starts an authorization-code flow with PKCE, a nonce and a state cookie, and `/_nari/callback` exchanges the code and verifies the ID token's signature, issuer, audience, expiry and nonce through the `openidconnect` crate. Each user's actor ID is a UUID derived from the issuer and the token's `sub`, so the same person owns the same rows on every sign-in. The app needs `NARI_PUBLIC_URL` (the URL browsers use, registered with the provider as `<url>/_nari/callback`), and `NARI_OIDC_CLIENT_SECRET` for confidential clients. Session cookies get `Secure` when the public URL is https.

## Environment

| Variable | Purpose |
| --- | --- |
| `NARI_TARGET_DIR` | Cargo target dir for generated apps (default `.nari/build/target`) |
| `NARI_PORT`, `NARI_DATABASE` | Trusted operator override of the declared port (0 picks a free one) and the database path |
| `NARI_DATABASE_URL` | A `postgres://` URL. The app then uses PostgreSQL instead of the SQLite file, over TLS when the URL's `sslmode` asks for it (rustls with the bundled webpki roots) |
| `NARI_PUBLIC_URL`, `NARI_OIDC_CLIENT_SECRET` | For `mode = oidc`: the URL browsers use to reach the app, and the client secret if the provider issued one |
| `NARI_BIND` | Trusted operator override of the listen address; deployment images set `0.0.0.0` inside the container |
| `NARI_FAULT` | Dev and test fault injection: `crash_on_start`, `crash_on_request:<action>`, `crash_before_commit:<action>`, `crash_after_commit:<action>` |
| `NARI_MUTATIONS_PER_MINUTE` | Operator override of the per-actor mutation limit (default 60); the check still runs |
| `NARI_SANDBOX=none` | Force the no-isolation path (repair reports `SANDBOX_UNAVAILABLE`) |
| `NARI_CHROMIUM`, `NARI_REQUIRE_BROWSER` | Browser for the journey test; with `NARI_REQUIRE_BROWSER` set, a missing browser fails the test instead of skipping it |
| `NARI_REQUIRE_DOCKER` | With it set, a missing Docker engine fails the deployment test instead of skipping it |
| `NARI_PG_BIN`, `NARI_REQUIRE_POSTGRES` | Directory holding `initdb` for the PostgreSQL tests (found on `PATH` or in Homebrew and Debian locations otherwise); with `NARI_REQUIRE_POSTGRES` set, a missing PostgreSQL fails the tests instead of skipping them |

## Tests

```sh
cargo test --workspace     # language, runtime, browser, agent workflow, recovery and repair suites
nari bench                # full benchmark (release CLI recommended: target/release/nari)
```

The suites build generated apps into `target/nari-apps` and run real HTTP against real SQLite, a throwaway PostgreSQL cluster, real worker processes, and real Seatbelt sandboxes. Nothing in them is mocked.

## Layout

| Path | Contents |
| --- | --- |
| `src/syntax.rs`, `src/fmt.rs` | Parser with byte spans and comments, and the formatter |
| `src/semantic.rs`, `src/ir.rs` | Schema, type and reference checks, typed IR, graph, canonical JSON and SHA-256 hashes |
| `src/codegen.rs`, `src/web/` | Rust handler generation (parameterized SQL, structurally escaped literals) and web assets |
| `src/pipeline.rs` | Artifacts, cargo invocation as an argv array, and mapping rustc errors back to Imanari spans |
| `runtime/` | `imanari-runtime`: sessions, CSRF/Origin/Host checks, limits, idempotent transactions, readiness |
| `src/testrun.rs`, `src/launch.rs`, `src/http.rs` | Contract checks, the protected test runner, app launcher, and a minimal HTTP client |
| `src/context.rs`, `src/diff.rs`, `src/patch.rs`, `src/progress.rs` | Context bundles, behavior diff, patch preconditions, and resumable task records |
| `src/supervisor.rs` | Supervisor, control DB, restart budget, reconciliation, and incidents |
| `src/repair.rs`, `src/sandbox.rs` | Repair state machine, fixture proposer, Seatbelt sandbox, and precedents |
| `src/bench.rs`, `bench/baseline/` | Benchmark harness and the hand-written baseline backend |
| `examples/fieldwork/` | Reference app and its protected acceptance tests |
| `docs/` | Specification, decision records and the progress report |
| `tests/` | Integration suites; `scripts/collect-evidence.sh` exports acceptance evidence |

## Implemented and unsupported

Implemented:

- The 0.1 grammar and schema, typed IR and graph, formatter, and diagnostics.
- Rust and web generation with SQLite or PostgreSQL persistence, and additive schema migrations on startup.
- Local development identity and OpenID Connect sign-in, with CSRF, Origin and Host checks, idempotency receipts, and revision checks.
- Request telemetry, and Docker deployments from reviewed plans with blue-green releases.
- Supervisor with persisted budgets and reconciliation, and incident records.
- Context bundles, behavior diff, and stale-patch rejection.
- The isolated repair pipeline with explicit apply, and the benchmark harness.

Unsupported so far, each rejected with a diagnostic:

- Mobile targets, identity providers other than OpenID Connect, and migrations that remove, rename or retype stored data.
- Workflows, external effects and adapters, native extensions, and policy expressions.
- Public or anonymous access, list filters, and floats or other types beyond Text, Bool, UUID and Int.
- `automatic_release = true`, and canary releases that split traffic between versions.
- Live LLM proposers.

## Limits and non-claims

- `local_dev` identity is a convenience for one developer on one machine. It does not protect against a hostile local user or malware.
- First-party crates and generated code use `#![forbid(unsafe_code)]`. Dependencies, SQLite and the OS still contain unsafe code and remain trusted.
- On macOS, the Seatbelt sandbox enforces filesystem confinement, network denial, a wall-clock limit, and CPU, file-size and open-file rlimits. Memory and process-count rlimits are not enforced by macOS for this use; the run phase instead denies `fork`. `sandbox-exec` is deprecated by Apple but still functional.
- Benchmark results are measurements on one machine with stated baselines. Missed budgets are reported, not hidden.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Report security issues privately as described in [SECURITY.md](SECURITY.md). Changes are listed in [CHANGELOG.md](CHANGELOG.md).

## License

Imanari is dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual-licensed as above, without any additional terms or conditions.
