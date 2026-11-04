use crate::ir::{
    self, Action, ActionKind, App, ArgBinding, Binding, Compiled, Entity, Field, Literal, Type,
};
use imanari_runtime as rt;
use serde_json::{Value as Json, json};
use std::collections::BTreeMap;

pub struct Generated {
    pub files: BTreeMap<String, String>,
    pub regions: Vec<Region>,
    pub schema_fingerprint: String,
}

#[derive(Clone, Debug)]
pub struct Region {
    pub start_line: usize,
    pub end_line: usize,
    pub node: String,
}

pub const APP_JS: &str = include_str!("web/app.js");
pub const APP_CSS: &str = include_str!("web/app.css");

fn rs(s: &str) -> String {
    format!("{s:?}")
}

fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn table(e: &Entity) -> String {
    q(&format!("entity_{}", e.name))
}

fn columns(e: &Entity) -> Vec<&Field> {
    e.implicit_fields.iter().chain(&e.fields).collect()
}

fn column_list(e: &Entity) -> String {
    columns(e)
        .iter()
        .map(|f| q(&f.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sql_type(t: &Type) -> &'static str {
    match t {
        Type::Text { .. } | Type::Uuid => "TEXT",
        Type::Bool | Type::Int => "INTEGER",
    }
}

fn column(f: &Field) -> String {
    let check = match &f.ty {
        Type::Bool => format!(" CHECK ({} IN (0, 1))", q(&f.name)),
        _ => String::new(),
    };
    format!("{} {} NOT NULL{check}", q(&f.name), sql_type(&f.ty))
}

fn sql_literal(l: &Literal) -> String {
    match l {
        Literal::Text(s) | Literal::Uuid(s) => format!("'{}'", s.replace('\'', "''")),
        Literal::Bool(b) => i32::from(*b).to_string(),
        Literal::Int(n) => n.to_string(),
    }
}

fn entity_ddl(e: &Entity) -> Vec<String> {
    let mut cols = vec![
        format!("{} TEXT PRIMARY KEY NOT NULL", q("id")),
        format!("{0} INTEGER NOT NULL CHECK ({0} >= 1)", q("revision")),
    ];
    cols.extend(e.fields.iter().map(column));
    vec![
        format!("CREATE TABLE {} ({}) STRICT", table(e), cols.join(", ")),
        format!(
            "CREATE INDEX {} ON {} ({}, {})",
            q(&format!("entity_{}_owner", e.name)),
            table(e),
            q(&e.owner_field),
            q("id")
        ),
    ]
}

pub fn schema_shape(app: &App) -> Vec<rt::EntityShape> {
    app.entities
        .iter()
        .map(|e| rt::EntityShape {
            name: e.name.clone(),
            owner: e.owner_field.clone(),
            create: entity_ddl(e),
            fields: e
                .fields
                .iter()
                .map(|f| rt::FieldShape {
                    name: f.name.clone(),
                    ty: ir::to_json(&f.ty),
                    add: f.default.as_ref().map(|d| {
                        format!(
                            "ALTER TABLE {} ADD COLUMN {} DEFAULT {}",
                            table(e),
                            column(f),
                            sql_literal(d)
                        )
                    }),
                })
                .collect(),
        })
        .collect()
}

pub fn schema(app: &App) -> (Vec<String>, String) {
    let stmts: Vec<String> = app.entities.iter().flat_map(entity_ddl).collect();
    let shapes: Vec<Json> = app
        .entities
        .iter()
        .map(|e| {
            let fields: Vec<Json> = e
                .fields
                .iter()
                .map(|f| json!({ "name": f.name, "type": ir::to_json(&f.ty) }))
                .collect();
            json!({ "name": e.name, "owner": e.owner_field, "fields": fields })
        })
        .collect();
    let fingerprint = ir::sha256_hex(
        format!(
            "{}\n{}",
            stmts.join(";\n"),
            ir::canonical_compact(&Json::Array(shapes))
        )
        .as_bytes(),
    );
    (stmts, fingerprint)
}

struct Emitter {
    out: String,
    line: usize,
    regions: Vec<Region>,
}

impl Emitter {
    fn line(&mut self, indent: usize, s: &str) {
        for _ in 0..indent {
            self.out.push_str("    ");
        }
        self.out.push_str(s);
        self.out.push('\n');
        self.line += 1;
    }

    fn region(&mut self, node: &str, f: impl FnOnce(&mut Emitter)) {
        let start = self.line + 1;
        f(self);
        self.regions.push(Region {
            start_line: start,
            end_line: self.line,
            node: node.into(),
        });
    }
}

fn literal_bind(l: &Literal) -> String {
    match l {
        Literal::Text(s) | Literal::Uuid(s) => rs(s),
        Literal::Bool(b) => format!("{}i64", i64::from(*b)),
        Literal::Int(n) => format!("{n}i64"),
    }
}

fn input_reader(i: &ir::Input) -> String {
    let name = rs(&i.name);
    match &i.ty {
        Type::Text {
            min_length,
            max_length,
        } => format!("input.text({name}, {min_length}, {max_length})?"),
        Type::Bool => format!("input.boolean({name})?"),
        Type::Uuid => format!("input.uuid({name})?"),
        Type::Int => format!("input.int({name})?"),
    }
}

fn input_bind(i: &ir::Input, var: &str) -> String {
    match &i.ty {
        Type::Bool => format!("i64::from({var})"),
        Type::Int => var.to_string(),
        _ => format!("{var}.clone()"),
    }
}

fn row_fn(e: &mut Emitter, idx: usize, ent: &Entity) {
    e.line(
        0,
        &format!(
            "fn row_{idx}(row: &rt::sqlx::sqlite::SqliteRow) -> Result<Json, rt::ApiError> {{"
        ),
    );
    e.line(1, "let mut m = rt::serde_json::Map::new();");
    for (i, f) in columns(ent).iter().enumerate() {
        let getter = match f.ty {
            Type::Text { .. } | Type::Uuid => "col_text",
            Type::Bool => "col_bool",
            Type::Int => "col_int",
        };
        e.line(
            1,
            &format!(
                "m.insert({}.to_string(), rt::{getter}(row, {i})?);",
                rs(&f.name)
            ),
        );
    }
    e.line(1, "Ok(Json::Object(m))");
    e.line(0, "}");
}

fn action_fn(e: &mut Emitter, idx: usize, app: &App, a: &Action, ent_idx: usize) {
    let ent = app.action_entity(a);
    let cols = column_list(ent);
    let vars: BTreeMap<&str, String> = a
        .inputs
        .iter()
        .enumerate()
        .map(|(i, input)| (input.name.as_str(), format!("in_{i}")))
        .collect();
    e.line(
        0,
        &format!("fn action_{idx}(ctx: rt::Ctx, input: rt::Input) -> rt::HandlerFuture {{"),
    );
    e.line(1, "Box::pin(async move {");
    if a.kind == ActionKind::List {
        let sql = format!(
            "SELECT {cols} FROM {} WHERE {} = ?1 AND (?2 IS NULL OR {} > ?2) ORDER BY {} ASC LIMIT ?3",
            table(ent),
            q(&ent.owner_field),
            q("id"),
            q("id")
        );
        e.line(2, "let (limit, after) = input.page()?;");
        e.line(2, &format!("let rows = rt::sqlx::query({})", rs(&sql)));
        e.line(3, ".bind(ctx.actor())");
        e.line(3, ".bind(after)");
        e.line(3, ".bind(limit + 1)");
        e.line(3, ".fetch_all(ctx.db())");
        e.line(3, ".await?;");
        e.line(2, &format!("rt::page(rows, limit, row_{ent_idx})"));
        e.line(1, "})");
        e.line(0, "}");
        return;
    }
    let names: Vec<String> = a.inputs.iter().map(|i| rs(&i.name)).collect();
    e.line(2, &format!("input.only(&[{}])?;", names.join(", ")));
    for i in &a.inputs {
        e.line(
            2,
            &format!("let {} = {};", vars[i.name.as_str()], input_reader(i)),
        );
    }
    for s in &a.set {
        if let (
            Binding::Input { input },
            Some(Type::Text {
                min_length,
                max_length,
            }),
        ) = (&s.binding, ent.field(&s.field).map(|f| &f.ty))
        {
            e.line(
                2,
                &format!(
                    "rt::check_text({}, &{}, {min_length}, {max_length})?;",
                    rs(input),
                    vars[input.as_str()]
                ),
            );
        }
    }
    let pairs: Vec<String> = a
        .inputs
        .iter()
        .map(|i| {
            format!(
                "({}, Json::from({}.clone()))",
                rs(&i.name),
                vars[i.name.as_str()]
            )
        })
        .collect();
    e.line(
        2,
        &format!(
            "let hash = rt::input_hash({}, &[{}]);",
            rs(&a.name),
            pairs.join(", ")
        ),
    );
    e.line(2, "let mut tx = match ctx.begin_mutation(hash).await? {");
    e.line(3, "rt::Begin::Replay(data) => return Ok(data),");
    e.line(3, "rt::Begin::Fresh(tx) => tx,");
    e.line(2, "};");
    let bind_for = |b: &Binding| -> String {
        match b {
            Binding::Input { input } => input_bind(
                a.input(input).expect("validated input"),
                &vars[input.as_str()],
            ),
            Binding::ActorId => "ctx.actor()".into(),
            Binding::Literal { value } => literal_bind(value),
        }
    };
    if a.kind == ActionKind::Insert {
        let mut col_names = vec![q("id"), q("revision")];
        let mut binds = vec!["rt::new_uuid()".to_string(), "1i64".to_string()];
        for f in &ent.fields {
            col_names.push(q(&f.name));
            match a.set.iter().find(|s| s.field == f.name) {
                Some(s) => binds.push(bind_for(&s.binding)),
                None => binds.push(literal_bind(f.default.as_ref().expect("validated default"))),
            }
        }
        let params: Vec<String> = (1..=binds.len()).map(|i| format!("?{i}")).collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) RETURNING {cols}",
            table(ent),
            col_names.join(", "),
            params.join(", ")
        );
        e.line(2, &format!("let row = rt::sqlx::query({})", rs(&sql)));
        for b in binds {
            e.line(3, &format!(".bind({b})"));
        }
        e.line(3, ".fetch_one(tx.conn())");
        e.line(3, ".await?;");
    } else {
        let mut sets = Vec::new();
        let mut binds = Vec::new();
        for (i, s) in a.set.iter().enumerate() {
            sets.push(format!("{} = ?{}", q(&s.field), i + 1));
            binds.push(bind_for(&s.binding));
        }
        let n = binds.len();
        let sql = format!(
            "UPDATE {} SET {}, {rev} = {rev} + 1 WHERE {} = ?{} AND {} = ?{} AND {rev} = ?{} AND {rev} < {} RETURNING {cols}",
            table(ent),
            sets.join(", "),
            q("id"),
            n + 1,
            q(&ent.owner_field),
            n + 2,
            n + 3,
            crate::syntax::MAX_SAFE_INT,
            rev = q("revision"),
        );
        let id_var = &vars[a.id_input.as_deref().expect("validated id input")];
        let rev_var = &vars[a
            .revision_input
            .as_deref()
            .expect("validated revision input")];
        e.line(2, &format!("let row = rt::sqlx::query({})", rs(&sql)));
        for b in binds {
            e.line(3, &format!(".bind({b})"));
        }
        e.line(3, &format!(".bind({id_var}.clone())"));
        e.line(3, ".bind(ctx.actor())");
        e.line(3, &format!(".bind({rev_var})"));
        e.line(3, ".fetch_optional(tx.conn())");
        e.line(3, ".await?;");
        let exists = format!(
            "SELECT {} FROM {} WHERE {} = ?1 AND {} = ?2",
            q("revision"),
            table(ent),
            q("id"),
            q(&ent.owner_field)
        );
        e.line(2, "let Some(row) = row else {");
        e.line(
            3,
            &format!(
                "return Err(ctx.update_miss(tx, {}, &{id_var}, {rev_var}).await);",
                rs(&exists)
            ),
        );
        e.line(2, "};");
    }
    e.line(2, &format!("let data = row_{ent_idx}(&row)?;"));
    e.line(2, "ctx.commit_mutation(tx, data).await");
    e.line(1, "})");
    e.line(0, "}");
}

