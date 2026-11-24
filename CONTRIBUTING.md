# Contributing to Imanari

Thanks for taking the time to help. Imanari is a 0.1 developer preview, so the language, the runtime interface and the CLI can still change between releases.

## Before you start

- For bugs, open an issue with the `.nr` source (or the smallest part that reproduces it), the command you ran, and the full diagnostic or JSON output.
- For language or runtime changes, open an issue first. [docs/spec.md](docs/spec.md) is the normative contract, so a change that alters accepted syntax, generated behavior, or a security property needs a decision record in [docs/decisions.md](docs/decisions.md) that states its acceptance impact.
- Unsupported features must keep failing with an explicit diagnostic. Please don't add placeholder behavior that silently does something else.

## Building and testing

The toolchain is pinned in `rust-toolchain.toml`, and rustup installs it on first use.

```sh
cargo build
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The test suites build real generated apps into `target/nari-apps` and talk to them over HTTP. The first run compiles Axum, SQLx and Tokio in release mode and takes a few minutes. Isolated repair tests need macOS (Seatbelt) and are reported as ignored on other platforms. The browser journey test runs when Node 22+ and a Chromium are available, and otherwise prints `BROWSER_UNAVAILABLE` and skips.

If you change benchmark-sensitive code in `runtime/` or `src/codegen.rs`, run `target/release/nari --project examples/fieldwork bench` and include the summary in your pull request. For changes to acceptance behavior, run `scripts/collect-evidence.sh` as well.

## Code guidelines

- No `unsafe` in first-party crates. Every crate uses `#![forbid(unsafe_code)]`.
- Generated code is built from typed IR nodes. Never splice source text into Rust, SQL, HTML or JavaScript.
- Keep diagnostics actionable: a stable code, the exact span, and the node id.
- New dependencies need a reason in the pull request. Keep default features off where possible.

## Pull requests

- Keep each pull request focused on one change, with tests.
- Write commit messages in the imperative mood with a short prefix (`fix:`, `feat:`, `docs:`, `test:`, `ci:`, `chore:`), for example `fix: reject duplicate host headers`.
- Update [CHANGELOG.md](CHANGELOG.md) under `Unreleased` for anything a user would notice.

## License

Unless you explicitly state otherwise, any contribution you intentionally submit for inclusion in Imanari, as defined in the Apache-2.0 license, is dual-licensed under MIT and Apache-2.0, without any additional terms or conditions.
