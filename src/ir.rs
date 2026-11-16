use crate::diag::Span;
use serde::Serialize;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const IR_SCHEMA: &str = "nari-ir/0.1";
pub const LANGUAGE_VERSION: &str = "0.1";
pub const COMPILER_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct App {
    pub id: String,
    pub name: String,
    pub targets: Vec<String>,
    pub identity: Identity,
    pub entities: Vec<Entity>,
    pub actions: Vec<Action>,
    pub screens: Vec<Screen>,
    pub runtime: Runtime,
    pub recovery: Recovery,
    pub repair: Repair,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Identity {
    pub id: String,
    pub name: String,
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "name")]
pub enum Type {
    Text {
        min_length: u32,
        max_length: u32,
    },
    Bool,
    #[serde(rename = "UUID")]
    Uuid,
    Int,
}

impl Type {
    pub fn base(&self) -> &'static str {
        match self {
            Type::Text { .. } => "Text",
            Type::Bool => "Bool",
            Type::Uuid => "UUID",
            Type::Int => "Int",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Type::Text {
                min_length,
                max_length,
            } => format!("Text[{min_length}..{max_length}]"),
            other => other.base().into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum Literal {
    Text(String),
    Bool(bool),
    #[serde(rename = "UUID")]
    Uuid(String),
    Int(i64),
}

impl Literal {
    pub fn to_json(&self) -> Json {
        match self {
            Literal::Text(s) | Literal::Uuid(s) => Json::String(s.clone()),
            Literal::Bool(b) => Json::Bool(*b),
            Literal::Int(n) => Json::from(*n),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Field {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: Type,
    pub default: Option<Literal>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Entity {
    pub id: String,
    pub name: String,
    pub owner_field: String,
    pub implicit_fields: Vec<Field>,
    pub fields: Vec<Field>,
}

impl Entity {
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn any_field(&self, name: &str) -> Option<&Field> {
        self.implicit_fields
            .iter()
            .chain(&self.fields)
            .find(|f| f.name == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Insert,
    List,
    Update,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Insert => "insert",
            ActionKind::List => "list",
            ActionKind::Update => "update",
        }
    }

    pub fn mutates(self) -> bool {
        self != ActionKind::List
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Input {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub ty: Type,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum Binding {
    Input { input: String },
    ActorId,
    Literal { value: Literal },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Assign {
    pub id: String,
    pub field: String,
    pub binding: Binding,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Contract {
    pub transport: String,
    pub inputs: BTreeMap<String, String>,
    pub output: String,
    pub errors: Vec<String>,
    pub effects: Vec<String>,
    pub requires_idempotency_key: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub kind: ActionKind,
    pub entity: String,
    pub access: String,
    pub inputs: Vec<Input>,
    pub id_input: Option<String>,
    pub revision_input: Option<String>,
    pub set: Vec<Assign>,
    pub contract: Contract,
}

impl Action {
    pub fn input(&self, name: &str) -> Option<&Input> {
        self.inputs.iter().find(|i| i.name == name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Screen {
    pub id: String,
    pub name: String,
    pub route: String,
    pub title: String,
    pub query: String,
    pub forms: Vec<Form>,
    pub lists: Vec<ListView>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Form {
    pub id: String,
    pub name: String,
    pub submit: String,
    pub inputs: Vec<FormInput>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FormInput {
    pub id: String,
    pub name: String,
    pub label: String,
    pub placeholder: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ListView {
    pub id: String,
    pub name: String,
    pub source: String,
    pub fields: Vec<String>,
    pub buttons: Vec<Button>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Button {
    pub id: String,
    pub name: String,
    pub label: String,
    pub action: String,
    pub arguments: Vec<Argument>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Argument {
    pub input: String,
    pub binding: ArgBinding,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "from", rename_all = "snake_case")]
pub enum ArgBinding {
    Item { field: String },
    Literal { value: Literal },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Runtime {
    pub id: String,
    pub name: String,
    pub bind: String,
    pub port: u16,
    pub database: String,
    pub request_limit_bytes: u32,
    pub request_timeout_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Recovery {
    pub id: String,
    pub name: String,
    pub target: String,
    pub max_restarts: u32,
    pub window_seconds: u32,
    pub backoff_ms: Vec<u32>,
    pub after_exhaustion: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Repair {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub max_attempts: u32,
    pub editable_actions: Vec<String>,
    pub automatic_release: bool,
}

impl App {
    pub fn entity(&self, name: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.name == name)
    }

    pub fn action(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|a| a.name == name)
    }

    pub fn action_entity(&self, a: &Action) -> &Entity {
        self.entity(&a.entity)
            .expect("validated IR resolves every action entity")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Node {
    pub id: String,
    pub kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct Edge {
    pub from: String,
    pub role: String,
    pub to: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl Graph {
    pub fn forward(&self, id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.from == id).collect()
    }

    pub fn reverse(&self, id: &str) -> Vec<&Edge> {
        self.edges.iter().filter(|e| e.to == id).collect()
    }

    pub fn kind(&self, id: &str) -> Option<&str> {
        self.nodes
            .binary_search_by(|n| n.id.as_str().cmp(id))
            .ok()
            .map(|i| self.nodes[i].kind.as_str())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Compiled {
    pub app: App,
    pub graph: Graph,
    pub file: String,
    pub spans: BTreeMap<String, Span>,
    pub source_sha256: String,
    pub semantic_sha256: String,
    pub graph_sha256: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn canonical(v: &Json) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out, 0, true);
    out
}

pub fn canonical_compact(v: &Json) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out, 0, false);
    out
}

fn write_canonical(v: &Json, out: &mut String, indent: usize, pretty: bool) {
    let pad = |out: &mut String, n: usize| {
        if pretty {
            out.push('\n');
            for _ in 0..n {
                out.push_str("  ");
            }
        }
    };
    match v {
        Json::Object(m) if !m.is_empty() => {
            let keys: BTreeSet<&String> = m.keys().collect();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                pad(out, indent + 1);
                out.push_str(&Json::String(k.clone()).to_string());
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                write_canonical(&m[k], out, indent + 1, pretty);
            }
            pad(out, indent);
            out.push('}');
        }
        Json::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                pad(out, indent + 1);
                write_canonical(item, out, indent + 1, pretty);
            }
            pad(out, indent);
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

pub fn to_json<T: Serialize>(v: &T) -> Json {
    serde_json::to_value(v).expect("IR types serialize infallibly")
}

pub fn semantic_hash(app: &App) -> String {
    sha256_hex(canonical_compact(&to_json(app)).as_bytes())
}

pub fn graph_hash(g: &Graph) -> String {
    sha256_hex(canonical_compact(&to_json(g)).as_bytes())
}

impl Compiled {
    pub fn ir_json(&self) -> Json {
        let mut forward: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let mut reverse: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for e in &self.graph.edges {
            forward
                .entry(&e.from)
                .or_default()
                .push(format!("{}:{}", e.role, e.to));
            reverse
                .entry(&e.to)
                .or_default()
                .push(format!("{}:{}", e.role, e.from));
        }
        serde_json::json!({
            "schema": IR_SCHEMA,
            "language": LANGUAGE_VERSION,
            "compiler": format!("nari {COMPILER_VERSION}"),
            "app": to_json(&self.app),
            "graph": to_json(&self.graph),
            "dependencies": { "forward": forward, "reverse": reverse },
            "source_map": { "file": self.file, "spans": to_json(&self.spans) },
            "hashes": {
                "source_sha256": self.source_sha256,
                "semantic_sha256": self.semantic_sha256,
                "graph_sha256": self.graph_sha256,
            },
        })
    }
}
