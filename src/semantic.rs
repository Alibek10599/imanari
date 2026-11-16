use crate::diag::{Diagnostic, Span};
use crate::ir::*;
use crate::syntax::{Block, Document, Entry, Property, Value, ValueKind};
use std::collections::{BTreeMap, BTreeSet};

const UNSUPPORTED_BLOCKS: &[&str] = &[
    "workflow",
    "effect",
    "adapter",
    "extension",
    "function",
    "job",
    "deployment",
    "infrastructure",
    "policy",
    "migration",
    "test",
    "secret",
];
fn issuer_problem(v: &str) -> Option<&'static str> {
    let rest = if let Some(r) = v.strip_prefix("https://") {
        r
    } else if let Some(r) = v.strip_prefix("http://") {
        let host = r.split(['/', ':']).next().unwrap_or_default();
        if host != "127.0.0.1" && host != "localhost" {
            return Some("must use https (plain http is allowed only for 127.0.0.1 and localhost)");
        }
        r
    } else {
        return Some("must be an https URL");
    };
    if rest.is_empty() || rest.starts_with('/') {
        return Some("has no host");
    }
    if rest.contains(['?', '#', ' ', '@']) || !rest.is_ascii() {
        return Some("must not contain a query, fragment, credentials or spaces");
    }
    None
}

const RESERVED_ROUTES: &[&str] = &["/api", "/_nari", "/assets"];
const RESERVED_DB_DIRS: &[&str] = &[
    "generated",
    "build",
    "context",
    "incidents",
    "repair",
    "test",
    "run",
    "control.db",
    "ir.json",
    "graph.json",
    "manifest.json",
];

pub fn check(doc: &Document, src: &str) -> Result<(Compiled, Vec<Diagnostic>), Vec<Diagnostic>> {
    let mut c = Checker {
        file: &doc.file,
        app: doc.app.name.clone(),
        diags: Vec::new(),
        spans: BTreeMap::new(),
    };
    let app = c.app_decl(doc);
    let diags: Vec<Diagnostic> = c.diags.into_iter().map(|d| d.locate(src)).collect();
    match app {
        Some(app) if !diags.iter().any(Diagnostic::is_error) => {
            let graph = build_graph(&app);
            let compiled = Compiled {
                semantic_sha256: semantic_hash(&app),
                graph_sha256: graph_hash(&graph),
                source_sha256: sha256_hex(src.as_bytes()),
                file: doc.file.clone(),
                spans: c.spans,
                app,
                graph,
            };
            Ok((compiled, diags))
        }
        _ => Err(diags),
    }
}

struct Checker<'a> {
    file: &'a str,
    app: String,
    diags: Vec<Diagnostic>,
    spans: BTreeMap<String, Span>,
}

struct Scope<'a> {
    id: String,
    span: Span,
    props: BTreeMap<&'a str, &'a Property>,
    blocks: Vec<&'a Block>,
}

impl<'a> Scope<'a> {
    fn blocks(&self, kind: &str) -> impl Iterator<Item = &'a Block> + '_ {
        let kind = kind.to_string();
        self.blocks
            .iter()
            .copied()
            .filter(move |b| b.kind.name == kind)
    }
}

fn is_canonical_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
}

impl<'a> Checker<'a> {
    fn err(&mut self, code: &str, msg: impl Into<String>, span: Span, node: &str) {
        self.diags
            .push(Diagnostic::error(code, msg, self.file, span).node(node));
    }

    fn id(&self, parts: &[&str]) -> String {
        let mut s = self.app.clone();
        for p in parts {
            s.push('/');
            s.push_str(p);
        }
        s
    }

