mod common;

use common::*;
use imanari::http;
use imanari::launch::{TIMEOUT, call};
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};

fn items(port: u16, s: &imanari::launch::Session) -> Vec<Value> {
    let r = call(port, Some(s), "list_tasks", &json!({}), None).unwrap();
    assert_eq!(r.status, 200, "{:?}", r.json());
    r.json()["data"]["items"].as_array().cloned().unwrap()
}

fn create(port: u16, s: &imanari::launch::Session, title: &str, key: &str) -> http::Response {
    call(
        port,
        Some(s),
        "create_task",
        &json!({ "title": title }),
        Some(key),
    )
    .unwrap()
}

#[test]
fn users_are_isolated_and_web_boundary_is_enforced() {
    let b = fieldwork();
    let dir = scratch("isolation");
    let (app, a, other) = app_with_sessions(b, &dir.join("app.db"), &[]);
    let port = app.port;
    let task = create(port, &a, "A's secret", "k1").json()["data"].clone();
    assert_eq!(task["owner"], json!(a.actor));
    assert!(items(port, &other).is_empty());
    let guess = call(
        port,
        Some(&other),
        "complete_task",
        &json!({ "id": task["id"], "revision": 1 }),
        Some("g1"),
    )
    .unwrap();
    assert_eq!(guess.status, 404);
    let missing = call(
        port,
        Some(&a),
        "complete_task",
        &json!({ "id": "00000000-0000-4000-8000-000000000123", "revision": 1 }),
        Some("g2"),
    )
    .unwrap();
    assert_eq!(missing.status, 404);
    assert_eq!(
        guess.json()["error"],
        missing.json()["error"],
        "ownership failure and absence are indistinguishable"
    );
    let before = "00000000-0000-0000-0000-000000000000";
    let page = call(
        port,
        Some(&other),
        "list_tasks",
        &json!({ "after": before, "limit": 100 }),
        None,
    )
    .unwrap();
    assert_eq!(page.json()["data"]["items"], json!([]));
    let forged_owner = call(
        port,
        Some(&other),
        "create_task",
        &json!({ "title": "x", "owner": a.actor }),
        Some("f1"),
    )
    .unwrap();
    assert_eq!(forged_owner.status, 400);
    assert_eq!(forged_owner.json()["error"]["field"], "owner");
    assert_eq!(items(port, &a).len(), 1);

    assert_eq!(
        call(port, None, "list_tasks", &json!({}), None)
            .unwrap()
            .status,
        401
    );
    let forged = imanari::launch::Session {
        cookie: "nari_session=deadbeef".into(),
        ..a.clone()
    };
    assert_eq!(
        call(port, Some(&forged), "list_tasks", &json!({}), None)
            .unwrap()
            .status,
        401
    );
    let cross_cookie = imanari::launch::Session {
        csrf: other.csrf.clone(),
        ..a.clone()
    };
    assert_eq!(
        call(port, Some(&cross_cookie), "list_tasks", &json!({}), None)
            .unwrap()
            .status,
        403
    );

    let body = json!({ "title": "x" }).to_string();
    let hdr = |extra: Vec<(&'static str, String)>| {
        let mut h: Vec<(String, String)> = vec![
            ("Content-Type".into(), "application/json".into()),
            ("Cookie".into(), a.cookie.clone()),
            ("X-Nari-CSRF".into(), a.csrf.clone()),
            ("Idempotency-Key".into(), imanari::random_hex(8)),
        ];
        for (k, v) in extra {
            h.push((k.into(), v));
        }
        h
    };
    let send = |h: Vec<(String, String)>, body: &[u8]| {
        let refs: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        http::request(
            port,
            "POST",
            "/api/actions/create_task",
            &refs,
            body,
            TIMEOUT,
        )
        .unwrap()
    };
    assert_eq!(
        send(hdr(vec![]), body.as_bytes()).status,
        200,
        "no Origin with CSRF header is allowed for CLI/test clients"
    );
    assert_eq!(
        send(
            hdr(vec![("Origin", "http://evil.example".into())]),
            body.as_bytes()
        )
        .status,
        403
    );
    assert_eq!(
        send(hdr(vec![("Origin", "null".into())]), body.as_bytes()).status,
        403
    );
    assert_eq!(
        send(
            hdr(vec![("Sec-Fetch-Site", "cross-site".into())]),
            body.as_bytes()
        )
        .status,
        403
    );
    let mut bad_host = hdr(vec![]);
    bad_host.push(("Host".into(), "attacker.example".into()));
    let refs: Vec<(&str, &str)> = bad_host
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let raw = http::request(
        port,
        "POST",
        "/api/actions/create_task",
        &refs,
        body.as_bytes(),
        TIMEOUT,
    )
    .unwrap();
    assert_eq!(raw.status, 403, "rebinding Host is rejected");
    let huge = json!({ "title": "x".repeat(70_000) }).to_string();
    assert_eq!(send(hdr(vec![]), huge.as_bytes()).status, 413);
    assert_eq!(send(hdr(vec![]), b"not json").status, 400);
    assert_eq!(send(hdr(vec![]), b"[1]").status, 400);
    let empty = send(hdr(vec![]), json!({ "title": "" }).to_string().as_bytes());
    assert_eq!(
        (empty.status, empty.json()["error"]["field"].clone()),
        (400, json!("title"))
    );
    let wrong = send(hdr(vec![]), json!({ "title": 5 }).to_string().as_bytes());
    assert_eq!(wrong.status, 400);
    let resp = send(hdr(vec![]), body.as_bytes());
    assert_eq!(resp.header("cache-control"), Some("no-store"));
    assert!(
        resp.header("content-security-policy")
            .unwrap()
            .contains("script-src 'self'")
    );
    assert!(resp.header("access-control-allow-origin").is_none());
}

