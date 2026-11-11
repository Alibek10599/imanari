mod common;

use common::*;
use imanari::launch::{Session, call};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Barrier};

struct Cluster {
    bin: PathBuf,
    data: PathBuf,
    dir: PathBuf,
    port: u16,
}

fn pg_bin() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("NARI_PG_BIN") {
        return Some(PathBuf::from(dir));
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    if let Some(dir) = std::env::split_paths(&path).find(|d| d.join("initdb").is_file()) {
        return Some(dir);
    }
    let mut found: Vec<PathBuf> = ["/opt/homebrew/opt", "/usr/local/opt", "/usr/lib/postgresql"]
        .iter()
        .flat_map(|root| std::fs::read_dir(root).into_iter().flatten().flatten())
        .map(|e| e.path().join("bin"))
        .filter(|b| b.join("initdb").is_file())
        .collect();
    found.sort();
    found.pop()
}

fn run(bin: &Path, args: &[&str]) {
    let out = Command::new(bin).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{} {args:?} failed: {}",
        bin.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Cluster {
    fn start(name: &str) -> Option<Cluster> {
        let Some(bin) = pg_bin() else {
            assert!(
                std::env::var_os("NARI_REQUIRE_POSTGRES").is_none(),
                "NARI_REQUIRE_POSTGRES is set but initdb was not found"
            );
            eprintln!("POSTGRES_UNAVAILABLE: initdb not found; set NARI_PG_BIN to run this test");
            return None;
        };
        let dir = scratch(name);
        let data = dir.join("pg");
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let data_s = data.to_str().unwrap();
        run(
            &bin.join("initdb"),
            &["-D", data_s, "-U", "nari", "--auth=trust", "-E", "UTF8"],
        );
        let log = dir.join("postgres.log");
        run(
            &bin.join("pg_ctl"),
            &[
                "-D",
                data_s,
                "-l",
                log.to_str().unwrap(),
                "-w",
                "-o",
                &format!("-p {port} -h 127.0.0.1 -k ''"),
                "start",
            ],
        );
        Some(Cluster {
            bin,
            data,
            dir,
            port,
        })
    }

    fn database(&self, name: &str) -> String {
        run(
            &self.bin.join("createdb"),
            &[
                "-h",
                "127.0.0.1",
                "-p",
                &self.port.to_string(),
                "-U",
                "nari",
                name,
            ],
        );
        format!("postgres://nari@127.0.0.1:{}/{name}", self.port)
    }

    fn psql(&self, db: &str, sql: &str) -> String {
        let out = Command::new(self.bin.join("psql"))
            .args([
                "-h",
                "127.0.0.1",
                "-p",
                &self.port.to_string(),
                "-U",
                "nari",
            ])
            .args(["-d", db, "-At", "-v", "ON_ERROR_STOP=1", "-c", sql])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let _ = Command::new(self.bin.join("pg_ctl"))
            .args([
                "-D",
                self.data.to_str().unwrap(),
                "-m",
                "immediate",
                "-w",
                "stop",
            ])
            .output();
    }
}

fn on(b: &Built, c: &Cluster, url: &str) -> (imanari::launch::AppProcess, Session, Session) {
    let log_path = c.dir.join(format!("app-{}.db", imanari::random_hex(4)));
    app_with_sessions(b, &log_path, &[("NARI_DATABASE_URL", url)])
}

fn refused(b: &Built, db: &Path, url: &str) -> String {
    let err = start(b, db, &[("NARI_DATABASE_URL", url)])
        .err()
        .expect("startup is refused");
    let log = std::fs::read_to_string(db.with_extension("log")).unwrap_or_default();
    format!("{err}\n{log}")
}

fn create(port: u16, s: &Session, title: &str, key: &str) -> Value {
    let r = call(
        port,
        Some(s),
        "create_task",
        &json!({ "title": title }),
        Some(key),
    )
    .unwrap();
    assert_eq!(r.status, 200, "{:?}", r.json());
    r.json()["data"].clone()
}

fn items(port: u16, s: &Session) -> Vec<Value> {
    let r = call(port, Some(s), "list_tasks", &json!({}), None).unwrap();
    assert_eq!(r.status, 200, "{:?}", r.json());
    r.json()["data"]["items"].as_array().cloned().unwrap()
}

#[test]
fn unsupported_database_urls_fail_before_serving() {
    let b = fieldwork();
    let dir = scratch("pg-url");
    let err = refused(b, &dir.join("app.db"), "mysql://root@localhost/app");
    assert!(err.contains("postgres://"), "{err}");
    assert!(
        !err.contains("root@localhost"),
        "the URL is never echoed: {err}"
    );
}

#[test]
fn postgres_backend_keeps_isolation_idempotency_and_revisions_across_instances() {
    let Some(c) = Cluster::start("pg-app") else {
        return;
    };
    let b = fieldwork();
    let tls = format!(
        "postgres://nari@127.0.0.1:{}/postgres?sslmode=require",
        c.port
    );
    let err = refused(b, &c.dir.join("tls.db"), &tls);
    assert!(err.contains("TLS"), "{err}");

    let url = c.database("fieldwork");
    let (app, a, other) = on(b, &c, &url);
    let task = create(app.port, &a, "A's secret", "k1");
    assert_eq!(task["owner"], json!(a.actor));
    assert!(items(app.port, &other).is_empty());
    let guess = call(
        app.port,
        Some(&other),
        "complete_task",
        &json!({ "id": task["id"], "revision": 1 }),
        Some("g1"),
    )
    .unwrap();
    assert_eq!(guess.status, 404);
    assert_eq!(create(app.port, &a, "A's secret", "k1")["id"], task["id"]);

    let (app2, a2, _) = on(b, &c, &url);
    assert_eq!(a2.actor, a.actor);
    let barrier = Arc::new(Barrier::new(8));
    let created: Vec<Value> = (0..8)
        .map(|i| {
            let port = if i % 2 == 0 { app.port } else { app2.port };
            let s = if i % 2 == 0 { a.clone() } else { a2.clone() };
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                create(port, &s, "once", "same-key")
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    assert!(
        created.iter().all(|t| t["id"] == created[0]["id"]),
        "two instances converge on one record: {created:?}"
    );

    let racing = create(app.port, &a, "race", "r1");
    let barrier = Arc::new(Barrier::new(8));
    let mut statuses: Vec<u16> = (0..8)
        .map(|i| {
            let port = if i % 2 == 0 { app.port } else { app2.port };
            let s = if i % 2 == 0 { a.clone() } else { a2.clone() };
            let id = racing["id"].clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                call(
                    port,
                    Some(&s),
                    "complete_task",
                    &json!({ "id": id, "revision": 1 }),
                    Some(&format!("c{i}")),
                )
                .unwrap()
                .status
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    statuses.sort();
    assert_eq!(statuses, [200, 409, 409, 409, 409, 409, 409, 409]);
    drop(app);
    drop(app2);

    let (app3, a3, _) = on(b, &c, &url);
    let kept = items(app3.port, &a3);
    assert_eq!(kept.len(), 3, "committed rows survive a restart");
    assert!(
        kept.windows(2)
            .all(|w| w[0]["id"].as_str() < w[1]["id"].as_str()),
        "ids page in byte order"
    );
    assert_eq!(
        c.psql("fieldwork", "SELECT count(*) FROM \"_nari_receipts\""),
        "4",
        "only committed writes keep receipts"
    );
}

#[test]
fn postgres_applies_additive_migrations_and_refuses_destructive_ones() {
    let Some(c) = Cluster::start("pg-migration") else {
        return;
    };
    let b = fieldwork();
    let url = c.database("migration");
    let (app, a, _) = on(b, &c, &url);
    create(app.port, &a, "keep me", "m1");
    drop(app);
    let changed = FIXTURE.replace(
        "    field completed {",
        "    field notes {\n      type = Text;\n      default = \"it's new\";\n    }\n\n    field completed {",
    );
    let b2 = build("pg-migration-v2", &changed);
    let (app2, a2, _) = on(&b2, &c, &url);
    let kept = items(app2.port, &a2);
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0]["notes"], "it's new");
    assert_eq!(create(app2.port, &a2, "after", "m2")["notes"], "it's new");
    drop(app2);
    assert_eq!(
        c.psql("migration", "SELECT steps FROM _nari_schema_history"),
        r#"["add field Task.notes"]"#
    );
    let err = refused(b, &c.dir.join("old.db"), &url);
    assert!(err.contains("MIGRATION_REQUIRED"), "{err}");
    assert!(err.contains("Task.notes was removed"), "{err}");

    let foreign = c.database("foreign");
    c.psql("foreign", "CREATE TABLE unrelated (id int)");
    let err = refused(b, &c.dir.join("foreign.db"), &foreign);
    assert!(err.contains("no Imanari schema fingerprint"), "{err}");
}
