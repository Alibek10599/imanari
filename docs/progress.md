# Imanari 0.1 progress and completion report

Status on 2026-11-02: milestones M0–M5 are implemented and verified locally on macOS 15.7 (Apple M1 Max, Rust 1.97.1). `cargo test --workspace` runs 37 tests, which pass in about 75 s. They exercise real HTTP, SQLite, worker processes, Seatbelt sandboxes and headless Chromium. On Linux (checked in a Debian container) the same suite passes, with the 4 Seatbelt-dependent repair tests reported as ignored.

Run `scripts/collect-evidence.sh` to export the evidence referenced below into `evidence/`. Neither `evidence/` nor the build artifacts under `.nari/` are tracked in git, since both contain machine-specific paths and timings. Spec clarifications are in [decisions.md](decisions.md), and the example app lives in [examples/fieldwork](../examples/fieldwork).

## Milestones

| Milestone | Status | Main code |
| --- | --- | --- |
| M0 Bootstrap and language | done | `src/syntax.rs`, `src/fmt.rs`, `src/semantic.rs`, `src/ir.rs`, `src/diag.rs` |
| M1 Executable app | done | `src/codegen.rs`, `src/web/`, `src/pipeline.rs`, `runtime/`, `src/testrun.rs`, `nari build/dev/test` |
| M2 Agent workflow | done | `src/context.rs`, `src/patch.rs`, `src/diff.rs`, `src/progress.rs` |
| M3 Recovery | done | `src/supervisor.rs` (control DB, budgets, reconciliation, incidents) |
| M4 Isolated repair and learning | done (Seatbelt; see blockers for other OSes) | `src/repair.rs`, `src/sandbox.rs` |
| M5 Measurement and packaging | done; budget results below | `src/bench.rs`, `bench/baseline/`, README, this file |

## Acceptance items

| # | Requirement | Evidence |
| --- | --- | --- |
| 1 | Parse, check and format the fixture. Formatted output keeps the semantic IR and the comments. Repeated builds give identical canonical outputs. | `tests/language.rs`: `fixture_checks_without_diagnostics`, `fixture_is_formatter_fixed_point`, `formatting_preserves_comments_and_semantics`, `canonical_outputs_are_deterministic`, `reordering_declarations_keeps_semantic_hash`, `graph_matches_committed_snapshot` (`tests/snapshots/fieldwork.graph.json`). Exports: `evidence/ir.json`, `evidence/graph.json`, `evidence/check.txt`. |
| 2 | Reject invalid programs with useful spans. | `tests/language.rs`: `rejects_invalid_programs_with_useful_spans`, with 43 cases asserting the exact spanned text. They cover unresolved actions, duplicates, unknown properties and blocks, types, references, owner reassignment, reserved fields, unsupported targets and features, and routes/paths. Also `enforces_parser_limits` (1 MiB, depth 64, 10,000 declarations) and `bounded_malformed_input_never_panics` (3,000 mutated inputs). |
| 3 | Create a task in the browser, reload from SQLite, list and complete it. Loading, error and empty states, labels, keyboard use, escaped malicious text. | `tests/browser.rs` runs `tests/browser/journey.mjs` in headless Chromium over CDP. Its 14 checks include triple-Enter producing a single request, the stale-revision conflict UI, and user B isolation. |
| 4 | Cross-user isolation; unauthenticated and forged sessions; CSRF; Origin and Host; limits; unknown fields. | `tests/runtime.rs`: `users_are_isolated_and_web_boundary_is_enforced`, `bootstrap_token_is_single_use`, `mutation_rate_limit_returns_429`, `pagination_is_bounded_and_owner_scoped`. Also the `nari test` generated contract checks (22 per run) in `evidence/test-report.json`. |
| 5 | One winner among same-revision updates; concurrent idempotent creates; a lost response returns its receipt. | `tests/runtime.rs`: `concurrent_updates_with_same_revision_yield_one_success` (8 racers: 1 success, 7 conflicts), `concurrent_idempotent_creates_converge_on_one_record`, `committed_write_with_lost_response_is_returned_on_retry`, `rolled_back_write_is_not_reported_or_persisted`. |
| 6 | Crash mid-request, then restart and preserve data. A durable stop on exhaustion. A supervisor restart neither duplicates the worker nor resets the budget. | `tests/recovery.rs`: `crash_mid_request_restarts_and_preserves_committed_data`, `exhausted_budget_stops_durably_and_survives_supervisor_restart`, `supervisor_restart_reconciles_without_duplicate_workers`. Also `tests/runtime.rs::additive_schema_changes_migrate_and_destructive_ones_stop`. Exports: `evidence/recovery/`. |
| 7 | Context includes the required nodes and omits unrelated ones. Oversized context fails explicitly. A source change rejects a patch. | `tests/agent_workflow.rs`: `context_includes_required_contracts_and_omits_unrelated_implementations`, `oversized_mandatory_context_fails_without_truncation`, `patch_preconditions_and_scope_are_enforced`, `task_record_resumes_only_when_hashes_and_evidence_match`. Exports: `evidence/context/`. |
| 8 | Seeded `completed = false` is caught, reproduced, proposed as `true`, validated in isolation and stored; the source is untouched until apply. | `tests/repair.rs::seeded_bug_is_repaired_in_isolation_and_applied_only_explicitly`. Exports: `evidence/repair/01…07`, `candidate.json`, `task-record.json`. |
| 9 | Reject ownership, recovery, protected-test, scope, outside-path and dependency changes, and stale candidates. Report a missing sandbox. | `tests/repair.rs`: `malicious_out_of_scope_and_stale_proposals_are_rejected` (13 adversarial patches plus a CLI rejection), `missing_sandbox_reports_unavailable_instead_of_validating`. Exports: `evidence/repair/02`, `05`, `08`. |
| 10 | Record failed, inconclusive and successful attempts. Use only version-compatible precedents. No secret canaries in exports. | `tests/repair.rs`: `precedents_require_compatible_versions`, the budget and exhaustion part of the malicious-proposal test, and `exported_evidence_contains_no_secret_canaries`. The canaries are the bootstrap token, both session cookies and CSRF tokens, the owner token, and a user-payload title. |
| 11 | Behavior diff for an input-bound change and an owner-policy change. Unknown changes are checked conservatively. | `tests/agent_workflow.rs::behavior_diff_classifies_bounds_owner_and_unknown_changes`. Exports: `evidence/diff/`. |
| 12 | Benchmarks, baselines and budget misses. | `nari bench`. Exports: `evidence/bench/report.json`, `runtime-samples.json`, `summary.txt`, and the section below. |