    fn scope(
        &mut self,
        id: String,
        span: Span,
        entries: &'a [Entry],
        props: &[&str],
        blocks: &[&str],
        top_level: bool,
    ) -> Scope<'a> {
        self.spans.insert(id.clone(), span);
        let mut s = Scope {
            id,
            span,
            props: BTreeMap::new(),
            blocks: Vec::new(),
        };
        let mut seen_blocks: BTreeMap<(String, String), Span> = BTreeMap::new();
        for e in entries {
            match e {
                Entry::Property(p) => {
                    let key = p.key.name.as_str();
                    if !props.contains(&key) {
                        let allowed = if props.is_empty() {
                            "none".to_string()
                        } else {
                            props.join(", ")
                        };
                        self.err(
                            "E_UNKNOWN_PROPERTY",
                            format!("unknown property '{key}' (allowed here: {allowed})"),
                            p.key.span,
                            &s.id,
                        );
                    } else if let Some(prev) = s.props.insert(key, p) {
                        self.err(
                            "E_DUPLICATE",
                            format!(
                                "duplicate property '{key}' (first defined at byte {})",
                                prev.span.start
                            ),
                            p.key.span,
                            &s.id,
                        );
                    } else {
                        self.spans.insert(format!("{}#{key}", s.id), p.value.span);
                    }
                }
                Entry::Block(b) => {
                    let kind = b.kind.name.as_str();
                    if !blocks.contains(&kind) {
                        if top_level && UNSUPPORTED_BLOCKS.contains(&kind) {
                            self.err(
                                "E_UNSUPPORTED_FEATURE",
                                format!("'{kind}' declarations are not supported in Imanari 0.1"),
                                b.kind.span,
                                &s.id,
                            );
                        } else {
                            let allowed = if blocks.is_empty() {
                                "none".to_string()
                            } else {
                                blocks.join(", ")
                            };
                            self.err(
                                "E_UNKNOWN_BLOCK",
                                format!("unknown block '{kind}' (allowed here: {allowed})"),
                                b.kind.span,
                                &s.id,
                            );
                        }
                        continue;
                    }
                    let key = (kind.to_string(), b.name.name.clone());
                    if let Some(prev) = seen_blocks.insert(key, b.span) {
                        self.err(
                            "E_DUPLICATE",
                            format!(
                                "duplicate {kind} '{}' (first declared at byte {})",
                                b.name.name, prev.start
                            ),
                            b.name.span,
                            &s.id,
                        );
                        continue;
                    }
                    s.blocks.push(b);
                }
            }
        }
        s
    }

    fn require(&mut self, s: &Scope<'a>, key: &str) -> Option<&'a Value> {
        match s.props.get(key) {
            Some(p) => Some(&p.value),
            None => {
                self.err(
                    "E_MISSING_PROPERTY",
                    format!("missing required property '{key}'"),
                    s.span,
                    &s.id,
                );
                None
            }
        }
    }

    fn type_err(&mut self, s: &Scope, v: &Value, expected: &str) {
        self.err(
            "E_TYPE",
            format!("expected {expected}, found {}", v.kind.describe()),
            v.span,
            &s.id,
        );
    }

    fn symbol(&mut self, s: &Scope<'a>, key: &str) -> Option<(&'a str, Span)> {
        let v = self.require(s, key)?;
        match &v.kind {
            ValueKind::Symbol(sym) => Some((sym.as_str(), v.span)),
            _ => {
                self.type_err(s, v, "a symbol");
                None
            }
        }
    }

    fn one_of(
        &mut self,
        s: &Scope<'a>,
        key: &str,
        allowed: &[&str],
        unsupported: &[&str],
    ) -> Option<&'a str> {
        let (sym, span) = self.symbol(s, key)?;
        if allowed.contains(&sym) {
            return Some(sym);
        }
        if unsupported.contains(&sym) {
            self.err(
                "E_UNSUPPORTED_FEATURE",
                format!("{key} = {sym} is not supported in Imanari 0.1"),
                span,
                &s.id,
            );
        } else {
            self.err(
                "E_VALUE",
                format!(
                    "invalid {key} '{sym}' (expected one of: {})",
                    allowed.join(", ")
                ),
                span,
                &s.id,
            );
        }
        None
    }

    fn int_value(&mut self, s: &Scope, v: &Value, name: &str, lo: i64, hi: i64) -> Option<i64> {
        match v.kind {
            ValueKind::Integer(n) if (lo..=hi).contains(&n) => Some(n),
            ValueKind::Integer(n) => {
                self.err(
                    "E_RANGE",
                    format!("{name} = {n} is outside {lo}..{hi}"),
                    v.span,
                    &s.id,
                );
                None
            }
            _ => {
                self.type_err(s, v, "an integer");
                None
            }
        }
    }

    fn int(&mut self, s: &Scope<'a>, key: &str, lo: i64, hi: i64) -> Option<i64> {
        let v = self.require(s, key)?;
        self.int_value(s, v, key, lo, hi)
    }

    fn string(&mut self, s: &Scope<'a>, key: &str, required: bool) -> Option<(&'a str, Span)> {
        let v = if required {
            self.require(s, key)?
        } else {
            &s.props.get(key)?.value
        };
        match &v.kind {
            ValueKind::String(text) if text.trim().is_empty() => {
                self.err("E_VALUE", format!("{key} must not be empty"), v.span, &s.id);
                None
            }
            ValueKind::String(text) => Some((text.as_str(), v.span)),
            _ => {
                self.type_err(s, v, "a string");
                None
            }
        }
    }

    fn app_decl(&mut self, doc: &'a Document) -> Option<App> {
        let app_id = self.app.clone();
        if doc.version != LANGUAGE_VERSION {
            self.err(
                "E_UNSUPPORTED_VERSION",
                format!(
                    "imanari \"{}\" is not supported; this compiler implements \"{LANGUAGE_VERSION}\"",
                    doc.version
                ),
                doc.version_span,
                &app_id,
            );
        }
        let top = self.scope(
            app_id.clone(),
            doc.span,
            &doc.entries,
            &["targets"],
            &[
                "identity", "entity", "action", "screen", "runtime", "recovery", "repair",
            ],
            true,
        );
        let targets = self.targets(&top);
        for kind in ["identity", "runtime", "recovery", "repair"] {
            let found: Vec<&Block> = top.blocks(kind).collect();
            if found.is_empty() {
                self.err(
                    "E_MISSING_BLOCK",
                    format!("exactly one {kind} block is required"),
                    doc.app.span,
                    &app_id,
                );
            }
            for extra in found.iter().skip(1) {
                self.err(
                    "E_DUPLICATE",
                    format!("only one {kind} block is allowed"),
                    extra.name.span,
                    &app_id,
                );
            }
        }
        let identity = top.blocks("identity").next().and_then(|b| self.identity(b));
        let mut entities: Vec<Entity> = top
            .blocks("entity")
            .filter_map(|b| self.entity(b))
            .collect();
        entities.sort_by(|a, b| a.name.cmp(&b.name));
        let entity_blocks: BTreeSet<&str> =
            top.blocks("entity").map(|b| b.name.name.as_str()).collect();
        let mut actions: Vec<Action> = top
            .blocks("action")
            .filter_map(|b| self.action(b, &entities, &entity_blocks))
            .collect();
        actions.sort_by(|a, b| a.name.cmp(&b.name));
        let action_blocks: BTreeSet<&str> =
            top.blocks("action").map(|b| b.name.name.as_str()).collect();
        let mut screens = Vec::new();
        let mut routes: BTreeMap<String, Span> = BTreeMap::new();
        for b in top.blocks("screen") {
            if let Some(scr) = self.screen(b, &entities, &actions, &action_blocks) {
                let route_span = self.spans[&format!("{}#route", scr.id)];
                if routes.insert(scr.route.clone(), route_span).is_some() {
                    self.err(
                        "E_DUPLICATE",
                        format!("route '{}' is used by more than one screen", scr.route),
                        route_span,
                        &scr.id,
                    );
                }
                screens.push(scr);
            }
        }
        screens.sort_by(|a, b| a.name.cmp(&b.name));
        let runtime = top.blocks("runtime").next().and_then(|b| self.runtime(b));
        let recovery = top.blocks("recovery").next().and_then(|b| self.recovery(b));
        let repair = top
            .blocks("repair")
            .next()
            .and_then(|b| self.repair(b, &actions, &action_blocks));
        Some(App {
            id: app_id,
            name: self.app.clone(),
            targets: targets?,
            identity: identity?,
            entities,
            actions,
            screens,
            runtime: runtime?,
            recovery: recovery?,
            repair: repair?,
        })
    }

    fn targets(&mut self, top: &Scope<'a>) -> Option<Vec<String>> {
        let v = self.require(top, "targets")?;
        let ValueKind::Array(items) = &v.kind else {
            self.type_err(top, v, "an array of targets");
            return None;
        };
        let mut out = Vec::new();
        for item in items {
            match &item.kind {
                ValueKind::Symbol(t) if t == "web" => {
                    if out.contains(t) {
                        self.err("E_DUPLICATE", "duplicate target 'web'", item.span, &top.id);
                    }
                    out.push(t.clone());
                }
                ValueKind::Symbol(t)
                    if ["ios", "android", "mobile", "desktop", "cloud"].contains(&t.as_str()) =>
                {
                    self.err(
                        "E_UNSUPPORTED_TARGET",
                        format!(
                            "target '{t}' is not supported in Imanari 0.1; only web is available"
                        ),
                        item.span,
                        &top.id,
                    );
                }
                ValueKind::Symbol(t) => self.err(
                    "E_VALUE",
                    format!("unknown target '{t}' (expected web)"),
                    item.span,
                    &top.id,
                ),
                _ => self.type_err(top, item, "a target symbol"),
            }
        }
        if out.is_empty() {
            self.err("E_VALUE", "targets must be exactly [web]", v.span, &top.id);
            return None;
        }
        Some(out)
    }

    fn identity(&mut self, b: &'a Block) -> Option<Identity> {
        let id = self.id(&["identity", &b.name.name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["mode", "issuer", "client_id"],
            &[],
            false,
        );
        let mode = self.one_of(
            &s,
            "mode",
            &["local_dev", "oidc"],
            &["oauth", "saml", "password", "production"],
        )?;
        if mode == "local_dev" {
            for key in ["issuer", "client_id"] {
                if let Some(p) = s.props.get(key) {
                    self.err(
                        "E_UNKNOWN_PROPERTY",
                        format!("{key} applies only to mode = oidc"),
                        p.key.span,
                        &id,
                    );
                }
            }
            return Some(Identity {
                id,
                name: b.name.name.clone(),
                mode: mode.into(),
                issuer: None,
                client_id: None,
            });
        }
        let issuer =
            self.string(&s, "issuer", true)
                .and_then(|(v, span)| match issuer_problem(v) {
                    Some(p) => {
                        self.err("E_VALUE", format!("issuer '{v}' {p}"), span, &id);
                        None
                    }
                    None => Some(v.to_string()),
                });
        let client_id = self.string(&s, "client_id", true).and_then(|(v, span)| {
            if v.len() <= 255 && v.bytes().all(|c| c.is_ascii_graphic()) {
                Some(v.to_string())
            } else {
                self.err(
                    "E_VALUE",
                    "client_id must be 1..255 visible ASCII characters",
                    span,
                    &id,
                );
                None
            }
        });
        Some(Identity {
            id,
            name: b.name.name.clone(),
            mode: mode.into(),
            issuer: Some(issuer?),
            client_id: Some(client_id?),
        })
    }

    fn typed(&mut self, s: &Scope<'a>, allow_default: bool) -> Option<(Type, Option<Literal>)> {
        let (sym, span) = self.symbol(s, "type")?;
        let has_bounds = s.props.contains_key("min_length") || s.props.contains_key("max_length");
        let ty = match sym {
            "Text" => {
                let min = match s.props.get("min_length") {
                    Some(p) => self.int_value(s, &p.value, "min_length", 0, 65_536)?,
                    None => 0,
                };
                let max = match s.props.get("max_length") {
                    Some(p) => self.int_value(s, &p.value, "max_length", 0, 65_536)?,
                    None => 4096,
                };
                if min > max {
                    self.err(
                        "E_RANGE",
                        format!("min_length {min} exceeds max_length {max}"),
                        s.span,
                        &s.id,
                    );
                    return None;
                }
                Type::Text {
                    min_length: min as u32,
                    max_length: max as u32,
                }
            }
            "Bool" | "UUID" | "Int" if has_bounds => {
                self.err(
                    "E_TYPE",
                    format!("min_length/max_length apply only to Text, not {sym}"),
                    span,
                    &s.id,
                );
                return None;
            }
            "Bool" => Type::Bool,
            "UUID" => Type::Uuid,
            "Int" => Type::Int,
            "Float" | "Decimal" | "Date" | "DateTime" | "Json" | "Bytes" => {
                self.err(
                    "E_UNSUPPORTED_FEATURE",
                    format!("type {sym} is not supported in Imanari 0.1 (Text, Bool, UUID, Int)"),
                    span,
                    &s.id,
                );
                return None;
            }
            other => {
                self.err(
                    "E_TYPE",
                    format!("unknown type '{other}' (expected Text, Bool, UUID or Int)"),
                    span,
                    &s.id,
                );
                return None;
            }
        };
        let default = match (allow_default, s.props.get("default")) {
            (true, Some(p)) => Some(self.literal(s, &p.value, &ty)?),
            _ => None,
        };
        Some((ty, default))
    }

    fn literal(&mut self, s: &Scope, v: &Value, ty: &Type) -> Option<Literal> {
        match (ty, &v.kind) {
            (
                Type::Text {
                    min_length,
                    max_length,
                },
                ValueKind::String(text),
            ) => {
                let n = text.chars().count();
                if n < *min_length as usize || n > *max_length as usize {
                    self.err(
                        "E_CONSTRAINT",
                        format!("text literal has {n} characters; {} requires {min_length}..{max_length}", ty.describe()),
                        v.span,
                        &s.id,
                    );
                    return None;
                }
                Some(Literal::Text(text.clone()))
            }
            (Type::Bool, ValueKind::Bool(b)) => Some(Literal::Bool(*b)),
            (Type::Int, ValueKind::Integer(n)) => Some(Literal::Int(*n)),
            (Type::Uuid, ValueKind::String(u)) if is_canonical_uuid(u) => {
                Some(Literal::Uuid(u.clone()))
            }
            (Type::Uuid, ValueKind::String(_)) => {
                self.err(
                    "E_TYPE",
                    "UUID literals must be canonical lowercase strings",
                    v.span,
                    &s.id,
                );
                None
            }
            _ => {
                self.type_err(s, v, &format!("a {} literal", ty.base()));
                None
            }
        }
    }

    fn entity(&mut self, b: &'a Block) -> Option<Entity> {
        let id = self.id(&["entity", &b.name.name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["owner_field"],
            &["field"],
            false,
        );
        let mut fields = Vec::new();
        let mut ok = true;
        for fb in s.blocks("field") {
            let name = fb.name.name.as_str();
            let fid = format!("{id}/field/{name}");
            let fs = self.scope(
                fid.clone(),
                fb.span,
                &fb.entries,
                &["type", "min_length", "max_length", "default"],
                &[],
                false,
            );
            if name == "id" || name == "revision" {
                self.err(
                    "E_RESERVED_FIELD",
                    format!("'{name}' is a reserved implicit field and cannot be declared"),
                    fb.name.span,
                    &fid,
                );
                ok = false;
                continue;
            }
            match self.typed(&fs, true) {
                Some((ty, default)) => fields.push(Field {
                    id: fid,
                    name: name.into(),
                    ty,
                    default,
                }),
                None => ok = false,
            }
        }
        fields.sort_by(|a, b| a.name.cmp(&b.name));
        let (owner, owner_span) = self.symbol(&s, "owner_field")?;
        match fields.iter().find(|f| f.name == owner) {
            Some(f) if f.ty != Type::Uuid => {
                self.err(
                    "E_OWNER_FIELD",
                    format!("owner_field '{owner}' must have type UUID"),
                    owner_span,
                    &id,
                );
                ok = false;
            }
            Some(f) if f.default.is_some() => {
                self.err(
                    "E_OWNER_FIELD",
                    format!("owner_field '{owner}' cannot declare a default"),
                    owner_span,
                    &id,
                );
                ok = false;
            }
            Some(_) => {}
            None if ok => {
                self.err(
                    "E_OWNER_FIELD",
                    format!("owner_field '{owner}' does not name a declared field"),
                    owner_span,
                    &id,
                );
                ok = false;
            }
            None => ok = false,
        }
        let implicit = |n: &str, ty: Type| Field {
            id: format!("{id}/field/{n}"),
            name: n.into(),
            ty,
            default: None,
        };
        ok.then(|| Entity {
            implicit_fields: vec![implicit("id", Type::Uuid), implicit("revision", Type::Int)],
            id: id.clone(),
            name: b.name.name.clone(),
            owner_field: owner.into(),
            fields,
        })
    }

    fn input_ref(
        &mut self,
        s: &Scope<'a>,
        key: &str,
        inputs: &[Input],
        want: &Type,
    ) -> Option<String> {
        let v = self.require(s, key)?;
        let ValueKind::Reference(path) = &v.kind else {
            self.type_err(s, v, "an @input reference");
            return None;
        };
        let names: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
        let [root, name] = names.as_slice() else {
            self.err("E_REFERENCE", "expected @input.<name>", v.span, &s.id);
            return None;
        };
        if *root != "input" {
            self.err(
                "E_REFERENCE_SCOPE",
                format!("{key} must reference @input, not @{root}"),
                v.span,
                &s.id,
            );
            return None;
        }
        let Some(input) = inputs.iter().find(|i| i.name == *name) else {
            self.err(
                "E_UNRESOLVED",
                format!("unknown input '{name}'"),
                v.span,
                &s.id,
            );
            return None;
        };
        if input.ty.base() != want.base() {
            self.err(
                "E_TYPE",
                format!(
                    "{key} requires a {} input, but '{name}' is {}",
                    want.base(),
                    input.ty.base()
                ),
                v.span,
                &s.id,
            );
            return None;
        }
        Some(input.name.clone())
    }

    fn action(
        &mut self,
        b: &'a Block,
        entities: &[Entity],
        entity_blocks: &BTreeSet<&str>,
    ) -> Option<Action> {
        let name = b.name.name.as_str();
        let id = self.id(&["action", name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["kind", "entity", "access", "set", "id", "expected_revision"],
            &["input"],
            false,
        );
        let kind = self.one_of(
            &s,
            "kind",
            &["insert", "list", "update"],
            &[
                "delete", "upsert", "custom", "function", "workflow", "effect",
            ],
        );
        let (entity_name, entity_span) = self.symbol(&s, "entity")?;
        let access = self.one_of(
            &s,
            "access",
            &["signed_in"],
            &["public", "anonymous", "role", "policy"],
        );
        let entity = entities.iter().find(|e| e.name == entity_name);
        if entity.is_none() && !entity_blocks.contains(entity_name) {
            self.err(
                "E_UNRESOLVED",
                format!("unknown entity '{entity_name}'"),
                entity_span,
                &id,
            );
        }
        let kind = match kind? {
            "insert" => ActionKind::Insert,
            "list" => ActionKind::List,
            _ => ActionKind::Update,
        };
        let mut inputs = Vec::new();
        let mut inputs_ok = true;
        for ib in s.blocks("input") {
            let iname = ib.name.name.as_str();
            let iid = format!("{id}/input/{iname}");
            let is = self.scope(
                iid.clone(),
                ib.span,
                &ib.entries,
                &["type", "min_length", "max_length"],
                &[],
                false,
            );
            if kind == ActionKind::List {
                if iname == "limit" || iname == "after" {
                    self.err(
                        "E_RESERVED_INPUT",
                        format!("'{iname}' is a reserved list transport input"),
                        ib.name.span,
                        &iid,
                    );
                } else {
                    self.err(
                        "E_UNSUPPORTED_FEATURE",
                        "list actions cannot declare inputs in Imanari 0.1 (no filters)",
                        ib.name.span,
                        &iid,
                    );
                }
                inputs_ok = false;
                continue;
            }
            match self.typed(&is, false) {
                Some((ty, _)) => inputs.push(Input {
                    id: iid,
                    name: iname.into(),
                    ty,
                }),
                None => inputs_ok = false,
            }
        }
        inputs.sort_by(|a, b| a.name.cmp(&b.name));
        let entity = entity?;
        access?;
        let allowed_props: &[&str] = match kind {
            ActionKind::Insert => &["set"],
            ActionKind::List => &[],
            ActionKind::Update => &["set", "id", "expected_revision"],
        };
        for key in ["set", "id", "expected_revision"] {
            if let Some(p) = s.props.get(key).filter(|_| !allowed_props.contains(&key)) {
                self.err(
                    "E_UNKNOWN_PROPERTY",
                    format!("'{key}' is not valid for {} actions", kind.as_str()),
                    p.key.span,
                    &id,
                );
                inputs_ok = false;
            }
        }
        let mut used: BTreeSet<String> = BTreeSet::new();
        let (mut id_input, mut revision_input) = (None, None);
        if kind == ActionKind::Update {
            id_input = self.input_ref(&s, "id", &inputs, &Type::Uuid);
            revision_input = self.input_ref(&s, "expected_revision", &inputs, &Type::Int);
            used.extend(id_input.iter().cloned());
            used.extend(revision_input.iter().cloned());
            if id_input.is_none() || revision_input.is_none() {
                inputs_ok = false;
            }
        }
        let set = if kind == ActionKind::List {
            Vec::new()
        } else {
            let assigns = self.set(&s, kind, entity, &inputs, &mut used);
            match assigns {
                Some(a) => a,
                None => {
                    inputs_ok = false;
                    Vec::new()
                }
            }
        };
        for i in &inputs {
            if !used.contains(&i.name) {
                self.diags.push(
                    Diagnostic::warning(
                        "W_UNUSED_INPUT",
                        format!("input '{}' is never used", i.name),
                        self.file,
                        self.spans[&i.id],
                    )
                    .node(&i.id),
                );
            }
        }
        if !inputs_ok {
            return None;
        }
        let contract = contract(name, kind, entity, &inputs);
        Some(Action {
            id,
            name: name.into(),
            kind,
            entity: entity.name.clone(),
            access: "signed_in".into(),
            inputs,
            id_input,
            revision_input,
            set,
            contract,
        })
    }

    fn set(
        &mut self,
        s: &Scope<'a>,
        kind: ActionKind,
        entity: &Entity,
        inputs: &[Input],
        used: &mut BTreeSet<String>,
    ) -> Option<Vec<Assign>> {
        let v = self.require(s, "set")?;
        let ValueKind::Object(members) = &v.kind else {
            self.type_err(s, v, "an object of field assignments");
            return None;
        };
        let mut ok = true;
        let mut out = Vec::new();
        for (key, val) in members {
            let field_name = key.name.as_str();
            let aid = format!("{}/set/{field_name}", s.id);
            self.spans.insert(aid.clone(), val.span);
            if field_name == "id" || field_name == "revision" {
                self.err(
                    "E_RESERVED_FIELD",
                    format!("'{field_name}' is managed by the runtime and cannot be assigned"),
                    key.span,
                    &s.id,
                );
                ok = false;
                continue;
            }
            let Some(field) = entity.field(field_name) else {
                self.err(
                    "E_UNKNOWN_FIELD",
                    format!("entity {} has no field '{field_name}'", entity.name),
                    key.span,
                    &s.id,
                );
                ok = false;
                continue;
            };
            let is_owner = field_name == entity.owner_field;
            if is_owner && kind == ActionKind::Update {
                self.err(
                    "E_OWNER_REASSIGNMENT",
                    format!("update actions cannot reassign owner field '{field_name}'"),
                    key.span,
                    &s.id,
                );
                ok = false;
                continue;
            }
            let binding = match &val.kind {
                ValueKind::Reference(path) => {
                    let names: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
                    match names.as_slice() {
                        ["actor", "id"] if is_owner => Some(Binding::ActorId),
                        ["actor", ..] => {
                            self.err(
                                "E_REFERENCE_SCOPE",
                                "@actor.id may only assign the owner field of an insert",
                                val.span,
                                &s.id,
                            );
                            None
                        }
                        ["input", iname] => match inputs.iter().find(|i| i.name == *iname) {
                            Some(input) if input.ty.base() != field.ty.base() => {
                                self.err(
                                    "E_TYPE",
                                    format!("input '{iname}' is {} but field '{field_name}' is {} (no implicit casts)", input.ty.base(), field.ty.base()),
                                    val.span,
                                    &s.id,
                                );
                                None
                            }
                            Some(input) => {
                                used.insert(input.name.clone());
                                Some(Binding::Input {
                                    input: input.name.clone(),
                                })
                            }
                            None => {
                                self.err(
                                    "E_UNRESOLVED",
                                    format!("unknown input '{iname}'"),
                                    val.span,
                                    &s.id,
                                );
                                None
                            }
                        },
                        ["query" | "item", ..] => {
                            self.err(
                                "E_REFERENCE_SCOPE",
                                format!("@{} is only valid inside screens", names[0]),
                                val.span,
                                &s.id,
                            );
                            None
                        }
                        _ => {
                            self.err(
                                "E_UNRESOLVED",
                                format!("cannot resolve @{}", names.join(".")),
                                val.span,
                                &s.id,
                            );
                            None
                        }
                    }
                }
                _ => self
                    .literal(s, val, &field.ty)
                    .map(|value| Binding::Literal { value }),
            };
            if is_owner && kind == ActionKind::Insert && binding != Some(Binding::ActorId) {
                if binding.is_some() {
                    self.err(
                        "E_OWNER_ASSIGNMENT",
                        format!("insert must assign owner field '{field_name}' from @actor.id"),
                        val.span,
                        &s.id,
                    );
                }
                ok = false;
                continue;
            }
            match binding {
                Some(binding) => out.push(Assign {
                    id: aid,
                    field: field_name.into(),
                    binding,
                }),
                None => ok = false,
            }
        }
        if kind == ActionKind::Insert && ok {
            for f in &entity.fields {
                if f.default.is_none() && !out.iter().any(|a| a.field == f.name) {
                    self.err(
                        "E_MISSING_ASSIGNMENT",
                        format!("insert must assign field '{}' (it has no default)", f.name),
                        v.span,
                        &s.id,
                    );
                    ok = false;
                }
            }
        }
        if kind == ActionKind::Update && members.is_empty() {
            self.err(
                "E_EMPTY_UPDATE",
                "update set must assign at least one field",
                v.span,
                &s.id,
            );
            ok = false;
        }
        out.sort_by(|a, b| a.field.cmp(&b.field));
        ok.then_some(out)
    }

    fn screen(
        &mut self,
        b: &'a Block,
        entities: &[Entity],
        actions: &[Action],
        action_blocks: &BTreeSet<&str>,
    ) -> Option<Screen> {
        let name = b.name.name.as_str();
        let id = self.id(&["screen", name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["route", "title", "query"],
            &["form", "list"],
            false,
        );
        let route = self.string(&s, "route", true).and_then(|(r, span)| {
            let valid_chars = r
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c));
            let reserved = RESERVED_ROUTES
                .iter()
                .any(|p| r == *p || r.starts_with(&format!("{p}/")));
            let problem = if !r.starts_with('/') {
                Some("must be an absolute path starting with '/'")
            } else if r.contains('?') || r.contains('#') {
                Some("must not contain a query string or fragment")
            } else if r.split('/').any(|seg| seg == ".." || seg == ".") {
                Some("must not contain '.' or '..' segments")
            } else if r.contains("//") {
                Some("must not contain empty segments")
            } else if !valid_chars {
                Some("may only contain ASCII letters, digits, '/', '_', '.', '-'")
            } else if reserved {
                Some("collides with a reserved runtime path (/api, /_nari, /assets)")
            } else {
                None
            };
            match problem {
                Some(p) => {
                    self.err("E_ROUTE", format!("route '{r}' {p}"), span, &id);
                    None
                }
                None => Some(r.to_string()),
            }
        });
        let title = self.string(&s, "title", true).map(|(t, _)| t.to_string());
        let query = self.symbol(&s, "query").and_then(|(q, span)| {
            match actions.iter().find(|a| a.name == q) {
                Some(a) if a.kind == ActionKind::List => Some(a),
                Some(a) => {
                    self.err(
                        "E_KIND",
                        format!(
                            "screen query '{q}' must be a list action, not {}",
                            a.kind.as_str()
                        ),
                        span,
                        &id,
                    );
                    None
                }
                None => {
                    if !action_blocks.contains(q) {
                        self.err("E_UNRESOLVED", format!("unknown action '{q}'"), span, &id);
                    }
                    None
                }
            }
        });
        let query_entity = query.and_then(|q| entities.iter().find(|e| e.name == q.entity));
        let mut forms = Vec::new();
        let mut lists = Vec::new();
        let mut ok = true;
        for child in &s.blocks {
            match child.kind.name.as_str() {
                "form" => match self.form(&id, child, actions, action_blocks) {
                    Some(f) => forms.push(f),
                    None => ok = false,
                },
                _ => match query_entity
                    .and_then(|e| self.list(&id, child, e, actions, action_blocks))
                {
                    Some(l) => lists.push(l),
                    None => ok = false,
                },
            }
        }
        if !ok {
            return None;
        }
        Some(Screen {
            id,
            name: name.into(),
            route: route?,
            title: title?,
            query: query?.name.clone(),
            forms,
            lists,
        })
    }

    fn resolve_action<'b>(
        &mut self,
        s: &Scope<'a>,
        key: &str,
        kind: ActionKind,
        actions: &'b [Action],
        action_blocks: &BTreeSet<&str>,
    ) -> Option<&'b Action> {
        let (name, span) = self.symbol(s, key)?;
        match actions.iter().find(|a| a.name == name) {
            Some(a) if a.kind == kind => Some(a),
            Some(a) => {
                self.err(
                    "E_KIND",
                    format!(
                        "{key} '{name}' must reference a {} action, not {}",
                        kind.as_str(),
                        a.kind.as_str()
                    ),
                    span,
                    &s.id,
                );
                None
            }
            None => {
                if !action_blocks.contains(name) {
                    self.err(
                        "E_UNRESOLVED",
                        format!("unknown action '{name}'"),
                        span,
                        &s.id,
                    );
                }
                None
            }
        }
    }

    fn form(
        &mut self,
        screen: &str,
        b: &'a Block,
        actions: &[Action],
        action_blocks: &BTreeSet<&str>,
    ) -> Option<Form> {
        let id = format!("{screen}/form/{}", b.name.name);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["submit"],
            &["input"],
            false,
        );
        let action = self.resolve_action(&s, "submit", ActionKind::Insert, actions, action_blocks);
        let mut inputs = Vec::new();
        let mut ok = true;
        for ib in s.blocks("input") {
            let iid = format!("{id}/input/{}", ib.name.name);
            let is = self.scope(
                iid.clone(),
                ib.span,
                &ib.entries,
                &["label", "placeholder"],
                &[],
                false,
            );
            if let Some(a) = action
                && a.input(&ib.name.name).is_none()
            {
                self.err(
                    "E_UNRESOLVED",
                    format!("action '{}' has no input '{}'", a.name, ib.name.name),
                    ib.name.span,
                    &iid,
                );
                ok = false;
            }
            let label = self.string(&is, "label", true);
            let placeholder = self.string(&is, "placeholder", false);
            match label {
                Some((label, _)) => inputs.push(FormInput {
                    id: iid,
                    name: ib.name.name.clone(),
                    label: label.into(),
                    placeholder: placeholder.map(|(p, _)| p.to_string()),
                }),
                None => ok = false,
            }
        }
        let action = action?;
        for i in &action.inputs {
            if !inputs.iter().any(|fi| fi.name == i.name) {
                self.err(
                    "E_MISSING_CONTROL",
                    format!(
                        "form must provide a control for input '{}' of '{}'",
                        i.name, action.name
                    ),
                    b.name.span,
                    &id,
                );
                ok = false;
            }
        }
        ok.then(|| Form {
            id,
            name: b.name.name.clone(),
            submit: action.name.clone(),
            inputs,
        })
    }

    fn list(
        &mut self,
        screen: &str,
        b: &'a Block,
        entity: &Entity,
        actions: &[Action],
        action_blocks: &BTreeSet<&str>,
    ) -> Option<ListView> {
        let id = format!("{screen}/list/{}", b.name.name);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["source", "fields"],
            &["button"],
            false,
        );
        let mut ok = true;
        if let Some(v) = self.require(&s, "source") {
            let is_query_items = matches!(&v.kind, ValueKind::Reference(p) if p.len() == 2 && p[0].name == "query" && p[1].name == "items");
            if !is_query_items {
                self.err(
                    "E_REFERENCE",
                    "list source must be @query.items",
                    v.span,
                    &id,
                );
                ok = false;
            }
        } else {
            ok = false;
        }
        let mut fields = Vec::new();
        match self.require(&s, "fields").map(|v| (v, &v.kind)) {
            Some((_, ValueKind::Array(items))) if !items.is_empty() => {
                for item in items {
                    match &item.kind {
                        ValueKind::Symbol(f) if entity.any_field(f).is_some() => {
                            if fields.contains(f) {
                                self.err(
                                    "E_DUPLICATE",
                                    format!("field '{f}' listed twice"),
                                    item.span,
                                    &id,
                                );
                                ok = false;
                            }
                            fields.push(f.clone());
                        }
                        ValueKind::Symbol(f) => {
                            self.err(
                                "E_UNRESOLVED",
                                format!("entity {} has no field '{f}'", entity.name),
                                item.span,
                                &id,
                            );
                            ok = false;
                        }
                        _ => {
                            self.type_err(&s, item, "a field symbol");
                            ok = false;
                        }
                    }
                }
            }
            Some((v, _)) => {
                self.type_err(&s, v, "a non-empty array of field symbols");
                ok = false;
            }
            None => ok = false,
        }
        let mut buttons = Vec::new();
        for bb in s.blocks("button") {
            match self.button(&id, bb, entity, actions, action_blocks) {
                Some(btn) => buttons.push(btn),
                None => ok = false,
            }
        }
        ok.then(|| ListView {
            id,
            name: b.name.name.clone(),
            source: "query.items".into(),
            fields,
            buttons,
        })
    }

    fn button(
        &mut self,
        list: &str,
        b: &'a Block,
        entity: &Entity,
        actions: &[Action],
        action_blocks: &BTreeSet<&str>,
    ) -> Option<Button> {
        let id = format!("{list}/button/{}", b.name.name);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &["label", "action", "arguments"],
            &[],
            false,
        );
        let label = self.string(&s, "label", true);
        let action = self.resolve_action(&s, "action", ActionKind::Update, actions, action_blocks);
        let args_value = self.require(&s, "arguments");
        let action = action?;
        if action.entity != entity.name {
            let span = self.spans[&format!("{id}#action")];
            self.err(
                "E_KIND",
                format!(
                    "button action '{}' updates {}, but the list shows {}",
                    action.name, action.entity, entity.name
                ),
                span,
                &id,
            );
            return None;
        }
        let v = args_value?;
        let ValueKind::Object(members) = &v.kind else {
            self.type_err(&s, v, "an object of argument bindings");
            return None;
        };
        let mut ok = true;
        let mut arguments = Vec::new();
        for (key, val) in members {
            let Some(input) = action.input(&key.name) else {
                self.err(
                    "E_UNRESOLVED",
                    format!("action '{}' has no input '{}'", action.name, key.name),
                    key.span,
                    &id,
                );
                ok = false;
                continue;
            };
            let binding = match &val.kind {
                ValueKind::Reference(path) => {
                    let names: Vec<&str> = path.iter().map(|p| p.name.as_str()).collect();
                    match names.as_slice() {
                        ["item", f] => match entity.any_field(f) {
                            Some(field) if field.ty.base() == input.ty.base() => {
                                Some(ArgBinding::Item {
                                    field: f.to_string(),
                                })
                            }
                            Some(field) => {
                                self.err(
                                    "E_TYPE",
                                    format!(
                                        "@item.{f} is {} but input '{}' is {}",
                                        field.ty.base(),
                                        input.name,
                                        input.ty.base()
                                    ),
                                    val.span,
                                    &id,
                                );
                                None
                            }
                            None => {
                                self.err(
                                    "E_UNRESOLVED",
                                    format!("entity {} has no field '{f}'", entity.name),
                                    val.span,
                                    &id,
                                );
                                None
                            }
                        },
                        [root, ..] => {
                            self.err(
                                "E_REFERENCE_SCOPE",
                                format!(
                                    "button arguments may only reference @item fields, not @{root}"
                                ),
                                val.span,
                                &id,
                            );
                            None
                        }
                        [] => None,
                    }
                }
                _ => self
                    .literal(&s, val, &input.ty)
                    .map(|value| ArgBinding::Literal { value }),
            };
            match binding {
                Some(binding) => arguments.push(Argument {
                    input: input.name.clone(),
                    binding,
                }),
                None => ok = false,
            }
        }
        for i in &action.inputs {
            if !members.iter().any(|(k, _)| k.name == i.name) {
                self.err(
                    "E_MISSING_ARGUMENT",
                    format!("button must bind input '{}' of '{}'", i.name, action.name),
                    v.span,
                    &id,
                );
                ok = false;
            }
        }
        arguments.sort_by(|a, b| a.input.cmp(&b.input));
        let (label, _) = label?;
        ok.then(|| Button {
            id,
            name: b.name.name.clone(),
            label: label.into(),
            action: action.name.clone(),
            arguments,
        })
    }

    fn runtime(&mut self, b: &'a Block) -> Option<Runtime> {
        let id = self.id(&["runtime", &b.name.name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &[
                "bind",
                "port",
                "database",
                "request_limit_bytes",
                "request_timeout_ms",
            ],
            &[],
            false,
        );
        let bind = self.string(&s, "bind", true).and_then(|(v, span)| {
            if v == "127.0.0.1" {
                Some(v.to_string())
            } else {
                self.err(
                    "E_VALUE",
                    format!("bind '{v}' is not allowed; Imanari 0.1 binds only to 127.0.0.1"),
                    span,
                    &id,
                );
                None
            }
        });
        let port = self.int(&s, "port", 1024, 65535);
        let database =
            self.string(&s, "database", true)
                .and_then(|(v, span)| match database_problem(v) {
                    Some(p) => {
                        self.err("E_PATH", format!("database '{v}' {p}"), span, &id);
                        None
                    }
                    None => Some(v.to_string()),
                });
        let limit = self.int(&s, "request_limit_bytes", 1, 65_536);
        let timeout = self.int(&s, "request_timeout_ms", 100, 30_000);
        Some(Runtime {
            id,
            name: b.name.name.clone(),
            bind: bind?,
            port: port? as u16,
            database: database?,
            request_limit_bytes: limit? as u32,
            request_timeout_ms: timeout? as u32,
        })
    }

    fn recovery(&mut self, b: &'a Block) -> Option<Recovery> {
        let id = self.id(&["recovery", &b.name.name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &[
                "target",
                "max_restarts",
                "window_seconds",
                "backoff_ms",
                "after_exhaustion",
            ],
            &[],
            false,
        );
        let target = self.one_of(
            &s,
            "target",
            &["backend"],
            &["frontend", "database", "worker", "queue"],
        );
        let max = self.int(&s, "max_restarts", 0, 10);
        let window = self.int(&s, "window_seconds", 1, 3600);
        let backoff = self.require(&s, "backoff_ms").and_then(|v| {
            let ValueKind::Array(items) = &v.kind else {
                self.type_err(&s, v, "an array of integers");
                return None;
            };
            let vals: Vec<Option<i64>> = items
                .iter()
                .map(|i| self.int_value(&s, i, "backoff_ms", 0, 60_000))
                .collect();
            let vals: Option<Vec<u32>> = vals.into_iter().map(|v| v.map(|n| n as u32)).collect();
            if let (Some(vals), Some(max)) = (&vals, max)
                && vals.len() as i64 != max
            {
                self.err(
                    "E_RANGE",
                    format!(
                        "backoff_ms has {} entries but max_restarts is {max}",
                        vals.len()
                    ),
                    v.span,
                    &id,
                );
                return None;
            }
            vals
        });
        let after = self.one_of(
            &s,
            "after_exhaustion",
            &["stop"],
            &["rollback", "failover", "scale", "restart_forever"],
        );
        Some(Recovery {
            id,
            name: b.name.name.clone(),
            target: target?.into(),
            max_restarts: max? as u32,
            window_seconds: window? as u32,
            backoff_ms: backoff?,
            after_exhaustion: after?.into(),
        })
    }

    fn repair(
        &mut self,
        b: &'a Block,
        actions: &[Action],
        action_blocks: &BTreeSet<&str>,
    ) -> Option<Repair> {
        let id = self.id(&["repair", &b.name.name]);
        let s = self.scope(
            id.clone(),
            b.span,
            &b.entries,
            &[
                "mode",
                "max_attempts",
                "editable_actions",
                "automatic_release",
            ],
            &[],
            false,
        );
        let mode = self.one_of(
            &s,
            "mode",
            &["isolated"],
            &["in_place", "production", "live"],
        );
        let attempts = self.int(&s, "max_attempts", 0, 3);
        let editable = self.require(&s, "editable_actions").and_then(|v| {
            let ValueKind::Array(items) = &v.kind else {
                self.type_err(&s, v, "an array of action symbols");
                return None;
            };
            let mut out: Vec<String> = Vec::new();
            let mut ok = true;
            for item in items {
                let ValueKind::Symbol(name) = &item.kind else {
                    self.type_err(&s, item, "an action symbol");
                    ok = false;
                    continue;
                };
                match actions.iter().find(|a| &a.name == name) {
                    Some(a) if a.kind == ActionKind::List => {
                        self.err(
                            "E_KIND",
                            format!("'{name}' is a list action and has no editable set bindings"),
                            item.span,
                            &id,
                        );
                        ok = false;
                    }
                    Some(_) if out.contains(name) => {
                        self.err(
                            "E_DUPLICATE",
                            format!("'{name}' listed twice"),
                            item.span,
                            &id,
                        );
                        ok = false;
                    }
                    Some(_) => out.push(name.clone()),
                    None => {
                        if !action_blocks.contains(name.as_str()) {
                            self.err(
                                "E_UNRESOLVED",
                                format!("unknown action '{name}'"),
                                item.span,
                                &id,
                            );
                        }
                        ok = false;
                    }
                }
            }
            out.sort();
            ok.then_some(out)
        });
        let release = self.require(&s, "automatic_release").and_then(|v| match v.kind {
            ValueKind::Bool(false) => Some(false),
            ValueKind::Bool(true) => {
                self.err(
                    "E_UNSUPPORTED_FEATURE",
                    "automatic_release = true is not supported in Imanari 0.1; repairs require explicit local apply",
                    v.span,
                    &id,
                );
                None
            }
            _ => {
                self.type_err(&s, v, "false");
                None
            }
        });
        Some(Repair {
            id,
            name: b.name.name.clone(),
            mode: mode?.into(),
            max_attempts: attempts? as u32,
            editable_actions: editable?,
            automatic_release: release?,
        })
    }
}

fn database_problem(v: &str) -> Option<&'static str> {
    let Some(rest) = v.strip_prefix(".nari/") else {
        return Some("must be a relative path under .nari/");
    };
    let parts: Vec<&str> = rest.split('/').collect();
    if rest.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Some("must not contain empty, '.' or '..' segments");
    }
    if !rest
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
    {
        return Some("may only contain ASCII letters, digits, '/', '_', '.', '-'");
    }
    if RESERVED_DB_DIRS.contains(&parts[0]) {
        return Some("collides with a reserved .nari artifact path");
    }
    None
}

