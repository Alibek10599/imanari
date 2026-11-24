use crate::ir::{self, Compiled};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub const SCHEMA: &str = "nari-patch/0.1";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    pub file: String,
    pub start: usize,
    pub end: usize,
    pub replacement: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Patch {
    pub schema: String,
    pub base_source_sha256: String,
    pub base_graph_sha256: String,
    pub affected_nodes: Vec<String>,
    pub allowed_scope: Vec<String>,
    pub edits: Vec<Edit>,
    #[serde(default)]
    pub rationale: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Rejected {
    pub code: &'static str,
    pub message: String,
}

fn reject(code: &'static str, message: impl Into<String>) -> Rejected {
    Rejected {
        code,
        message: message.into(),
    }
}

impl Patch {
    pub fn hash(&self) -> String {
        ir::sha256_hex(ir::canonical_compact(&ir::to_json(self)).as_bytes())
    }
}

pub fn classify_path(file: &str, project_file: &str) -> Result<(), Rejected> {
    let forbidden = |why: &str| Err(reject("PATCH_PATH_FORBIDDEN", format!("'{file}': {why}")));
    let normalized = file.replace('\\', "/");
    if normalized.starts_with('/')
        || normalized.split('/').any(|s| s == ".." || s == ".")
        || normalized.contains(':')
    {
        return forbidden("path is outside the project");
    }
    if normalized == project_file {
        return Ok(());
    }
    let name = normalized.rsplit('/').next().unwrap_or_default();
    if normalized.starts_with("acceptance/") {
        return forbidden("protected acceptance tests are immutable to repair");
    }
    if [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "package.json",
        "package-lock.json",
        "build.rs",
    ]
    .contains(&name)
    {
        return forbidden("dependency and toolchain files are protected");
    }
    if normalized.starts_with(".nari/") {
        return forbidden("generated artifacts are not canonical source");
    }
    forbidden("only the project's .nr source may be edited")
}

pub fn apply_text(p: &Patch, project_file: &str, src: &str) -> Result<String, Rejected> {
    if p.schema != SCHEMA {
        return Err(reject(
            "PATCH_INVALID",
            format!("unsupported patch schema '{}'", p.schema),
        ));
    }
    if p.edits.is_empty() {
        return Err(reject("PATCH_INVALID", "patch contains no edits"));
    }
    for e in &p.edits {
        classify_path(&e.file, project_file)?;
    }
    if ir::sha256_hex(src.as_bytes()) != p.base_source_sha256 {
        return Err(reject(
            "PATCH_STALE",
            "source changed since the patch was captured; recapture context and re-propose",
        ));
    }
    let mut edits: Vec<&Edit> = p.edits.iter().collect();
    edits.sort_by_key(|e| e.start);
    for w in edits.windows(2) {
        if w[0].end > w[1].start {
            return Err(reject("PATCH_INVALID", "edits overlap"));
        }
    }
    let mut out = src.to_string();
    for e in edits.iter().rev() {
        if e.start > e.end
            || e.end > src.len()
            || !src.is_char_boundary(e.start)
            || !src.is_char_boundary(e.end)
        {
            return Err(reject(
                "PATCH_INVALID",
                format!("edit range {}..{} is invalid", e.start, e.end),
            ));
        }
        out.replace_range(e.start..e.end, &e.replacement);
    }
    Ok(out)
}

fn masked(c: &Compiled, editable: &[String]) -> Json {
    let mut app = ir::to_json(&c.app);
    if let Some(actions) = app["actions"].as_array_mut() {
        for a in actions {
            if a["name"]
                .as_str()
                .is_some_and(|n| editable.iter().any(|e| e == n))
            {
                a["set"] = Json::Null;
            }
        }
    }
    app
}

pub fn check_scope(
    base: &Compiled,
    candidate: &Compiled,
    allowed: &[String],
) -> Result<Vec<String>, Rejected> {
    let editable: Vec<String> = base
        .app
        .repair
        .editable_actions
        .iter()
        .filter(|a| {
            allowed
                .iter()
                .any(|x| x == *a || x.ends_with(&format!("/action/{a}")))
        })
        .cloned()
        .collect();
    let (b, c) = (masked(base, &editable), masked(candidate, &editable));
    if b != c {
        let mut changed = Vec::new();
        for key in [
            "identity", "runtime", "recovery", "repair", "targets", "entities", "screens",
        ] {
            if b[key] != c[key] {
                changed.push(key.to_string());
            }
        }
        let (ba, ca) = (b["actions"].as_array(), c["actions"].as_array());
        if ba.map(Vec::len) != ca.map(Vec::len) {
            changed.push("actions added or removed".into());
        } else if let (Some(ba), Some(ca)) = (ba, ca) {
            for (x, y) in ba.iter().zip(ca) {
                if x != y {
                    changed.push(format!("action {}", x["name"].as_str().unwrap_or("?")));
                }
            }
        }
        return Err(reject(
            "PATCH_SCOPE_VIOLATION",
            format!(
                "patch changes protected declarations outside the editable set bindings of {:?}: {}",
                editable,
                changed.join(", ")
            ),
        ));
    }
    let changed: Vec<String> = base
        .app
        .actions
        .iter()
        .filter(|a| editable.contains(&a.name))
        .filter(|a| {
            candidate
                .app
                .action(&a.name)
                .is_some_and(|x| x.set != a.set)
        })
        .map(|a| a.id.clone())
        .collect();
    if changed.is_empty() {
        return Err(reject(
            "PATCH_INVALID",
            "patch does not change any editable set binding",
        ));
    }
    Ok(changed)
}
