use crate::codegen;
use crate::ir::{self, Compiled, Graph};
use serde::Serialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub node: String,
    pub kind: String,
    pub change: String,
    pub category: String,
    pub detail: String,
    pub affected: Vec<String>,
}

fn strip(mut v: Json, keys: &[&str]) -> Json {
    if let Some(m) = v.as_object_mut() {
        for k in keys {
            m.remove(*k);
        }
    }
    v
}

fn flatten(c: &Compiled) -> BTreeMap<String, (String, Json)> {
    let app = &c.app;
    let mut out = BTreeMap::new();
    let mut put = |id: &str, kind: &str, v: Json| {
        out.insert(id.to_string(), (kind.to_string(), v));
    };
    put(
        &format!("{}#targets", app.id),
        "targets",
        json!(app.targets),
    );
    put(&app.identity.id, "identity", ir::to_json(&app.identity));
    put(&app.runtime.id, "runtime", ir::to_json(&app.runtime));
    put(&app.recovery.id, "recovery", ir::to_json(&app.recovery));
    put(&app.repair.id, "repair", ir::to_json(&app.repair));
    for e in &app.entities {
        put(
            &e.id,
            "entity",
            strip(ir::to_json(e), &["fields", "implicit_fields"]),
        );
        for f in &e.fields {
            put(&f.id, "field", ir::to_json(f));
        }
    }
    for a in &app.actions {
        put(
            &a.id,
            "action",
            strip(ir::to_json(a), &["inputs", "set", "contract"]),
        );
        for i in &a.inputs {
            put(&i.id, "input", ir::to_json(i));
        }
        for s in &a.set {
            put(&s.id, "assignment", ir::to_json(s));
        }
    }
    for s in &app.screens {
        put(&s.id, "screen", strip(ir::to_json(s), &["forms", "lists"]));
        for f in &s.forms {
            put(&f.id, "form", strip(ir::to_json(f), &["inputs"]));
            for i in &f.inputs {
                put(&i.id, "control", ir::to_json(i));
            }
        }
        for l in &s.lists {
            put(&l.id, "list", strip(ir::to_json(l), &["buttons"]));
            for b in &l.buttons {
                put(&b.id, "button", ir::to_json(b));
            }
        }
    }
    out
}

fn category(kind: &str, key: &str) -> Option<&'static str> {
    Some(match (kind, key) {
        ("action", "") => "capability",
        ("action", "access") => "access",
        ("action", "kind" | "entity") => "capability",
        ("action", "id_input" | "revision_input") => "input/output",
        ("input", _) => "input/output",
        ("assignment", _) => "behavior",
        ("entity", "") => "data migration",
        ("entity", "owner_field") => "access",
        ("field", "" | "type") => "data migration",
        ("field", "default") => "behavior",
        ("screen" | "form" | "list" | "button" | "control", _) => "ui",
        ("runtime", _) => "runtime",
        ("targets", _) => "runtime",
        ("identity", _) => "access",
        ("recovery", _) => "recovery authority",
        ("repair", _) => "repair authority",
        _ => return None,
    })
}

fn affected(graphs: &[&Graph], start: &str) -> Vec<String> {
    let follow = [
        "invokes",
        "submits",
        "queries",
        "controls",
        "binds",
        "passes",
        "displays",
        "writes",
        "reads",
        "scoped_by",
        "has_input",
        "has_assignment",
        "has_field",
        "owned_by",
        "assigns",
        "reads_input",
        "selects_by",
        "checks_revision",
        "may_edit",
        "contains",
    ];
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut frontier = vec![start.to_string()];
    for _ in 0..4 {
        let mut next = Vec::new();
        for id in &frontier {
            for g in graphs {
                for e in g.reverse(id) {
                    if follow.contains(&e.role.as_str())
                        && e.from != start
                        && seen.insert(e.from.clone())
                    {
                        next.push(e.from.clone());
                    }
                }
            }
        }
        frontier = next;
    }
    seen.into_iter()
        .filter(|id| {
            graphs.iter().any(|g| {
                matches!(
                    g.kind(id),
                    Some("action" | "screen" | "form" | "list" | "button" | "entity" | "repair")
                )
            })
        })
        .collect()
}

fn text_bounds(v: &Json) -> Option<(i64, i64)> {
    let t = &v["type"];
    (t["name"] == "Text").then(|| {
        (
            t["min_length"].as_i64().unwrap_or(0),
            t["max_length"].as_i64().unwrap_or(0),
        )
    })
}

