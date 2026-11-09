#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use imanari::diag::{Diagnostic, Span};
use imanari::{Project, ir};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "nari",
    version,
    about = "Imanari language compiler and local runtime"
)]
struct Cli {
    #[arg(
        long,
        global = true,
        help = "Project directory or .nari file (default: current directory)"
    )]
    project: Option<PathBuf>,
    #[arg(long, global = true, help = "Machine-readable JSON output")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Create a non-overwriting standalone example project")]
    Init { name: String },
    #[command(about = "Format the source deterministically, preserving comments")]
    Fmt {
        #[arg(long)]
        check: bool,
    },
    #[command(about = "Parse, type, reference and policy validation (offline)")]
    Check,
    #[command(about = "Emit IR, graph, native app, browser files and artifact manifest")]
    Build,
    #[command(about = "Build, then run the supervisor and app with local development identity")]
    Dev {
        #[arg(
            long,
            help = "Acknowledge a StoppedNeedsAttention state and start again (restart history is kept)"
        )]
        acknowledge: bool,
    },
    #[command(about = "Run generated contract checks and protected project tests")]
    Test,
    #[command(about = "Emit versioned context and an inclusion manifest for a node")]
    Context {
        id: String,
        #[arg(long, default_value_t = 32768)]
        max_bytes: usize,
    },
    #[command(about = "Compare two source snapshots by behavior and dependencies")]
    Diff {
        #[arg(long)]
        base: PathBuf,
    },
    #[command(about = "Read supervisor state and recent incidents")]
    Status,
    #[command(about = "Read redacted incident records")]
    Incidents {
        #[arg(long)]
        id: Option<String>,
    },
    #[command(about = "Run the isolated repair pipeline (never releases)")]
    Repair {
        id: String,
        #[arg(long, default_value = "fixture")]
        proposer: String,
    },
    #[command(about = "Apply a validated local repair candidate after rechecks")]
    ApplyRepair {
        id: String,
        #[arg(long)]
        candidate: String,
    },
    #[command(about = "Run the reproducible benchmark suite")]
    Bench {
        #[arg(long, help = "Smaller workload (still 5 measured runs after warmup)")]
        quick: bool,
    },
    #[command(about = "Production planning (unsupported in 0.1)")]
    Plan,
    #[command(about = "Production release (unsupported in 0.1)")]
    Release,
}

struct Failure {
    exit: u8,
    diagnostics: Vec<Diagnostic>,
}

impl Failure {
    fn new(exit: u8, code: &str, message: impl Into<String>) -> Self {
        Failure {
            exit,
            diagnostics: vec![Diagnostic::error(code, message, "", Span::default())],
        }
    }

    fn diags(diagnostics: Vec<Diagnostic>) -> Self {
        Failure {
            exit: imanari::diag::exit_code(&diagnostics).max(1),
            diagnostics,
        }
    }
}

struct Output {
    human: String,
    json: Json,
}

type CmdResult = Result<Output, Failure>;

