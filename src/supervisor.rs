use crate::Project;
use crate::artifacts;
use crate::http;
use crate::ir::{self, Compiled};
use crate::pipeline;
use serde_json::{Value as Json, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

pub const TARGET: &str = "backend";
const READY_TIMEOUT: Duration = Duration::from_secs(30);
const EXIT_CONFIG: i32 = 77;
const EXIT_MIGRATION: i32 = 78;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Starting,
    Healthy,
    Backoff,
    StoppedNeedsAttention,
    Stopped,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Starting => "Starting",
            State::Healthy => "Healthy",
            State::Backoff => "Backoff",
            State::StoppedNeedsAttention => "StoppedNeedsAttention",
            State::Stopped => "Stopped",
        }
    }

    fn parse(s: &str) -> Option<State> {
        Some(match s {
            "Starting" => State::Starting,
            "Healthy" => State::Healthy,
            "Backoff" => State::Backoff,
            "StoppedNeedsAttention" => State::StoppedNeedsAttention,
            "Stopped" => State::Stopped,
            _ => return None,
        })
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn redact(v: &Json) -> Json {
    match v {
        Json::String(s) => {
            let mut out = String::with_capacity(s.len());
            let mut run = String::new();
            for ch in s.chars().chain(std::iter::once(' ')) {
                if ch.is_ascii_hexdigit() {
                    run.push(ch);
                    continue;
                }
                if run.len() >= 32 {
                    out.push_str("[redacted]");
                } else {
                    out.push_str(&run);
                }
                run.clear();
                out.push(ch);
            }
            out.pop();
            Json::String(out)
        }
        Json::Array(a) => Json::Array(a.iter().map(redact).collect()),
        Json::Object(m) => Json::Object(
            m.iter()
                .map(|(k, v)| {
                    let keep = k.ends_with("sha256") || k == "incident_id" || k == "candidate_hash";
                    (k.clone(), if keep { v.clone() } else { redact(v) })
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

pub struct Control {
    pub db: SqlitePool,
    pub dir: PathBuf,
}

impl Control {
    pub async fn open(p: &Project) -> Result<Control, String> {
        let dir = artifacts::prepare(p)?;
        let path = artifacts::safe_join(&dir, "control.db")?;
        let opts = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let db = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(opts)
            .await
            .map_err(|e| format!("cannot open control database: {e}"))?;
        artifacts::restrict(&path, 0o600);
        for stmt in [
            "CREATE TABLE IF NOT EXISTS supervisor (target TEXT PRIMARY KEY NOT NULL, state TEXT NOT NULL, owner_token TEXT, pid INTEGER, port INTEGER, instance TEXT, binary_sha256 TEXT, reason TEXT, updated_ms INTEGER NOT NULL) STRICT",
            "CREATE TABLE IF NOT EXISTS restarts (id INTEGER PRIMARY KEY AUTOINCREMENT, target TEXT NOT NULL, at_ms INTEGER NOT NULL, incident_id TEXT NOT NULL) STRICT",
            "CREATE TABLE IF NOT EXISTS transitions (id INTEGER PRIMARY KEY AUTOINCREMENT, target TEXT NOT NULL, from_state TEXT, to_state TEXT NOT NULL, detail TEXT NOT NULL, at_ms INTEGER NOT NULL) STRICT",
            "CREATE TABLE IF NOT EXISTS incidents (id TEXT PRIMARY KEY NOT NULL, target_id TEXT NOT NULL, kind TEXT NOT NULL, signature TEXT NOT NULL, outcome TEXT NOT NULL, record TEXT NOT NULL, created_ms INTEGER NOT NULL) STRICT",
            "CREATE TABLE IF NOT EXISTS repairs (id TEXT PRIMARY KEY NOT NULL, incident_id TEXT NOT NULL, signature TEXT NOT NULL, target_id TEXT NOT NULL, attempt INTEGER NOT NULL, state TEXT NOT NULL, candidate_hash TEXT, policy_sha256 TEXT NOT NULL, source_sha256 TEXT NOT NULL, compat TEXT NOT NULL, outcome TEXT NOT NULL, evidence TEXT NOT NULL, updated_ms INTEGER NOT NULL) STRICT",
            "CREATE TABLE IF NOT EXISTS repair_transitions (id INTEGER PRIMARY KEY AUTOINCREMENT, repair_id TEXT NOT NULL, incident_id TEXT NOT NULL, state TEXT NOT NULL, candidate_hash TEXT, attempt INTEGER NOT NULL, policy_sha256 TEXT NOT NULL, evidence TEXT NOT NULL, at_ms INTEGER NOT NULL) STRICT",
        ] {
            sqlx::query(stmt)
                .execute(&db)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(Control { db, dir })
    }

    pub async fn state(&self) -> Result<Option<(State, Json)>, String> {
        let row: Option<(String, Option<String>, Option<i64>, Option<i64>, Option<String>, Option<String>, i64)> = sqlx::query_as(
            "SELECT state, owner_token, pid, port, instance, reason, updated_ms FROM supervisor WHERE target = ?1",
        )
        .bind(TARGET)
        .fetch_optional(&self.db)
        .await
        .map_err(|e| e.to_string())?;
        Ok(row.and_then(|(s, token, pid, port, instance, reason, at)| {
            Some((
                State::parse(&s)?,
                json!({ "owner_token": token, "pid": pid, "port": port, "instance": instance, "reason": reason, "updated_ms": at }),
            ))
        }))
    }

    pub async fn transition(
        &self,
        to: State,
        detail: &str,
        child: Option<(&str, i64, i64, &str, &str)>,
    ) -> Result<(), String> {
        let mut tx = self.db.begin().await.map_err(|e| e.to_string())?;
        let from: Option<String> =
            sqlx::query_scalar("SELECT state FROM supervisor WHERE target = ?1")
                .bind(TARGET)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        let (token, pid, port, instance, binary) = match child {
            Some((t, pid, port, inst, bin)) => (
                Some(t.to_string()),
                Some(pid),
                Some(port),
                Some(inst.to_string()),
                Some(bin.to_string()),
            ),
            None => (None, None, None, None, None),
        };
        sqlx::query(
            "INSERT INTO supervisor (target, state, owner_token, pid, port, instance, binary_sha256, reason, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(target) DO UPDATE SET state = excluded.state, owner_token = COALESCE(excluded.owner_token, supervisor.owner_token), pid = excluded.pid, port = COALESCE(excluded.port, supervisor.port), instance = excluded.instance, binary_sha256 = COALESCE(excluded.binary_sha256, supervisor.binary_sha256), reason = excluded.reason, updated_ms = excluded.updated_ms",
        )
        .bind(TARGET)
        .bind(to.as_str())
        .bind(token)
        .bind(pid)
        .bind(port)
        .bind(instance)
        .bind(binary)
        .bind(detail)
        .bind(now_ms())
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO transitions (target, from_state, to_state, detail, at_ms) VALUES (?1, ?2, ?3, ?4, ?5)")
            .bind(TARGET)
            .bind(from)
            .bind(to.as_str())
            .bind(detail)
            .bind(now_ms())
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        tx.commit().await.map_err(|e| e.to_string())
    }

    pub async fn restarts_in_window(&self, window_seconds: u32) -> Result<i64, String> {
        sqlx::query_scalar("SELECT count(*) FROM restarts WHERE target = ?1 AND at_ms > ?2")
            .bind(TARGET)
            .bind(now_ms() - i64::from(window_seconds) * 1000)
            .fetch_one(&self.db)
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn record_incident(&self, record: &Json, restart: bool) -> Result<(), String> {
        let record = redact(record);
        let id = record["incident_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let mut tx = self.db.begin().await.map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO incidents (id, target_id, kind, signature, outcome, record, created_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)")
            .bind(&id)
            .bind(record["target_id"].as_str().unwrap_or_default())
            .bind(record["kind"].as_str().unwrap_or_default())
            .bind(record["failure_signature"].as_str().unwrap_or_default())
            .bind(record["outcome"].as_str().unwrap_or_default())
            .bind(ir::canonical_compact(&record))
            .bind(now_ms())
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        if restart {
            sqlx::query("INSERT INTO restarts (target, at_ms, incident_id) VALUES (?1, ?2, ?3)")
                .bind(TARGET)
                .bind(now_ms())
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(|e| e.to_string())?;
        }
        tx.commit().await.map_err(|e| e.to_string())?;
        self.export(&record)
    }

    pub async fn set_outcome(&self, id: &str, outcome: &str) -> Result<(), String> {
        let rec: Option<String> = sqlx::query_scalar("SELECT record FROM incidents WHERE id = ?1")
            .bind(id)
            .fetch_optional(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        let Some(rec) = rec else { return Ok(()) };
        let mut j: Json = serde_json::from_str(&rec).unwrap_or(Json::Null);
        j["outcome"] = json!(outcome);
        j["timestamps"]["resolved_ms"] = json!(now_ms());
        sqlx::query("UPDATE incidents SET outcome = ?1, record = ?2 WHERE id = ?3")
            .bind(outcome)
            .bind(ir::canonical_compact(&j))
            .bind(id)
            .execute(&self.db)
            .await
            .map_err(|e| e.to_string())?;
        self.export(&j)
    }

    pub fn export(&self, record: &Json) -> Result<(), String> {
        let dir = self.dir.join("incidents");
        artifacts::ensure_real_dir(&dir)?;
        let id = record["incident_id"].as_str().unwrap_or("unknown");
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!("invalid incident id '{id}'"));
        }
        artifacts::write_file(
            &dir,
            &format!("{id}.json"),
            ir::canonical(&redact(record)) + "\n",
        )
        .map(|_| ())
    }

    pub async fn incidents(&self, id: Option<&str>, limit: i64) -> Result<Vec<Json>, String> {
        let rows: Vec<String> = match id {
            Some(id) => {
                sqlx::query_scalar("SELECT record FROM incidents WHERE id = ?1")
                    .bind(id)
                    .fetch_all(&self.db)
                    .await
            }
            None => {
                sqlx::query_scalar(
                    "SELECT record FROM incidents ORDER BY created_ms DESC, id DESC LIMIT ?1",
                )
                .bind(limit)
                .fetch_all(&self.db)
                .await
            }
        }
        .map_err(|e| e.to_string())?;
        Ok(rows
            .iter()
            .filter_map(|r| serde_json::from_str(r).ok())
            .map(|j| redact(&j))
            .collect())
    }

    pub async fn transitions(&self, limit: i64) -> Result<Vec<Json>, String> {
        let rows: Vec<(Option<String>, String, String, i64)> =
            sqlx::query_as("SELECT from_state, to_state, detail, at_ms FROM transitions WHERE target = ?1 ORDER BY id DESC LIMIT ?2")
                .bind(TARGET)
                .bind(limit)
                .fetch_all(&self.db)
                .await
                .map_err(|e| e.to_string())?;
        Ok(rows
            .into_iter()
            .map(|(f, t, d, at)| json!({ "from": f, "to": t, "detail": d, "at_ms": at }))
            .collect())
    }
}

pub fn incident(
    c: &Compiled,
    kind: &str,
    signature: &str,
    observed: Json,
    intervention: Json,
    outcome: &str,
) -> Json {
    json!({
        "schema": "nari-incident/0.1",
        "incident_id": format!("inc-{}", crate::random_hex(8)),
        "target_id": format!("{}/artifact/{TARGET}", c.app.name),
        "kind": kind,
        "failure_signature": signature,
        "versions": {
            "source_sha256": c.source_sha256,
            "semantic_sha256": c.semantic_sha256,
            "runtime": "imanari-runtime/0.1",
            "compiler": format!("nari {}", ir::COMPILER_VERSION),
            "policy_sha256": pipeline::policy_hash(c),
            "dependency_lock_sha256": pipeline::lock_hash(),
        },
        "observed": observed,
        "hypotheses": [],
        "reproduction": null,
        "intervention": intervention,
        "preconditions": { "recovery_policy": ir::to_json(&c.app.recovery) },
        "checks": [],
        "outcome": outcome,
        "timestamps": { "detected_ms": now_ms() },
        "supersedes": null,
        "revoked_by": null,
    })
}

pub struct Lock {
    _file: File,
}

pub fn lock(p: &Project) -> Result<Lock, String> {
    let dir = artifacts::prepare(p)?.join("run");
    artifacts::ensure_real_dir(&dir)?;
    let path = artifacts::safe_join(&dir, "supervisor.lock")?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Lock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => {
            Err("another nari supervisor holds the lock for this project".into())
        }
        Err(std::fs::TryLockError::Error(e)) => Err(format!("cannot lock {}: {e}", path.display())),
    }
}

fn probe(port: u16) -> Option<Json> {
    http::request(
        port,
        "GET",
        "/_nari/health",
        &[],
        b"",
        Duration::from_millis(800),
    )
    .ok()
    .filter(|r| r.status == 200)
    .map(|r| r.json())
}

fn live_metrics(port: u16, token: &str) -> Option<Json> {
    let health = probe(port)?;
    if token.is_empty() || health["owner_sha256"] != ir::sha256_hex(token.as_bytes()).as_str() {
        return None;
    }
    http::request(
        port,
        "GET",
        "/_nari/metrics",
        &[("X-Nari-Owner", token)],
        b"",
        Duration::from_millis(800),
    )
    .ok()
    .filter(|r| r.status == 200)
    .map(|r| r.json()["data"].clone())
}

pub async fn reconcile(ctl: &Control, emit: &dyn Fn(&str, Json)) -> Result<(), String> {
    let Some((state, info)) = ctl.state().await? else {
        return Ok(());
    };
    let active = matches!(state, State::Starting | State::Healthy | State::Backoff);
    let Some(port) = info["port"].as_i64().filter(|p| *p > 0).map(|p| p as u16) else {
        if active {
            ctl.transition(
                State::Stopped,
                "reconciled: no recorded child endpoint",
                None,
            )
            .await?;
        }
        return Ok(());
    };
    let token = info["owner_token"].as_str().unwrap_or_default().to_string();
    let health = tokio::task::spawn_blocking(move || probe(port))
        .await
        .ok()
        .flatten();
    match health {
        None if active => {
            ctl.transition(State::Stopped, "reconciled: previous worker is gone", None)
                .await?;
            emit("reconciled", json!({ "detail": "previous worker is gone" }));
            Ok(())
        }
        None => Ok(()),
        Some(h)
            if active
                && !token.is_empty()
                && h["owner_sha256"] == ir::sha256_hex(token.as_bytes()).as_str() =>
        {
            let t = token.clone();
            let stopped = tokio::task::spawn_blocking(move || {
                let _ = http::request(
                    port,
                    "POST",
                    "/_nari/control/shutdown",
                    &[
                        ("X-Nari-Owner", &t),
                        ("Origin", &format!("http://127.0.0.1:{port}")),
                    ],
                    b"",
                    Duration::from_secs(2),
                );
                crate::launch::wait_port_closed(port, Duration::from_secs(10))
            })
            .await
            .unwrap_or(false);
            if !stopped {
                return Err(format!(
                    "RECONCILE_FAILED: verified orphan worker on port {port} did not stop"
                ));
            }
            ctl.transition(
                State::Stopped,
                "reconciled: stopped verified orphan worker",
                None,
            )
            .await?;
            emit(
                "reconciled",
                json!({ "detail": "stopped verified orphan worker", "instance": h["instance"] }),
            );
            Ok(())
        }
        Some(h) => {
            let msg = format!(
                "RECONCILE_FAILED: port {port} is served by an unverified process (instance {}); refusing to start a duplicate worker",
                h["instance"].as_str().unwrap_or("?")
            );
            ctl.transition(State::Stopped, &msg, None).await?;
            Err(msg)
        }
    }
}

pub struct DevOptions {
    pub binary: PathBuf,
    pub acknowledge: bool,
}

pub enum DevExit {
    Stopped,
    NeedsAttention(String),
}

async fn launch(binary: &Path, cwd: &Path, token: &str) -> Result<Child, String> {
    Command::new(binary)
        .current_dir(cwd)
        .env("NARI_OWNER_TOKEN", token)
        .env("NARI_SUPERVISED", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("cannot launch {}: {e}", binary.display()))
}

enum Waited {
    Ready(
        Json,
        tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    ),
    Exited(String, Option<i32>),
    Timeout,
}

async fn wait_ready(child: &mut Child, emit: &dyn Fn(&str, Json)) -> Waited {
    let Some(stdout) = child.stdout.take() else {
        return Waited::Timeout;
    };
    let mut lines = BufReader::new(stdout).lines();
    let deadline = tokio::time::sleep(READY_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(l)) => {
                    if let Some(url) = l.strip_prefix("NARI_BOOTSTRAP ") {
                        emit("bootstrap", json!({ "url": url }));
                    } else if let Some(r) = l.strip_prefix("NARI_READY ") {
                        return Waited::Ready(serde_json::from_str(r).unwrap_or(Json::Null), lines);
                    } else if let Some(r) = l.strip_prefix("NARI_STOP ") {
                        emit("app_stop", serde_json::from_str(r).unwrap_or(Json::Null));
                    }
                }
                _ => {
                    let status = child.wait().await.ok();
                    return Waited::Exited(describe(status), status.and_then(|s| s.code()));
                }
            },
            _ = &mut deadline => return Waited::Timeout,
        }
    }
}

fn describe(status: Option<std::process::ExitStatus>) -> String {
    match status {
        None => "unknown".into(),
        Some(s) => match s.code() {
            Some(c) => format!("code:{c}"),
            None => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    s.signal()
                        .map_or("signal:unknown".into(), |sig| format!("signal:{sig}"))
                }
                #[cfg(not(unix))]
                "signal:unknown".to_string()
            }
        },
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(_) => return std::future::pending().await,
            };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn run_dev(
    p: &Project,
    c: &Compiled,
    o: &DevOptions,
    emit: &dyn Fn(&str, Json),
) -> Result<DevExit, String> {
    let _lock = lock(p)?;
    let ctl = Control::open(p).await?;
    if let Some((State::StoppedNeedsAttention, info)) = ctl.state().await? {
        if !o.acknowledge {
            return Err(format!(
                "STOPPED_NEEDS_ATTENTION: {}; fix the cause, then run `nari dev --acknowledge` (restart history is kept)",
                info["reason"].as_str().unwrap_or("previous failure")
            ));
        }
        ctl.transition(
            State::Stopped,
            "operator acknowledged; restart history retained",
            None,
        )
        .await?;
    }
    reconcile(&ctl, emit).await?;
    let policy = &c.app.recovery;
    let binary_sha = std::fs::read(&o.binary)
        .map(|b| ir::sha256_hex(&b))
        .unwrap_or_default();
    let signal = shutdown_signal();
    tokio::pin!(signal);
    let mut pending_incident: Option<String> = None;
    loop {
        let token = crate::random_hex(32);
        ctl.transition(
            State::Starting,
            "launching worker",
            Some((&token, 0, 0, "", &binary_sha)),
        )
        .await?;
        emit("state", json!({ "state": "Starting" }));
        let started = Instant::now();
        let mut child = launch(&o.binary, &p.dir, &token).await?;
        let stdin = child.stdin.take();
        let exit = match wait_ready(&mut child, emit).await {
            Waited::Ready(ready, mut lines) => {
                let port = ready["port"].as_i64().unwrap_or(0);
                let instance = ready["instance"].as_str().unwrap_or_default().to_string();
                let pid = i64::from(child.id().unwrap_or(0));
                ctl.transition(
                    State::Healthy,
                    "worker ready",
                    Some((&token, pid, port, &instance, &binary_sha)),
                )
                .await?;
                if let Some(id) = pending_incident.take() {
                    ctl.set_outcome(&id, "recovered_by_restart").await?;
                }
                emit(
                    "ready",
                    json!({ "state": "Healthy", "port": port, "pid": pid, "instance": instance }),
                );
                tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
                tokio::select! {
                    status = child.wait() => {
                        let status = status.ok();
                        (describe(status), status.and_then(|s| s.code()))
                    }
                    _ = &mut signal => {
                        emit("state", json!({ "state": "Stopping" }));
                        let t = token.clone();
                        let port = port as u16;
                        let _ = tokio::task::spawn_blocking(move || http::request(
                            port,
                            "POST",
                            "/_nari/control/shutdown",
                            &[("X-Nari-Owner", &t), ("Origin", &format!("http://127.0.0.1:{port}"))],
                            b"",
                            Duration::from_secs(2),
                        )).await;
                        if tokio::time::timeout(Duration::from_secs(5), child.wait()).await.is_err() {
                            let _ = child.kill().await;
                        }
                        drop(stdin);
                        ctl.transition(State::Stopped, "deliberate shutdown", None).await?;
                        emit("state", json!({ "state": "Stopped" }));
                        return Ok(DevExit::Stopped);
                    }
                }
            }
            Waited::Exited(desc, code) => (desc, code),
            Waited::Timeout => {
                let _ = child.kill().await;
                ("readiness_timeout".to_string(), None)
            }
        };
        drop(stdin);
        let (desc, code) = exit;
        let uptime = started.elapsed().as_millis() as u64;
        if matches!(code, Some(EXIT_MIGRATION) | Some(EXIT_CONFIG)) {
            let reason = if code == Some(EXIT_MIGRATION) {
                "MIGRATION_REQUIRED"
            } else {
                "CONFIGURATION_ERROR"
            };
            let rec = incident(
                c,
                "startup_refused",
                &format!("{TARGET}:{reason}"),
                json!({ "exit": desc, "uptime_ms": uptime }),
                json!({ "kind": "stop", "reason": "deterministic startup refusal; restarting cannot help" }),
                "stopped_needs_attention",
            );
            ctl.record_incident(&rec, false).await?;
            let msg = format!(
                "{reason}: worker refused to start (incident {})",
                rec["incident_id"].as_str().unwrap_or("")
            );
            ctl.transition(State::StoppedNeedsAttention, &msg, None)
                .await?;
            emit(
                "state",
                json!({ "state": "StoppedNeedsAttention", "reason": msg }),
            );
            return Ok(DevExit::NeedsAttention(msg));
        }
        let used = ctl.restarts_in_window(policy.window_seconds).await?;
        let signature = format!(
            "{TARGET}:exit:{}",
            if desc == "readiness_timeout" {
                "readiness_timeout"
            } else {
                &desc
            }
        );
        if used >= i64::from(policy.max_restarts) {
            let rec = incident(
                c,
                "process_exit",
                &signature,
                json!({ "exit": desc, "uptime_ms": uptime, "restarts_in_window": used }),
                json!({ "kind": "stop", "reason": "restart budget exhausted" }),
                "stopped_needs_attention",
            );
            ctl.record_incident(&rec, false).await?;
            let msg = format!(
                "restart budget exhausted: {used} restarts within {}s (incident {})",
                policy.window_seconds,
                rec["incident_id"].as_str().unwrap_or("")
            );
            ctl.transition(State::StoppedNeedsAttention, &msg, None)
                .await?;
            emit(
                "state",
                json!({ "state": "StoppedNeedsAttention", "reason": msg }),
            );
            return Ok(DevExit::NeedsAttention(msg));
        }
        let backoff = policy.backoff_ms.get(used as usize).copied().unwrap_or(0);
        let rec = incident(
            c,
            "process_exit",
            &signature,
            json!({ "exit": desc, "uptime_ms": uptime, "restarts_in_window": used }),
            json!({ "kind": "restart", "attempt": used + 1, "backoff_ms": backoff }),
            "restart_scheduled",
        );
        ctl.record_incident(&rec, true).await?;
        pending_incident = rec["incident_id"].as_str().map(String::from);
        ctl.transition(
            State::Backoff,
            &format!(
                "unexpected exit ({desc}); restart {} of {} after {backoff} ms",
                used + 1,
                policy.max_restarts
            ),
            None,
        )
        .await?;
        emit(
            "restart",
            json!({ "state": "Backoff", "exit": desc, "attempt": used + 1, "max_restarts": policy.max_restarts, "backoff_ms": backoff, "incident_id": rec["incident_id"] }),
        );
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(u64::from(backoff))) => {}
            _ = &mut signal => {
                ctl.transition(State::Stopped, "deliberate shutdown during backoff", None).await?;
                return Ok(DevExit::Stopped);
            }
        }
    }
}

