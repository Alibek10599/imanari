use imanari::diag::Diagnostic;
use imanari::{compile, format_source, ir};

const FIXTURE: &str = include_str!("../examples/fieldwork/fieldwork.nari");
const FILE: &str = "fieldwork.nari";

fn errors(src: &str) -> Vec<Diagnostic> {
    match compile(src, FILE) {
        Ok((_, warnings)) => panic!("expected errors, compiled with warnings {warnings:?}"),
        Err(d) => d,
    }
}

fn mutate(from: &str, to: &str) -> String {
    assert!(FIXTURE.contains(from), "fixture lacks {from:?}");
    FIXTURE.replacen(from, to, 1)
}

#[test]
fn oidc_identity_compiles_with_https_or_loopback_issuers() {
    for issuer in ["https://accounts.example.com", "http://127.0.0.1:8081"] {
        let src = mutate(
            "mode = local_dev;",
            &format!("mode = oidc;\n    issuer = \"{issuer}\";\n    client_id = \"tasks\";"),
        );
        let (c, warnings) = compile(&src, FILE).expect("oidc fixture compiles");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(c.app.identity.issuer.as_deref(), Some(issuer));
        assert_eq!(c.app.identity.client_id.as_deref(), Some("tasks"));
    }
}

#[test]
fn fixture_checks_without_diagnostics() {
    let (c, warnings) = compile(FIXTURE, FILE).expect("fixture compiles");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(c.app.name, "fieldwork");
    let ids: Vec<&str> = c.graph.nodes.iter().map(|n| n.id.as_str()).collect();
    for id in [
        "fieldwork/action/complete_task",
        "fieldwork/entity/Task/field/revision",
        "fieldwork/screen/tasks/list/task_list/button/complete",
        "fieldwork/identity/development",
    ] {
        assert!(ids.contains(&id), "missing node {id}");
    }
    let has = |from: &str, role: &str, to: &str| {
        c.graph
            .edges
            .iter()
            .any(|e| e.from == from && e.role == role && e.to == to)
    };
    assert!(has(
        "fieldwork/action/complete_task",
        "writes",
        "fieldwork/entity/Task"
    ));
    assert!(has(
        "fieldwork/screen/tasks",
        "queries",
        "fieldwork/action/list_tasks"
    ));
    assert!(has(
        "fieldwork/identity/development",
        "authenticates",
        "fieldwork/action/create_task"
    ));
    assert!(has(
        "fieldwork/runtime/local",
        "configures",
        "fieldwork/artifact/backend"
    ));
    assert!(has(
        "fieldwork/recovery/backend_process",
        "recovers",
        "fieldwork/artifact/backend"
    ));
    assert!(has(
        "fieldwork/repair/local_candidate",
        "may_edit",
        "fieldwork/action/complete_task"
    ));
}

#[test]
fn fixture_is_formatter_fixed_point() {
    assert_eq!(format_source(FIXTURE, FILE).unwrap(), FIXTURE);
}

#[test]
fn formatting_preserves_comments_and_semantics() {
    let messy: String = FIXTURE
        .lines()
        .map(|l| l.trim_start())
        .collect::<Vec<_>>()
        .join("\n")
        .replace(
            "set = { completed: true };",
            "set = {completed:true,} ; // keep me",
        )
        .replace("targets = [web];", "targets=[ web , ];\n\n\n// floating\n")
        .replace("imanari \"0.1\";", "imanari \"0.1\";\n// leading");
    let formatted = format_source(&messy, FILE).unwrap();
    assert_eq!(
        format_source(&formatted, FILE).unwrap(),
        formatted,
        "idempotent"
    );
    for comment in [
        "// keep me",
        "// floating",
        "// id and revision",
        "// leading",
    ] {
        assert!(formatted.contains(comment), "lost {comment}");
    }
    assert!(formatted.contains("set = { completed: true }; // keep me"));
    let a = compile(&messy, FILE).unwrap().0;
    let b = compile(&formatted, FILE).unwrap().0;
    assert_eq!(a.semantic_sha256, b.semantic_sha256);
    assert_eq!(ir::to_json(&a.app), ir::to_json(&b.app));
    assert_ne!(a.source_sha256, b.source_sha256);
}