fn out(human: impl Into<String>, json: Json) -> CmdResult {
    Ok(Output {
        human: human.into(),
        json,
    })
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().collect();
    match argv.get(1).map(String::as_str) {
        Some("__limit") => {
            let Err(e) = imanari::sandbox::limit_and_exec(&argv[2..]);
            eprintln!("nari __limit: {e}");
            return ExitCode::from(3);
        }
        Some("__probe") => {
            return if imanari::sandbox::run_probe(&argv[2..]) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        _ => {}
    }
    let cli = Cli::parse();
    let json = cli.json;
    let result = run(cli, json);
    match result {
        Ok(o) => {
            if json {
                println!("{}", ir::canonical(&o.json));
            } else if !o.human.is_empty() {
                println!("{}", o.human);
            }
            ExitCode::SUCCESS
        }
        Err(f) => {
            if json {
                println!(
                    "{}",
                    ir::canonical(&json!({ "ok": false, "diagnostics": f.diagnostics }))
                );
            } else {
                for d in &f.diagnostics {
                    eprintln!("{}", d.render());
                }
            }
            ExitCode::from(f.exit)
        }
    }
}

fn project(path: Option<PathBuf>) -> Result<Project, Failure> {
    Project::locate(path).map_err(|e| Failure::new(2, "E_PROJECT", e))
}

fn load(p: &Project) -> Result<(String, ir::Compiled, Vec<Diagnostic>), Failure> {
    let src = p.read().map_err(|e| Failure::new(3, "E_IO", e))?;
    let (compiled, warnings) = imanari::compile(&src, &p.file_name).map_err(Failure::diags)?;
    Ok((src, compiled, warnings))
}

fn render_warnings(w: &[Diagnostic]) -> String {
    w.iter().map(|d| format!("{}\n", d.render())).collect()
}

fn run(cli: Cli, json: bool) -> CmdResult {
    match cli.command {
        Command::Init { name } => init(cli.project, &name),
        Command::Fmt { check } => fmt(&project(cli.project)?, check),
        Command::Check => {
            let p = project(cli.project)?;
            let (_, c, warnings) = load(&p)?;
            out(
                format!("{}ok: {} is valid", render_warnings(&warnings), p.file_name),
                json!({
                    "ok": true,
                    "diagnostics": warnings,
                    "semantic_sha256": c.semantic_sha256,
                    "source_sha256": c.source_sha256,
                }),
            )
        }
        Command::Build => build(&project(cli.project)?),
        Command::Test => test(&project(cli.project)?),
        Command::Dev { acknowledge } => dev(&project(cli.project)?, acknowledge, json),
        Command::Status => status(&project(cli.project)?),
        Command::Incidents { id } => incidents(&project(cli.project)?, id),
        Command::Bench { quick } => bench(&project(cli.project)?, quick),
        Command::Repair { id, proposer } => repair(&project(cli.project)?, &id, &proposer),
        Command::ApplyRepair { id, candidate } => {
            apply_repair(&project(cli.project)?, &id, &candidate)
        }
        Command::Context { id, max_bytes } => context(&project(cli.project)?, &id, max_bytes),
        Command::Diff { base } => diff(&project(cli.project)?, base),
        Command::Plan | Command::Release => Err(Failure::new(
            2,
            "E_UNSUPPORTED_FEATURE",
            "production planning and release are not available in Imanari 0.1 (local_dev identity only)",
        )),
    }
}

fn init(dir: Option<PathBuf>, name: &str) -> CmdResult {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name != "true"
        && name != "false";
    if !valid {
        return Err(Failure::new(
            2,
            "E_USAGE",
            "project name must match [a-z_][a-z0-9_]*",
        ));
    }
    let root = dir.unwrap_or_else(|| PathBuf::from(".")).join(name);
    if root.exists() {
        return Err(Failure::new(
            3,
            "E_EXISTS",
            format!("{} already exists; init never overwrites", root.display()),
        ));
    }
    let template = include_str!("../examples/fieldwork/fieldwork.nari").replacen(
        "app fieldwork {",
        &format!("app {name} {{"),
        1,
    );
    let file = format!("{name}.nari");
    imanari::compile(&template, &file).map_err(Failure::diags)?;
    fs::create_dir_all(&root).map_err(|e| Failure::new(3, "E_IO", e.to_string()))?;
    fs::write(root.join(&file), template).map_err(|e| Failure::new(3, "E_IO", e.to_string()))?;
    fs::write(root.join(".gitignore"), ".nari/\n")
        .map_err(|e| Failure::new(3, "E_IO", e.to_string()))?;
    fs::create_dir_all(root.join("acceptance"))
        .map_err(|e| Failure::new(3, "E_IO", e.to_string()))?;
    for (name, body) in [
        (
            "complete_task.json",
            include_str!("../examples/fieldwork/acceptance/complete_task.json"),
        ),
        (
            "ownership.json",
            include_str!("../examples/fieldwork/acceptance/ownership.json"),
        ),
        (
            "stale_revision.json",
            include_str!("../examples/fieldwork/acceptance/stale_revision.json"),
        ),
    ] {
        fs::write(
            root.join("acceptance").join(name),
            body.replace("fieldwork/", &format!("{name}/")),
        )
        .map_err(|e| Failure::new(3, "E_IO", e.to_string()))?;
    }
    out(
        format!("created {}", root.join(&file).display()),
        json!({ "ok": true, "path": root.join(&file) }),
    )
}

fn fmt(p: &Project, check: bool) -> CmdResult {
    let src = p.read().map_err(|e| Failure::new(3, "E_IO", e))?;
    let formatted = imanari::format_source(&src, &p.file_name).map_err(Failure::diags)?;
    let reparsed = imanari::syntax::parse(&formatted, &p.file_name).map_err(|d| {
        Failure::new(
            3,
            "E_INTERNAL",
            format!("formatter produced unparsable output: {}", d.message),
        )
    })?;
    let before = imanari::compile(&src, &p.file_name)
        .ok()
        .map(|(c, _)| c.semantic_sha256);
    let after = imanari::semantic::check(&reparsed, &formatted)
        .ok()
        .map(|(c, _)| c.semantic_sha256);
    if before.is_some() && before != after {
        return Err(Failure::new(
            3,
            "E_INTERNAL",
            "formatter changed program semantics; refusing to write",
        ));
    }
    let changed = formatted != src;
    if check {
        if changed {
            let mut f = Failure::new(
                1,
                "E_FORMAT",
                format!("{} is not formatted; run `nari fmt`", p.file_name),
            );
            f.diagnostics[0].file = p.file_name.clone();
            return Err(f);
        }
        return out(
            format!("ok: {} is formatted", p.file_name),
            json!({ "ok": true, "changed": false }),
        );
    }
    if changed {
        fs::write(&p.file, &formatted).map_err(|e| Failure::new(3, "E_IO", e.to_string()))?;
    }
    out(
        if changed {
            format!("formatted {}", p.file_name)
        } else {
            format!("ok: {} already formatted", p.file_name)
        },
        json!({ "ok": true, "changed": changed }),
    )
}

fn test(p: &Project) -> CmdResult {
    let (_, c, _) = load(p)?;
    let tests =
        imanari::testrun::load_protected(&p.dir).map_err(|e| Failure::new(1, "E_TEST", e))?;
    let built = imanari::pipeline::build(p, &c).map_err(build_failure)?;
    let run_dir = p
        .artifacts()
        .join("test")
        .join(format!("run-{}", imanari::random_hex(4)));
    let run = imanari::testrun::run_all(
        &c,
        &tests,
        &imanari::testrun::RunOptions {
            binary: &built.binary,
            cwd: &p.dir,
            run_dir: &run_dir,
            wrapper: Vec::new(),
        },
        true,
    )
    .map_err(|e| Failure::new(3, "E_TEST_ENVIRONMENT", e))?;
    let failed: Vec<_> = run.results.iter().filter(|r| !r.passed).collect();
    let incident_ids = if failed.iter().any(|r| r.kind == "protected") {
        runtime()?
            .block_on(imanari::repair::record_test_failures(
                p,
                &c,
                &tests,
                &run.results,
            ))
            .map_err(|e| Failure::new(3, "E_CONTROL_DB", e))?
    } else {
        Vec::new()
    };
    let report = json!({
        "incidents": incident_ids,
        "ok": failed.is_empty(),
        "source_sha256": c.source_sha256,
        "protected_tests_sha256": run.protected_sha256,
        "protected_tests": tests.iter().map(|t| json!({ "file": t.file, "sha256": t.sha256 })).collect::<Vec<_>>(),
        "results": run.results,
    });
    imanari::artifacts::write_file(
        &p.artifacts().join("test"),
        "report.json",
        ir::canonical(&report) + "\n",
    )
    .map_err(|e| Failure::new(3, "E_IO", e))?;
    let mut human = String::new();
    for r in &run.results {
        let mark = if r.passed { "pass" } else { "FAIL" };
        human.push_str(&format!("{mark} [{}] {}", r.kind, r.name));
        if !r.passed {
            human.push_str(&format!("\n     {}", r.detail));
        }
        human.push('\n');
    }
    for id in &incident_ids {
        human.push_str(&format!(
            "incident {id} recorded; repair with: nari repair {id} --proposer fixture\n"
        ));
    }
    human.push_str(&format!(
        "{} checks, {} failed (report: .nari/test/report.json)",
        run.results.len(),
        failed.len()
    ));
    if failed.is_empty() {
        out(human, report)
    } else {
        eprintln!("{human}");
        let mut f = Failure::new(
            1,
            "E_TEST_FAILED",
            format!("{} of {} checks failed", failed.len(), run.results.len()),
        );
        f.diagnostics[0].file = p.file_name.clone();
        Err(f)
    }
}

fn context(p: &Project, id: &str, max_bytes: usize) -> CmdResult {
    let (src, c, _) = load(p)?;
    let bundle = imanari::context::extract(p, &c, &src, id, max_bytes)
        .map_err(|e| Failure::new(1, "E_UNKNOWN_NODE", e))?;
    let dir = imanari::context::write(p, id, &bundle).map_err(|e| Failure::new(3, "E_IO", e))?;
    if bundle.over_budget {
        let mut f = Failure::new(
            1,
            "CONTEXT_BUDGET_EXCEEDED",
            format!(
                "required context needs {} bytes but --max-bytes is {max_bytes}; nothing was truncated. Required expansion manifest: {}",
                bundle.manifest["required_bytes"],
                dir.join("manifest.json").display()
            ),
        );
        f.diagnostics[0].node_id = Some(id.to_string());
        return Err(f);
    }
    out(
        format!(
            "context for {id}: {} bytes ({} included, {} omitted) -> {}",
            bundle.manifest["bytes"],
            bundle.manifest["included"].as_array().map_or(0, Vec::len),
            bundle.manifest["omitted"].as_array().map_or(0, Vec::len),
            dir.display()
        ),
        bundle.manifest,
    )
}

fn diff(p: &Project, base: PathBuf) -> CmdResult {
    let (_, cur, _) = load(p)?;
    let base_project = Project::locate(Some(base)).map_err(|e| Failure::new(2, "E_PROJECT", e))?;
    let (_, base_c, _) = load(&base_project)?;
    let d = imanari::diff::diff(&base_c, &cur);
    out(imanari::diff::render(&d), d)
}

fn build_failure((exit, diagnostics): (u8, Vec<Diagnostic>)) -> Failure {
    Failure { exit, diagnostics }
}

fn build(p: &Project) -> CmdResult {
    let (_, c, warnings) = load(p)?;
    let built = imanari::pipeline::build(p, &c).map_err(build_failure)?;
    out(
        format!(
            "{}built {} -> {}",
            render_warnings(&warnings),
            p.file_name,
            built.binary.display()
        ),
        json!({ "ok": true, "manifest": built.manifest }),
    )
}

fn runtime() -> Result<tokio::runtime::Runtime, Failure> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::new(3, "E_RUNTIME", e.to_string()))
}

