mod common;

use common::*;
use imanari::http;
use imanari::launch::{Session, TIMEOUT, call, dev_sessions};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn docker_available() -> bool {
    let ok = Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        assert!(
            std::env::var("NARI_REQUIRE_DOCKER")
                .unwrap_or_default()
                .is_empty(),
            "NARI_REQUIRE_DOCKER is set but no Docker engine is reachable"
        );
        eprintln!("DOCKER_UNAVAILABLE: skipping the deployment test");
    }
    ok
}

fn nari(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (Option<i32>, Value) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nari"));
    cmd.args(["--json", "--project", dir.to_str().unwrap()])
        .args(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code(),
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
    )
}

fn failure(r: &(Option<i32>, Value)) -> (Option<i32>, &str) {
    (
        r.0,
        r.1["diagnostics"][0]["code"].as_str().unwrap_or_default(),
    )
}

fn actions(plan: &Value) -> Vec<String> {
    plan["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{}:{}",
                r["kind"].as_str().unwrap(),
                r["action"].as_str().unwrap()
            )
        })
        .collect()
}

fn session(name: &str, port: u16) -> Session {
    let out = Command::new("docker")
        .args(["logs", name])
        .output()
        .unwrap();
    let logs = String::from_utf8_lossy(&out.stdout);
    let token = logs
        .lines()
        .filter_map(|l| l.strip_prefix("NARI_BOOTSTRAP "))
        .next_back()
        .and_then(|url| url.rsplit("#bootstrap=").next())
        .unwrap()
        .to_string();
    dev_sessions(port, &token).unwrap().0
}

fn items(port: u16, s: &Session) -> Vec<Value> {
    let r = call(port, Some(s), "list_tasks", &json!({}), None).unwrap();
    assert_eq!(r.status, 200, "{:?}", r.json());
    r.json()["data"]["items"].as_array().cloned().unwrap()
}

struct Cleanup(String);

impl Drop for Cleanup {
    fn drop(&mut self) {
        let n = &self.0;
        let _ = Command::new("docker")
            .args([
                "rm",
                "-f",
                n,
                &format!("{n}-blue"),
                &format!("{n}-green"),
                &format!("{n}-proxy"),
            ])
            .output();
        let _ = Command::new("docker")
            .args(["volume", "rm", "-f", &format!("{n}-data")])
            .output();
        let _ = Command::new("docker").args(["network", "rm", n]).output();
    }
}

struct Load {
    stop: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<(usize, Vec<String>, BTreeSet<String>)>,
}

impl Load {
    fn start(port: u16) -> Load {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let (mut ok, mut failures, mut sources) = (0, Vec::new(), BTreeSet::new());
            while !flag.load(Ordering::Relaxed) {
                match http::request(port, "GET", "/_nari/health", &[], b"", TIMEOUT) {
                    Ok(r) if r.status == 200 => {
                        ok += 1;
                        sources.insert(
                            r.json()["source_sha256"]
                                .as_str()
                                .unwrap_or_default()
                                .to_string(),
                        );
                    }
                    Ok(r) => failures.push(format!("status {}", r.status)),
                    Err(e) => failures.push(e.to_string()),
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            (ok, failures, sources)
        });
        Load { stop, handle }
    }

    fn finish(self) -> (usize, Vec<String>, BTreeSet<String>) {
        self.stop.store(true, Ordering::Relaxed);
        self.handle.join().unwrap()
    }
}

fn containers(name: &str) -> Vec<String> {
    let out = Command::new("docker")
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("name=^{name}"),
            "--format",
            "{{.Names}}",
        ])
        .output()
        .unwrap();
    let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    names.sort();
    names
}

