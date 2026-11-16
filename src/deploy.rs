use crate::ir::{self, Compiled};
use crate::supervisor::now_ms;
use crate::{Project, artifacts, codegen, pipeline};
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const ADAPTER: &str = "docker";
const RUN_IMAGE: &str = "gcr.io/distroless/cc-debian12:nonroot";
const READY_TIMEOUT: Duration = Duration::from_secs(90);

pub type Fail = (u8, &'static str, String);

pub struct PlanOptions {
    pub name: Option<String>,
    pub port: Option<u16>,
    pub database_url_env: Option<String>,
    pub production: bool,
}

pub struct Released {
    pub record: Json,
    pub bootstrap: Option<String>,
}

fn usage(msg: String) -> Fail {
    (2, "E_USAGE", msg)
}

fn io(msg: String) -> Fail {
    (3, "E_IO", msg)
}

fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.starts_with("NARI_BOOTSTRAP"))
        .collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn docker_cmd(args: &[&str]) -> Command {
    let mut cmd = Command::new("docker");
    cmd.args(args).stdin(Stdio::null());
    cmd
}

fn run(mut cmd: Command, what: &str) -> Result<String, Fail> {
    let out = cmd
        .output()
        .map_err(|e| (3, "E_DOCKER_UNAVAILABLE", format!("cannot run docker: {e}")))?;
    if !out.status.success() {
        let mut log = String::from_utf8_lossy(&out.stdout).into_owned();
        log.push_str(&String::from_utf8_lossy(&out.stderr));
        return Err((
            3,
            "E_DOCKER",
            format!("docker {what} failed:\n{}", tail(&log, 30)),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn docker(args: &[&str]) -> Result<String, Fail> {
    run(docker_cmd(args), args.first().copied().unwrap_or_default())
}

fn exists(args: &[&str]) -> bool {
    docker_cmd(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn valid_name(n: &str) -> bool {
    (1..=63).contains(&n.len())
        && n.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && n.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c))
}

fn valid_env_name(n: &str) -> bool {
    !n.is_empty()
        && !n.starts_with(|c: char| c.is_ascii_digit())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

pub fn civil_date(days: i64) -> String {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

fn connectivity() -> Result<Json, Fail> {
    let server = docker(&["version", "--format", "{{json .Server}}"]).map_err(|(_, _, m)| {
        (
            3,
            "E_DOCKER_UNAVAILABLE",
            format!("no reachable Docker engine: {m}"),
        )
    })?;
    let server: Json = serde_json::from_str(&server).unwrap_or(Json::Null);
    let endpoint = match std::env::var("DOCKER_HOST") {
        Ok(h) if !h.is_empty() => h,
        _ => docker(&[
            "context",
            "inspect",
            "--format",
            "{{.Endpoints.docker.Host}}",
        ])?,
    };
    Ok(json!({
        "endpoint": endpoint,
        "server_version": server["Version"],
        "os": server["Os"],
        "arch": server["Arch"],
    }))
}

fn container(name: &str) -> Option<Json> {
    let out = docker(&["container", "inspect", "--format", "{{json .}}", name]).ok()?;
    let c: Json = serde_json::from_str(&out).ok()?;
    Some(json!({ "id": c["Id"], "image": c["Image"] }))
}

fn observe(name: &str, volume: Option<&str>) -> Json {
    json!({
        "container": container(name),
        "previous_container": exists(&["container", "inspect", &format!("{name}-prev")]),
        "volume": volume.map(|v| exists(&["volume", "inspect", v])),
    })
}

fn labels(image: &str) -> Json {
    docker(&[
        "image",
        "inspect",
        "--format",
        "{{json .Config.Labels}}",
        image,
    ])
    .ok()
    .and_then(|l| serde_json::from_str(&l).ok())
    .unwrap_or(Json::Null)
}

fn toolchain_channel() -> String {
    pipeline::TOOLCHAIN_PIN
        .lines()
        .find_map(|l| l.trim().strip_prefix("channel = "))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_default()
}

fn build_image(p: &Project, c: &Compiled, repo: &str) -> Result<Json, Fail> {
    let out = artifacts::prepare(p).map_err(io)?;
    let package = pipeline::package_name(p, &c.app.name);
    pipeline::generate(&out, c, &package).map_err(io)?;
    let deploy = out.join("deploy");
    artifacts::ensure_real_dir(&deploy).map_err(io)?;
    let channel = toolchain_channel();
    let build_image = format!("rust:{channel}-slim-bookworm");
    let dockerfile = format!(
        "FROM {build_image} AS build\n\
         ENV RUSTUP_TOOLCHAIN={channel} CARGO_TARGET_DIR=/target\n\
         WORKDIR /src\n\
         COPY . .\n\
         RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/target \
         cargo build --release --manifest-path app/Cargo.toml \
         && install -D /target/release/{package} /out/app && install -d /out/data\n\
         FROM {RUN_IMAGE}\n\
         COPY --from=build /out/app /app\n\
         COPY --from=build --chown=65532:65532 /out/data /data\n\
         ENV NARI_BIND=0.0.0.0 NARI_DATABASE=/data/app.db\n\
         ENTRYPOINT [\"/app\"]\n"
    );
    let file = artifacts::write_file(&deploy, "Dockerfile", dockerfile).map_err(io)?;
    let iid = artifacts::safe_join(&deploy, "image.id").map_err(io)?;
    let _ = std::fs::remove_file(&iid);
    let (_, fingerprint) = codegen::schema(&c.app);
    let shape = serde_json::to_string(&codegen::schema_shape(&c.app)).unwrap_or_default();
    let tag = format!("{repo}:{}", &c.source_sha256[..12]);
    let context = out.join("generated");
    let label = |k: &str, v: &str| format!("nari.{k}={v}");
    let args = [
        "build".to_string(),
        "--iidfile".into(),
        iid.to_string_lossy().into_owned(),
        "-f".into(),
        file.to_string_lossy().into_owned(),
        "-t".into(),
        tag.clone(),
        "--label".into(),
        label("app", &c.app.name),
        "--label".into(),
        label("source_sha256", &c.source_sha256),
        "--label".into(),
        label("semantic_sha256", &c.semantic_sha256),
        "--label".into(),
        label("schema_fingerprint", &fingerprint),
        "--label".into(),
        label("schema_shape", &shape),
        "--label".into(),
        label("compiler", ir::COMPILER_VERSION),
        context.to_string_lossy().into_owned(),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    run(docker_cmd(&args), "build")?;
    let digest = std::fs::read_to_string(&iid)
        .map_err(|e| io(format!("docker build wrote no image id: {e}")))?
        .trim()
        .to_string();
    Ok(json!({
        "image": tag,
        "digest": digest,
        "build_image": build_image,
        "run_image": RUN_IMAGE,
        "compiler": format!("nari {}", ir::COMPILER_VERSION),
        "dependency_lock_sha256": pipeline::lock_hash(),
        "toolchain_sha256": pipeline::toolchain_hash(),
    }))
}

fn schema_transition(c: &Compiled, deployed: &Json, postgres: bool) -> Result<Json, Fail> {
    let (_, fingerprint) = codegen::schema(&c.app);
    let from = deployed["nari.schema_fingerprint"].as_str();
    let automatic: Vec<String> = match (from, deployed["nari.schema_shape"].as_str()) {
        (Some(fp), _) if fp == fingerprint => Vec::new(),
        (Some(_), Some(shape)) => {
            let old: Vec<imanari_runtime::EntityShape> =
                serde_json::from_str(shape).map_err(|e| {
                    (
                        4,
                        "E_MIGRATION_BLOCKED",
                        format!("deployed schema shape is unreadable: {e}"),
                    )
                })?;
            let dialect = if postgres {
                imanari_runtime::Dialect::Postgres
            } else {
                imanari_runtime::Dialect::Sqlite
            };
            imanari_runtime::migration_plan(&old, &codegen::schema_shape(&c.app), dialect)
                .map_err(|blocked| (4, "E_MIGRATION_BLOCKED", blocked.join("; ")))?
                .into_iter()
                .map(|s| s.summary)
                .collect()
        }
        _ => Vec::new(),
    };
    Ok(json!({ "from": from, "to": fingerprint, "automatic": automatic }))
}

fn plans_dir(p: &Project) -> Result<PathBuf, Fail> {
    let deploy = artifacts::prepare(p).map_err(io)?.join("deploy");
    artifacts::ensure_real_dir(&deploy).map_err(io)?;
    let plans = deploy.join("plans");
    artifacts::ensure_real_dir(&plans).map_err(io)?;
    Ok(plans)
}

pub fn plan(p: &Project, c: &Compiled, o: &PlanOptions) -> Result<Json, Fail> {
    if o.production {
        return Err((
            4,
            "E_DEV_IDENTITY",
            format!(
                "production plans need a production identity adapter, and {} declares identity mode {}; plan a preview instead, which publishes on host loopback only",
                c.file, c.app.identity.mode
            ),
        ));
    }
    let name = o
        .name
        .clone()
        .unwrap_or_else(|| format!("nari-{}", c.app.name.to_lowercase().replace('_', "-")));
    if !valid_name(&name) {
        return Err(usage(format!(
            "invalid deployment name '{name}': use 1..63 lowercase letters, digits, '_', '.' or '-'"
        )));
    }
    if let Some(v) = &o.database_url_env
        && !valid_env_name(v)
    {
        return Err(usage(format!("invalid environment variable name '{v}'")));
    }
    let port = o.port.unwrap_or(c.app.runtime.port);
    if port < 1024 {
        return Err(usage(format!("port {port} is outside 1024..65535")));
    }
    let target = connectivity()?;
    let volume = o.database_url_env.is_none().then(|| format!("{name}-data"));
    let observed = observe(&name, volume.as_deref());
    if observed["previous_container"] == true {
        return Err((
            4,
            "E_PREVIOUS_RELEASE_PENDING",
            format!(
                "container {name}-prev from an unfinished release exists; inspect it, then remove or restore it before planning"
            ),
        ));
    }
    let deployed = observed["container"]["image"]
        .as_str()
        .map(labels)
        .unwrap_or(Json::Null);
    let schema = schema_transition(c, &deployed, o.database_url_env.is_some())?;
    let repo = format!("nari-{}", c.app.name.to_lowercase().replace('_', "-"));
    let artifact = build_image(p, c, &repo)?;
    let mut resources = vec![json!({
        "kind": "image",
        "ref": artifact["image"],
        "digest": artifact["digest"],
        "action": "use",
    })];
    if let Some(v) = &volume {
        resources.push(json!({
            "kind": "volume",
            "name": v,
            "mount": "/data",
            "action": if observed["volume"] == true { "keep" } else { "create" },
        }));
    }
    let secrets = match &o.database_url_env {
        Some(var) => json!({ "NARI_DATABASE_URL": format!("env:{var}") }),
        None => json!({}),
    };
    resources.push(json!({
        "kind": "container",
        "name": name,
        "action": if observed["container"].is_null() { "create" } else { "replace" },
        "image": artifact["digest"],
        "publish": format!("127.0.0.1:{port}:{port}/tcp"),
        "port": port,
        "restart": format!("on-failure:{}", c.app.recovery.max_restarts),
        "env": { "NARI_PORT": port.to_string() },
        "secrets": secrets,
    }));
    let database = match &o.database_url_env {
        Some(var) => json!({
            "engine": "postgresql",
            "url": format!("env:{var}"),
            "tls": "rustls with webpki roots; the URL's sslmode applies",
        }),
        None => json!({ "engine": "sqlite", "volume": volume }),
    };
    let migrates = schema["automatic"]
        .as_array()
        .is_some_and(|a| !a.is_empty());
    let plan = json!({
        "schema": "nari-plan/0.1",
        "adapter": ADAPTER,
        "environment": "preview",
        "app": {
            "name": c.app.name,
            "source_file": c.file,
            "source_sha256": c.source_sha256,
            "semantic_sha256": c.semantic_sha256,
            "policy_sha256": pipeline::policy_hash(c),
            "schema_fingerprint": schema["to"],
        },
        "authentication": { "mode": c.app.identity.mode, "exposure": "host loopback only" },
        "connectivity": target,
        "database": database,
        "artifact": artifact,
        "resources": resources,
        "schema_transition": schema,
        "observed": observed,
        "readiness": {
            "signal": "NARI_READY with the planned source_sha256 in the container log",
            "timeout_seconds": READY_TIMEOUT.as_secs(),
        },
        "rollback": if migrates {
            "the previous container stays stopped as <name>-prev, because this release migrates the schema"
        } else {
            "the previous container is restarted if the new one does not become ready"
        },
        "estimated_cost": {
            "monthly_usd": 0,
            "basis": "runs on an existing Docker engine and creates no metered cloud resources; host, storage and database costs are not included",
            "as_of": civil_date(now_ms().div_euclid(86_400_000)),
        },
        "created_ms": now_ms(),
    });
    let text = ir::canonical(&plan) + "\n";
    let hash = ir::sha256_hex(text.as_bytes());
    let path = artifacts::write_file(&plans_dir(p)?, &format!("{hash}.json"), &text).map_err(io)?;
    Ok(json!({ "plan_sha256": hash, "plan_file": path, "plan": plan }))
}

struct Ready {
    evidence: Json,
    bootstrap: Option<String>,
}

fn wait_ready(name: &str, expected: Option<&str>, since: Option<i64>) -> Result<Ready, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    let since = since.map(|s| s.to_string());
    loop {
        let mut args = vec!["logs"];
        if let Some(s) = &since {
            args.extend(["--since", s]);
        }
        args.push(name);
        let out = docker_cmd(&args).output().map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut bootstrap = None;
        for line in stdout.lines() {
            if let Some(url) = line.strip_prefix("NARI_BOOTSTRAP ") {
                bootstrap = Some(url.to_string());
            } else if let Some(rest) = line.strip_prefix("NARI_STOP ") {
                return Err(format!("app stopped during startup: {rest}"));
            } else if let Some(rest) = line.strip_prefix("NARI_READY ") {
                let ready: Json = serde_json::from_str(rest).unwrap_or(Json::Null);
                if expected.is_none_or(|e| ready["source_sha256"] == e) {
                    return Ok(Ready {
                        evidence: ready,
                        bootstrap,
                    });
                }
            }
        }
        let state = docker(&[
            "container",
            "inspect",
            "--format",
            "{{.State.Status}}",
            name,
        ])
        .unwrap_or_default();
        if state == "exited" || state == "dead" {
            return Err(format!("container {state} before becoming ready"));
        }
        if Instant::now() > deadline {
            return Err(format!("not ready within {} s", READY_TIMEOUT.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn container_logs(name: &str) -> String {
    docker_cmd(&["logs", name])
        .output()
        .map(|o| {
            let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
            s.push_str(&String::from_utf8_lossy(&o.stderr));
            tail(&s, 20)
        })
        .unwrap_or_default()
}

pub fn release(p: &Project, c: &Compiled, hash: &str) -> Result<Released, Fail> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(usage(format!(
            "'{hash}' is not a plan hash (64 hex characters)"
        )));
    }
    let path = plans_dir(p)?.join(format!("{hash}.json"));
    let text = std::fs::read(&path).map_err(|_| {
        (
            4,
            "E_PLAN_NOT_FOUND",
            format!("no plan {hash} in {}", path.display()),
        )
    })?;
    if ir::sha256_hex(&text) != hash {
        return Err((
            4,
            "E_PLAN_TAMPERED",
            format!("{} no longer matches its hash", path.display()),
        ));
    }
    let plan: Json = serde_json::from_slice(&text)
        .map_err(|e| (4, "E_PLAN_TAMPERED", format!("unreadable plan: {e}")))?;
    if plan["app"]["source_sha256"] != c.source_sha256.as_str() {
        return Err((
            4,
            "E_STALE_PLAN",
            "the source changed since this plan was made; run nari plan again".into(),
        ));
    }
    let target = connectivity()?;
    if target["endpoint"] != plan["connectivity"]["endpoint"] {
        return Err((
            4,
            "E_DRIFT",
            format!(
                "the Docker endpoint is {} but the plan targets {}",
                target["endpoint"], plan["connectivity"]["endpoint"]
            ),
        ));
    }
    let resources = plan["resources"].as_array().cloned().unwrap_or_default();
    let find = |kind: &str| resources.iter().find(|r| r["kind"] == kind).cloned();
    let spec =
        find("container").ok_or_else(|| (4, "E_PLAN_TAMPERED", "plan has no container".into()))?;
    let volume = find("volume");
    let name = spec["name"].as_str().unwrap_or_default().to_string();
    let image = spec["image"].as_str().unwrap_or_default().to_string();
    if !valid_name(&name) {
        return Err((
            4,
            "E_PLAN_TAMPERED",
            format!("invalid container name '{name}'"),
        ));
    }
    let present = docker(&["image", "inspect", "--format", "{{.Id}}", &image]).unwrap_or_default();
    if present != image {
        return Err((
            4,
            "E_ARTIFACT_MISSING",
            format!("image {image} is not on the Docker engine; run nari plan again"),
        ));
    }
    let observed = observe(&name, volume.as_ref().and_then(|v| v["name"].as_str()));
    if observed != plan["observed"] {
        return Err((
            4,
            "E_DRIFT",
            format!(
                "the target changed since the plan: planned {} but found {}",
                ir::canonical_compact(&plan["observed"]),
                ir::canonical_compact(&observed)
            ),
        ));
    }
    let mut secrets = Vec::new();
    for (key, reference) in spec["secrets"].as_object().into_iter().flatten() {
        let var = reference
            .as_str()
            .and_then(|r| r.strip_prefix("env:"))
            .unwrap_or_default();
        match std::env::var(var) {
            Ok(v) if !v.is_empty() => secrets.push((key.clone(), v)),
            _ => {
                return Err((
                    3,
                    "E_SECRET_MISSING",
                    format!("secret {key} references env:{var}, which is not set"),
                ));
            }
        }
    }
    let release_id = crate::random_hex(8);
    let started_ms = now_ms();
    if let Some(v) = &volume
        && v["action"] == "create"
    {
        docker(&[
            "volume",
            "create",
            "--label",
            &format!(
                "nari.app={}",
                plan["app"]["name"].as_str().unwrap_or_default()
            ),
            v["name"].as_str().unwrap_or_default(),
        ])?;
    }
    let previous = observed["container"].clone();
    let prev_name = format!("{name}-prev");
    if !previous.is_null() {
        docker(&["stop", "-t", "15", &name])?;
        docker(&["rename", &name, &prev_name])?;
    }
    let port = spec["port"].as_u64().unwrap_or_default();
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--name",
        &name,
        "--restart",
        spec["restart"].as_str().unwrap_or("no"),
        "-p",
        spec["publish"].as_str().unwrap_or_default(),
        "-e",
        &format!("NARI_PORT={port}"),
        "--label",
        &format!("nari.plan={hash}"),
        "--label",
        &format!("nari.release={release_id}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(v) = &volume {
        args.extend([
            "-v".into(),
            format!("{}:/data", v["name"].as_str().unwrap_or_default()),
        ]);
    }
    for (key, _) in &secrets {
        args.extend(["-e".into(), key.clone()]);
    }
    args.push(image.clone());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut cmd = docker_cmd(&argv);
    for (key, value) in &secrets {
        cmd.env(key, value);
    }
    let launched = run(cmd, "run");
    let expected = plan["app"]["source_sha256"].as_str();
    let ready = match &launched {
        Ok(_) => wait_ready(&name, expected, None),
        Err((_, _, m)) => Err(m.clone()),
    };
    let container_id = launched.unwrap_or_default();
    let (outcome, reason, evidence, bootstrap, log_tail) = match ready {
        Ok(r) => {
            if !previous.is_null() {
                let _ = docker(&["rm", &prev_name]);
            }
            ("released", None, r.evidence, r.bootstrap, String::new())
        }
        Err(reason) => {
            let logs = container_logs(&name);
            let _ = docker(&["rm", "-f", &name]);
            let migrates = plan["schema_transition"]["automatic"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            if previous.is_null() {
                ("failed", Some(reason), Json::Null, None, logs)
            } else if migrates {
                (
                    "stopped_needs_attention",
                    Some(reason),
                    Json::Null,
                    None,
                    logs,
                )
            } else {
                let since = now_ms() / 1000 - 1;
                let restored = docker(&["rename", &prev_name, &name])
                    .and_then(|_| docker(&["start", &name]))
                    .map_err(|(_, _, m)| m)
                    .and_then(|_| wait_ready(&name, None, Some(since)));
                match restored {
                    Ok(r) => ("rolled_back", Some(reason), r.evidence, r.bootstrap, logs),
                    Err(e) => (
                        "rollback_failed",
                        Some(format!("{reason}; rollback: {e}")),
                        Json::Null,
                        None,
                        logs,
                    ),
                }
            }
        }
    };
    let record = json!({
        "schema": "nari-release/0.1",
        "release_id": release_id,
        "plan_sha256": hash,
        "adapter": ADAPTER,
        "name": name,
        "image": image,
        "container_id": container_id,
        "previous": previous,
        "outcome": outcome,
        "reason": reason,
        "readiness": evidence,
        "log_tail": log_tail,
        "started_ms": started_ms,
        "finished_ms": now_ms(),
    });
    let releases = plans_dir(p)?
        .parent()
        .map(|d| d.join("releases"))
        .ok_or_else(|| io("no deploy directory".into()))?;
    artifacts::ensure_real_dir(&releases).map_err(io)?;
    let record_path = artifacts::write_file(
        &releases,
        &format!("{release_id}.json"),
        ir::canonical(&record) + "\n",
    )
    .map_err(io)?;
    if outcome == "released" {
        Ok(Released { record, bootstrap })
    } else {
        Err((
            3,
            "E_RELEASE_FAILED",
            format!(
                "release {release_id} ended {outcome}: {}; record {}",
                record["reason"].as_str().unwrap_or_default(),
                record_path.display()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_and_names() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(11_016), "2000-02-29");
        assert_eq!(civil_date(20_773), "2026-11-16");
        assert!(valid_name("nari-fieldwork"));
        assert!(!valid_name("Nari"));
        assert!(!valid_name("-x"));
        assert!(!valid_name(&"a".repeat(64)));
        assert!(valid_env_name("FIELDWORK_DATABASE_URL"));
        assert!(!valid_env_name("1X"));
        assert!(!valid_env_name("A-B"));
    }
}