fn contract(name: &str, kind: ActionKind, entity: &Entity, inputs: &[Input]) -> Contract {
    let mut ins: BTreeMap<String, String> = inputs
        .iter()
        .map(|i| (i.name.clone(), i.ty.describe()))
        .collect();
    let mut errors = vec![
        "400 INVALID_INPUT",
        "401 UNAUTHENTICATED",
        "403 FORBIDDEN",
        "413 PAYLOAD_TOO_LARGE",
        "429 RATE_LIMITED",
        "500 INTERNAL",
        "503 UNAVAILABLE",
    ];
    let (output, effects) = match kind {
        ActionKind::List => {
            ins.insert("limit".into(), "Int[1..100] = 50 (transport)".into());
            ins.insert("after".into(), "UUID? (transport cursor)".into());
            (
                format!("{{items: [{}], next_cursor: UUID?}}", entity.name),
                vec![format!("reads:{} owner=actor", entity.name)],
            )
        }
        ActionKind::Insert => {
            errors.push("409 IDEMPOTENCY_CONFLICT");
            (
                entity.name.clone(),
                vec![format!("inserts:{} owner=actor", entity.name)],
            )
        }
        ActionKind::Update => {
            errors.extend([
                "404 NOT_FOUND",
                "409 IDEMPOTENCY_CONFLICT",
                "409 REVISION_CONFLICT",
            ]);
            (
                entity.name.clone(),
                vec![format!(
                    "updates:{} owner=actor revision=checked",
                    entity.name
                )],
            )
        }
    };
    errors.sort();
    Contract {
        transport: format!("POST /api/actions/{name}"),
        inputs: ins,
        output,
        errors: errors.into_iter().map(String::from).collect(),
        effects,
        requires_idempotency_key: kind.mutates(),
    }
}

