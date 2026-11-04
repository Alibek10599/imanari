# Changelog

All notable changes to Imanari are documented here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html). Before 1.0, minor versions may contain breaking changes.

## [Unreleased]

### Added

- Additive schema migrations. When an app starts against a database built from an older schema, new entities, new fields with a default, and widened text bounds are applied in one transaction and recorded in `_nari_schema_history`. Changes that would drop or reinterpret stored data still stop startup with `MIGRATION_REQUIRED`, and the message now names each change.
- `nari diff` lists which schema changes apply automatically and which block startup (`migration.automatic` and `migration.blocked` in JSON).

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

[Unreleased]: https://github.com/Alibek10599/imanari/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Alibek10599/imanari/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Alibek10599/imanari/releases/tag/v0.1.0
