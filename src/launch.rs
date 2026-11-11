use crate::http::{self, Response};
use serde_json::{Value as Json, json};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

pub const TIMEOUT: Duration = Duration::from_secs(10);

pub struct LaunchOptions {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    pub database: PathBuf,
    pub port: u16,
    pub log: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub wrapper: Vec<String>,
}

pub struct AppProcess {
    pub child: Child,
    pub port: u16,
    pub bootstrap: String,
    pub instance: String,
}

impl AppProcess {
    pub fn start(o: &LaunchOptions) -> Result<AppProcess, String> {
        let mut cmd = match o.wrapper.split_first() {
            Some((exe, args)) => {
                let mut c = Command::new(exe);
                c.args(args).arg(&o.binary);
                c
            }
            None => Command::new(&o.binary),
        };
        if !o.wrapper.is_empty() {
            cmd.env_clear().env("PATH", "/usr/bin:/bin");
        }
        cmd.current_dir(&o.cwd)
            .env("NARI_DATABASE", &o.database)
            .env("NARI_PORT", o.port.to_string())
            .env_remove("NARI_FAULT")
            .env_remove("NARI_DATABASE_URL")
            .env_remove("NARI_OWNER_TOKEN")
            .env_remove("NARI_SUPERVISED")
            .stdin(Stdio::null())
            .stdout(Stdio::piped());
        for (k, v) in &o.env {
            cmd.env(k, v);
        }
        match &o.log {
            Some(path) => {
                let f = std::fs::File::create(path)
                    .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
                cmd.stderr(f);
            }
            None => {
                cmd.stderr(Stdio::null());
            }
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", o.binary.display()))?;
        let stdout = child.stdout.take().ok_or("no stdout pipe")?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut bootstrap = String::new();
        loop {
            match rx.recv_timeout(Duration::from_secs(30)) {
                Ok(line) => {
                    if let Some(url) = line.strip_prefix("NARI_BOOTSTRAP ") {
                        bootstrap = url
                            .rsplit("#bootstrap=")
                            .next()
                            .unwrap_or_default()
                            .to_string();
                    } else if let Some(rest) = line.strip_prefix("NARI_READY ") {
                        let ready: Json = serde_json::from_str(rest).unwrap_or(Json::Null);
                        let port = ready["port"].as_u64().unwrap_or(0) as u16;
                        let instance = ready["instance"].as_str().unwrap_or_default().to_string();
                        std::thread::spawn(move || while rx.recv().is_ok() {});
                        return Ok(AppProcess {
                            child,
                            port,
                            bootstrap,
                            instance,
                        });
                    } else if line.starts_with("NARI_STOP ") {
                        let _ = child.wait();
                        return Err(format!("app stopped during startup: {line}"));
                    }
                }
                Err(_) => {
                    let _ = child.kill();
                    let status = child.wait().ok();
                    return Err(format!(
                        "app exited or timed out before readiness (status {status:?})"
                    ));
                }
            }
        }
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for AppProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub cookie: String,
    pub csrf: String,
    pub actor: String,
}

fn origin(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

pub fn dev_sessions(port: u16, bootstrap: &str) -> Result<(Session, Session), String> {
    let o = origin(port);
    let body = json!({ "token": bootstrap }).to_string();
    let r = http::request(
        port,
        "POST",
        "/_nari/bootstrap",
        &[("Content-Type", "application/json"), ("Origin", &o)],
        body.as_bytes(),
        TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    if r.status != 200 {
        return Err(format!("bootstrap failed with {}", r.status));
    }
    let dev = r
        .cookies()
        .into_iter()
        .find(|(k, _)| k == "nari_dev")
        .map(|(_, v)| format!("nari_dev={v}"))
        .ok_or("bootstrap returned no controller cookie")?;
    let s = http::request(
        port,
        "GET",
        "/_nari/session",
        &[("Cookie", &dev)],
        b"",
        TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    let dev_csrf = s.json()["data"]["dev"]["csrf"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut out = Vec::new();
    for user in ["a", "b"] {
        let body = json!({ "user": user }).to_string();
        let r = http::request(
            port,
            "POST",
            "/_nari/dev/select",
            &[
                ("Content-Type", "application/json"),
                ("Origin", &o),
                ("Cookie", &dev),
                ("X-Nari-CSRF", &dev_csrf),
            ],
            body.as_bytes(),
            TIMEOUT,
        )
        .map_err(|e| e.to_string())?;
        let cookie = r
            .cookies()
            .into_iter()
            .find(|(k, _)| k == "nari_session")
            .map(|(_, v)| format!("nari_session={v}"))
            .ok_or("select returned no session cookie")?;
        let s = http::request(
            port,
            "GET",
            "/_nari/session",
            &[("Cookie", &cookie)],
            b"",
            TIMEOUT,
        )
        .map_err(|e| e.to_string())?;
        let data = s.json();
        out.push(Session {
            cookie,
            csrf: data["data"]["user"]["csrf"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            actor: data["data"]["user"]["id"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        });
    }
    let b = out.pop().ok_or("missing session")?;
    let a = out.pop().ok_or("missing session")?;
    Ok((a, b))
}

pub fn call(
    port: u16,
    s: Option<&Session>,
    action: &str,
    input: &Json,
    key: Option<&str>,
) -> std::io::Result<Response> {
    let o = origin(port);
    let mut headers: Vec<(&str, &str)> = vec![("Content-Type", "application/json"), ("Origin", &o)];
    if let Some(s) = s {
        headers.push(("Cookie", &s.cookie));
        headers.push(("X-Nari-CSRF", &s.csrf));
    }
    if let Some(k) = key {
        headers.push(("Idempotency-Key", k));
    }
    let body = input.to_string();
    http::request(
        port,
        "POST",
        &format!("/api/actions/{action}"),
        &headers,
        body.as_bytes(),
        TIMEOUT,
    )
}

pub fn wait_port_closed(port: u16, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if http::request(
            port,
            "GET",
            "/_nari/health",
            &[],
            b"",
            Duration::from_millis(200),
        )
        .is_err()
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}