pub fn diff(base: &Compiled, cur: &Compiled) -> Json {
    let (b, c) = (flatten(base), flatten(cur));
    let graphs = [&base.graph, &cur.graph];
    let mut changes = Vec::new();
    let mut full = false;
    let ids: BTreeSet<&String> = b.keys().chain(c.keys()).collect();
    for id in ids {
        let (old, new) = (b.get(id), c.get(id));
        let kind = old.or(new).map(|(k, _)| k.as_str()).unwrap_or("unknown");
        let mut push = |change: &str, key: &str, detail: String| {
            let cat = category(kind, key).unwrap_or("unknown");
            if cat == "unknown" {
                full = true;
            }
            changes.push(Change {
                node: id.clone(),
                kind: kind.into(),
                change: change.into(),
                category: cat.into(),
                detail,
                affected: affected(&graphs, id),
            });
        };
        match (old, new) {
            (None, Some(_)) => push("added", "", format!("{kind} added")),
            (Some(_), None) => push("removed", "", format!("{kind} removed")),
            (Some((_, o)), Some((_, n))) if o != n => {
                let keys: BTreeSet<&String> = o
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(k, _)| k)
                    .chain(n.as_object().into_iter().flatten().map(|(k, _)| k))
                    .collect();
                for k in keys {
                    if o.get(k) == n.get(k) {
                        continue;
                    }
                    let mut detail = format!(
                        "{k}: {} -> {}",
                        ir::canonical_compact(o.get(k).unwrap_or(&Json::Null)),
                        ir::canonical_compact(n.get(k).unwrap_or(&Json::Null))
                    );
                    if k == "type"
                        && let (Some((omin, omax)), Some((nmin, nmax))) =
                            (text_bounds(o), text_bounds(n))
                    {
                        let narrower = nmin > omin || nmax < omax;
                        let wider = nmin < omin || nmax > omax;
                        let verdict = match (narrower, wider) {
                            (true, false) => {
                                "narrowed: previously valid requests may now be rejected"
                            }
                            (false, true) => "widened: accepts inputs previously rejected",
                            _ => "changed in both directions",
                        };
                        detail = format!("{detail} ({verdict})");
                    }
                    push("modified", k, detail);
                }
            }
            _ => {}
        }
    }
    let base_roles: BTreeSet<&str> = base.graph.edges.iter().map(|e| e.role.as_str()).collect();
    let cur_roles: BTreeSet<&str> = cur.graph.edges.iter().map(|e| e.role.as_str()).collect();
    for role in cur_roles.difference(&base_roles) {
        if !base.graph.edges.is_empty() {
            full = true;
            changes.push(Change {
                node: cur.app.id.clone(),
                kind: "graph".into(),
                change: "added".into(),
                category: "unknown".into(),
                detail: format!("new dependency edge class '{role}'"),
                affected: Vec::new(),
            });
        }
    }
    if base.semantic_sha256 != cur.semantic_sha256 && changes.is_empty() {
        full = true;
        changes.push(Change {
            node: cur.app.id.clone(),
            kind: "app".into(),
            change: "modified".into(),
            category: "unknown".into(),
            detail: "semantic hash changed without a classified difference".into(),
            affected: Vec::new(),
        });
    }
    let mut summary: BTreeMap<String, usize> = BTreeMap::new();
    for ch in &changes {
        *summary.entry(ch.category.clone()).or_default() += 1;
    }
    let schema_changed = changes.iter().any(|c| c.category == "data migration");
    let (automatic, blocked) = match imanari_runtime::migration_plan(
        &codegen::schema_shape(&base.app),
        &codegen::schema_shape(&cur.app),
    ) {
        Ok(steps) => (steps.into_iter().map(|s| s.summary).collect(), Vec::new()),
        Err(blocked) => (Vec::new(), blocked),
    };
    json!({
        "schema": "nari-diff/0.1",
        "base": { "source_sha256": base.source_sha256, "semantic_sha256": base.semantic_sha256 },
        "current": { "source_sha256": cur.source_sha256, "semantic_sha256": cur.semantic_sha256 },
        "behavior_changed": base.semantic_sha256 != cur.semantic_sha256,
        "migration_required": !blocked.is_empty(),
        "migration": { "automatic": automatic, "blocked": blocked },
        "full_validation_required": full || schema_changed,
        "summary": summary,
        "changes": changes,
    })
}

pub fn render(d: &Json) -> String {
    let mut out = String::new();
    let changes = d["changes"].as_array().cloned().unwrap_or_default();
    if changes.is_empty() {
        let note = if d["base"]["source_sha256"] == d["current"]["source_sha256"] {
            "identical source"
        } else {
            "source differs only in formatting/comments"
        };
        return format!("no behavioral changes ({note})");
    }
    for c in &changes {
        out.push_str(&format!(
            "[{}] {} {} — {}\n",
            c["category"].as_str().unwrap_or_default(),
            c["change"].as_str().unwrap_or_default(),
            c["node"].as_str().unwrap_or_default(),
            c["detail"].as_str().unwrap_or_default()
        ));
        let affected: Vec<&str> = c["affected"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str())
            .collect();
        if !affected.is_empty() {
            out.push_str(&format!("    affects: {}\n", affected.join(", ")));
        }
    }
    let list = |k: &str| -> Vec<String> {
        d["migration"][k]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    };
    let (automatic, blocked) = (list("automatic"), list("blocked"));
    if !automatic.is_empty() {
        out.push_str(&format!(
            "data migration: applied automatically on the next start: {}\n",
            automatic.join(", ")
        ));
    }
    if !blocked.is_empty() {
        out.push_str(&format!(
            "data migration: existing databases will stop with MIGRATION_REQUIRED: {}\n",
            blocked.join("; ")
        ));
    }
    if d["full_validation_required"] == true {
        out.push_str("full validation required: run `nari check` and `nari test`\n");
    }
    out.trim_end().to_string()
}
