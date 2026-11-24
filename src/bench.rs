use crate::ir::{self, Compiled, Type};
use crate::launch::{self, AppProcess, LaunchOptions, Session};
use crate::{Project, artifacts, codegen, context, pipeline, progress, testrun};
use serde_json::{Value as Json, json};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

pub struct Options {
    pub quick: bool,
    pub out: PathBuf,
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(if o.stdout.is_empty() {
                &o.stderr
            } else {
                &o.stdout
            })
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
        })
        .unwrap_or_else(|| "unavailable".into())
}

pub fn stats(samples: &[f64]) -> Json {
    if samples.is_empty() {
        return json!({ "n": 0 });
    }
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let pct = |p: f64| s[((p * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1];
    json!({
        "n": s.len(),
        "median": pct(0.5),
        "p95": pct(0.95),
        "mean": s.iter().sum::<f64>() / s.len() as f64,
        "min": s[0],
        "max": s[s.len() - 1],
    })
}

fn machine() -> Json {
    let lock = pipeline::WORKSPACE_LOCK;
    let version = |name: &str| {
        lock.split("[[package]]")
            .find(|b| b.contains(&format!("name = \"{name}\"\n")))
            .and_then(|b| b.lines().find(|l| l.starts_with("version = ")))
            .map(|l| {
                l.trim_start_matches("version = ")
                    .trim_matches('"')
                    .to_string()
            })
            .unwrap_or_default()
    };
    json!({
        "cpu": sh("sysctl", &["-n", "machdep.cpu.brand_string"]),
        "cores": sh("sysctl", &["-n", "hw.ncpu"]),
        "memory_bytes": sh("sysctl", &["-n", "hw.memsize"]),
        "os": format!("{} {}", sh("sw_vers", &["-productName"]), sh("sw_vers", &["-productVersion"])),
        "rustc": sh("rustc", &["-V"]),
        "cxx": sh("c++", &["--version"]),
        "nari_cli_profile": if cfg!(debug_assertions) { "debug (compiler/context timings are not representative; use cargo build --release)" } else { "release" },
        "generated_app_profile": "release, opt-level 3, codegen-units 16",
        "dependencies": { "axum": version("axum"), "tokio": version("tokio"), "sqlx": version("sqlx"), "hyper": version("hyper"), "serde_json": version("serde_json"), "libsqlite3-sys": version("libsqlite3-sys") },
    })
}

pub fn big_source(n: usize) -> String {
    let mut s = String::from(
        "imanari \"0.1\";\n\napp bench {\n  targets = [web];\n\n  identity development {\n    mode = local_dev;\n  }\n\n  entity Task {\n    owner_field = owner;\n\n    field title {\n      type = Text;\n      min_length = 1;\n      max_length = 200;\n    }\n\n    field owner {\n      type = UUID;\n    }\n\n    field completed {\n      type = Bool;\n      default = false;\n    }\n  }\n",
    );
    for i in 0..n {
        s.push_str(&match i % 3 {
            0 => format!("\n  action a{i} {{\n    kind = insert;\n    entity = Task;\n    access = signed_in;\n\n    input title {{\n      type = Text;\n      min_length = 1;\n      max_length = 200;\n    }}\n\n    set = {{\n      title: @input.title,\n      owner: @actor.id,\n    }};\n  }}\n"),
            1 => format!("\n  action a{i} {{\n    kind = list;\n    entity = Task;\n    access = signed_in;\n  }}\n"),
            _ => format!("\n  action a{i} {{\n    kind = update;\n    entity = Task;\n    access = signed_in;\n\n    input id {{\n      type = UUID;\n    }}\n\n    input revision {{\n      type = Int;\n    }}\n\n    id = @input.id;\n    expected_revision = @input.revision;\n    set = {{ completed: true }};\n  }}\n"),
        });
    }
    s.push_str("\n  screen tasks {\n    route = \"/\";\n    title = \"Tasks\";\n    query = a1;\n\n    form add {\n      submit = a0;\n\n      input title {\n        label = \"Title\";\n      }\n    }\n\n    list items {\n      source = @query.items;\n      fields = [title, completed];\n\n      button done {\n        label = \"Done\";\n        action = a2;\n        arguments = {\n          id: @item.id,\n          revision: @item.revision,\n        };\n      }\n    }\n  }\n\n  runtime local {\n    bind = \"127.0.0.1\";\n    port = 7331;\n    database = \".nari/app.db\";\n    request_limit_bytes = 65536;\n    request_timeout_ms = 5000;\n  }\n\n  recovery backend_process {\n    target = backend;\n    max_restarts = 3;\n    window_seconds = 60;\n    backoff_ms = [100, 500, 2000];\n    after_exhaustion = stop;\n  }\n\n  repair local_candidate {\n    mode = isolated;\n    max_attempts = 2;\n    editable_actions = [a2];\n    automatic_release = false;\n  }\n}\n");
    s
}

fn compiler(out: &Path, runs: usize) -> Result<Json, String> {
    let n = 1000;
    let src = big_source(n);
    let dir = out.join("compiler");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("bench.nr"), &src).map_err(|e| e.to_string())?;
    let p = Project::locate(Some(dir.clone()))?;
    let mut check = Vec::new();
    let mut semantic = None;
    for i in 0..=runs {
        let t = Instant::now();
        let (c, _) = crate::compile(&src, "bench.nr")
            .map_err(|d| format!("1000-action fixture failed to compile: {}", d[0].render()))?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if c.app.actions.len() != n {
            return Err(format!(
                "correctness: expected {n} actions, got {}",
                c.app.actions.len()
            ));
        }
        if semantic.get_or_insert(c.semantic_sha256.clone()) != &c.semantic_sha256 {
            return Err("correctness: semantic hash differs between runs".into());
        }
        if i > 0 {
            check.push(ms);
        }
    }
    let (c, _) = crate::compile(&src, "bench.nr").map_err(|_| "compile failed")?;
    let target = "bench/action/a2";
    let mut ctx = Vec::new();
    let mut bytes = 0;
    for i in 0..=runs {
        let t = Instant::now();
        let b = context::extract(&p, &c, &src, target, context::DEFAULT_MAX_BYTES)?;
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if b.over_budget
            || !b.markdown.contains("action a2 {")
            || b.markdown.contains("action a5 {")
        {
            return Err("correctness: context bundle is wrong for the 1000-action fixture".into());
        }
        bytes = b.markdown.len();
        if i > 0 {
            ctx.push(ms);
        }
    }
    let check_stats = stats(&check);
    let ctx_stats = stats(&ctx);
    Ok(json!({
        "fixture": { "actions": n, "source_bytes": src.len(), "graph_nodes": c.graph.nodes.len(), "graph_edges": c.graph.edges.len() },
        "check_ms": { "stats": check_stats, "samples": check, "budget_ms": 1000.0, "pass": check_stats["median"].as_f64().unwrap_or(f64::MAX) <= 1000.0 },
        "warm_context_ms": { "stats": ctx_stats, "samples": ctx, "bytes": bytes, "budget_ms": 100.0, "pass": ctx_stats["median"].as_f64().unwrap_or(f64::MAX) <= 100.0 },
        "correctness": "every run compiled 1000 actions with identical semantic hashes; every context bundle contained the target and excluded unrelated actions",
    }))
}

struct Conn {
    stream: TcpStream,
    port: u16,
    buf: Vec<u8>,
}

impl Conn {
    fn new(port: u16) -> std::io::Result<Conn> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(Conn {
            stream,
            port,
            buf: Vec::with_capacity(16384),
        })
    }

    fn post(&mut self, action: &str, head: &str, body: &str) -> std::io::Result<u16> {
        let req = format!(
            "POST /api/actions/{action} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{head}\r\n{body}",
            self.port,
            body.len()
        );
        self.stream.write_all(req.as_bytes())?;
        self.buf.clear();
        let mut tmp = [0u8; 16384];
        loop {
            if let Some(end) = self.buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&self.buf[..end]).to_ascii_lowercase();
                let status: u16 = head
                    .split(' ')
                    .nth(1)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                while self.buf.len() < end + 4 + len {
                    let n = self.stream.read(&mut tmp)?;
                    if n == 0 {
                        return Err(std::io::ErrorKind::UnexpectedEof.into());
                    }
                    self.buf.extend_from_slice(&tmp[..n]);
                }
                return Ok(status);
            }
            let n = self.stream.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            self.buf.extend_from_slice(&tmp[..n]);
        }
    }
}

