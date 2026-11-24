mod common;

use common::*;
use imanari::patch::{self, Edit, Patch};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

const BUGGY_FROM: &str = "set = { completed: true };";
const BUGGY_TO: &str = "set = { completed: false };";

fn nari(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> (i32, Value, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nari"));
    cmd.args(["--json", "--project", dir.to_str().unwrap()])
        .args(args)
        .env("NARI_TARGET_DIR", SHARED_TARGET)
        .env_remove("NARI_SANDBOX");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let json = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (
        out.status.code().unwrap_or(-1),
        json,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn seeded(name: &str) -> (std::path::PathBuf, String) {
    fieldwork();
    let src = FIXTURE.replace(BUGGY_FROM, BUGGY_TO);
    let p = write_project(name, &src);
    let (code, report, _) = nari(&p.dir, &[], &["test"]);
    assert_eq!(
        code, 1,
        "seeded bug is caught by the unchanged protected test: {report}"
    );
    let (_, list, _) = nari(&p.dir, &[], &["incidents"]);
    let inc = list["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "test_failure")
        .unwrap()
        .clone();
    assert_eq!(inc["target_id"], "fieldwork/action/complete_task");
    assert_eq!(
        inc["failure_signature"],
        "test:acceptance/complete_task.json:step1:data.completed"
    );
    (p.dir, inc["incident_id"].as_str().unwrap().to_string())
}

fn sha(path: &Path) -> String {
    imanari::ir::sha256_hex(&std::fs::read(path).unwrap())
}

fn details(v: &Value) -> Value {
    serde_json::from_str(v["diagnostics"][0]["details"].as_str().unwrap_or("null"))
        .unwrap_or(Value::Null)
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "isolated repair needs the macOS Seatbelt sandbox"
)]
fn seeded_bug_is_repaired_in_isolation_and_applied_only_explicitly() {
    let (dir, inc) = seeded("repair-success");
    let src_file = dir.join("fieldwork.nr");
    let before = sha(&src_file);
    let (code, report, err) = nari(&dir, &[], &["repair", &inc, "--proposer", "fixture"]);
    assert_eq!(code, 0, "{err}\n{report}");
    assert_eq!(report["state"], "Validated");
    assert_eq!(report["reproduction"]["result"], "reproduced");
    assert_eq!(report["dynamic_validation"]["reproduction_after"], "passed");
    assert_eq!(
        report["dynamic_validation"]["protected_fixtures_unchanged"],
        true
    );
    assert_eq!(
        report["proposal"]["patch"]["edits"][0]["replacement"],
        "true"
    );
    assert_eq!(
        sha(&src_file),
        before,
        "running source untouched before explicit apply"
    );
    let candidate = report["proposal"]["candidate_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        dir.join(format!(".nari/repair/{inc}/candidates/{candidate}.json"))
            .exists()
    );
    let task = imanari::progress::TaskRecord::load(
        &dir.join(format!(".nari/repair/{inc}/attempt-1/task.json")),
    )
    .unwrap();
    assert_eq!(task.state, "validated");
    let c = imanari::compile(&std::fs::read_to_string(&src_file).unwrap(), "fieldwork.nr")
        .unwrap()
        .0;
    assert!(
        task.verify(&dir, &c.source_sha256, &c.graph_sha256)
            .is_empty(),
        "resumable record verifies"
    );

    let original = std::fs::read_to_string(&src_file).unwrap();
    std::fs::write(&src_file, format!("// unrelated edit\n{original}")).unwrap();
    let (code, stale, _) = nari(
        &dir,
        &[],
        &["apply-repair", &inc, "--candidate", &candidate],
    );
    assert_eq!(code, 4);
    assert_eq!(stale["diagnostics"][0]["code"], "PATCH_STALE");
    std::fs::write(&src_file, &original).unwrap();

    let (code, applied, err) = nari(
        &dir,
        &[],
        &["apply-repair", &inc, "--candidate", &candidate],
    );
    assert_eq!(code, 0, "{err}\n{applied}");
    let now = std::fs::read_to_string(&src_file).unwrap();
    assert!(now.contains("set = { completed: true };"));
    assert_eq!(
        now.replace(BUGGY_FROM, BUGGY_TO),
        original,
        "only the binding changed"
    );
    let (code, _, _) = nari(&dir, &[], &["test"]);
    assert_eq!(code, 0, "repaired app passes the full suite");
    let (_, i, _) = nari(&dir, &[], &["incidents", "--id", &inc]);
    assert_eq!(i["incidents"][0]["outcome"], "repaired_by_local_apply");
    let (code, again, _) = nari(
        &dir,
        &[],
        &["apply-repair", &inc, "--candidate", &candidate],
    );
    assert_eq!(code, 4, "a candidate cannot be applied twice: {again}");
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "isolated repair needs the macOS Seatbelt sandbox"
)]
fn malicious_out_of_scope_and_stale_proposals_are_rejected() {
    let src = FIXTURE.replace(BUGGY_FROM, BUGGY_TO);
    let c = imanari::compile(&src, "fieldwork.nr").unwrap().0;
    let mk = |file: &str, from: &str, to: &str| {
        let start = src.find(from).unwrap();
        Patch {
            schema: patch::SCHEMA.into(),
            base_source_sha256: c.source_sha256.clone(),
            base_graph_sha256: c.graph_sha256.clone(),
            affected_nodes: vec!["fieldwork/action/complete_task".into()],
            allowed_scope: vec!["fieldwork/action/complete_task".into()],
            edits: vec![Edit {
                file: file.into(),
                start,
                end: start + from.len(),
                replacement: to.into(),
            }],
            rationale: "adversarial".into(),
        }
    };
    let code = |p: &Patch| {
        imanari::repair::validate_static(&c, &src, "fieldwork.nr", p)
            .err()
            .map(|r| r.code)
    };
    let f = "fieldwork.nr";
    assert_eq!(
        code(&mk(f, BUGGY_TO, "set = { completed: true };")),
        None,
        "the honest fix passes static validation"
    );
    assert_eq!(
        code(&mk(
            f,
            "owner_field = owner;",
            "owner_field = owner;\n    field shadow {\n      type = UUID;\n      default = \"00000000-0000-4000-8000-00000000000b\";\n    }"
        )),
        Some("PATCH_SCOPE_VIOLATION"),
        "schema/ownership change"
    );
    assert_eq!(
        code(&mk(
            f,
            "owner: @actor.id,",
            "owner: \"00000000-0000-4000-8000-00000000000b\","
        )),
        Some("PATCH_CHECK_FAILED"),
        "owner reassignment fails the whole-app recheck"
    );
    assert_eq!(
        code(&mk(
            f,
            "access = signed_in;\n\n    input id",
            "access = public;\n\n    input id"
        )),
        Some("PATCH_CHECK_FAILED"),
        "permission widening"
    );
    assert_eq!(
        code(&mk(
            f,
            "max_restarts = 3;\n    window_seconds = 60;\n    backoff_ms = [100, 500, 2000];",
            "max_restarts = 10;\n    window_seconds = 1;\n    backoff_ms = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0];"
        )),
        Some("PATCH_SCOPE_VIOLATION"),
        "recovery limits"
    );
    assert_eq!(
        code(&mk(
            f,
            "editable_actions = [complete_task];",
            "editable_actions = [complete_task, create_task];"
        )),
        Some("PATCH_SCOPE_VIOLATION"),
        "editable scope"
    );
    assert_eq!(
        code(&mk(f, "max_attempts = 2;", "max_attempts = 3;")),
        Some("PATCH_SCOPE_VIOLATION"),
        "repair policy"
    );
    assert_eq!(
        code(&mk(f, "title: @input.title,", "title: \"pwned\",")),
        Some("PATCH_SCOPE_VIOLATION"),
        "non-editable action binding"
    );
    assert_eq!(
        code(&mk("acceptance/complete_task.json", BUGGY_TO, "x")),
        Some("PATCH_PATH_FORBIDDEN"),
        "protected tests"
    );
    assert_eq!(
        code(&mk("../outside.nr", BUGGY_TO, "x")),
        Some("PATCH_PATH_FORBIDDEN"),
        "path outside project"
    );
    assert_eq!(
        code(&mk("Cargo.toml", BUGGY_TO, "x")),
        Some("PATCH_PATH_FORBIDDEN"),
        "dependency file"
    );
    assert_eq!(
        code(&mk("runtime/Cargo.lock", BUGGY_TO, "x")),
        Some("PATCH_PATH_FORBIDDEN"),
        "dependency lock"
    );
    let mut stale = mk(f, BUGGY_TO, "set = { completed: true };");
    stale.base_source_sha256 = "0".repeat(64);
    assert_eq!(code(&stale), Some("PATCH_STALE"));

    let (dir, inc) = seeded("repair-reject");
    let evil = mk(
        f,
        "max_restarts = 3;\n    window_seconds = 60;\n    backoff_ms = [100, 500, 2000];",
        "max_restarts = 0;\n    window_seconds = 60;\n    backoff_ms = [];",
    );
    let evil_path = dir.join("evil.json");
    std::fs::write(&evil_path, serde_json::to_vec(&evil).unwrap()).unwrap();
    let before = sha(&dir.join("fieldwork.nr"));
    let (code, out, _) = nari(
        &dir,
        &[],
        &[
            "repair",
            &inc,
            "--proposer",
            &format!("file:{}", evil_path.display()),
        ],
    );
    assert_eq!(code, 4);
    let report = details(&out);
    assert_eq!(report["state"], "Rejected");
    assert_eq!(report["static_validation"]["code"], "PATCH_SCOPE_VIOLATION");
    assert_eq!(sha(&dir.join("fieldwork.nr")), before);

    let (code, out, _) = nari(&dir, &[], &["repair", &inc, "--proposer", "fixture"]);
    assert_eq!(code, 0, "second attempt within max_attempts = 2: {out}");
    assert_eq!(out["attempt"], 2);
    let ranked = out["precedents"].as_array().unwrap();
    assert_eq!(ranked.len(), 1);
    assert_eq!(ranked[0]["state"], "Rejected");
    assert_eq!(
        ranked[0]["reusable"], false,
        "failed attempts stay visible but are not reused"
    );
    let (code, out, _) = nari(&dir, &[], &["repair", &inc, "--proposer", "fixture"]);
    assert_eq!(code, 4);
    assert_eq!(details(&out)["state"], "Exhausted");
    let (_, list, _) = nari(&dir, &[], &["incidents"]);
    let inc2 = list["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["incident_id"] == inc.as_str())
        .unwrap();
    assert_eq!(
        inc2["outcome"], "open",
        "exhaustion does not pretend the failure is resolved"
    );
}

#[test]
fn missing_sandbox_reports_unavailable_instead_of_validating() {
    let (dir, inc) = seeded("repair-nosandbox");
    let (code, out, _) = nari(
        &dir,
        &[("NARI_SANDBOX", "none")],
        &["repair", &inc, "--proposer", "fixture"],
    );
    assert_eq!(code, 3);
    let report = details(&out);
    assert_eq!(out["diagnostics"][0]["code"], "SANDBOX_UNAVAILABLE");
    assert_eq!(report["state"], "Validating");
    assert_eq!(
        report["static_validation"]["result"], "passed",
        "proposal and static validation still work"
    );
    assert_eq!(report["reproduction"]["result"], "SANDBOX_UNAVAILABLE");
    assert!(
        !dir.join(format!(".nari/repair/{inc}/candidates")).exists(),
        "no candidate is stored as validated"
    );
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "isolated repair needs the macOS Seatbelt sandbox"
)]
fn precedents_require_compatible_versions() {
    let (dir, inc) = seeded("repair-precedent");
    let (code, out, _) = nari(&dir, &[], &["repair", &inc, "--proposer", "fixture"]);
    assert_eq!(code, 0, "{out}");
    let p = imanari::Project::locate(Some(dir.clone())).unwrap();
    let c = imanari::compile(&std::fs::read_to_string(&p.file).unwrap(), "fieldwork.nr")
        .unwrap()
        .0;
    let sig = "test:acceptance/complete_task.json:step1:data.completed";
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (compatible, incompatible) = rt.block_on(async {
        let ctl = imanari::supervisor::Control::open(&p).await.unwrap();
        let a = imanari::repair::precedents(&ctl, sig, &imanari::repair::compat_key(&c), "none")
            .await
            .unwrap();
        let b = imanari::repair::precedents(&ctl, sig, "different-compiler-or-schema", "none")
            .await
            .unwrap();
        (a, b)
    });
    assert_eq!(compatible[0]["reusable"], true);
    assert_eq!(incompatible[0]["reusable"], false);
    assert!(
        incompatible[0]["note"]
            .as_str()
            .unwrap()
            .contains("incompatible")
    );

    let candidate = out["proposal"]["candidate_hash"].as_str().unwrap();
    assert_eq!(
        nari(&dir, &[], &["apply-repair", &inc, "--candidate", candidate]).0,
        0
    );
    let fixed = std::fs::read_to_string(&p.file).unwrap();
    std::fs::write(&p.file, fixed.replace(BUGGY_FROM, BUGGY_TO)).unwrap();
    assert_eq!(nari(&dir, &[], &["test"]).0, 1);
    let (_, list, _) = nari(&dir, &[], &["incidents"]);
    let open = list["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["outcome"] == "open")
        .unwrap();
    let new_inc = open["incident_id"].as_str().unwrap();
    assert_ne!(new_inc, inc);
    let (code, again, _) = nari(&dir, &[], &["repair", new_inc, "--proposer", "fixture"]);
    assert_eq!(
        code, 0,
        "regression after a resolved repair starts a new budget: {again}"
    );
    assert!(
        again["proposal"]["patch"]["rationale"]
            .as_str()
            .unwrap()
            .contains("validated precedent"),
        "compatible validated precedent ranked first"
    );
}

#[test]
#[cfg_attr(
    not(target_os = "macos"),
    ignore = "isolated repair needs the macOS Seatbelt sandbox"
)]
fn exported_evidence_contains_no_secret_canaries() {
    let (dir, inc) = seeded("repair-canary");
    let p = imanari::Project::locate(Some(dir.clone())).unwrap();
    let b = fieldwork();
    let app = start(b, &dir.join(".nari/canary.db"), &[]).unwrap();
    let (a, other) = imanari::launch::dev_sessions(app.port, &app.bootstrap).unwrap();
    let payload = format!("CANARY-USER-PAYLOAD-{}", imanari::random_hex(6));
    imanari::launch::call(
        app.port,
        Some(&a),
        "create_task",
        &json!({ "title": payload }),
        Some("canary"),
    )
    .unwrap();
    let mut canaries = vec![
        app.bootstrap.clone(),
        a.csrf.clone(),
        other.csrf.clone(),
        payload.clone(),
    ];
    for s in [&a, &other] {
        canaries.push(s.cookie.trim_start_matches("nari_session=").to_string());
    }
    drop(app);
    let (code, _, _) = nari(
        &dir,
        &[("NARI_PORT", "0"), ("NARI_FAULT", "crash_on_start")],
        &["dev"],
    );
    assert_eq!(code, 3);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let owner = rt.block_on(async {
        let ctl = imanari::supervisor::Control::open(&p).await.unwrap();
        ctl.state().await.unwrap().unwrap().1["owner_token"]
            .as_str()
            .unwrap()
            .to_string()
    });
    canaries.push(owner);
    assert_eq!(
        nari(&dir, &[], &["repair", &inc, "--proposer", "fixture"]).0,
        0
    );
    assert_eq!(
        nari(&dir, &[], &["context", "fieldwork/action/complete_task"]).0,
        0
    );
    let mut outputs = vec![
        nari(&dir, &[], &["status"]).1.to_string(),
        nari(&dir, &[], &["incidents"]).1.to_string(),
    ];
    let mut stack = vec![
        dir.join(".nari/context"),
        dir.join(".nari/incidents"),
        dir.join(".nari/test/report.json"),
        dir.join(".nari/repair"),
    ];
    let mut scanned = 0;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if path.ends_with("target") || path.ends_with("run") {
                continue;
            }
            stack.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
        } else if path.extension().is_some_and(|x| x == "json" || x == "md") {
            outputs.push(std::fs::read_to_string(&path).unwrap());
            scanned += 1;
        }
    }
    assert!(scanned >= 8, "scanned {scanned} evidence files");
    for c in &canaries {
        for o in &outputs {
            assert!(
                !o.contains(c.as_str()),
                "canary leaked into exported evidence"
            );
        }
    }
}
