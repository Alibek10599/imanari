# Changelog

All notable changes to Imanari are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, minor versions may contain breaking changes.

## [Unreleased]

### Added

- A language guide, `docs/guide.md`, and a hello-world example in `examples/hello`.

## [0.3.0] - 2026-11-23

### Added

- Additive schema migrations. When an app starts against a database built from an older schema, new entities, new fields with a default, and widened text bounds are applied in one transaction and recorded in `_nari_schema_history`. Changes that would drop or reinterpret stored data still stop startup with `MIGRATION_REQUIRED`, and the message now names each change.
- `nari diff` lists which schema changes apply automatically and which block startup (`migration.automatic` and `migration.blocked` in JSON).
- OpenID Connect sign-in. `identity { mode = oidc; issuer = "..."; client_id = "..."; }` replaces the development bootstrap with an authorization-code flow (PKCE, nonce, state cookie) through the `openidconnect` crate. Actor IDs derive from the issuer and subject, signing-key rotation is handled, and `POST /_nari/logout` ends a session. `nari plan --production` accepts oidc apps with an https `--public-url`.
- Docker deployments. `nari plan` builds the app image and writes a reviewable plan with the image digest, resources, schema migration, readiness rule and cost estimate. `nari release HASH` applies exactly that plan after checking the file, source, image and target for drift. Releases are blue-green behind an nginx proxy: the new color starts next to the old one, passes readiness and health checks, then traffic switches with a graceful reload and the old color drains. A failed release never moves traffic.
- An older build now starts against a database whose schema only adds to its own, so two versions can share a database during a release.
- `mode = oidc` sessions are stored in the database and survive a switch to another instance.
- PostgreSQL connections can use TLS (rustls with webpki roots), following the URL's `sslmode`.
- PostgreSQL. Set `NARI_DATABASE_URL` to a `postgres://` URL and the same app binary stores its data in PostgreSQL, with startup migrations, idempotency receipts and revision checks that hold across several instances on one database.
- Request telemetry. Apps count requests per action and status and keep a latency histogram, fed through a bounded queue that counts dropped events instead of blocking. `nari status` shows the numbers for the running worker, and the owner-only `GET /_nari/metrics` returns them as `nari-metrics/0.1` JSON.

### Changed

- Generated handlers call the runtime's query helpers (`Pool`, `Conn`, `Arg`, `Col`) instead of SQLx, and `AppSpec` no longer carries DDL: the runtime derives it from the schema shape for the open backend. Hand-written apps on `imanari-runtime` need the same change.

## [0.2.0] - 2026-11-04

### Changed

- Frame is now Imanari. The command is `nari`, source files use the `.nari` extension and open with `imanari "0.1";`, and build artifacts live in `.nari/`. Environment variables, internal routes, headers and cookies use the `nari` prefix (for example `NARI_PORT` and `X-Nari-CSRF`). The crates are `imanari` and `imanari-runtime`, and release archives are named `imanari-<target>.tar.gz`. To migrate a project, rename the `.frame` file, change its first line, and delete the old `.frame/` directory.

## [0.1.0] - 2026-11-03

### Added

- `.frame` parser with byte spans and comments, a comment-preserving formatter, and semantic checks with stable diagnostic codes.
- Deterministic typed IR and dependency graph with semantic and source hashes.
- Rust backend generation (Axum, SQLx, SQLite) and a browser UI built from native controls.
- `frame-runtime`: local development identity, CSRF, Origin and Host checks, idempotency receipts, revision-checked updates, rate and size limits, and a FIFO writer queue.
- Bounded context bundles, behavior diff, and stale-patch rejection for agent workflows.
- A supervisor with its own control database, persisted restart budgets, reconciliation, and redacted incident records.
- An isolated repair pipeline in the macOS Seatbelt sandbox with a deterministic fixture proposer, explicit local apply, and version-compatible precedents.
- `frame bench` with a hand-written baseline backend and a C++ kernel comparison.
- Prebuilt `frame` binaries for macOS (Apple silicon) and Linux (x86_64 and arm64). The CLI bundles `frame-runtime`, so generated apps do not need a source checkout.
- CLI commands: `init`, `fmt`, `check`, `build`, `dev`, `test`, `context`, `diff`, `status`, `incidents`, `repair`, `apply-repair` and `bench`. `plan` and `release` fail as unsupported.

[Unreleased]: https://github.com/Alibek10599/imanari/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Alibek10599/imanari/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Alibek10599/imanari/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Alibek10599/imanari/releases/tag/v0.1.0