Also covered: rustc errors map back to Imanari spans (`tests/runtime.rs::rust_compile_errors_map_back_to_imanari_spans`); database symlink escapes are refused while system symlinks such as `/var` are allowed; `nari init` never overwrites.

## Benchmarks

Setup:

- **Machine:** Apple M1 Max (10 cores, 64 GB) on macOS 15.7.9, rustc 1.97.1, Apple clang 16.0.0.
- **Builds:** release builds of the CLI and generated apps (opt-level 3, codegen-units 16).
- **Dependencies:** axum 0.8.9, hyper 1.12.0, tokio 1.54.1, sqlx 0.8.6, libsqlite3-sys 0.30.1.
- **Runs:** one warmup run plus 5 measured runs per measurement, in alternating order.
- **Raw data:** `evidence/bench/report.json`; raw latency samples in `evidence/bench/runtime-samples.json`.

| Budget | Result | Measured |
| --- | --- | --- |
| Check a 1,000-action fixture (197 KB, 3,020 nodes, 8,030 edges) in ≤ 1 s median | pass | 32.4 ms median (5 runs, with correctness checks) |
| Warm context extraction ≤ 100 ms median | pass | 3.7 ms median, 6.6 KB bundle |
| Runtime p95 overhead ≤ 15% vs hand-written: `list` | pass | −4.4% (p95 2,995 vs 3,133 µs; 3,732 vs 3,537 rps) |
| Runtime p95 overhead ≤ 15%: `create` | pass | −8.3% (p95 2,504 vs 2,732 µs; 4,407 vs 4,388 rps) |
| Runtime p95 overhead ≤ 15%: `complete` | pass | −6.7% (p95 2,372 vs 2,543 µs; 4,842 vs 4,829 rps) |
| Peak memory overhead ≤ 20% | pass | +0.5% (14.9 MB vs 14.8 MB RSS) |
| Generated Rust kernel within 20% of `-O3` C++ | pass | −7.7% (70.9 vs 76.8 ms median; outputs identical) |

**Runtime workload.** 8 closed-loop workers each send 500 requests per scenario over keep-alive connections, against 50 seeded rows per user, with zero errors in every run. The baseline (`bench/baseline/`) is hand-written, serde-typed Rust on the same `imanari-runtime`: same HTTP stack, SQLite, sessions, CSRF/Origin/Host checks, idempotency receipts, revision checks and rate-limit check. Under saturation with one SQLite writer, write medians (about 1.5 ms) are dominated by queueing, consistent with Little's law (8 workers / ~4,400 rps ≈ 1.8 ms). The runtime numbers measure relative overhead, not absolute single-request latency.

**Earlier failed run, kept on record.** The first full run on the same machine missed two runtime budgets:

| Scenario | p95 overhead | Imanari p95 (µs) | Baseline p95 (µs) | Imanari median (µs) | Baseline median (µs) |
| --- | --- | --- | --- | --- | --- |
| `create` | +39% (miss) | 905 | 649 | 218 | 204 |
| `complete` | +16% (miss) | 874 | 752 | 190 | 180 |

Profiling the raw samples showed the cause was not handler code:

- Per-run p95 for the *same* binary varied 3–4× (baseline `create`: 359–1,638 µs).
- Write p99s were 10–38 ms and maxima up to 0.9 s in both apps. Reads (`list`) stayed within 3%.

The tails came from concurrent `BEGIN IMMEDIATE` writers parked in SQLite's sleeping busy handler. The fix, in the shared runtime, is a FIFO writer queue held for each mutation's transaction ([D14](decisions.md)). It raised write throughput for both apps (`create` 3,517 → 4,407 rps for Imanari and 3,919 → 4,388 for the baseline) and cut p99 to mostly 3–11 ms. No validation, authorization or durability check was removed.

**Kernel.** The kernel processes 100,000 records × 10 iterations, applying the following checks and producing equal counts and checksums in both languages:

- Text[1..200] Unicode-scalar validation, with bounds taken from the IR.
- A canonical UUID check.
- The safe-integer range check.
- A checked revision increment.
- A wrapping checksum.

The generated Rust calls the same `imanari-runtime` validators the handlers use. This is a single bounded kernel; it does not establish whole-stack C++ parity.

**Agent iteration.** One scripted change (tighten `create_task` title to 120), with byte counts only. No model token counts were measured and no token savings are claimed.

| | Imanari | Hand-written baseline |
| --- | --- | --- |
| Context read | 4,653 bytes (dependency-scoped bundle) | 5,825 bytes (whole handler file) |
| Changed source | 3 bytes | 3 bytes |
| Checks run | 27: check, diff, 22 contract checks, 3 protected tests | 4: build, 3 protected tests |
| Failures | 0 | 0 |
| Elapsed | 1.8 s | 1.8 s |
| Fresh-session continuation | Task record verified by hash | Not available |

## Known gaps and blockers

- **Isolation off macOS.** Repair isolation uses macOS Seatbelt. Elsewhere, `nari repair` runs proposal and static validation and reports `SANDBOX_UNAVAILABLE` (tested with `NARI_SANDBOX=none`), so the dynamic gate stays open on those platforms. A Linux backend (namespaces, seccomp or a container) is the missing prerequisite.
- **Unenforced macOS limits.** Memory and process-count rlimits are not enforced by macOS in the sandbox. Wall-clock, CPU, file-size and open-file limits are, and the run phase denies `fork`.
- **No telemetry pipeline.** 0.1 emits no telemetry, so §12's batched telemetry with dropped-event counters is not implemented. Incidents and recovery transitions are persisted before any action.
- **Incident links.** Incident `supersedes`/`revoked_by` fields exist but nothing sets them yet. No CLI exists to link decision records to nodes.
- **File permissions.** SQLite WAL and SHM side files are created with the process umask. Only the main app and control database files are set to `0600`.
- **Untested paths.** The readiness-timeout restart path and the `E_DEPENDENCIES` classification are implemented but not covered by automated tests.
- **Browser skips.** The browser journey needs Node and Chromium. It reports `BROWSER_UNAVAILABLE` and skips unless `NARI_REQUIRE_BROWSER` is set.

## Dependencies and security boundary

Locked in `Cargo.lock` and pinned by `rust-toolchain.toml` (1.97.1).

- **Direct dependencies:** clap 4, serde and serde_json 1, sha2 0.10, getrandom 0.3, tokio 1.53, sqlx 0.8.6 (SQLite, bundled libsqlite3), rlimit 0.11 (CLI only), and axum 0.8.9 (runtime only).
- **Unsafe code:** all first-party crates and generated code use `#![forbid(unsafe_code)]`. Dependencies, SQLite and the OS are trusted components and still contain unsafe code.
- **Untrusted inputs:** Imanari source, HTTP input, repair proposals, and incident and telemetry text. Proposals are parsed as data, path-checked, applied to a snapshot, fully rechecked, and then built and run only inside the sandbox with a cleared environment.
- **No shells:** builds and runs pass executable and argument arrays.
- **Escaping:** generated Rust literals are escaped structurally (Rust `Debug`), SQL is parameterized with quoted identifiers, and UI text is rendered as text nodes.
- **Secrets:** session, bootstrap and owner tokens never appear in source, context, incidents, reports or JSON output (canary-tested). The bootstrap URL is printed only in human mode.
- **`local_dev` scope:** the development identity does not protect against other local users or malware.