pub fn rust_main(
    c: &Compiled,
    schema_stmts: &[String],
    fingerprint: &str,
) -> (String, Vec<Region>) {
    let app = &c.app;
    let mut e = Emitter {
        out: String::new(),
        line: 0,
        regions: Vec::new(),
    };
    e.line(0, "#![forbid(unsafe_code)]");
    e.line(0, "#![allow(clippy::all)]");
    e.line(0, "");
    e.line(0, "use imanari_runtime as rt;");
    e.line(0, "use rt::serde_json::Value as Json;");
    e.line(0, "");
    for (i, ent) in app.entities.iter().enumerate() {
        e.region(&ent.id, |e| row_fn(e, i, ent));
        e.line(0, "");
    }
    for (i, a) in app.actions.iter().enumerate() {
        let ent_idx = app
            .entities
            .iter()
            .position(|x| x.name == a.entity)
            .expect("validated entity");
        e.region(&a.id, |e| action_fn(e, i, app, a, ent_idx));
        e.line(0, "");
    }
    e.region(&app.runtime.id, |e| {
        e.line(0, "static SPEC: rt::AppSpec = rt::AppSpec {");
        e.line(1, &format!("name: {},", rs(&app.name)));
        e.line(1, &format!("source_sha256: {},", rs(&c.source_sha256)));
        e.line(1, &format!("semantic_sha256: {},", rs(&c.semantic_sha256)));
        e.line(1, "schema: &[");
        for s in schema_stmts {
            e.line(2, &format!("{},", rs(s)));
        }
        e.line(1, "],");
        e.line(1, &format!("schema_fingerprint: {},", rs(fingerprint)));
        e.line(
            1,
            &format!(
                "schema_shape: {},",
                rs(&serde_json::to_string(&schema_shape(app)).unwrap_or_default())
            ),
        );
        e.line(1, &format!("database: {},", rs(&app.runtime.database)));
        e.line(1, &format!("port: {},", app.runtime.port));
        e.line(
            1,
            &format!("request_limit_bytes: {},", app.runtime.request_limit_bytes),
        );
        e.line(
            1,
            &format!("request_timeout_ms: {},", app.runtime.request_timeout_ms),
        );
        let routes: Vec<String> = app.screens.iter().map(|s| rs(&s.route)).collect();
        e.line(1, &format!("routes: &[{}],", routes.join(", ")));
        e.line(1, "index_html: include_str!(\"../../web/index.html\"),");
        e.line(1, "assets: &[");
        for (path, ctype) in [
            ("app.js", "text/javascript; charset=utf-8"),
            ("app.css", "text/css; charset=utf-8"),
            ("nari.json", "application/json"),
        ] {
            e.line(
                2,
                &format!(
                    "rt::Asset {{ path: {}, content_type: {}, body: include_str!({}) }},",
                    rs(&format!("/assets/{path}")),
                    rs(ctype),
                    rs(&format!("../../web/{path}"))
                ),
            );
        }
        e.line(1, "],");
        e.line(1, "actions: &[");
        for (i, a) in app.actions.iter().enumerate() {
            e.line(
                2,
                &format!(
                    "rt::ActionSpec {{ name: {}, mutates: {}, handler: action_{i} }},",
                    rs(&a.name),
                    a.kind.mutates()
                ),
            );
        }
        e.line(1, "],");
        e.line(0, "};");
    });
    e.line(0, "");
    e.line(0, "fn main() -> std::process::ExitCode {");
    e.line(1, "rt::run(&SPEC)");
    e.line(0, "}");
    (e.out, e.regions)
}