pub async fn status(p: &Project, c: Option<&Compiled>) -> Result<Json, String> {
    let ctl = Control::open(p).await?;
    let state = ctl.state().await?;
    let window = c.map(|c| c.app.recovery.window_seconds).unwrap_or(60);
    let used = ctl.restarts_in_window(window).await?;
    let endpoint = match &state {
        Some((State::Healthy, info)) => info["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
            .map(|p| {
                (
                    p,
                    info["owner_token"].as_str().unwrap_or_default().to_string(),
                )
            }),
        _ => None,
    };
    let metrics = match endpoint {
        Some((port, token)) => tokio::task::spawn_blocking(move || live_metrics(port, &token))
            .await
            .ok()
            .flatten(),
        None => None,
    };
    let (state_name, info) = match state {
        Some((s, mut info)) => {
            info.as_object_mut().map(|m| m.remove("owner_token"));
            (s.as_str(), info)
        }
        None => ("NeverStarted", Json::Null),
    };
    Ok(json!({
        "target": TARGET,
        "state": state_name,
        "detail": info,
        "restarts_in_window": used,
        "max_restarts": c.map(|c| c.app.recovery.max_restarts),
        "window_seconds": window,
        "metrics": metrics,
        "recent_transitions": ctl.transitions(10).await?,
        "recent_incidents": ctl.incidents(None, 5).await?.iter().map(|i| json!({
            "incident_id": i["incident_id"], "kind": i["kind"], "failure_signature": i["failure_signature"], "outcome": i["outcome"],
        })).collect::<Vec<_>>(),
    }))
}
