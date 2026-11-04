mod common;

use common::*;
use imanari::context;
use imanari::patch::{self, Edit, Patch};
use serde_json::json;

const TARGET: &str = "fieldwork/action/complete_task";

fn compiled(src: &str) -> imanari::ir::Compiled {
    imanari::compile(src, "fieldwork.nari").unwrap().0
}

fn set_patch(src: &str, c: &imanari::ir::Compiled, from: &str, to: &str) -> Patch {
    let start = src.find(from).expect("fragment present");
    Patch {
        schema: patch::SCHEMA.into(),
        base_source_sha256: c.source_sha256.clone(),
        base_graph_sha256: c.graph_sha256.clone(),
        affected_nodes: vec![TARGET.into()],
        allowed_scope: vec![TARGET.into()],
        edits: vec![Edit {
            file: "fieldwork.nari".into(),
            start,
            end: start + from.len(),
            replacement: to.into(),
        }],
        rationale: String::new(),
    }
}

#[test]
fn context_includes_required_contracts_and_omits_unrelated_implementations() {
    let p = write_project("context", FIXTURE);
    let c = compiled(FIXTURE);
    let b = context::extract(&p, &c, FIXTURE, TARGET, context::DEFAULT_MAX_BYTES).unwrap();
    assert!(!b.over_budget);
    let md = &b.markdown;
    for needle in [
        "entity Task {",
        "owner_field = owner;",
        "identity development {",
        "button complete {",
        "editable_actions = [complete_task];",
        "acceptance/complete_task.json",
        "updates:Task owner=actor revision=checked",
        "request_limit_bytes 65536",
        &c.source_sha256,
        &c.graph_sha256,
    ] {
        assert!(md.contains(needle), "missing {needle}");
    }
    for absent in [
        "kind = insert;",
        "title: @input.title",
        "submit = create_task;",
        "kind = list;",
        "max_restarts = 3;",
    ] {
        assert!(
            !md.contains(absent),
            "unrelated implementation leaked: {absent}"
        );
    }
    let m = &b.manifest;
    assert_eq!(m["bytes"].as_u64().unwrap() as usize, md.len());
    assert!(
        m["token_estimate"]["label"]
            .as_str()
            .unwrap()
            .contains("estimate")
    );
    let omitted: Vec<&str> = m["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert!(
        omitted.contains(&"fieldwork/action/create_task")
            && omitted.contains(&"fieldwork/action/list_tasks")
    );
    assert!(
        m["omitted"][0]["expand"]
            .as_str()
            .unwrap()
            .starts_with("nari context ")
    );
    let again = context::extract(&p, &c, FIXTURE, TARGET, context::DEFAULT_MAX_BYTES).unwrap();
    assert_eq!(again.markdown, b.markdown, "deterministic");
}