fn ts_type(t: &Type) -> &'static str {
    match t {
        Type::Text { .. } | Type::Uuid => "string",
        Type::Bool => "boolean",
        Type::Int => "number",
    }
}

pub fn typescript(app: &App, source_sha256: &str) -> String {
    let mut out = format!(
        "export declare const NARI_SOURCE_SHA256: {};\n\n",
        serde_json::to_string(source_sha256).unwrap_or_default()
    );
    out.push_str("export type ApiError = { code: string; message: string; field?: string };\n");
    out.push_str("export type ApiResponse<T> = { data: T; request_id: string } | { error: ApiError; request_id: string };\n");
    out.push_str("export type Page<T> = { items: T[]; next_cursor: string | null };\n\n");
    for ent in &app.entities {
        out.push_str(&format!("export type Entity_{} = {{\n", ent.name));
        for f in columns(ent) {
            out.push_str(&format!(
                "  {}: {};\n",
                serde_json::to_string(&f.name).unwrap_or_default(),
                ts_type(&f.ty)
            ));
        }
        out.push_str("};\n\n");
    }
    out.push_str("export type Actions = {\n");
    for a in &app.actions {
        let input = if a.kind == ActionKind::List {
            "{ limit?: number; after?: string | null }".to_string()
        } else {
            let fields: Vec<String> = a
                .inputs
                .iter()
                .map(|i| {
                    format!(
                        "{}: {}",
                        serde_json::to_string(&i.name).unwrap_or_default(),
                        ts_type(&i.ty)
                    )
                })
                .collect();
            format!("{{ {} }}", fields.join("; "))
        };
        let output = match a.kind {
            ActionKind::List => format!("Page<Entity_{}>", a.entity),
            _ => format!("Entity_{}", a.entity),
        };
        out.push_str(&format!(
            "  {}: {{ input: {input}; output: {output}; idempotency_key: {} }};\n",
            serde_json::to_string(&a.name).unwrap_or_default(),
            a.kind.mutates()
        ));
    }
    out.push_str("};\n");
    out
}