#[test]
fn canonical_outputs_are_deterministic() {
    let a = compile(FIXTURE, FILE).unwrap().0;
    let b = compile(FIXTURE, FILE).unwrap().0;
    assert_eq!(ir::canonical(&a.ir_json()), ir::canonical(&b.ir_json()));
    let commented = FIXTURE.replace("app fieldwork {", "// extra\napp fieldwork {");
    let c = compile(&commented, FILE).unwrap().0;
    assert_eq!(a.semantic_sha256, c.semantic_sha256);
    assert_eq!(a.graph_sha256, c.graph_sha256);
    assert_ne!(a.source_sha256, c.source_sha256);
    let json = ir::canonical(&a.ir_json());
    assert!(!json.contains("/Users/") && !json.contains("\"timestamp"));
}

#[test]
fn reordering_declarations_keeps_semantic_hash() {
    let a = compile(FIXTURE, FILE).unwrap().0;
    let block = "  action list_tasks {\n    kind = list;\n    entity = Task;\n    access = signed_in;\n  }\n\n";
    let moved = FIXTURE.replacen(block, "", 1).replacen(
        "  screen tasks {",
        &format!("{block}  screen tasks {{"),
        1,
    );
    assert_ne!(moved, FIXTURE);
    assert_eq!(
        compile(&moved, FILE).unwrap().0.semantic_sha256,
        a.semantic_sha256
    );
}

fn expect(src: &str, code: &str, span_text: Option<&str>) {
    let ds = errors(src);
    let d = ds
        .iter()
        .find(|d| d.code == code)
        .unwrap_or_else(|| panic!("expected {code}, got {ds:#?}"));
    assert!(d.line > 0 && d.column > 0, "{d:?}");
    assert!(d.span.end <= src.len() && d.span.start <= d.span.end);
    if let Some(t) = span_text {
        assert_eq!(&src[d.span.start..d.span.end], t, "span of {code}");
    }
}