fn emit_event(json_mode: bool, event: &str, data: &Json) {
    use std::io::Write;
    let line = if json_mode {
        let mut d = data.clone();
        if let Some(url) = d["url"].as_str() {
            d["url"] = json!(
                url.split('#').next().unwrap_or_default().to_string() + "#bootstrap=[redacted]"
            );
        }
        ir::canonical_compact(&json!({ "event": event, "data": d }))
    } else {
        match event {
            "bootstrap" => format!("open: {}", data["url"].as_str().unwrap_or_default()),
            "ready" => format!(
                "ready: http://127.0.0.1:{}/ (pid {}, instance {})",
                data["port"],
                data["pid"],
                data["instance"].as_str().unwrap_or_default()
            ),
            "restart" => format!(
                "worker exited unexpectedly ({}); restart {}/{} in {} ms (incident {})",
                data["exit"].as_str().unwrap_or_default(),
                data["attempt"],
                data["max_restarts"],
                data["backoff_ms"],
                data["incident_id"].as_str().unwrap_or_default()
            ),
            "state" => match data["state"].as_str() {
                Some("StoppedNeedsAttention") => format!(
                    "STOPPED (needs attention): {}",
                    data["reason"].as_str().unwrap_or_default()
                ),
                Some(s) => format!("state: {s}"),
                None => String::new(),
            },
            "reconciled" => format!(
                "reconciled: {}",
                data["detail"].as_str().unwrap_or_default()
            ),
            "app_stop" => format!(
                "worker refused to start: {}",
                data["reason"].as_str().unwrap_or_default()
            ),
            _ => format!("{event}: {}", ir::canonical_compact(data)),
        }
    };
    let mut stdout = std::io::stdout();
    let _ = writeln!(stdout, "{line}");
    let _ = stdout.flush();
}

