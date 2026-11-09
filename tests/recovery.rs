mod common;

use common::*;
use imanari::launch::{call, dev_sessions};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

struct Dev {
    child: Child,
    lines: Receiver<String>,
}

impl Dev {
    fn start(dir: &Path, env: &[(&str, &str)], args: &[&str]) -> Dev {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nari"));
        cmd.args(["--project", dir.to_str().unwrap(), "dev"])
            .args(args)
            .env("NARI_TARGET_DIR", SHARED_TARGET)
            .env_remove("NARI_FAULT")
            .env_remove("NARI_PORT")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                let _ = tx.send(l);
            }
        });
        Dev { child, lines: rx }
    }

    fn expect(&self, prefix: &str, secs: u64) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) if l.starts_with(prefix) => return l,
                Ok(_) => {}
                Err(_) => panic!("no '{prefix}' line within {secs}s"),
            }
        }
    }

    fn ready(&self) -> (u16, String) {
        let open = self.expect("open: ", 120);
        let token = open.rsplit("#bootstrap=").next().unwrap().to_string();
        let ready = self.expect("ready: ", 30);
        let port = ready
            .split(':')
            .nth(3)
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        (port, token)
    }

    fn signal(&self, sig: &str) {
        let ok = Command::new("kill")
            .args([sig, &self.child.id().to_string()])
            .status()
            .unwrap();
        assert!(ok.success());
    }

    fn wait(&mut self, secs: u64) -> Option<i32> {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while std::time::Instant::now() < deadline {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s.code();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("nari dev did not exit within {secs}s");
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn nari_json(dir: &Path, args: &[&str]) -> (Option<i32>, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_nari"))
        .args(["--json", "--project", dir.to_str().unwrap()])
        .args(args)
        .env("NARI_TARGET_DIR", SHARED_TARGET)
        .output()
        .unwrap();
    (
        out.status.code(),
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
    )
}

fn project(name: &str) -> std::path::PathBuf {
    fieldwork();
    write_project(name, FIXTURE).dir
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn crash_mid_request_restarts_and_preserves_committed_data() {
    let dir = project("recovery-crash");
    let mut dev = Dev::start(
        &dir,
        &[
            ("NARI_PORT", "0"),
            ("NARI_FAULT", "crash_after_commit:create_task"),
        ],
        &[],
    );
    let (port, token) = dev.ready();
    let (a, _) = dev_sessions(port, &token).unwrap();
    let lost = call(
        port,
        Some(&a),
        "create_task",
        &json!({ "title": "survives" }),
        Some("crash-key"),
    );
    assert!(
        lost.is_err(),
        "response lost when the worker died: {lost:?}"
    );
    let restart = dev.expect("worker exited unexpectedly", 10);
    assert!(restart.contains("restart 1/3 in 100 ms"), "{restart}");
    let (port2, token2) = dev.ready();
    let (a2, _) = dev_sessions(port2, &token2).unwrap();
    let retry = call(
        port2,
        Some(&a2),
        "create_task",
        &json!({ "title": "survives" }),
        Some("crash-key"),
    )
    .unwrap();
    assert_eq!(retry.status, 200);
    let list = call(port2, Some(&a2), "list_tasks", &json!({}), None)
        .unwrap()
        .json();
    assert_eq!(list["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["data"]["items"][0]["id"], retry.json()["data"]["id"]);

    let (_, status) = nari_json(&dir, &["status"]);
    assert_eq!(status["state"], "Healthy");
    assert_eq!(status["restarts_in_window"], 1);
    assert!(
        status["detail"].get("owner_token").is_none(),
        "status never exposes the owner token"
    );
    let metrics = &status["metrics"];
    assert_eq!(metrics["instance"], status["detail"]["instance"]);
    assert_eq!(
        metrics["actions"]["create_task"]["by_status"],
        json!({ "200": 1 })
    );
    assert_eq!(metrics["actions"]["list_tasks"]["requests"], 1);
    let (_, incidents) = nari_json(&dir, &["incidents"]);
    let inc = &incidents["incidents"][0];
    assert_eq!(inc["outcome"], "recovered_by_restart");
    assert_eq!(inc["failure_signature"], "backend:exit:signal:6");
    let id = inc["incident_id"].as_str().unwrap();
    assert!(dir.join(format!(".nari/incidents/{id}.json")).exists());

    dev.signal("-TERM");
    assert_eq!(dev.wait(15), Some(0));
    assert_eq!(nari_json(&dir, &["status"]).1["state"], "Stopped");
    assert!(imanari::launch::wait_port_closed(
        port2,
        Duration::from_secs(5)
    ));
    assert_eq!(
        nari_json(&dir, &["status"]).1["restarts_in_window"],
        1,
        "deliberate shutdown is not a restart"
    );
}

#[test]
fn exhausted_budget_stops_durably_and_survives_supervisor_restart() {
    let dir = project("recovery-exhaust");
    let mut dev = Dev::start(
        &dir,
        &[("NARI_PORT", "0"), ("NARI_FAULT", "crash_on_start")],
        &[],
    );
    for (attempt, backoff) in [(1, 100), (2, 500), (3, 2000)] {
        let l = dev.expect("worker exited unexpectedly", 120);
        assert!(
            l.contains(&format!("restart {attempt}/3 in {backoff} ms")),
            "{l}"
        );
    }
    dev.expect("STOPPED (needs attention): restart budget exhausted", 15);
    assert_eq!(dev.wait(10), Some(3));
    let (_, status) = nari_json(&dir, &["status"]);
    assert_eq!(status["state"], "StoppedNeedsAttention");
    assert_eq!(status["restarts_in_window"], 3);

    let (code, refused) = nari_json(&dir, &["dev"]);
    assert_eq!(code, Some(3));
    assert_eq!(refused["diagnostics"][0]["code"], "STOPPED_NEEDS_ATTENTION");

    let mut again = Dev::start(
        &dir,
        &[("NARI_PORT", "0"), ("NARI_FAULT", "crash_on_start")],
        &["--acknowledge"],
    );
    let l = again.expect("STOPPED (needs attention): restart budget exhausted", 60);
    assert!(
        l.contains("3 restarts"),
        "budget was not reset by restarting the supervisor: {l}"
    );
    assert_eq!(again.wait(10), Some(3));
    let (_, status) = nari_json(&dir, &["status"]);
    assert_eq!(
        status["restarts_in_window"], 3,
        "no restart was granted after acknowledgement"
    );
    let (_, incidents) = nari_json(&dir, &["incidents"]);
    let outcomes: Vec<&str> = incidents["incidents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == "stopped_needs_attention")
            .count(),
        2
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == "restart_scheduled")
            .count(),
        3
    );
}

#[test]
fn supervisor_restart_reconciles_without_duplicate_workers() {
    let dir = project("recovery-reconcile");
    let port = free_port();
    let port_s = port.to_string();
    let mut first = Dev::start(&dir, &[("NARI_PORT", &port_s)], &[]);
    first.ready();
    let mut dup = Dev::start(&dir, &[("NARI_PORT", "0")], &[]);
    assert_eq!(
        dup.wait(120),
        Some(3),
        "a second supervisor is refused while the first holds the lock"
    );

    first.child.kill().unwrap();
    first.child.wait().unwrap();
    assert!(
        imanari::launch::wait_port_closed(port, Duration::from_secs(10)),
        "orphaned worker exits when its supervisor dies"
    );
    assert_eq!(
        nari_json(&dir, &["status"]).1["state"],
        "Healthy",
        "killed supervisor left its last known state"
    );

    let mut second = Dev::start(&dir, &[("NARI_PORT", &port_s)], &[]);
    second.expect("reconciled: previous worker is gone", 120);
    second.ready();
    second.signal("-TERM");
    assert_eq!(second.wait(15), Some(0));

    let b = fieldwork();
    let stray_db = scratch("stray").join("stray.db");
    let stray = imanari::launch::AppProcess::start(&imanari::launch::LaunchOptions {
        binary: b.binary.clone(),
        cwd: dir.clone(),
        database: stray_db,
        port,
        log: None,
        env: Vec::new(),
        wrapper: Vec::new(),
    })
    .unwrap();
    let stray_instance = stray.instance.clone();
    let mut blocked = Dev::start(&dir, &[("NARI_PORT", &port_s)], &[]);
    assert_eq!(blocked.wait(120), Some(3));
    let (_, refused) = nari_json(&dir, &["status"]);
    assert_eq!(refused["state"], "Stopped");
    assert!(
        refused["detail"]["reason"]
            .as_str()
            .unwrap()
            .contains("RECONCILE_FAILED")
    );
    let health = imanari::http::request(
        port,
        "GET",
        "/_nari/health",
        &[],
        b"",
        Duration::from_secs(2),
    )
    .unwrap()
    .json();
    assert_eq!(
        health["instance"],
        stray_instance.as_str(),
        "no duplicate worker replaced the unverified process"
    );
    drop(stray);
    assert!(imanari::launch::wait_port_closed(
        port,
        Duration::from_secs(5)
    ));

    let mut recovered = Dev::start(&dir, &[("NARI_PORT", &port_s)], &[]);
    recovered.ready();
    assert_eq!(nari_json(&dir, &["status"]).1["state"], "Healthy");
    recovered.signal("-TERM");
    assert_eq!(recovered.wait(15), Some(0));
}