fn type_json(t: &Type) -> Json {
    match t {
        Type::Text {
            min_length,
            max_length,
        } => json!({ "name": "Text", "min_length": min_length, "max_length": max_length }),
        other => json!({ "name": other.base() }),
    }
}

pub fn web_config(app: &App, source_sha256: &str) -> Json {
    let screens: Vec<Json> = app
        .screens
        .iter()
        .map(|s| {
            let query = app.action(&s.query).expect("validated query");
            let ent = app.action_entity(query);
            let forms: Vec<Json> = s
                .forms
                .iter()
                .map(|f| {
                    let action = app.action(&f.submit).expect("validated submit");
                    let inputs: Vec<Json> = f
                        .inputs
                        .iter()
                        .map(|i| {
                            let ty = &action.input(&i.name).expect("validated control").ty;
                            json!({ "name": i.name, "label": i.label, "placeholder": i.placeholder, "type": type_json(ty) })
                        })
                        .collect();
                    json!({ "name": f.name, "submit": f.submit, "inputs": inputs })
                })
                .collect();
            let lists: Vec<Json> = s
                .lists
                .iter()
                .map(|l| {
                    let fields: Vec<Json> = l
                        .fields
                        .iter()
                        .map(|f| json!({ "name": f, "type": type_json(&ent.any_field(f).expect("validated field").ty) }))
                        .collect();
                    let buttons: Vec<Json> = l
                        .buttons
                        .iter()
                        .map(|b| {
                            let args: Vec<Json> = b
                                .arguments
                                .iter()
                                .map(|a| match &a.binding {
                                    ArgBinding::Item { field } => json!({ "input": a.input, "from": "item", "field": field }),
                                    ArgBinding::Literal { value } => json!({ "input": a.input, "from": "literal", "value": value.to_json() }),
                                })
                                .collect();
                            json!({ "name": b.name, "label": b.label, "action": b.action, "arguments": args })
                        })
                        .collect();
                    json!({ "name": l.name, "fields": fields, "buttons": buttons })
                })
                .collect();
            json!({ "route": s.route, "title": s.title, "query": s.query, "entity": ent.name, "forms": forms, "lists": lists })
        })
        .collect();
    json!({ "app": app.name, "source_sha256": source_sha256, "screens": screens })
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            c => c.to_string(),
        })
        .collect()
}

