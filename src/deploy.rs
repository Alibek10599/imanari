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
const PROXY_IMAGE: &str = "nginx:1.30.6-alpine";
const DRAIN: Duration = Duration::from_secs(5);
const COLORS: [&str; 2] = ["blue", "green"];

pub type Fail = (u8, &'static str, String);

pub struct PlanOptions {
    pub name: Option<String>,
    pub port: Option<u16>,
    pub database_url_env: Option<String>,
    pub production: bool,
    pub public_url: Option<String>,
    pub oidc_secret_env: Option<String>,
    pub publish_address: Option<std::net::IpAddr>,
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

fn docker_input(args: &[&str], input: &str) -> Result<String, Fail> {
    use std::io::Write;
    let mut child = Command::new("docker")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| (3, "E_DOCKER_UNAVAILABLE", format!("cannot run docker: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| (3, "E_DOCKER", e.to_string()))?;
    if !out.status.success() {
        return Err((
            3,
            "E_DOCKER",
            format!(
                "docker {} failed: {}",
                args.first().copied().unwrap_or_default(),
                tail(&String::from_utf8_lossy(&out.stderr), 10)
            ),
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
    Some(json!({ "id": c["Id"], "image": c["Image"], "running": c["State"]["Running"] }))
}

fn proxy_conf(name: &str, color: &str, port: u64) -> String {
    format!(
        "resolver 127.0.0.11 valid=5s ipv6=off;\n\
         server {{\n\
         \x20   listen {port};\n\
         \x20   client_max_body_size 1m;\n\
         \x20   location / {{\n\
         \x20       set $nari_upstream http://{name}-{color}:{port};\n\
         \x20       proxy_pass $nari_upstream;\n\
         \x20       proxy_http_version 1.1;\n\
         \x20       proxy_set_header Host $http_host;\n\
         \x20       proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;\n\
         \x20       proxy_set_header X-Forwarded-Proto $scheme;\n\
         \x20   }}\n\
         }}\n"
    )
}

fn active_color(name: &str) -> Option<&'static str> {
    let conf = docker(&[
        "exec",
        &format!("{name}-proxy"),
        "cat",
        "/etc/nginx/conf.d/default.conf",
    ])
    .ok()?;
    COLORS
        .into_iter()
        .find(|c| conf.contains(&format!("http://{name}-{c}:")))
}

fn observe(name: &str, volume: Option<&str>) -> Json {
    let proxy = container(&format!("{name}-proxy"));
    let active = proxy
        .as_ref()
        .filter(|p| p["running"] == true)
        .and_then(|_| active_color(name));
    json!({
        "legacy_container": exists(&["container", "inspect", name]),
        "network": exists(&["network", "inspect", name]),
        "volume": volume.map(|v| exists(&["volume", "inspect", v])),
        "proxy": proxy,
        "active": active,
        "blue": container(&format!("{name}-blue")),
        "green": container(&format!("{name}-green")),
    })
}

fn route(proxy: &str, conf: &str) -> Result<(), Fail> {
    docker_input(
        &[
            "exec",
            "-i",
            proxy,
            "sh",
            "-c",
            "cp /etc/nginx/conf.d/default.conf /tmp/nari-prev.conf && cat > /etc/nginx/conf.d/default.conf && if nginx -t 2>/tmp/nari-test; then nginx -s reload; else cp /tmp/nari-prev.conf /etc/nginx/conf.d/default.conf; cat /tmp/nari-test >&2; exit 1; fi",
        ],
        conf,
    )
    .map(|_| ())
}

fn probe(proxy: &str, host: &str, port: u64) -> Option<Json> {
    let out = docker(&[
        "exec",
        proxy,
        "wget",
        "-q",
        "-O",
        "-",
        "-T",
        "5",
        "--header",
        &format!("Host: 127.0.0.1:{port}"),
        &format!("http://{host}:{port}/_nari/health"),
    ])
    .ok()?;
    serde_json::from_str(&out).ok()
}

fn serves(proxy: &str, host: &str, port: u64, expected: &str, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        if probe(proxy, host, port).is_some_and(|h| h["source_sha256"] == expected) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
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
    let (automatic, behind): (Vec<String>, bool) =
        match (from, deployed["nari.schema_shape"].as_str()) {
            (Some(fp), _) if fp == fingerprint => (Vec::new(), false),
            (Some(_), Some(shape)) => {
                let old: Vec<imanari_runtime::EntityShape> =
                    serde_json::from_str(shape).map_err(|e| {
                        (
                            4,
                            "E_MIGRATION_BLOCKED",
                            format!("deployed schema shape is unreadable: {e}"),
                        )
                    })?;
                let new = codegen::schema_shape(&c.app);
                let dialect = if postgres {
                    imanari_runtime::Dialect::Postgres
                } else {
                    imanari_runtime::Dialect::Sqlite
                };
                match imanari_runtime::migration_plan(&old, &new, dialect) {
                    Ok(steps) => (steps.into_iter().map(|s| s.summary).collect(), false),
                    Err(_) if imanari_runtime::migration_plan(&new, &old, dialect).is_ok() => {
                        (Vec::new(), true)
                    }
                    Err(blocked) => return Err((4, "E_MIGRATION_BLOCKED", blocked.join("; "))),
                }
            }
            _ => (Vec::new(), false),
        };
    Ok(json!({ "from": from, "to": fingerprint, "automatic": automatic, "behind": behind }))
}

fn plans_dir(p: &Project) -> Result<PathBuf, Fail> {
    let deploy = artifacts::prepare(p).map_err(io)?.join("deploy");
    artifacts::ensure_real_dir(&deploy).map_err(io)?;
    let plans = deploy.join("plans");
    artifacts::ensure_real_dir(&plans).map_err(io)?;
    Ok(plans)
}

pub fn plan(p: &Project, c: &Compiled, o: &PlanOptions) -> Result<Json, Fail> {
    let oidc = c.app.identity.mode == "oidc";
    if o.production && !oidc {
        return Err((
            4,
            "E_DEV_IDENTITY",
            format!(
                "production plans need a production identity adapter, and {} declares identity mode {}; declare mode = oidc, or plan a preview, which publishes on host loopback only",
                c.file, c.app.identity.mode
            ),
        ));
    }
    if !oidc && (o.public_url.is_some() || o.oidc_secret_env.is_some()) {
        return Err(usage(
            "--public-url and --oidc-secret-env apply only to identity mode oidc".into(),
        ));
    }
    if !o.production && o.publish_address.is_some_and(|a| !a.is_loopback()) {
        return Err(usage(
            "preview plans publish on loopback only; use --production to publish elsewhere".into(),
        ));
    }
    if let Some(v) = &o.oidc_secret_env
        && !valid_env_name(v)
    {
        return Err(usage(format!("invalid environment variable name '{v}'")));
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
    let public_url = match (&o.public_url, oidc) {
        (Some(u), _) => Some(u.trim_end_matches('/').to_string()),
        (None, true) if !o.production => Some(format!("http://127.0.0.1:{port}")),
        (None, true) => {
            return Err(usage(
                "production plans for oidc apps need --public-url https://<host>".into(),
            ));
        }
        (None, false) => None,
    };
    if let Some(u) = &public_url {
        let host = u
            .strip_prefix("https://")
            .or_else(|| u.strip_prefix("http://"))
            .unwrap_or_default();
        let valid = !host.is_empty()
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:[]".contains(&b));
        if !valid {
            return Err(usage(format!(
                "invalid --public-url '{u}': use http(s)://host[:port] without a path"
            )));
        }
        if o.production && !u.starts_with("https://") {
            return Err(usage(format!(
                "production plans need an https --public-url, not '{u}'"
            )));
        }
    }
    let address = o
        .publish_address
        .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
    let environment = if o.production {
        "production"
    } else {
        "preview"
    };
    let target = connectivity()?;
    let volume = o.database_url_env.is_none().then(|| format!("{name}-data"));
    let observed = observe(&name, volume.as_deref());
    if observed["legacy_container"] == true {
        return Err((
            4,
            "E_LEGACY_DEPLOYMENT",
            format!(
                "container {name} was released without a proxy; remove it with `docker rm -f {name}` and plan again"
            ),
        ));
    }
    let active = observed["active"]
        .as_str()
        .or_else(|| COLORS.into_iter().find(|c| observed[*c]["running"] == true));
    let color = if active == Some("blue") {
        "green"
    } else {
        "blue"
    };
    let deployed = active
        .and_then(|a| observed[a]["image"].as_str())
        .map(labels)
        .unwrap_or(Json::Null);
    let schema = schema_transition(c, &deployed, o.database_url_env.is_some())?;
    let repo = format!("nari-{}", c.app.name.to_lowercase().replace('_', "-"));
    let artifact = build_image(p, c, &repo)?;
    let mut resources = vec![
        json!({
            "kind": "image",
            "ref": artifact["image"],
            "digest": artifact["digest"],
            "action": "use",
        }),
        json!({
            "kind": "network",
            "name": name,
            "action": if observed["network"] == true { "keep" } else { "create" },
        }),
    ];
    if let Some(v) = &volume {
        resources.push(json!({
            "kind": "volume",
            "name": v,
            "mount": "/data",
            "action": if observed["volume"] == true { "keep" } else { "create" },
        }));
    }
    let mut secrets = json!({});
    if let Some(var) = &o.database_url_env {
        secrets["NARI_DATABASE_URL"] = json!(format!("env:{var}"));
    }
    if let Some(var) = &o.oidc_secret_env {
        secrets["NARI_OIDC_CLIENT_SECRET"] = json!(format!("env:{var}"));
    }
    let mut env = json!({ "NARI_PORT": port.to_string() });
    if let Some(u) = &public_url {
        env["NARI_PUBLIC_URL"] = json!(u);
    }
    let publish = match address {
        std::net::IpAddr::V6(a) => format!("[{a}]:{port}:{port}/tcp"),
        std::net::IpAddr::V4(a) => format!("{a}:{port}:{port}/tcp"),
    };
    resources.push(json!({
        "kind": "container",
        "name": format!("{name}-{color}"),
        "color": color,
        "action": if observed[color].is_null() { "create" } else { "replace" },
        "image": artifact["digest"],
        "network": name,
        "port": port,
        "restart": format!("on-failure:{}", c.app.recovery.max_restarts),
        "env": env,
        "secrets": secrets,
    }));
    let proxy_ready = observed["proxy"]["running"] == true && observed["active"].is_string();
    resources.push(json!({
        "kind": "proxy",
        "name": format!("{name}-proxy"),
        "action": if observed["proxy"].is_null() {
            "create"
        } else if proxy_ready {
            "keep"
        } else {
            "replace"
        },
        "image": PROXY_IMAGE,
        "publish": publish,
        "route": { "from": active, "to": color },
    }));
    if let Some(a) = active {
        resources.push(json!({
            "kind": "retire",
            "name": format!("{name}-{a}"),
            "action": "stop and remove",
            "drain_seconds": DRAIN.as_secs(),
        }));
    }
    let database = match &o.database_url_env {
        Some(var) => json!({
            "engine": "postgresql",
            "url": format!("env:{var}"),
            "tls": "rustls with webpki roots; the URL's sslmode applies",
        }),
        None => json!({ "engine": "sqlite", "volume": volume }),
    };
    let plan = json!({
        "schema": "nari-plan/0.1",
        "adapter": ADAPTER,
        "environment": environment,
        "app": {
            "name": c.app.name,
            "source_file": c.file,
            "source_sha256": c.source_sha256,
            "semantic_sha256": c.semantic_sha256,
            "policy_sha256": pipeline::policy_hash(c),
            "schema_fingerprint": schema["to"],
        },
        "authentication": {
            "mode": c.app.identity.mode,
            "issuer": c.app.identity.issuer,
            "client_id": c.app.identity.client_id,
            "public_url": public_url,
            "exposure": if address.is_loopback() {
                "host loopback only".to_string()
            } else {
                format!("{address} port {port}; terminate TLS for {} in front of it", public_url.as_deref().unwrap_or_default())
            },
        },
        "connectivity": target,
        "database": database,
        "artifact": artifact,
        "resources": resources,
        "schema_transition": schema,
        "observed": observed,
        "readiness": {
            "signal": "NARI_READY with the planned source_sha256 in the container log, then /_nari/health from inside the network",
            "timeout_seconds": READY_TIMEOUT.as_secs(),
        },
        "switch": "the proxy is reloaded to route to the new color only after it is ready; the old color keeps serving until then and drains afterwards",
        "rollback": "if the new color is not ready, it is removed and traffic never moves (aborted); if it does not answer through the proxy after the switch, the proxy routes back to the old color (rolled_back)",
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

fn wait_ready(name: &str, expected: &str) -> Result<Ready, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        let out = docker_cmd(&["logs", name])
            .output()
            .map_err(|e| e.to_string())?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        let mut bootstrap = None;
        for line in stdout.lines() {
            if let Some(url) = line.strip_prefix("NARI_BOOTSTRAP ") {
                bootstrap = Some(url.to_string());
            } else if let Some(rest) = line.strip_prefix("NARI_STOP ") {
                return Err(format!("app stopped during startup: {rest}"));
            } else if let Some(rest) = line.strip_prefix("NARI_READY ") {
                let ready: Json = serde_json::from_str(rest).unwrap_or(Json::Null);
                if ready["source_sha256"] == expected {
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
    let base = find("network")
        .and_then(|n| n["name"].as_str().map(str::to_string))
        .unwrap_or_default();
    let color = spec["color"].as_str().unwrap_or_default().to_string();
    let name = spec["name"].as_str().unwrap_or_default().to_string();
    let image = spec["image"].as_str().unwrap_or_default().to_string();
    if !valid_name(&base) || !COLORS.contains(&color.as_str()) || name != format!("{base}-{color}")
    {
        return Err((
            4,
            "E_PLAN_TAMPERED",
            format!("invalid deployment '{base}' or container '{name}'"),
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
    let observed = observe(&base, volume.as_ref().and_then(|v| v["name"].as_str()));
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
    let proxy = find("proxy").ok_or_else(|| (4, "E_PLAN_TAMPERED", "plan has no proxy".into()))?;
    let network =
        find("network").ok_or_else(|| (4, "E_PLAN_TAMPERED", "plan has no network".into()))?;
    let retire = find("retire");
    let text = |v: &Json| v.as_str().unwrap_or_default().to_string();
    let (proxy_name, network_name) = (format!("{base}-proxy"), base.clone());
    let port = spec["port"].as_u64().unwrap_or_default();
    let expected = text(&plan["app"]["source_sha256"]);
    let release_id = crate::random_hex(8);
    let started_ms = now_ms();
    let app_label = format!("nari.app={}", text(&plan["app"]["name"]));
    if network["action"] == "create" {
        docker(&["network", "create", "--label", &app_label, &network_name])?;
    }
    if let Some(v) = &volume
        && v["action"] == "create"
    {
        docker(&["volume", "create", "--label", &app_label, &text(&v["name"])])?;
    }
    if spec["action"] == "replace" {
        docker(&["rm", "-f", &name])?;
    }
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--name",
        &name,
        "--network",
        &network_name,
        "--restart",
        spec["restart"].as_str().unwrap_or("no"),
        "--label",
        &app_label,
        "--label",
        &format!("nari.plan={hash}"),
        "--label",
        &format!("nari.release={release_id}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for (key, value) in spec["env"].as_object().into_iter().flatten() {
        args.extend(["-e".into(), format!("{key}={}", text(value))]);
    }
    if let Some(v) = &volume {
        args.extend(["-v".into(), format!("{}:/data", text(&v["name"]))]);
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
    let ready = match &launched {
        Ok(_) => wait_ready(&name, &expected),
        Err((_, _, m)) => Err(m.clone()),
    };
    let container_id = launched.unwrap_or_default();
    let previous = plan["observed"]["active"].clone();
    let switched: Result<Ready, (String, bool)> = ready.map_err(|e| (e, false)).and_then(|r| {
        if proxy["action"] == "keep" {
            if !serves(&proxy_name, &name, port, &expected, Duration::from_secs(10)) {
                return Err((
                    format!("{name} did not answer /_nari/health inside the network"),
                    false,
                ));
            }
        } else {
            let _ = docker(&["rm", "-f", &proxy_name]);
            docker(&[
                "run",
                "-d",
                "--name",
                &proxy_name,
                "--network",
                &network_name,
                "--restart",
                "unless-stopped",
                "-p",
                &text(&proxy["publish"]),
                "--label",
                &app_label,
                PROXY_IMAGE,
            ])
            .map_err(|(_, _, m)| (m, false))?;
        }
        route(&proxy_name, &proxy_conf(&base, &color, port)).map_err(|(_, _, m)| (m, false))?;
        if serves(
            &proxy_name,
            "127.0.0.1",
            port,
            &expected,
            Duration::from_secs(15),
        ) {
            Ok(r)
        } else {
            Err((
                "the proxy did not reach the new color after the switch".into(),
                true,
            ))
        }
    });
    let (outcome, reason, evidence, bootstrap, log_tail) = match switched {
        Ok(r) => {
            if let Some(old) = &retire {
                std::thread::sleep(DRAIN);
                let old = text(&old["name"]);
                let _ = docker(&["stop", "-t", "15", &old]);
                let _ = docker(&["rm", &old]);
            }
            ("released", None, r.evidence, r.bootstrap, String::new())
        }
        Err((reason, after_switch)) => {
            let logs = container_logs(&name);
            let outcome = match (after_switch, previous.as_str()) {
                (false, Some(_)) => "aborted",
                (true, Some(old)) if route(&proxy_name, &proxy_conf(&base, old, port)).is_ok() => {
                    "rolled_back"
                }
                _ => "failed",
            };
            let _ = docker(&["rm", "-f", &name]);
            (outcome, Some(reason), Json::Null, None, logs)
        }
    };
    let record = json!({
        "schema": "nari-release/0.1",
        "release_id": release_id,
        "plan_sha256": hash,
        "adapter": ADAPTER,
        "name": name,
        "color": color,
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
