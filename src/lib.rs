#![forbid(unsafe_code)]
#![allow(clippy::result_large_err, clippy::type_complexity)]

pub mod artifacts;
pub mod bench;
pub mod codegen;
pub mod context;
pub mod deploy;
pub mod diag;
pub mod diff;
pub mod fmt;
pub mod http;
pub mod ir;
pub mod launch;
pub mod patch;
pub mod pipeline;
pub mod progress;
pub mod repair;
pub mod sandbox;
pub mod semantic;
pub mod supervisor;
pub mod syntax;
pub mod testrun;

use diag::Diagnostic;
use ir::Compiled;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Project {
    pub dir: PathBuf,
    pub file: PathBuf,
    pub file_name: String,
}

impl Project {
    pub fn locate(path: Option<PathBuf>) -> Result<Project, String> {
        let path = path
            .map(|p| {
                std::path::absolute(&p).map_err(|e| format!("cannot resolve {}: {e}", p.display()))
            })
            .transpose()?;
        let dir = match path {
            Some(p) if p.is_file() => {
                let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
                return Project::from_file(dir, p);
            }
            Some(p) => p,
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| format!("cannot read project directory {}: {e}", dir.display()))?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "nari") && p.is_file() && !is_hidden(p))
            .collect();
        files.sort();
        if files.len() != 1 {
            return Err(format!(
                "expected exactly one .nari file in {}, found {}",
                dir.display(),
                files.len()
            ));
        }
        Project::from_file(dir, files.remove(0))
    }

    fn from_file(dir: PathBuf, file: PathBuf) -> Result<Project, String> {
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir
        };
        let file_name = file
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or("project file name must be valid UTF-8")?
            .to_string();
        Ok(Project {
            dir,
            file,
            file_name,
        })
    }

    pub fn artifacts(&self) -> PathBuf {
        self.dir.join(".nari")
    }

    pub fn read(&self) -> Result<String, String> {
        let meta = fs::metadata(&self.file).map_err(|e| e.to_string())?;
        if meta.len() as usize > syntax::MAX_SOURCE {
            return Err(format!(
                "{} is {} bytes; the source limit is {} bytes",
                self.file_name,
                meta.len(),
                syntax::MAX_SOURCE
            ));
        }
        fs::read_to_string(&self.file).map_err(|e| e.to_string())
    }
}

pub fn compile(src: &str, file: &str) -> Result<(Compiled, Vec<Diagnostic>), Vec<Diagnostic>> {
    let doc = syntax::parse(src, file).map_err(|d| vec![d.locate(src)])?;
    semantic::check(&doc, src)
}

pub fn format_source(src: &str, file: &str) -> Result<String, Vec<Diagnostic>> {
    let doc = syntax::parse(src, file).map_err(|d| vec![d.locate(src)])?;
    Ok(fmt::format(src, &doc))
}

pub fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
}

pub fn random_hex(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("operating system randomness is available");
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