fn head(s: &Session, key: Option<&str>) -> String {
    let mut h = format!("Cookie: {}\r\nX-Nari-CSRF: {}\r\n", s.cookie, s.csrf);
    if let Some(k) = key {
        h.push_str(&format!("Idempotency-Key: {k}\r\n"));
    }
    h
}

fn rss_kb(pid: u32) -> u64 {
    sh("ps", &["-o", "rss=", "-p", &pid.to_string()])
        .trim()
        .parse()
        .unwrap_or(0)
}

struct Load {
    latencies_us: Vec<f64>,
    errors: usize,
    wall_s: f64,
}

fn drive(
    port: u16,
    sessions: &[Session],
    workers: usize,
    per_worker: usize,
    scenario: &str,
    ids: &[Vec<String>],
) -> Load {
    let barrier = Arc::new(Barrier::new(workers + 1));
    let started = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let results: Vec<(Vec<f64>, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|w| {
                let barrier = barrier.clone();
                let s = &sessions[w % sessions.len()];
                let ids = ids.get(w).cloned().unwrap_or_default();
                let tag = crate::random_hex(4);
                scope.spawn(move || {
                    let mut conn = Conn::new(port).expect("connect");
                    let mut lat = Vec::with_capacity(per_worker);
                    let mut errors = 0;
                    barrier.wait();
                    for i in 0..per_worker {
                        let (action, body, key) = match scenario {
                            "list" => ("list_tasks", "{}".to_string(), None),
                            "create" => (
                                "create_task",
                                format!("{{\"title\":\"bench {w} {i}\"}}"),
                                Some(format!("c-{tag}-{w}-{i}")),
                            ),
                            _ => (
                                "complete_task",
                                format!(
                                    "{{\"id\":\"{}\",\"revision\":1}}",
                                    ids.get(i).map(String::as_str).unwrap_or_default()
                                ),
                                Some(format!("u-{tag}-{w}-{i}")),
                            ),
                        };
                        let h = head(s, key.as_deref());
                        let t = Instant::now();
                        let status = conn.post(action, &h, &body).unwrap_or_else(|_| {
                            conn = Conn::new(port).expect("reconnect");
                            0
                        });
                        lat.push(t.elapsed().as_secs_f64() * 1e6);
                        if status != 200 {
                            errors += 1;
                        }
                    }
                    (lat, errors)
                })
            })
            .collect();
        barrier.wait();
        *started.lock().expect("lock") = Some(Instant::now());
        handles
            .into_iter()
            .map(|h| h.join().expect("worker"))
            .collect()
    });
    let wall_s = started
        .lock()
        .expect("lock")
        .map(|t| t.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    Load {
        latencies_us: results
            .iter()
            .flat_map(|(l, _)| l.iter().copied())
            .collect(),
        errors: results.iter().map(|(_, e)| e).sum(),
        wall_s,
    }
}