#[test]
fn bootstrap_token_is_single_use() {
    let b = fieldwork();
    let dir = scratch("bootstrap");
    let app = start(b, &dir.join("app.db"), &[]).unwrap();
    imanari::launch::dev_sessions(app.port, &app.bootstrap).unwrap();
    assert!(imanari::launch::dev_sessions(app.port, &app.bootstrap).is_err());
    let page = http::request(app.port, "GET", "/", &[], b"", TIMEOUT).unwrap();
    assert_eq!(page.status, 200);
    let html = String::from_utf8(page.body).unwrap();
    assert!(!html.contains(&app.bootstrap));
    assert!(!html.contains("<script>"), "no inline scripts");
}

#[test]
fn concurrent_updates_with_same_revision_yield_one_success() {
    let b = fieldwork();
    let dir = scratch("revisions");
    let (app, a, _) = app_with_sessions(b, &dir.join("app.db"), &[]);
    let port = app.port;
    let task = create(port, &a, "race", "r1").json()["data"].clone();
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let (a, task, barrier) = (a.clone(), task.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                call(
                    port,
                    Some(&a),
                    "complete_task",
                    &json!({ "id": task["id"], "revision": 1 }),
                    Some(&format!("u{i}")),
                )
                .unwrap()
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let ok = results.iter().filter(|r| r.status == 200).count();
    let conflicts = results
        .iter()
        .filter(|r| r.status == 409 && r.json()["error"]["code"] == "REVISION_CONFLICT")
        .count();
    assert_eq!((ok, conflicts), (1, 7));
    assert_eq!(items(port, &a)[0]["revision"], 2);
}

#[test]
fn concurrent_idempotent_creates_converge_on_one_record() {
    let b = fieldwork();
    let dir = scratch("idempotency");
    let (app, a, other) = app_with_sessions(b, &dir.join("app.db"), &[]);
    let port = app.port;
    let barrier = Arc::new(Barrier::new(8));
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let (a, barrier) = (a.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                create(port, &a, "once", "same-key")
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.iter().all(|r| r.status == 200));
    let ids: std::collections::BTreeSet<String> = results
        .iter()
        .map(|r| r.json()["data"]["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(items(port, &a).len(), 1);
    assert_eq!(create(port, &a, "different", "same-key").status, 409);
    assert_eq!(
        create(port, &other, "once", "same-key").status,
        200,
        "receipts are scoped per actor"
    );
}

#[test]
fn committed_write_with_lost_response_is_returned_on_retry() {
    let b = fieldwork();
    let dir = scratch("lost-response");
    let db = dir.join("app.db");
    let (mut app, a, _) =
        app_with_sessions(b, &db, &[("NARI_FAULT", "crash_after_commit:create_task")]);
    let lost = call(
        app.port,
        Some(&a),
        "create_task",
        &json!({ "title": "durable" }),
        Some("retry-me"),
    );
    assert!(lost.is_err(), "process died before responding: {lost:?}");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(app.exited());
    let (app2, a2, _) = app_with_sessions(b, &db, &[]);
    let retry = create(app2.port, &a2, "durable", "retry-me");
    assert_eq!(retry.status, 200);
    let list = items(app2.port, &a2);
    assert_eq!(list.len(), 1, "no duplicate");
    assert_eq!(list[0]["id"], retry.json()["data"]["id"]);
}

#[test]
fn rolled_back_write_is_not_reported_or_persisted() {
    let b = fieldwork();
    let dir = scratch("rollback");
    let db = dir.join("app.db");
    let (mut app, a, _) =
        app_with_sessions(b, &db, &[("NARI_FAULT", "crash_before_commit:create_task")]);
    let r = call(
        app.port,
        Some(&a),
        "create_task",
        &json!({ "title": "never" }),
        Some("k"),
    );
    assert!(r.is_err(), "no success reported: {r:?}");
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(app.exited());
    let (app2, a2, _) = app_with_sessions(b, &db, &[]);
    assert!(items(app2.port, &a2).is_empty());
    let retry = create(app2.port, &a2, "never", "k");
    assert_eq!(
        retry.status, 200,
        "key was not consumed by the rolled-back attempt"
    );
}

#[test]
fn mutation_rate_limit_returns_429() {
    let b = fieldwork();
    let dir = scratch("rate");
    let (app, a, other) = app_with_sessions(b, &dir.join("app.db"), &[]);
    let statuses: Vec<u16> = (0..61)
        .map(|i| create(app.port, &a, "r", &format!("k{i}")).status)
        .collect();
    assert!(statuses[..60].iter().all(|s| *s == 200));
    assert_eq!(statuses[60], 429);
    assert_eq!(
        create(app.port, &other, "r", "k").status,
        200,
        "limit is per actor"
    );
    assert_eq!(
        call(app.port, Some(&a), "list_tasks", &json!({}), None)
            .unwrap()
            .status,
        200
    );
}

#[test]
fn pagination_is_bounded_and_owner_scoped() {
    let b = fieldwork();
    let dir = scratch("pages");
    let (app, a, _) = app_with_sessions(b, &dir.join("app.db"), &[]);
    for i in 0..5 {
        create(app.port, &a, &format!("t{i}"), &format!("p{i}"));
    }
    let mut seen = Vec::new();
    let mut after = Value::Null;
    loop {
        let r = call(
            app.port,
            Some(&a),
            "list_tasks",
            &json!({ "limit": 2, "after": after }),
            None,
        )
        .unwrap();
        let data = r.json()["data"].clone();
        let page: Vec<Value> = data["items"].as_array().unwrap().clone();
        assert!(page.len() <= 2);
        seen.extend(page.iter().map(|i| i["id"].as_str().unwrap().to_string()));
        after = data["next_cursor"].clone();
        if after.is_null() {
            break;
        }
    }
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(seen, sorted);
    assert_eq!(seen.len(), 5);
    for bad in [
        json!({ "limit": 0 }),
        json!({ "limit": 101 }),
        json!({ "after": "XYZ" }),
    ] {
        assert_eq!(
            call(app.port, Some(&a), "list_tasks", &bad, None)
                .unwrap()
                .status,
            400
        );
    }
}

#[test]
fn additive_schema_changes_migrate_and_destructive_ones_stop() {
    let b = fieldwork();
    let dir = scratch("migration");
    let db = dir.join("app.db");
    let (app, a, _) = app_with_sessions(b, &db, &[]);
    create(app.port, &a, "keep me", "m1");
    drop(app);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let sql = |q: &'static str| {
        rt.block_on(async {
            let pool =
                imanari_runtime::sqlx::SqlitePool::connect(&format!("sqlite:{}", db.display()))
                    .await
                    .unwrap();
            imanari_runtime::sqlx::query_scalar::<_, String>(q)
                .fetch_all(&pool)
                .await
                .unwrap()
        })
    };
    sql("DELETE FROM \"_nari_meta\" WHERE \"key\" = 'schema_shape' RETURNING \"key\"");
    let changed = FIXTURE.replace(
        "    field completed {",
        "    field notes {\n      type = Text;\n      default = \"it's new\";\n    }\n\n    field completed {",
    );
    let b2 = build("migration-v2", &changed);
    let err = start(&b2, &db, &[])
        .err()
        .expect("a database without a stored shape refuses");
    assert!(err.contains("predates recorded schema shapes"), "{err}");
    drop(start(b, &db, &[]).expect("an identical fingerprint still starts"));
    let (app2, a2, _) = app_with_sessions(&b2, &db, &[]);
    let kept = items(app2.port, &a2);
    assert_eq!(kept.len(), 1, "data kept");
    assert_eq!(kept[0]["title"], "keep me");
    assert_eq!(kept[0]["notes"], "it's new");
    assert_eq!(create(app2.port, &a2, "after", "m2").status, 200);
    drop(app2);
    assert_eq!(
        sql("SELECT \"steps\" FROM \"_nari_schema_history\""),
        vec![r#"["add field Task.notes"]"#.to_string()]
    );
    let err = start(b, &db, &[])
        .err()
        .expect("removing a field refuses to start");
    assert!(err.contains("MIGRATION_REQUIRED"), "{err}");
    assert!(err.contains("Task.notes was removed"), "{err}");
    let (app3, a3, _) = app_with_sessions(&b2, &db, &[]);
    assert_eq!(items(app3.port, &a3).len(), 2, "data untouched");
}

#[test]
fn rust_compile_errors_map_back_to_imanari_spans() {
    let project = write_project("rustc-map", FIXTURE);
    let (c, _) = imanari::compile(FIXTURE, &project.file_name).unwrap();
    let out = imanari::artifacts::prepare(&project).unwrap();
    let package = imanari::pipeline::package_name(&project, &c.app.name);
    let generated = imanari::pipeline::generate(&out, &c, &package).unwrap();
    let action = generated
        .regions
        .iter()
        .find(|r| r.node == "fieldwork/action/complete_task")
        .unwrap()
        .clone();
    let main = out.join("generated/app/src/main.rs");
    let mut lines: Vec<String> = std::fs::read_to_string(&main)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    lines.insert(
        action.start_line + 1,
        "        let broken: u8 = \"not a number\";".into(),
    );
    std::fs::write(&main, lines.join("\n")).unwrap();
    let shifted = imanari::codegen::Generated {
        regions: generated
            .regions
            .iter()
            .map(|r| {
                let mut r = r.clone();
                if r.start_line > action.start_line {
                    r.start_line += 1;
                }
                if r.end_line >= action.start_line {
                    r.end_line += 1;
                }
                r
            })
            .collect(),
        ..generated
    };
    let (code, diags) = imanari::pipeline::compile_app(
        &c,
        &out,
        &shifted,
        &package,
        std::path::Path::new(SHARED_TARGET),
        None,
    )
    .unwrap_err();
    assert_eq!(code, 3);
    let d = diags
        .iter()
        .find(|d| d.code == "E_BACKEND_COMPILE")
        .unwrap();
    assert_eq!(d.node_id.as_deref(), Some("fieldwork/action/complete_task"));
    assert_eq!(d.file, "fieldwork.nari");
    assert_eq!(
        &FIXTURE[d.span.start..d.span.start + 22],
        "action complete_task {"
    );
    assert!(d.details.as_deref().unwrap().contains("mismatched types"));
}

#[test]
fn database_symlink_escapes_are_rejected_but_system_symlinks_are_not() {
    let b = fieldwork();
    let dir = scratch("symlink");
    let via_var = std::path::Path::new("/var").join(
        std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .strip_prefix("/private/var")
            .unwrap_or(std::path::Path::new("tmp")),
    );
    if via_var.exists() {
        let db = via_var.join(format!("nari-{}.db", imanari::random_hex(4)));
        assert!(
            start(b, &db, &[]).is_ok(),
            "a system-level symlink such as /var must not block startup"
        );
    }
    let outside = scratch("symlink-target");
    let link_dir = dir.join("project");
    std::fs::create_dir_all(&link_dir).unwrap();
    std::os::unix::fs::symlink(&outside, link_dir.join(".nari")).unwrap();
    let app = imanari::launch::AppProcess::start(&imanari::launch::LaunchOptions {
        binary: b.binary.clone(),
        cwd: link_dir.clone(),
        database: std::path::PathBuf::from(".nari/app.db"),
        port: 0,
        log: None,
        env: Vec::new(),
        wrapper: Vec::new(),
    });
    assert!(
        app.is_err(),
        "a symlinked .nari directory inside the project is refused"
    );
    assert!(!outside.join("app.db").exists());
}