fn dev(p: &Project, acknowledge: bool, json_mode: bool) -> CmdResult {
    let (_, c, warnings) = load(p)?;
    if !json_mode {
        eprint!("{}", render_warnings(&warnings));
    }
    let built = imanari::pipeline::build(p, &c).map_err(build_failure)?;
    let rt = runtime()?;
    let emit = |event: &str, data: Json| emit_event(json_mode, event, &data);
    let opts = imanari::supervisor::DevOptions {
        binary: built.binary,
        acknowledge,
    };
    match rt.block_on(imanari::supervisor::run_dev(p, &c, &opts, &emit)) {
        Ok(imanari::supervisor::DevExit::Stopped) => {
            out("stopped", json!({ "ok": true, "state": "Stopped" }))
        }
        Ok(imanari::supervisor::DevExit::NeedsAttention(msg)) => {
            Err(Failure::new(3, "STOPPED_NEEDS_ATTENTION", msg))
        }
        Err(e) => {
            let code = e
                .split(':')
                .next()
                .filter(|c| c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'))
                .unwrap_or("E_SUPERVISOR")
                .to_string();
            let msg = e
                .strip_prefix(&format!("{code}: "))
                .unwrap_or(&e)
                .to_string();
            Err(Failure::new(3, &code, msg))
        }
    }
}

fn status(p: &Project) -> CmdResult {
    let c = load(p).ok().map(|(_, c, _)| c);
    let s = runtime()?
        .block_on(imanari::supervisor::status(p, c.as_ref()))
        .map_err(|e| Failure::new(3, "E_CONTROL_DB", e))?;
    let mut human = format!(
        "backend: {} (restarts in last {}s: {}/{})",
        s["state"].as_str().unwrap_or_default(),
        s["window_seconds"],
        s["restarts_in_window"],
        s["max_restarts"]
    );
    if let Some(r) = s["detail"]["reason"].as_str() {
        human.push_str(&format!("\n  last transition: {r}"));
    }
    for (name, a) in s["metrics"]["actions"].as_object().into_iter().flatten() {
        let errors: u64 = a["by_status"]
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(code, _)| !code.starts_with('2'))
            .filter_map(|(_, n)| n.as_u64())
            .sum();
        human.push_str(&format!(
            "\n  {name}: {} requests, {errors} non-2xx, p95 <= {} us",
            a["requests"], a["latency_us"]["p95_le"]
        ));
    }
    if let Some(d) = s["metrics"]["dropped_events"].as_u64().filter(|d| *d > 0) {
        human.push_str(&format!("\n  telemetry events dropped: {d}"));
    }
    for i in s["recent_incidents"].as_array().into_iter().flatten() {
        human.push_str(&format!(
            "\n  {} {} -> {}",
            i["incident_id"].as_str().unwrap_or_default(),
            i["failure_signature"].as_str().unwrap_or_default(),
            i["outcome"].as_str().unwrap_or_default()
        ));
    }
    out(human, s)
}

