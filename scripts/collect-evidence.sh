#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/evidence"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/nari-evidence.XXXXXX")"
export NARI_TARGET_DIR="${NARI_TARGET_DIR:-$ROOT/target/nari-apps}"

cargo build --release --manifest-path "$ROOT/Cargo.toml" >/dev/null
NARI="$ROOT/target/release/nari"
rm -rf "$OUT"
mkdir -p "$OUT"/{context,diff,recovery,repair,bench}

project() {
  rm -rf "$WORK/$1"
  mkdir -p "$WORK/$1"
  cp "$ROOT/examples/fieldwork/fieldwork.nr" "$WORK/$1/"
  cp -R "$ROOT/examples/fieldwork/acceptance" "$WORK/$1/"
  echo "$WORK/$1"
}

capture() {
  local file="$1"; shift
  set +e
  "$@" >"$file" 2>&1
  echo "exit=$?" >>"$file"
  set -e
}

P="$(project main)"
"$NARI" --project "$P" check >"$OUT/check.txt"
"$NARI" --project "$P" fmt --check >>"$OUT/check.txt"
"$NARI" --json --project "$P" test >"$OUT/test-report.json"
"$NARI" --project "$P" context fieldwork/action/complete_task >/dev/null
cp "$P/.nari/context/fieldwork.action.complete_task/context.md" "$OUT/context/complete_task.context.md"
cp "$P/.nari/context/fieldwork.action.complete_task/manifest.json" "$OUT/context/complete_task.manifest.json"
capture "$OUT/context/budget-exceeded.txt" "$NARI" --project "$P" context fieldwork/action/complete_task --max-bytes 600
cp "$P/.nari/context/fieldwork.action.complete_task/manifest.json" "$OUT/context/budget-exceeded.manifest.json"
cp "$P/.nari/ir.json" "$OUT/ir.json"
cp "$P/.nari/graph.json" "$OUT/graph.json"
cp "$P/.nari/manifest.json" "$OUT/artifact-manifest.json"

D1="$(project diff-bound)"
perl -0pi -e 's/(input title \{\n      type = Text;\n      min_length = 1;\n      max_length = )200/${1}100/' "$D1/fieldwork.nr"
"$NARI" --project "$D1" diff --base "$P" >"$OUT/diff/input-bound.txt"
"$NARI" --json --project "$D1" diff --base "$P" >"$OUT/diff/input-bound.json"
D2="$(project diff-owner)"
perl -0pi -e 's/owner_field = owner;/owner_field = assignee;/; s/field owner \{/field assignee {/; s/owner: \@actor.id,/assignee: \@actor.id,/' "$D2/fieldwork.nr"
"$NARI" --project "$D2" diff --base "$P" >"$OUT/diff/owner-policy.txt"
"$NARI" --json --project "$D2" diff --base "$P" >"$OUT/diff/owner-policy.json"

R="$(project recovery)"
capture "$OUT/recovery/exhaustion-dev-output.txt" env NARI_PORT=0 NARI_FAULT=crash_on_start "$NARI" --project "$R" dev
"$NARI" --json --project "$R" status >"$OUT/recovery/status-after-exhaustion.json"
"$NARI" --json --project "$R" incidents >"$OUT/recovery/incidents.json"
capture "$OUT/recovery/restart-refused.txt" "$NARI" --project "$R" dev
capture "$OUT/recovery/acknowledged-still-exhausted.txt" env NARI_PORT=0 NARI_FAULT=crash_on_start "$NARI" --project "$R" dev --acknowledge
"$NARI" --json --project "$R" status >"$OUT/recovery/status-after-acknowledge.json"

S="$(project seeded)"
perl -pi -e 's/set = \{ completed: true \};/set = { completed: false };/' "$S/fieldwork.nr"
capture "$OUT/repair/01-test-catches-seeded-bug.txt" "$NARI" --project "$S" test
INC="$("$NARI" --json --project "$S" incidents | perl -MJSON::PP -e 'local $/; print decode_json(<STDIN>)->{incidents}[0]{incident_id}')"
cat >"$WORK/evil.json" <<EOF
{"schema":"nari-patch/0.1","base_source_sha256":"$(shasum -a 256 "$S/fieldwork.nr" | cut -d' ' -f1)","base_graph_sha256":"","affected_nodes":["fieldwork/action/complete_task"],"allowed_scope":["fieldwork/action/complete_task"],"edits":[{"file":"acceptance/complete_task.json","start":0,"end":1,"replacement":"{"}],"rationale":"tries to edit a protected test"}
EOF
capture "$OUT/repair/02-malicious-proposal-rejected.txt" "$NARI" --project "$S" repair "$INC" --proposer "file:$WORK/evil.json"
SHA_BEFORE="$(shasum -a 256 "$S/fieldwork.nr")"
"$NARI" --json --project "$S" repair "$INC" --proposer fixture >"$OUT/repair/03-validated-report.json"
SHA_AFTER="$(shasum -a 256 "$S/fieldwork.nr")"
printf 'before repair: %s\nafter repair:  %s\n' "$SHA_BEFORE" "$SHA_AFTER" >"$OUT/repair/04-running-source-untouched.txt"
HASH="$(perl -MJSON::PP -e 'local $/; print decode_json(<STDIN>)->{proposal}{candidate_hash}' <"$OUT/repair/03-validated-report.json")"
cp "$S/.nari/repair/$INC/candidates/$HASH.json" "$OUT/repair/candidate.json"
LAST_ATTEMPT="$(ls -d "$S/.nari/repair/$INC"/attempt-* | sort -V | tail -1)"
cp "$LAST_ATTEMPT/task.json" "$OUT/repair/task-record.json"
cp "$S/fieldwork.nr" "$WORK/seeded.nr"
printf '// unrelated edit\n' | cat - "$WORK/seeded.nr" >"$S/fieldwork.nr"
capture "$OUT/repair/05-stale-candidate-rejected.txt" "$NARI" --project "$S" apply-repair "$INC" --candidate "$HASH"
cp "$WORK/seeded.nr" "$S/fieldwork.nr"
capture "$OUT/repair/06-explicit-apply.txt" "$NARI" --project "$S" apply-repair "$INC" --candidate "$HASH"
capture "$OUT/repair/07-tests-after-apply.txt" "$NARI" --project "$S" test
S2="$(project nosandbox)"
perl -pi -e 's/set = \{ completed: true \};/set = { completed: false };/' "$S2/fieldwork.nr"
"$NARI" --project "$S2" test >/dev/null 2>&1 || true
INC2="$("$NARI" --json --project "$S2" incidents | perl -MJSON::PP -e 'local $/; print decode_json(<STDIN>)->{incidents}[0]{incident_id}')"
capture "$OUT/repair/08-sandbox-unavailable.txt" env NARI_SANDBOX=none "$NARI" --project "$S2" repair "$INC2" --proposer fixture

if [ "${SKIP_BENCH:-0}" != "1" ]; then
  B="$(project bench)"
  "$NARI" --project "$B" bench ${BENCH_ARGS:-} >"$OUT/bench/summary.txt"
  cp "$B/.nari/bench/report.json" "$OUT/bench/report.json"
  cp "$B/.nari/bench/runtime/samples.json" "$OUT/bench/runtime-samples.json"
fi

find "$OUT" -type f | sort | sed "s|$ROOT/||" >"$OUT/INDEX.txt"
echo "evidence written to $OUT"