#[test]
fn rejects_invalid_programs_with_useful_spans() {
    let cases: Vec<(String, &str, Option<&str>)> = vec![
        (
            mutate("query = list_tasks;", "query = list_taskz;"),
            "E_UNRESOLVED",
            Some("list_taskz"),
        ),
        (
            mutate("submit = create_task;", "submit = nope;"),
            "E_UNRESOLVED",
            Some("nope"),
        ),
        (
            mutate(
                "action list_tasks {",
                "action list_tasks {}\n  action list_tasks {",
            ),
            "E_DUPLICATE",
            Some("list_tasks"),
        ),
        (
            mutate(
                "owner_field = owner;",
                "owner_field = owner;\n    colour = red;",
            ),
            "E_UNKNOWN_PROPERTY",
            Some("colour"),
        ),
        (
            mutate("form add_task {", "widget w {}\n    form add_task {"),
            "E_UNKNOWN_BLOCK",
            Some("widget"),
        ),
        (
            mutate("type = Bool;", "type = Boolean;"),
            "E_TYPE",
            Some("Boolean"),
        ),
        (
            mutate("title: @input.title,", "title: @input.titel,"),
            "E_UNRESOLVED",
            Some("@input.titel"),
        ),
        (
            mutate("title: @input.title,", "title: @item.title,"),
            "E_REFERENCE_SCOPE",
            Some("@item.title"),
        ),
        (
            mutate("source = @query.items;", "source = @query.rows;"),
            "E_REFERENCE",
            Some("@query.rows"),
        ),
        (
            mutate(
                "set = { completed: true };",
                "set = { completed: true, owner: @input.id };",
            ),
            "E_OWNER_REASSIGNMENT",
            Some("owner"),
        ),
        (
            mutate(
                "owner: @actor.id,",
                "owner: \"00000000-0000-4000-8000-000000000001\",",
            ),
            "E_OWNER_ASSIGNMENT",
            None,
        ),
        (
            mutate(
                "field owner {",
                "field id {\n      type = UUID;\n    }\n\n    field owner {",
            ),
            "E_RESERVED_FIELD",
            Some("id"),
        ),
        (
            mutate("set = { completed: true };", "set = { revision: 5 };"),
            "E_RESERVED_FIELD",
            Some("revision"),
        ),
        (
            mutate("targets = [web];", "targets = [web, ios];"),
            "E_UNSUPPORTED_TARGET",
            Some("ios"),
        ),
        (
            mutate("default = false;", "default = 1;"),
            "E_TYPE",
            Some("1"),
        ),
        (
            mutate(
                "title: @input.title,",
                "title: @input.title,\n      completed: @input.title,",
            ),
            "E_TYPE",
            Some("@input.title"),
        ),
        (
            mutate("set = { completed: true };", "set = {};"),
            "E_EMPTY_UPDATE",
            Some("{}"),
        ),
        (
            mutate("title: @input.title,\n", ""),
            "E_MISSING_ASSIGNMENT",
            None,
        ),
        (
            mutate("backoff_ms = [100, 500, 2000];", "backoff_ms = [100, 500];"),
            "E_RANGE",
            Some("[100, 500]"),
        ),
        (
            mutate("max_restarts = 3;", "max_restarts = 11;"),
            "E_RANGE",
            Some("11"),
        ),
        (
            mutate("route = \"/\";", "route = \"/../x\";"),
            "E_ROUTE",
            Some("\"/../x\""),
        ),
        (
            mutate("route = \"/\";", "route = \"/api/x\";"),
            "E_ROUTE",
            None,
        ),
        (
            mutate("database = \".nari/app.db\";", "database = \"../app.db\";"),
            "E_PATH",
            None,
        ),
        (
            mutate("bind = \"127.0.0.1\";", "bind = \"0.0.0.0\";"),
            "E_VALUE",
            Some("\"0.0.0.0\""),
        ),
        (
            mutate("automatic_release = false;", "automatic_release = true;"),
            "E_UNSUPPORTED_FEATURE",
            Some("true"),
        ),
        (
            mutate("mode = local_dev;", "mode = saml;"),
            "E_UNSUPPORTED_FEATURE",
            Some("saml"),
        ),
        (
            mutate(
                "mode = local_dev;",
                "mode = oidc;\n    client_id = \"tasks\";",
            ),
            "E_MISSING_PROPERTY",
            None,
        ),
        (
            mutate(
                "mode = local_dev;",
                "mode = oidc;\n    issuer = \"http://idp.example.com\";\n    client_id = \"tasks\";",
            ),
            "E_VALUE",
            Some("\"http://idp.example.com\""),
        ),
        (
            mutate(
                "mode = local_dev;",
                "mode = oidc;\n    issuer = \"https://idp.example.com/?x=1\";\n    client_id = \"tasks\";",
            ),
            "E_VALUE",
            Some("\"https://idp.example.com/?x=1\""),
        ),
        (
            mutate(
                "mode = local_dev;",
                "mode = oidc;\n    issuer = \"https://idp.example.com\";\n    client_id = \"has space\";",
            ),
            "E_VALUE",
            Some("\"has space\""),
        ),
        (
            mutate(
                "mode = local_dev;",
                "mode = local_dev;\n    issuer = \"https://idp.example.com\";",
            ),
            "E_UNKNOWN_PROPERTY",
            Some("issuer"),
        ),
        (
            mutate(
                "kind = list;",
                "kind = list;\n\n    input limit {\n      type = Int;\n    }",
            ),
            "E_RESERVED_INPUT",
            Some("limit"),
        ),
        (
            mutate("kind = list;", "kind = delete;"),
            "E_UNSUPPORTED_FEATURE",
            Some("delete"),
        ),
        (
            mutate(
                "runtime local {",
                "workflow nightly {}\n\n  runtime local {",
            ),
            "E_UNSUPPORTED_FEATURE",
            Some("workflow"),
        ),
        (
            mutate(
                "      input title {\n        label = \"Task title\";\n        placeholder = \"What needs doing?\";\n      }\n",
                "",
            ),
            "E_MISSING_CONTROL",
            None,
        ),
        (
            mutate("revision: @item.revision,", ""),
            "E_MISSING_ARGUMENT",
            None,
        ),
        (
            mutate("id: @item.id,", "id: @item.title,"),
            "E_TYPE",
            Some("@item.title"),
        ),
        (
            mutate("action = complete_task;", "action = create_task;"),
            "E_KIND",
            Some("create_task"),
        ),
        (
            mutate(
                "editable_actions = [complete_task];",
                "editable_actions = [list_tasks];",
            ),
            "E_KIND",
            Some("list_tasks"),
        ),
        (
            mutate("identity development {\n    mode = local_dev;\n  }\n", ""),
            "E_MISSING_BLOCK",
            None,
        ),
        (
            mutate("imanari \"0.1\";", "imanari \"0.2\";"),
            "E_UNSUPPORTED_VERSION",
            Some("\"0.2\""),
        ),
        (
            mutate("max_length = 200;", "max_length = 2.5;"),
            "E_INTEGER",
            None,
        ),
        (
            mutate("max_length = 200;", "max_length = 9007199254740992;"),
            "E_INTEGER",
            Some("9007199254740992"),
        ),
        (
            mutate("arguments = {", "arguments = {\n          id: @item.id,"),
            "E_DUPLICATE",
            Some("id"),
        ),
        (
            mutate(
                "label = \"Mark complete\";",
                "label = \"Mark complete\";\n        label = \"again\";",
            ),
            "E_DUPLICATE",
            Some("label"),
        ),
        (
            mutate("title = \"My tasks\";", "title = \"My tasks;"),
            "E_STRING",
            None,
        ),
        (
            mutate("field title {", "field true {"),
            "E_RESERVED_WORD",
            Some("true"),
        ),
        (
            mutate(
                "min_length = 1;\n      max_length = 200;\n    }\n\n    field owner",
                "min_length = 300;\n      max_length = 200;\n    }\n\n    field owner",
            ),
            "E_RANGE",
            None,
        ),
    ];
    for (src, code, span) in cases {
        expect(&src, code, span);
    }
}