fn incidents(p: &Project, id: Option<String>) -> CmdResult {
    let list = runtime()?
        .block_on(async {
            let ctl = imanari::supervisor::Control::open(p).await?;
            ctl.incidents(id.as_deref(), 50).await
        })
        .map_err(|e| Failure::new(3, "E_CONTROL_DB", e))?;
    if id.is_some() && list.is_empty() {
        return Err(Failure::new(
            1,
            "E_UNKNOWN_INCIDENT",
            format!("no incident {}", id.unwrap_or_default()),
        ));
    }
    let human = if id.is_some() {
        list.iter()
            .map(ir::canonical)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        list.iter()
            .map(|i| {
                format!(
                    "{} {} {} -> {}",
                    i["incident_id"].as_str().unwrap_or_default(),
                    i["kind"].as_str().unwrap_or_default(),
                    i["failure_signature"].as_str().unwrap_or_default(),
                    i["outcome"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    out(
        if human.is_empty() {
            "no incidents".into()
        } else {
            human
        },
        json!({ "incidents": list }),
    )
}

fn repair_options(proposer: &str) -> Result<imanari::repair::Options, Failure> {
    Ok(imanari::repair::Options {
        nari_exe: std::env::current_exe().map_err(|e| Failure::new(3, "E_IO", e.to_string()))?,
        proposer: imanari::repair::Proposer::parse(proposer)
            .map_err(|e| Failure::new(2, "E_USAGE", e))?,
    })
}

fn split_error(e: String) -> Failure {
    let code = e
        .split(':')
        .next()
        .filter(|c| c.len() > 2 && c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_'))
        .unwrap_or("E_REPAIR")
        .to_string();
    let msg = e
        .strip_prefix(&format!("{code}: "))
        .unwrap_or(&e)
        .to_string();
    Failure::new(3, &code, msg)
}

fn repair(p: &Project, id: &str, proposer: &str) -> CmdResult {
    let (src, c, _) = load(p)?;
    let o = repair_options(proposer)?;
    let r = imanari::repair::run(p, &c, &src, id, &o).map_err(split_error)?;
    let mut human = format!(
        "repair {} for {}: {} ({})",
        r.report["repair_id"].as_str().unwrap_or_default(),
        id,
        r.state,
        r.report["outcome"].as_str().unwrap_or_default()
    );
    if let Some(h) = r.report["proposal"]["candidate_hash"].as_str() {
        human.push_str(&format!("\n  candidate: {h}"));
    }
    for key in ["reason"] {
        if let Some(v) = r.report[key].as_str() {
            human.push_str(&format!("\n  {key}: {v}"));
        }
    }
    if let Some(m) = r.report["static_validation"]["message"].as_str() {
        human.push_str(&format!(
            "\n  rejected: {} {m}",
            r.report["static_validation"]["code"]
                .as_str()
                .unwrap_or_default()
        ));
    }
    if let Some(iso) = r.report["isolation"]["reason"].as_str() {
        human.push_str(&format!("\n  isolation unavailable: {iso}"));
    }
    if r.state == "Validated" {
        human.push_str(&format!(
            "\n  running source untouched; apply explicitly with: nari apply-repair {id} --candidate {}",
            r.report["proposal"]["candidate_hash"].as_str().unwrap_or_default()
        ));
    }
    if r.exit == 0 {
        out(human, r.report)
    } else {
        if !std::env::args().any(|a| a == "--json") {
            eprintln!("{human}");
        }
        let code = r.report["outcome"]
            .as_str()
            .unwrap_or("REPAIR_FAILED")
            .to_string();
        let mut f = Failure::new(r.exit, &code, format!("repair ended in state {}", r.state));
        f.diagnostics[0].details = Some(ir::canonical_compact(&r.report));
        Err(f)
    }
}

fn apply_repair(p: &Project, id: &str, candidate: &str) -> CmdResult {
    let (src, c, _) = load(p)?;
    let o = repair_options("fixture")?;
    let r = imanari::repair::apply(p, &c, &src, id, candidate, &o).map_err(split_error)?;
    if r.exit == 0 {
        out(
            format!(
                "applied candidate {candidate} to {} after rechecking hashes and protected tests",
                p.file_name
            ),
            r.report,
        )
    } else {
        let code = r.report["code"]
            .as_str()
            .unwrap_or("APPLY_REJECTED")
            .to_string();
        Err(Failure::new(
            r.exit,
            &code,
            r.report["message"]
                .as_str()
                .unwrap_or("rejected")
                .to_string(),
        ))
    }
}

fn bench(p: &Project, quick: bool) -> CmdResult {
    let (_, c, _) = load(p)?;
    let opts = imanari::bench::Options {
        quick,
        out: p.artifacts().join("bench"),
    };
    let r = imanari::bench::run(p, &c, &opts).map_err(|e| Failure::new(3, "E_BENCH", e))?;
    let mut human = format!("{}\n", r["machine"]["cpu"].as_str().unwrap_or_default());
    for b in r["budgets"].as_array().into_iter().flatten() {
        human.push_str(&format!(
            "{:>12}  {}  (measured {})\n",
            b["result"].as_str().unwrap_or_default(),
            b["budget"].as_str().unwrap_or_default(),
            b["measured"]
        ));
    }
    human.push_str("report: .nari/bench/report.json");
    out(human, r)
}