pub fn build_graph(app: &App) -> Graph {
    let mut nodes: BTreeMap<String, String> = BTreeMap::new();
    let mut edges: BTreeSet<Edge> = BTreeSet::new();
    let mut edge = |from: &str, role: &str, to: &str| {
        edges.insert(Edge {
            from: from.into(),
            role: role.into(),
            to: to.into(),
        });
    };
    let mut node = |id: &str, kind: &str| {
        nodes.insert(id.into(), kind.into());
    };
    let artifact = |name: &str| format!("{}/artifact/{name}", app.name);
    let (backend, web, database) = (artifact("backend"), artifact("web"), artifact("database"));
    node(&app.id, "app");
    node(&backend, "artifact");
    node(&web, "artifact");
    node(&database, "artifact");
    node(&app.identity.id, "identity");
    node(&app.runtime.id, "runtime");
    node(&app.recovery.id, "recovery");
    node(&app.repair.id, "repair");
    for e in &app.entities {
        node(&e.id, "entity");
        edge(&e.id, "stored_in", &database);
        for f in e.implicit_fields.iter().chain(&e.fields) {
            node(&f.id, "field");
            edge(&e.id, "has_field", &f.id);
        }
        edge(
            &e.id,
            "owned_by",
            &format!("{}/field/{}", e.id, e.owner_field),
        );
    }
    for a in &app.actions {
        let ent = app.action_entity(a);
        node(&a.id, "action");
        edge(&a.id, "compiled_into", &backend);
        edge(&app.identity.id, "authenticates", &a.id);
        let role = if a.kind.mutates() { "writes" } else { "reads" };
        edge(&a.id, role, &ent.id);
        edge(
            &a.id,
            "scoped_by",
            &format!("{}/field/{}", ent.id, ent.owner_field),
        );
        for i in &a.inputs {
            node(&i.id, "input");
            edge(&a.id, "has_input", &i.id);
        }
        for s in &a.set {
            node(&s.id, "assignment");
            edge(&a.id, "has_assignment", &s.id);
            edge(&s.id, "assigns", &format!("{}/field/{}", ent.id, s.field));
            if let Binding::Input { input } = &s.binding {
                edge(&s.id, "reads_input", &format!("{}/input/{input}", a.id));
            }
        }
        if let Some(i) = &a.id_input {
            edge(&a.id, "selects_by", &format!("{}/input/{i}", a.id));
        }
        if let Some(i) = &a.revision_input {
            edge(&a.id, "checks_revision", &format!("{}/input/{i}", a.id));
        }
    }
    let action_id = |name: &str| format!("{}/action/{name}", app.name);
    for s in &app.screens {
        node(&s.id, "screen");
        edge(&s.id, "compiled_into", &web);
        edge(&s.id, "queries", &action_id(&s.query));
        for f in &s.forms {
            node(&f.id, "form");
            edge(&s.id, "contains", &f.id);
            edge(&f.id, "submits", &action_id(&f.submit));
            for i in &f.inputs {
                node(&i.id, "control");
                edge(&f.id, "contains", &i.id);
                edge(
                    &i.id,
                    "controls",
                    &format!("{}/input/{}", action_id(&f.submit), i.name),
                );
            }
        }
        let entity = app
            .action(&s.query)
            .map(|q| format!("{}/entity/{}", app.name, q.entity))
            .unwrap_or_default();
        for l in &s.lists {
            node(&l.id, "list");
            edge(&s.id, "contains", &l.id);
            for f in &l.fields {
                edge(&l.id, "displays", &format!("{entity}/field/{f}"));
            }
            for b in &l.buttons {
                node(&b.id, "button");
                edge(&l.id, "contains", &b.id);
                edge(&b.id, "invokes", &action_id(&b.action));
                for arg in &b.arguments {
                    let target = format!("{}/input/{}", action_id(&b.action), arg.input);
                    edge(&b.id, "binds", &target);
                    if let ArgBinding::Item { field } = &arg.binding {
                        edge(&b.id, "passes", &format!("{entity}/field/{field}"));
                    }
                }
            }
        }
    }
    for target in [&backend, &web, &database] {
        edge(&app.runtime.id, "configures", target);
    }
    edge(&app.identity.id, "configures", &backend);
    edge(&app.recovery.id, "recovers", &backend);
    for a in &app.repair.editable_actions {
        edge(&app.repair.id, "may_edit", &action_id(a));
    }
    Graph {
        nodes: nodes
            .into_iter()
            .map(|(id, kind)| Node { id, kind })
            .collect(),
        edges: edges.into_iter().collect(),
    }
}