pub fn index_html(app: &App, source_sha256: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta name=\"nari-source-sha256\" content=\"{}\">\n<title>{}</title>\n<link rel=\"stylesheet\" href=\"/assets/app.css\">\n<script src=\"/assets/app.js\" defer></script>\n</head>\n<body>\n<header id=\"dev-bar\" aria-label=\"Development identity\"></header>\n<main id=\"app\"><p role=\"status\">Loading…</p></main>\n</body>\n</html>\n",
        html_escape(source_sha256),
        html_escape(&app.name)
    )
}

pub const RUNTIME_MANIFEST: &str = include_str!("../runtime/Cargo.toml");
pub const RUNTIME_SOURCE: &str = include_str!("../runtime/src/lib.rs");

pub fn generate(c: &Compiled, package: &str) -> Generated {
    let (stmts, fingerprint) = schema(&c.app);
    let (main_rs, regions) = rust_main(c, &stmts, &fingerprint);
    let mut files = BTreeMap::new();
    files.insert("app/src/main.rs".into(), main_rs);
    files.insert(
        "app/Cargo.toml".into(),
        format!(
            "[package]\nname = {}\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n[dependencies]\nimanari-runtime = {{ path = {} }}\n\n[profile.release]\ncodegen-units = 16\n\n[workspace]\n",
            toml_str(package),
            toml_str("../runtime")
        ),
    );
    files.insert("runtime/Cargo.toml".into(), RUNTIME_MANIFEST.to_string());
    files.insert("runtime/src/lib.rs".into(), RUNTIME_SOURCE.to_string());
    files.insert(
        "app/rust-toolchain.toml".into(),
        include_str!("../rust-toolchain.toml").to_string(),
    );
    files.insert(
        "web/index.html".into(),
        index_html(&c.app, &c.source_sha256),
    );
    files.insert("web/app.js".into(), APP_JS.to_string());
    files.insert("web/app.css".into(), APP_CSS.to_string());
    files.insert(
        "web/nari.json".into(),
        ir::canonical(&web_config(&c.app, &c.source_sha256)) + "\n",
    );
    files.insert("web/api.d.ts".into(), typescript(&c.app, &c.source_sha256));
    Generated {
        files,
        regions,
        schema_fingerprint: fingerprint,
    }
}

fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}