#[test]
fn docker_blue_green_releases_switch_without_dropping_requests() {
    if !docker_available() {
        return;
    }
    let dir = write_project("deploy", FIXTURE).dir;
    let name = format!("nari-test-{}", imanari::random_hex(4));
    let _cleanup = Cleanup(name.clone());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let ps = port.to_string();
    let plan_args = ["plan", "--name", name.as_str(), "--port", ps.as_str()];

    let r = nari(&dir, &["plan", "--production"], &[]);
    assert_eq!(failure(&r), (Some(4), "E_DEV_IDENTITY"));

    let (exit, v) = nari(&dir, &plan_args, &[]);
    assert_eq!(exit, Some(0), "{v}");
    let plan = &v["plan"];
    assert_eq!(plan["adapter"], "docker");
    assert!(
        plan["artifact"]["digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(
        actions(plan),
        [
            "image:use",
            "network:create",
            "volume:create",
            "container:create",
            "proxy:create"
        ]
    );
    assert_eq!(plan["resources"][3]["name"], format!("{name}-blue"));
    assert_eq!(
        plan["resources"][4]["publish"],
        format!("127.0.0.1:{port}:{port}/tcp")
    );
    assert_eq!(plan["estimated_cost"]["monthly_usd"], 0);
    let h1 = v["plan_sha256"].as_str().unwrap().to_string();

    let file = dir.join(format!(".nari/deploy/plans/{h1}.json"));
    let original = std::fs::read_to_string(&file).unwrap();
    std::fs::write(&file, original.replace("127.0.0.1:", "0.0.0.0:")).unwrap();
    let r = nari(&dir, &["release", &h1], &[]);
    assert_eq!(failure(&r), (Some(4), "E_PLAN_TAMPERED"));
    std::fs::write(&file, &original).unwrap();

    let (exit, v) = nari(&dir, &["release", &h1], &[]);
    assert_eq!(exit, Some(0), "{v}");
    assert_eq!(v["release"]["outcome"], "released");
    assert_eq!(v["release"]["color"], "blue");
    assert!(!v.to_string().contains("bootstrap="));
    let a = session(&format!("{name}-blue"), port);
    let r = call(
        port,
        Some(&a),
        "create_task",
        &json!({ "title": "deployed" }),
        Some("d1"),
    )
    .unwrap();
    assert_eq!(r.status, 200, "{:?}", r.json());

    let r = nari(&dir, &["release", &h1], &[]);
    assert_eq!(failure(&r), (Some(4), "E_DRIFT"));

    let src = dir.join("fieldwork.nari");
    let v2 = FIXTURE.replace(
        "    field completed {",
        "    field notes {\n      type = Text;\n      default = \"from v2\";\n    }\n\n    field completed {",
    );
    std::fs::write(&src, &v2).unwrap();
    let (exit, v) = nari(&dir, &plan_args, &[]);
    assert_eq!(exit, Some(0), "{v}");
    assert_eq!(
        v["plan"]["schema_transition"]["automatic"],
        json!(["add field Task.notes"])
    );
    assert_eq!(
        actions(&v["plan"]),
        [
            "image:use",
            "network:keep",
            "volume:keep",
            "container:create",
            "proxy:keep",
            "retire:stop and remove"
        ]
    );
    assert_eq!(
        v["plan"]["resources"][4]["route"],
        json!({ "from": "blue", "to": "green" })
    );
    let h2 = v["plan_sha256"].as_str().unwrap().to_string();
    let v2_sha = v["plan"]["app"]["source_sha256"]
        .as_str()
        .unwrap()
        .to_string();
    std::fs::write(&src, format!("{v2}\n// edited after planning\n")).unwrap();
    let r = nari(&dir, &["release", &h2], &[]);
    assert_eq!(failure(&r), (Some(4), "E_STALE_PLAN"));
    std::fs::write(&src, &v2).unwrap();

    let load = Load::start(port);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (exit, v) = nari(&dir, &["release", &h2], &[]);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (ok, failures, sources) = load.finish();
    assert_eq!(exit, Some(0), "{v}");
    assert_eq!(v["release"]["color"], "green");
    assert!(
        failures.is_empty(),
        "requests failed during the switch: {failures:?}"
    );
    assert!(ok > 20, "the load ran through the release ({ok} requests)");
    assert_eq!(sources.len(), 2, "both versions answered: {sources:?}");
    assert!(sources.contains(&v2_sha));
    assert_eq!(
        containers(&name),
        [format!("{name}-green"), format!("{name}-proxy")],
        "the old color is retired"
    );
    let a = session(&format!("{name}-green"), port);
    let kept = items(port, &a);
    assert_eq!(kept.len(), 1, "the shared volume keeps data across colors");
    assert_eq!(kept[0]["title"], "deployed");
    assert_eq!(kept[0]["notes"], "from v2");

    std::fs::write(&src, FIXTURE).unwrap();
    let (exit, v) = nari(&dir, &plan_args, &[]);
    assert_eq!(exit, Some(0), "{v}");
    assert_eq!(v["plan"]["schema_transition"]["behind"], true);
    assert_eq!(v["plan"]["schema_transition"]["automatic"], json!([]));
    let retyped = FIXTURE.replace(
        "    field completed {",
        "    field notes {\n      type = Int;\n      default = 0;\n    }\n\n    field completed {",
    );
    std::fs::write(&src, &retyped).unwrap();
    let r = nari(&dir, &plan_args, &[]);
    assert_eq!(failure(&r), (Some(4), "E_MIGRATION_BLOCKED"));
    assert!(r.1.to_string().contains("type of Task.notes changed"));
    std::fs::write(&src, &v2).unwrap();

    let var = "NARI_TEST_DEPLOY_DATABASE_URL";
    let mut pg_args = plan_args.to_vec();
    pg_args.extend(["--database-url-env", var]);
    let (exit, v) = nari(&dir, &pg_args, &[]);
    assert_eq!(exit, Some(0), "{v}");
    assert_eq!(
        actions(&v["plan"]),
        [
            "image:use",
            "network:keep",
            "container:create",
            "proxy:keep",
            "retire:stop and remove"
        ]
    );
    assert_eq!(
        v["plan"]["resources"][2]["secrets"],
        json!({ "NARI_DATABASE_URL": format!("env:{var}") })
    );
    let h3 = v["plan_sha256"].as_str().unwrap().to_string();
    let r = nari(&dir, &["release", &h3], &[]);
    assert_eq!(failure(&r), (Some(3), "E_SECRET_MISSING"));

    let canary = "postgres://nari:canary-secret-7f3a@127.0.0.1:1/none";
    let load = Load::start(port);
    let r = nari(&dir, &["release", &h3], &[(var, canary)]);
    let (_, failures, sources) = load.finish();
    assert_eq!(failure(&r), (Some(3), "E_RELEASE_FAILED"));
    assert!(r.1.to_string().contains("ended aborted"), "{}", r.1);
    assert!(
        failures.is_empty(),
        "a failed release drops no requests: {failures:?}"
    );
    assert_eq!(
        sources.into_iter().collect::<Vec<_>>(),
        [v2_sha],
        "traffic never moved"
    );
    assert_eq!(
        containers(&name),
        [format!("{name}-green"), format!("{name}-proxy")]
    );
    assert_eq!(
        items(port, &a).len(),
        1,
        "the serving color kept its sessions and data"
    );

    let mut stored = String::new();
    for sub in ["deploy/plans", "deploy/releases"] {
        for entry in std::fs::read_dir(dir.join(".nari").join(sub)).unwrap() {
            stored.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
        }
    }
    assert!(stored.contains("\"aborted\""));
    assert!(
        !stored.contains("canary-secret-7f3a"),
        "secrets never reach plans or release records"
    );

    let mut local = plan_args.to_vec();
    local.extend(["--public-url", "https://tasks.example.test"]);
    assert_eq!(failure(&nari(&dir, &local, &[])), (Some(2), "E_USAGE"));
    let mut wide = plan_args.to_vec();
    wide.extend(["--publish-address", "0.0.0.0"]);
    assert_eq!(failure(&nari(&dir, &wide, &[])), (Some(2), "E_USAGE"));

    let oidc = v2.replace(
        "    mode = local_dev;",
        "    mode = oidc;\n    issuer = \"https://idp.example.test\";\n    client_id = \"tasks\";",
    );
    std::fs::write(&src, &oidc).unwrap();
    let mut prod = plan_args.to_vec();
    prod.push("--production");
    assert_eq!(failure(&nari(&dir, &prod, &[])), (Some(2), "E_USAGE"));
    prod.extend(["--public-url", "http://tasks.example.test"]);
    assert_eq!(failure(&nari(&dir, &prod, &[])), (Some(2), "E_USAGE"));
    prod.truncate(prod.len() - 2);
    prod.extend([
        "--public-url",
        "https://tasks.example.test",
        "--oidc-secret-env",
        "NARI_TEST_OIDC_SECRET",
        "--publish-address",
        "0.0.0.0",
    ]);
    let (exit, v) = nari(&dir, &prod, &[]);
    assert_eq!(exit, Some(0), "{v}");
    let plan = &v["plan"];
    assert_eq!(plan["environment"], "production");
    assert_eq!(plan["authentication"]["mode"], "oidc");
    assert_eq!(
        plan["authentication"]["public_url"],
        "https://tasks.example.test"
    );
    let container = &plan["resources"][3];
    assert_eq!(
        container["env"]["NARI_PUBLIC_URL"],
        "https://tasks.example.test"
    );
    assert_eq!(
        container["secrets"],
        json!({ "NARI_OIDC_CLIENT_SECRET": "env:NARI_TEST_OIDC_SECRET" })
    );
    assert_eq!(
        plan["resources"][4]["publish"],
        format!("0.0.0.0:{port}:{port}/tcp")
    );
}
