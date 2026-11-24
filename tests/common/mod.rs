#![allow(dead_code)]

use imanari::Project;
use imanari::ir::Compiled;
use imanari::launch::{AppProcess, LaunchOptions, Session};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub const FIXTURE: &str = include_str!("../../examples/fieldwork/fieldwork.nr");
pub const SHARED_TARGET: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/target/nari-apps");

pub struct Built {
    pub project: Project,
    pub binary: PathBuf,
    pub compiled: Compiled,
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("nari-tests")
        .join(format!("{name}-{}", imanari::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn write_project(name: &str, src: &str) -> Project {
    let dir = scratch(name);
    std::fs::write(dir.join("fieldwork.nr"), src).unwrap();
    let acceptance = dir.join("acceptance");
    std::fs::create_dir_all(&acceptance).unwrap();
    for entry in std::fs::read_dir(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/fieldwork/acceptance"
    ))
    .unwrap()
    {
        let p = entry.unwrap().path();
        std::fs::copy(&p, acceptance.join(p.file_name().unwrap())).unwrap();
    }
    Project::locate(Some(dir)).unwrap()
}

pub fn build(name: &str, src: &str) -> Built {
    let project = write_project(name, src);
    let (compiled, _) = imanari::compile(src, &project.file_name).expect("source compiles");
    let built = imanari::pipeline::build_in(&project, &compiled, Path::new(SHARED_TARGET))
        .unwrap_or_else(|(_, d)| panic!("build failed: {d:#?}"));
    Built {
        project,
        binary: built.binary,
        compiled,
    }
}

pub fn fieldwork() -> &'static Built {
    static BUILT: OnceLock<Built> = OnceLock::new();
    BUILT.get_or_init(|| build("fieldwork", FIXTURE))
}

pub fn start(b: &Built, db: &Path, env: &[(&str, &str)]) -> Result<AppProcess, String> {
    AppProcess::start(&LaunchOptions {
        binary: b.binary.clone(),
        cwd: b.project.dir.clone(),
        database: db.to_path_buf(),
        port: 0,
        log: Some(db.with_extension("log")),
        env: env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        wrapper: Vec::new(),
    })
}

pub fn app_with_sessions(
    b: &Built,
    db: &Path,
    env: &[(&str, &str)],
) -> (AppProcess, Session, Session) {
    let app = start(b, db, env).expect("app starts");
    let (a, s) = imanari::launch::dev_sessions(app.port, &app.bootstrap).expect("dev sessions");
    (app, a, s)
}