#[test]
fn enforces_parser_limits() {
    let big = format!("{FIXTURE}{}", " ".repeat(imanari::syntax::MAX_SOURCE));
    expect(&big, "E_LIMIT_SOURCE", None);
    let deep = mutate(
        "default = false;",
        &format!("default = {}0{};", "[".repeat(70), "]".repeat(70)),
    );
    expect(&deep, "E_LIMIT_DEPTH", None);
    let mut nested = String::new();
    for i in 0..70 {
        nested.push_str(&format!("field f{i} {{ "));
    }
    nested.push_str(&"} ".repeat(70));
    expect(
        &mutate(
            "owner_field = owner;",
            &format!("owner_field = owner; {nested}"),
        ),
        "E_LIMIT_DEPTH",
        None,
    );
    let many: String = (0..10_001).map(|i| format!("p{i} = 1;\n")).collect();
    expect(
        &mutate("owner_field = owner;", &many),
        "E_LIMIT_DECLARATIONS",
        None,
    );
}

#[test]
fn bounded_malformed_input_never_panics() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let alphabet = b"{}[]();=@.,:\"\\/ \n\tabz_09-";
    let bytes = FIXTURE.as_bytes();
    let mut rejected = 0;
    for _ in 0..3000 {
        let mut b = bytes.to_vec();
        for _ in 0..(1 + next() % 4) {
            let at = (next() as usize) % b.len();
            match next() % 3 {
                0 => {
                    b.remove(at);
                }
                1 => b.insert(at, alphabet[(next() as usize) % alphabet.len()]),
                _ => b[at] = alphabet[(next() as usize) % alphabet.len()],
            }
        }
        let Ok(src) = String::from_utf8(b) else {
            continue;
        };
        match compile(&src, FILE) {
            Ok(_) => {}
            Err(ds) => {
                rejected += 1;
                assert!(!ds.is_empty());
                for d in ds {
                    assert!(
                        d.span.start <= d.span.end && d.span.end <= src.len(),
                        "{d:?}"
                    );
                }
            }
        }
        if let Ok(f) = format_source(&src, FILE) {
            assert_eq!(
                format_source(&f, FILE).unwrap(),
                f,
                "fmt idempotent for {src}"
            );
        }
    }
    assert!(rejected > 1000);
}

#[test]
fn graph_matches_committed_snapshot() {
    let (c, _) = compile(FIXTURE, FILE).unwrap();
    let graph = ir::canonical(&ir::to_json(&c.graph)) + "\n";
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/snapshots/fieldwork.graph.json"
    );
    let snapshot = std::fs::read_to_string(path)
        .expect("committed snapshot; regenerate with `nari build` and copy .nari/graph.json");
    assert_eq!(graph, snapshot);
    assert_eq!(
        c.graph_sha256,
        ir::sha256_hex(ir::canonical_compact(&ir::to_json(&c.graph)).as_bytes())
    );
}

#[test]
fn init_creates_a_checked_project_and_never_overwrites() {
    let dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("init-{}", imanari::random_hex(4)));
    std::fs::create_dir_all(&dir).unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_nari"))
            .args(["--project", dir.to_str().unwrap()])
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["init", "demo_app"]).status.success());
    let src = std::fs::read_to_string(dir.join("demo_app/demo_app.nari")).unwrap();
    assert!(compile(&src, "demo_app.nari").is_ok());
    assert!(dir.join("demo_app/acceptance/complete_task.json").exists());
    let again = run(&["init", "demo_app"]);
    assert_eq!(again.status.code(), Some(3));
    assert_eq!(run(&["init", "Bad-Name"]).status.code(), Some(2));
}

#[test]
fn hello_example_compiles_and_is_formatted() {
    let src = include_str!("../examples/hello/hello.nari");
    let (c, warnings) = compile(src, "hello.nari").expect("hello compiles");
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(c.app.name, "hello");
    assert_eq!(
        format_source(src, "hello.nari").expect("hello formats"),
        src
    );
}