fn one_run(
    binary: &Path,
    dir: &Path,
    label: &str,
    workers: usize,
    per_worker: usize,
) -> Result<Json, String> {
    let db = dir.join(format!("{label}.db"));
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
    let app = AppProcess::start(&LaunchOptions {
        binary: binary.to_path_buf(),
        cwd: dir.to_path_buf(),
        database: db,
        port: 0,
        log: Some(dir.join(format!("{label}.log"))),
        env: vec![("NARI_MUTATIONS_PER_MINUTE".into(), "1000000".into())],
        wrapper: Vec::new(),
    })?;
    let (a, b) = launch::dev_sessions(app.port, &app.bootstrap)?;
    let sessions = vec![a, b];
    for s in &sessions {
        for i in 0..50 {
            launch::call(
                app.port,
                Some(s),
                "create_task",
                &json!({ "title": format!("seed {i}") }),
                Some(&format!("seed-{i}")),
            )
            .map_err(|e| e.to_string())?;
        }
    }
    let pid = app.child.id();
    let peak = Arc::new(std::sync::atomic::AtomicU64::new(rss_kb(pid)));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sampler = {
        let (peak, stop) = (peak.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                peak.fetch_max(rss_kb(pid), std::sync::atomic::Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let mut scenarios = serde_json::Map::new();
    for scenario in ["list", "create", "complete"] {
        let mut ids: Vec<Vec<String>> = Vec::new();
        if scenario == "complete" {
            for w in 0..workers {
                let s = &sessions[w % sessions.len()];
                let mut v = Vec::new();
                for i in 0..per_worker {
                    let key = format!("pre-{label}-{w}-{i}");
                    let body = format!("{{\"title\":\"pre {w} {i}\"}}");
                    let r = launch::call(
                        app.port,
                        Some(s),
                        "create_task",
                        &serde_json::from_str(&body).unwrap_or(Json::Null),
                        Some(&key),
                    )
                    .map_err(|e| e.to_string())?;
                    v.push(
                        r.json()["data"]["id"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                }
                ids.push(v);
            }
        }
        let load = drive(app.port, &sessions, workers, per_worker, scenario, &ids);
        let total = load.latencies_us.len();
        scenarios.insert(
            scenario.into(),
            json!({
                "stats_us": stats(&load.latencies_us),
                "throughput_rps": total as f64 / load.wall_s.max(1e-9),
                "error_rate": load.errors as f64 / total.max(1) as f64,
                "samples_us": load.latencies_us,
            }),
        );
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = sampler.join();
    drop(app);
    Ok(
        json!({ "peak_rss_kb": peak.load(std::sync::atomic::Ordering::Relaxed), "scenarios": scenarios }),
    )
}

const BASELINE_SOURCE: &str = include_str!("../bench/baseline/src/main.rs");
const SERDE_DEP: &str = "serde = { version = \"1\", features = [\"derive\"] }\n";

fn standalone(out: &Path, name: &str, main_rs: &str, extra_deps: &str) -> Result<PathBuf, String> {
    let crates = out.join("crates");
    fs::create_dir_all(&crates).map_err(|e| e.to_string())?;
    artifacts::write_file(
        &crates,
        "imanari-runtime/Cargo.toml",
        codegen::RUNTIME_MANIFEST,
    )?;
    artifacts::write_file(
        &crates,
        "imanari-runtime/src/lib.rs",
        codegen::RUNTIME_SOURCE,
    )?;
    artifacts::write_file(&crates, &format!("{name}/src/main.rs"), main_rs)?;
    artifacts::write_file(
        &crates,
        &format!("{name}/Cargo.toml"),
        format!(
            "[package]\nname = {name:?}\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\nimanari-runtime = {{ path = \"../imanari-runtime\" }}\n{extra_deps}\n[workspace]\n"
        ),
    )?;
    artifacts::write_file(
        &crates,
        &format!("{name}/Cargo.lock"),
        pipeline::WORKSPACE_LOCK,
    )?;
    artifacts::write_file(
        &crates,
        &format!("{name}/rust-toolchain.toml"),
        pipeline::TOOLCHAIN_PIN,
    )?;
    Ok(crates.join(name))
}

fn cargo_bin(p: &Project, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let out = Command::new(pipeline::cargo())
        .args([
            "build",
            "--release",
            "--message-format=json",
            "--manifest-path",
        ])
        .arg(dir.join("Cargo.toml"))
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", pipeline::target_dir(p))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str::<Json>(l).ok())
        .find_map(|m| {
            m["executable"]
                .as_str()
                .filter(|_| m["target"]["name"] == name)
                .map(PathBuf::from)
        })
        .ok_or_else(|| {
            format!(
                "{name} failed to build: {}",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .last()
                    .unwrap_or_default()
            )
        })
}

fn runtime(
    p: &Project,
    c: &Compiled,
    out: &Path,
    quick: bool,
    runs: usize,
) -> Result<Json, String> {
    let fixture = crate::compile(
        include_str!("../examples/fieldwork/fieldwork.nr"),
        "fieldwork.nr",
    )
    .map_err(|_| "fixture")?
    .0;
    if fixture.semantic_sha256 != c.semantic_sha256 {
        return Ok(
            json!({ "status": "not measured", "reason": "the hand-written baseline implements only the Fieldwork reference app; this project's semantics differ" }),
        );
    }
    let nari_bin = pipeline::build(p, c)
        .map_err(|(_, d)| d.iter().map(|x| x.render()).collect::<Vec<_>>().join("\n"))?
        .binary;
    let base_dir = standalone(out, "fieldwork-baseline", BASELINE_SOURCE, SERDE_DEP)?;
    let base_bin = cargo_bin(p, &base_dir, "fieldwork-baseline")?;
    let dir = out.join("runtime");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let (workers, per_worker) = if quick { (4, 100) } else { (8, 500) };
    let mut runs_out = Vec::new();
    for i in 0..=runs {
        let order: [(&str, &Path); 2] = if i % 2 == 0 {
            [("nari", &nari_bin), ("baseline", &base_bin)]
        } else {
            [("baseline", &base_bin), ("nari", &nari_bin)]
        };
        for (label, bin) in order {
            let r = one_run(bin, &dir, label, workers, per_worker)?;
            if i > 0 {
                runs_out.push(json!({ "run": i, "app": label, "result": r }));
            }
        }
    }
    let mut summary = serde_json::Map::new();
    for scenario in ["list", "create", "complete"] {
        let collect = |app: &str| -> (Vec<f64>, Vec<f64>, Vec<f64>) {
            let mut all = Vec::new();
            let mut tput = Vec::new();
            let mut err = Vec::new();
            for r in runs_out.iter().filter(|r| r["app"] == app) {
                let s = &r["result"]["scenarios"][scenario];
                all.extend(
                    s["samples_us"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Json::as_f64),
                );
                tput.push(s["throughput_rps"].as_f64().unwrap_or(0.0));
                err.push(s["error_rate"].as_f64().unwrap_or(1.0));
            }
            (all, tput, err)
        };
        let (fa, ft, fe) = collect("nari");
        let (ba, bt, be) = collect("baseline");
        let (fs_, bs) = (stats(&fa), stats(&ba));
        let overhead = fs_["p95"].as_f64().unwrap_or(0.0) / bs["p95"].as_f64().unwrap_or(1.0) - 1.0;
        summary.insert(
            scenario.into(),
            json!({
                "nari_us": fs_, "baseline_us": bs,
                "nari_throughput_rps": stats(&ft), "baseline_throughput_rps": stats(&bt),
                "nari_error_rate_max": fe.iter().cloned().fold(0.0, f64::max), "baseline_error_rate_max": be.iter().cloned().fold(0.0, f64::max),
                "p95_overhead": overhead, "budget": 0.15, "pass": overhead <= 0.15,
            }),
        );
    }
    let peaks = |app: &str| -> Vec<f64> {
        runs_out
            .iter()
            .filter(|r| r["app"] == app)
            .filter_map(|r| r["result"]["peak_rss_kb"].as_f64())
            .collect()
    };
    let (fp, bp) = (stats(&peaks("nari")), stats(&peaks("baseline")));
    let mem = fp["max"].as_f64().unwrap_or(0.0) / bp["max"].as_f64().unwrap_or(1.0) - 1.0;
    let raw = dir.join("samples.json");
    fs::write(&raw, ir::canonical_compact(&json!(runs_out))).map_err(|e| e.to_string())?;
    Ok(json!({
        "baseline": "bench/baseline: hand-written serde-typed handlers on the same imanari-runtime (axum/hyper HTTP, SQLite via sqlx, sessions, CSRF/Origin/Host checks, idempotency receipts, revision checks, rate limiting)",
        "workload": { "workers": workers, "requests_per_worker_per_scenario": per_worker, "connections": "one keep-alive connection per worker", "measured_runs": runs, "warmup_runs": 1, "order": "alternating per run", "seeded_rows_per_user": 50, "rate_limit": "NARI_MUTATIONS_PER_MINUTE=1000000 for both apps (check still executed)" },
        "scenarios": summary,
        "peak_rss_kb": { "nari": fp, "baseline": bp, "overhead": mem, "budget": 0.20, "pass": mem <= 0.20, "method": "ps rss sampled every 50 ms" },
        "raw_samples": raw.strip_prefix(&p.dir).unwrap_or(&raw),
    }))
}

fn kernel(c: &Compiled, p: &Project, out: &Path, quick: bool, runs: usize) -> Result<Json, String> {
    let dir = out.join("kernel");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let title = c
        .app
        .actions
        .iter()
        .find_map(|a| {
            a.inputs.iter().find_map(|i| match i.ty {
                Type::Text {
                    min_length,
                    max_length,
                } => Some((min_length, max_length)),
                _ => None,
            })
        })
        .unwrap_or((1, 200));
    let n = if quick { 20_000 } else { 100_000 };
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let pieces = ["a", "Z", "7", " ", "é", "漢", "😀", "-", "q"];
    let mut input = String::new();
    for _ in 0..n {
        let len = (next() % 260) as usize;
        let t: String = (0..len)
            .map(|_| pieces[(next() % pieces.len() as u64) as usize])
            .collect();
        let mut id = crate::random_hex(16);
        id = format!(
            "{}-{}-{}-{}-{}",
            &id[0..8],
            &id[8..12],
            &id[12..16],
            &id[16..20],
            &id[20..32]
        );
        match next() % 10 {
            0 => id = id.to_uppercase(),
            1 => id.truncate(35),
            _ => {}
        }
        let rev: i64 = match next() % 20 {
            0 => crate::syntax::MAX_SAFE_INT,
            1 => crate::syntax::MAX_SAFE_INT + 3,
            2 => -crate::syntax::MAX_SAFE_INT - 2,
            _ => (next() % 1_000_000) as i64,
        };
        input.push_str(&format!("{}\t{id}\t{rev}\n", t.replace(['\t', '\n'], " ")));
    }
    fs::write(dir.join("input.tsv"), &input).map_err(|e| e.to_string())?;
    let (min, max) = title;
    let cpp = format!(
        "#include <chrono>\n#include <cstdint>\n#include <cstdio>\n#include <cstdlib>\n#include <fstream>\n#include <sstream>\n#include <string>\n#include <vector>\nstruct Rec {{ std::string title, id; long long rev; }};\nstatic const long long MAX_SAFE = 9007199254740991LL;\nstatic size_t scalars(const std::string& s) {{ size_t n = 0; for (unsigned char c : s) if ((c & 0xC0) != 0x80) ++n; return n; }}\nstatic bool uuid(const std::string& s) {{ if (s.size() != 36) return false; for (size_t i = 0; i < 36; ++i) {{ char c = s[i]; if (i == 8 || i == 13 || i == 18 || i == 23) {{ if (c != '-') return false; }} else if (!((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f'))) return false; }} return true; }}\nint main(int argc, char** argv) {{\n  std::ifstream f(argv[1]); int iters = std::atoi(argv[2]); std::vector<Rec> recs; std::string line;\n  while (std::getline(f, line)) {{ size_t a = line.find('\\t'), b = line.find('\\t', a + 1); recs.push_back({{line.substr(0, a), line.substr(a + 1, b - a - 1), std::strtoll(line.c_str() + b + 1, nullptr, 10)}}); }}\n  unsigned long long ok = 0, bt = 0, bi = 0, br = 0, ov = 0, sum = 0;\n  auto t0 = std::chrono::steady_clock::now();\n  for (int it = 0; it < iters; ++it) {{\n    ok = bt = bi = br = ov = sum = 0;\n    for (const Rec& r : recs) {{\n      size_t n = scalars(r.title);\n      if (n < {min} || n > {max}) {{ ++bt; continue; }}\n      if (!uuid(r.id)) {{ ++bi; continue; }}\n      if (r.rev < -MAX_SAFE || r.rev > MAX_SAFE) {{ ++br; continue; }}\n      if (r.rev >= MAX_SAFE) {{ ++ov; continue; }}\n      ++ok; sum += (unsigned long long)(r.rev + 1) * 31ULL + (unsigned long long)r.title.size();\n    }}\n  }}\n  long long ns = std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::steady_clock::now() - t0).count();\n  std::printf(\"{{\\\"ok\\\":%llu,\\\"bad_title\\\":%llu,\\\"bad_id\\\":%llu,\\\"bad_revision\\\":%llu,\\\"overflow\\\":%llu,\\\"checksum\\\":%llu,\\\"ns\\\":%lld}}\\n\", ok, bt, bi, br, ov, sum, ns);\n}}\n"
    );
    fs::write(dir.join("kernel.cpp"), cpp).map_err(|e| e.to_string())?;
    let rust = format!(
        "#![forbid(unsafe_code)]\nuse imanari_runtime as rt;\nfn main() {{\n    let args: Vec<String> = std::env::args().collect();\n    let text = std::fs::read_to_string(&args[1]).unwrap();\n    let iters: usize = args[2].parse().unwrap();\n    let recs: Vec<(String, String, i64)> = text.lines().map(|l| {{ let mut p = l.splitn(3, '\\t'); (p.next().unwrap().to_string(), p.next().unwrap().to_string(), p.next().unwrap().parse().unwrap()) }}).collect();\n    let (mut ok, mut bt, mut bi, mut br, mut ov, mut sum) = (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);\n    let t0 = std::time::Instant::now();\n    for _ in 0..iters {{\n        ok = 0; bt = 0; bi = 0; br = 0; ov = 0; sum = 0;\n        for (title, id, rev) in &recs {{\n            if rt::check_text({}, title, {min}, {max}).is_err() {{ bt += 1; continue; }}\n            if !rt::is_canonical_uuid(id) {{ bi += 1; continue; }}\n            if rev.abs() > rt::MAX_SAFE_INT {{ br += 1; continue; }}\n            if *rev >= rt::MAX_SAFE_INT {{ ov += 1; continue; }}\n            ok += 1;\n            sum = sum.wrapping_add(((rev + 1) as u64).wrapping_mul(31).wrapping_add(title.len() as u64));\n        }}\n    }}\n    let ns = t0.elapsed().as_nanos();\n    println!(\"{{{{\\\"ok\\\":{{ok}},\\\"bad_title\\\":{{bt}},\\\"bad_id\\\":{{bi}},\\\"bad_revision\\\":{{br}},\\\"overflow\\\":{{ov}},\\\"checksum\\\":{{sum}},\\\"ns\\\":{{ns}}}}}}\");\n}}\n",
        "\"title\""
    );
    let kernel_dir = standalone(out, "nari-kernel", &rust, "")?;
    let cxx = Command::new("c++")
        .args(["-O3", "-std=c++17", "-o"])
        .arg(dir.join("kernel-cpp"))
        .arg(dir.join("kernel.cpp"))
        .output();
    let cxx_ok = cxx.as_ref().is_ok_and(|o| o.status.success());
    let rust_bin = cargo_bin(p, &kernel_dir, "nari-kernel")
        .map_err(|e| format!("generated Rust kernel failed to build: {e}"))?;
    let iters = if quick { "3" } else { "10" };
    let exec = |bin: &Path| -> Result<Json, String> {
        let o = Command::new(bin)
            .arg(dir.join("input.tsv"))
            .arg(iters)
            .output()
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&o.stdout)
            .map_err(|_| format!("{} produced no result", bin.display()))
    };
    let mut rust_ns = Vec::new();
    let mut cpp_ns = Vec::new();
    let mut outputs_equal = true;
    let mut sample_output = Json::Null;
    for i in 0..=runs {
        let r = exec(&rust_bin)?;
        let strip = |mut v: Json| {
            v.as_object_mut().map(|m| m.remove("ns"));
            v
        };
        if cxx_ok {
            let c = exec(&dir.join("kernel-cpp"))?;
            if strip(c.clone()) != strip(r.clone()) {
                outputs_equal = false;
            }
            if i > 0 {
                cpp_ns.push(c["ns"].as_f64().unwrap_or(0.0));
            }
        }
        sample_output = strip(r.clone());
        if i > 0 {
            rust_ns.push(r["ns"].as_f64().unwrap_or(0.0));
        }
    }
    let (rs_s, cpp_s) = (stats(&rust_ns), stats(&cpp_ns));
    let ratio =
        rs_s["median"].as_f64().unwrap_or(0.0) / cpp_s["median"].as_f64().unwrap_or(f64::MAX) - 1.0;
    Ok(json!({
        "workload": format!("{n} records x {iters} iterations: Text[{min}..{max}] scalar-count validation (bounds from IR), canonical UUID check, safe-integer range and checked revision increment, wrapping checksum"),
        "generated_rust": "kernel emitted from IR bounds calling the same imanari-runtime validators the handlers use (check_text allocates an error on failure)",
        "cxx_flags": "c++ -O3 -std=c++17",
        "outputs": sample_output,
        "outputs_equal": if cxx_ok { json!(outputs_equal) } else { json!("not measured") },
        "rust_ns": { "stats": rs_s, "samples": rust_ns },
        "cpp_ns": if cxx_ok { json!({ "stats": cpp_s, "samples": cpp_ns }) } else { json!("not measured: no C++ toolchain") },
        "rust_vs_cpp": if cxx_ok { json!(ratio) } else { json!(null) },
        "budget": 0.20,
        "pass": if cxx_ok { json!(outputs_equal && ratio <= 0.20) } else { json!("not measured") },
        "nonclaim": "a single bounded kernel; this does not establish whole-stack C++ parity",
    }))
}

fn agent_iteration(p: &Project, c: &Compiled, out: &Path) -> Result<Json, String> {
    let fixture = crate::compile(
        include_str!("../examples/fieldwork/fieldwork.nr"),
        "fieldwork.nr",
    )
    .map_err(|_| "fixture")?
    .0;
    if fixture.semantic_sha256 != c.semantic_sha256 {
        return Ok(
            json!({ "status": "not measured", "reason": "scripted change is defined for the Fieldwork reference app" }),
        );
    }
    let dir = out.join("agent");
    let fdir = dir.join("nari");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(fdir.join("acceptance")).map_err(|e| e.to_string())?;
    let src = fs::read_to_string(&p.file).map_err(|e| e.to_string())?;
    fs::write(fdir.join(&p.file_name), &src).map_err(|e| e.to_string())?;
    for t in testrun::load_protected(&p.dir)? {
        fs::copy(p.dir.join(&t.file), fdir.join(&t.file)).map_err(|e| e.to_string())?;
    }
    let fp = Project::locate(Some(fdir.clone()))?;
    let change = (
        "      max_length = 200;\n    }\n\n    set = {",
        "      max_length = 120;\n    }\n\n    set = {",
    );
    let t0 = Instant::now();
    let bundle = context::extract(
        &fp,
        c,
        &src,
        "fieldwork/action/create_task",
        context::DEFAULT_MAX_BYTES,
    )?;
    let mut task = progress::TaskRecord::new(
        "bench-agent",
        "tighten create_task title to 120",
        &c.source_sha256,
        &c.graph_sha256,
    );
    let edited = src.replacen(change.0, change.1, 1);
    fs::write(&fp.file, &edited).map_err(|e| e.to_string())?;
    let (cand, _) =
        crate::compile(&edited, &fp.file_name).map_err(|_| "edited source failed to check")?;
    let d = crate::diff::diff(c, &cand);
    let built = pipeline::build(&fp, &cand)
        .map_err(|(_, d)| d.iter().map(|x| x.render()).collect::<Vec<_>>().join("\n"))?;
    let tests = testrun::load_protected(&fp.dir)?;
    let run = testrun::run_all(
        &cand,
        &tests,
        &testrun::RunOptions {
            binary: &built.binary,
            cwd: &fp.dir,
            run_dir: &fdir.join(".nari/test"),
            wrapper: Vec::new(),
        },
        true,
    )?;
    let nari_elapsed = t0.elapsed().as_secs_f64();
    task.completed.push("edited create_task input bound".into());
    task.base_source_sha256 = cand.source_sha256.clone();
    task.base_graph_sha256 = cand.graph_sha256.clone();
    task.add_evidence(&fp.dir, "acceptance/complete_task.json", "protected-test")?;
    task.save(&fdir.join("task.json"))?;
    let resumed = progress::TaskRecord::load(&fdir.join("task.json"))?.verify(
        &fp.dir,
        &cand.source_sha256,
        &cand.graph_sha256,
    );

    let bsrc = BASELINE_SOURCE;
    let bchange = (
        "rt::check_text(\"title\", &req.title, 1, 200)",
        "rt::check_text(\"title\", &req.title, 1, 120)",
    );
    let t1 = Instant::now();
    let bedited = bsrc.replacen(bchange.0, bchange.1, 1);
    let bdir = standalone(out, "fieldwork-baseline-edit", &bedited, SERDE_DEP)?;
    let bbin = cargo_bin(p, &bdir, "fieldwork-baseline-edit")
        .map_err(|e| format!("edited baseline failed to build: {e}"))?;
    let brun = testrun::run_all(
        c,
        &tests,
        &testrun::RunOptions {
            binary: &bbin,
            cwd: &bdir,
            run_dir: &bdir.join("test"),
            wrapper: Vec::new(),
        },
        false,
    )?;
    let base_elapsed = t1.elapsed().as_secs_f64();
    Ok(json!({
        "change": "tighten create_task title max_length 200 -> 120",
        "nari": {
            "context_bytes": bundle.markdown.len(),
            "changed_source_bytes": 3,
            "checks_run": run.results.len() + 2,
            "checks": "nari check, nari diff, generated contract checks, protected tests",
            "failures": run.results.iter().filter(|r| !r.passed).count(),
            "diff_categories": d["summary"],
            "elapsed_s": nari_elapsed,
            "fresh_session_continuation": if resumed.is_empty() { "verified: task record hashes and evidence match" } else { "drift detected" },
        },
        "baseline": {
            "context_bytes": bsrc.len(),
            "context_note": "the whole handler file; no dependency-scoped context exists for hand-written code",
            "changed_source_bytes": 3,
            "checks_run": brun.results.len() + 1,
            "checks": "cargo build, protected HTTP tests (no generated contract checks)",
            "failures": brun.results.iter().filter(|r| !r.passed).count(),
            "elapsed_s": base_elapsed,
            "fresh_session_continuation": "not available: no structured progress record",
        },
        "nonclaim": "byte counts only; no model token counts were measured, so no token savings are claimed",
    }))
}

pub fn run(p: &Project, c: &Compiled, o: &Options) -> Result<Json, String> {
    fs::create_dir_all(&o.out).map_err(|e| e.to_string())?;
    let runs = 5;
    let started = Instant::now();
    let compiler = compiler(&o.out, runs)?;
    let runtime = runtime(p, c, &o.out, o.quick, runs)?;
    let kernel = kernel(c, p, &o.out, o.quick, runs)?;
    let agent = agent_iteration(p, c, &o.out)?;
    let budget = |v: &Json| match v {
        Json::Bool(true) => "pass",
        Json::Bool(false) => "MISS",
        _ => "not measured",
    };
    let mut budgets = vec![
        json!({ "budget": "check 1000-action fixture <= 1 s median", "result": budget(&compiler["check_ms"]["pass"]), "measured": compiler["check_ms"]["stats"]["median"] }),
        json!({ "budget": "warm context extraction <= 100 ms median", "result": budget(&compiler["warm_context_ms"]["pass"]), "measured": compiler["warm_context_ms"]["stats"]["median"] }),
    ];
    for s in ["list", "create", "complete"] {
        budgets.push(json!({ "budget": format!("runtime {s}: p95 overhead <= 15% vs hand-written"), "result": budget(&runtime["scenarios"][s]["pass"]), "measured": runtime["scenarios"][s]["p95_overhead"] }));
    }
    budgets.push(json!({ "budget": "runtime peak memory overhead <= 20%", "result": budget(&runtime["peak_rss_kb"]["pass"]), "measured": runtime["peak_rss_kb"]["overhead"] }));
    budgets.push(json!({ "budget": "generated Rust kernel within 20% of optimized C++", "result": budget(&kernel["pass"]), "measured": kernel["rust_vs_cpp"] }));
    let report = json!({
        "schema": "nari-bench/0.1",
        "quick": o.quick,
        "machine": machine(),
        "budgets": budgets,
        "compiler": compiler,
        "runtime": runtime,
        "kernel": kernel,
        "agent_iteration": agent,
        "elapsed_s": started.elapsed().as_secs_f64(),
        "note": "budgets are provisional acceptance targets; misses are reported, not hidden",
    });
    let mut slim = report.clone();
    if let Some(sc) = slim["runtime"]["scenarios"].as_object_mut() {
        for v in sc.values_mut() {
            v.as_object_mut().map(|m| m.remove("samples_us"));
        }
    }
    fs::write(o.out.join("report.json"), ir::canonical(&slim) + "\n").map_err(|e| e.to_string())?;
    Ok(slim)
}