#[test]
fn oversized_mandatory_context_fails_without_truncation() {
    let p = write_project("budget", FIXTURE);
    let c = compiled(FIXTURE);
    let b = context::extract(&p, &c, FIXTURE, TARGET, 600).unwrap();
    assert!(b.over_budget);
    assert!(b.markdown.is_empty());
    assert_eq!(b.manifest["status"], "CONTEXT_BUDGET_EXCEEDED");
    let required: Vec<&str> = b.manifest["required_expansion"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(required.contains(&TARGET) && required.contains(&"fieldwork/entity/Task"));
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_nari"))
        .args([
            "--project",
            p.dir.to_str().unwrap(),
            "context",
            TARGET,
            "--max-bytes",
            "600",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("CONTEXT_BUDGET_EXCEEDED"));
    assert!(
        p.artifacts()
            .join("context/fieldwork.action.complete_task/manifest.json")
            .exists()
    );
    assert!(
        !p.artifacts()
            .join("context/fieldwork.action.complete_task/context.md")
            .exists()
    );
}

#[test]
fn patch_preconditions_and_scope_are_enforced() {
    let c = compiled(FIXTURE);
    let good = set_patch(
        FIXTURE,
        &c,
        "set = { completed: true };",
        "set = { completed: false };",
    );
    let applied = patch::apply_text(&good, "fieldwork.nari", FIXTURE).unwrap();
    let cand = compiled(&applied);
    assert_eq!(
        patch::check_scope(&c, &cand, &good.allowed_scope).unwrap(),
        vec![TARGET.to_string()]
    );

    let edited = FIXTURE.replace("app fieldwork {", "// a comment-only edit\napp fieldwork {");
    assert_eq!(
        patch::apply_text(&good, "fieldwork.nari", &edited)
            .unwrap_err()
            .code,
        "PATCH_STALE"
    );

    for file in [
        "acceptance/complete_task.json",
        "Cargo.toml",
        "Cargo.lock",
        "../fieldwork.nari",
        "/etc/passwd",
        ".nari/generated/app/src/main.rs",
        "other.nari",
    ] {
        let mut p = good.clone();
        p.edits[0].file = file.into();
        assert_eq!(
            patch::apply_text(&p, "fieldwork.nari", FIXTURE)
                .unwrap_err()
                .code,
            "PATCH_PATH_FORBIDDEN",
            "{file}"
        );
    }

    let scope = |from: &str, to: &str| {
        let p = set_patch(FIXTURE, &c, from, to);
        let text = patch::apply_text(&p, "fieldwork.nari", FIXTURE).unwrap();
        match imanari::compile(&text, "fieldwork.nari") {
            Ok((cand, _)) => {
                patch::check_scope(&c, &cand, &p.allowed_scope)
                    .unwrap_err()
                    .code
            }
            Err(_) => "COMPILE_REJECTED",
        }
    };
    assert_eq!(
        scope(
            "max_restarts = 3;\n    window_seconds = 60;\n    backoff_ms = [100, 500, 2000];",
            "max_restarts = 0;\n    window_seconds = 60;\n    backoff_ms = [];"
        ),
        "PATCH_SCOPE_VIOLATION"
    );
    assert_eq!(
        scope(
            "editable_actions = [complete_task];",
            "editable_actions = [complete_task, create_task];"
        ),
        "PATCH_SCOPE_VIOLATION"
    );
    assert_eq!(
        scope(
            "owner_field = owner;",
            "owner_field = owner;\n    field extra {\n      type = Bool;\n      default = true;\n    }"
        ),
        "PATCH_SCOPE_VIOLATION"
    );
    assert_eq!(
        scope("title: @input.title,", "title: \"constant\","),
        "PATCH_SCOPE_VIOLATION"
    );
    assert_eq!(
        scope(
            "set = { completed: true };",
            "set = { completed: true, owner: @input.id };"
        ),
        "COMPILE_REJECTED"
    );
}

#[test]
fn behavior_diff_classifies_bounds_owner_and_unknown_changes() {
    let base = compiled(FIXTURE);
    let narrowed = FIXTURE.replacen(
        "    input title {\n      type = Text;\n      min_length = 1;\n      max_length = 200;",
        "    input title {\n      type = Text;\n      min_length = 1;\n      max_length = 100;",
        1,
    );
    let d = imanari::diff::diff(&base, &compiled(&narrowed));
    let ch = &d["changes"][0];
    assert_eq!(ch["node"], "fieldwork/action/create_task/input/title");
    assert_eq!(ch["category"], "input/output");
    assert!(ch["detail"].as_str().unwrap().contains("narrowed"));
    let affected: Vec<&str> = ch["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(affected.contains(&"fieldwork/screen/tasks/form/add_task"));
    assert!(affected.contains(&"fieldwork/action/create_task"));
    assert!(!affected.contains(&"fieldwork/action/complete_task"));
    assert_eq!(d["migration_required"], false);

    let owner = FIXTURE
        .replace("owner_field = owner;", "owner_field = assignee;")
        .replace("field owner {", "field assignee {")
        .replace("owner: @actor.id,", "assignee: @actor.id,");
    let d = imanari::diff::diff(&base, &compiled(&owner));
    let access = d["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["category"] == "access")
        .unwrap();
    assert_eq!(access["node"], "fieldwork/entity/Task");
    let affected: Vec<&str> = access["affected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for a in [
        "fieldwork/action/complete_task",
        "fieldwork/action/list_tasks",
        "fieldwork/action/create_task",
        "fieldwork/screen/tasks/list/task_list/button/complete",
    ] {
        assert!(affected.contains(&a), "{a} not affected");
    }
    assert_eq!(d["migration_required"], true);
    assert_eq!(d["full_validation_required"], true);

    let commented = FIXTURE.replace("app fieldwork {", "// note\napp fieldwork {");
    let d = imanari::diff::diff(&base, &compiled(&commented));
    assert_eq!(d["behavior_changed"], false);
    assert_eq!(d["changes"], json!([]));

    let mut odd = base.clone();
    odd.graph.edges.push(imanari::ir::Edge {
        from: "fieldwork/action/list_tasks".into(),
        role: "mystery".into(),
        to: "fieldwork/entity/Task".into(),
    });
    let d = imanari::diff::diff(&base, &odd);
    assert_eq!(d["full_validation_required"], true);
    assert!(
        d["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["category"] == "unknown")
    );
}

#[test]
fn task_record_resumes_only_when_hashes_and_evidence_match() {
    let p = write_project("progress", FIXTURE);
    let c = compiled(FIXTURE);
    let mut rec = imanari::progress::TaskRecord::new(
        "demo-1",
        "fix complete_task",
        &c.source_sha256,
        &c.graph_sha256,
    );
    rec.completed.push("captured context".into());
    rec.outstanding_checks.push("nari test".into());
    rec.add_evidence(&p.dir, "acceptance/complete_task.json", "protected-test")
        .unwrap();
    let path = p.dir.join("task.json");
    rec.save(&path).unwrap();
    let loaded = imanari::progress::TaskRecord::load(&path).unwrap();
    assert_eq!(loaded, rec);
    assert!(
        loaded
            .verify(&p.dir, &c.source_sha256, &c.graph_sha256)
            .is_empty()
    );
    std::fs::write(p.dir.join("acceptance/complete_task.json"), b"{}").unwrap();
    let drift = loaded.verify(&p.dir, "other", &c.graph_sha256);
    assert_eq!(drift.len(), 2, "{drift:?}");
}

#[test]
fn macos_metadata_files_are_ignored_by_project_discovery() {
    let p = write_project("appledouble", FIXTURE);
    let junk = [0u8, 5, 22, 7, 0, 2, 0, 0];
    std::fs::write(p.dir.join("._fieldwork.nari"), junk).unwrap();
    std::fs::write(p.dir.join("acceptance/._complete_task.json"), junk).unwrap();
    let located = imanari::Project::locate(Some(p.dir.clone())).unwrap();
    assert_eq!(located.file_name, "fieldwork.nari");
    let tests = imanari::testrun::load_protected(&p.dir).unwrap();
    assert_eq!(tests.len(), 3);
}

#[test]
fn relative_project_paths_resolve_to_absolute_directories() {
    let p = write_project("relative", FIXTURE);
    let cwd = std::env::current_dir().unwrap();
    let rel = p.dir.strip_prefix(&cwd).unwrap().to_path_buf();
    assert!(rel.is_relative());
    let located = imanari::Project::locate(Some(rel)).unwrap();
    assert!(located.dir.is_absolute());
    assert_eq!(located.dir, p.dir);
}

#[test]
fn schema_changes_are_planned_as_automatic_or_blocked() {
    let base = compiled(FIXTURE);
    let with_notes = FIXTURE.replace(
        "    field completed {",
        "    field notes {\n      type = Text;\n      default = \"it's new\";\n    }\n\n    field completed {",
    );
    let d = imanari::diff::diff(&base, &compiled(&with_notes));
    assert_eq!(d["migration"]["automatic"], json!(["add field Task.notes"]));
    assert_eq!(d["migration_required"], false);
    assert_eq!(d["full_validation_required"], true);
    assert!(imanari::diff::render(&d).contains("applied automatically on the next start"));

    let back = imanari::diff::diff(&compiled(&with_notes), &base);
    assert_eq!(back["migration_required"], true);
    assert_eq!(
        back["migration"]["blocked"],
        json!(["field Task.notes was removed, which would drop its stored values"])
    );

    let field_title =
        "    field title {\n      type = Text;\n      min_length = 1;\n      max_length = 200;";
    let widened = FIXTURE.replacen(field_title, &field_title.replace("200", "500"), 1);
    let d = imanari::diff::diff(&base, &compiled(&widened));
    assert_eq!(
        d["migration"]["automatic"],
        json!(["widen Task.title from Text 1..200 to Text 1..500"])
    );
    let narrowed = FIXTURE.replacen(field_title, &field_title.replace("200", "100"), 1);
    let d = imanari::diff::diff(&base, &compiled(&narrowed));
    assert_eq!(d["migration_required"], true);
    assert!(
        d["migration"]["blocked"][0]
            .as_str()
            .unwrap()
            .contains("Task.title")
    );

    let old = imanari::codegen::schema_shape(&base.app);
    let mut new = old.clone();
    new[0].fields.push(imanari_runtime::FieldShape {
        name: "due".into(),
        ty: json!({ "name": "Int" }),
        add: None,
    });
    let blocked = imanari_runtime::migration_plan(&old, &new).unwrap_err();
    assert!(
        blocked[0].contains("Task.due was added without a default"),
        "{blocked:?}"
    );
    new[0].owner = "title".into();
    assert_eq!(
        imanari_runtime::migration_plan(&old, &new)
            .unwrap_err()
            .len(),
        2
    );
    let mut extra = old.clone();
    extra.push(imanari_runtime::EntityShape {
        name: "Note".into(),
        ..old[0].clone()
    });
    let steps = imanari_runtime::migration_plan(&old, &extra).unwrap();
    assert_eq!(steps[0].summary, "create entity Note");
    assert!(
        imanari_runtime::migration_plan(&extra, &old).unwrap_err()[0]
            .contains("entity Note was removed")
    );
}
