use codegraph_rust_pilot::{dead_code, nestjs, store::Store, text::strip_comments};
use std::{fs, path::PathBuf};

fn test_directory(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!("{}-{name}", std::process::id()));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn comment_stripping_preserves_utf16_columns_and_route_strings() {
    let source = "/* 😀\n路 */ @Get('http://a/*b*/😀') send() {} // @Get('fake')";
    let stripped = strip_comments(source);
    assert_eq!(
        source.encode_utf16().count(),
        stripped.encode_utf16().count()
    );
    let output = nestjs::extract("unicode.controller.ts", source, 123);
    assert_eq!(output.nodes.len(), 1);
    assert_eq!(output.nodes[0].name, "GET /http://a/*b*/😀");
    assert_eq!(output.nodes[0].start_line, 2);
    assert_eq!(
        output.nodes[0].end_column,
        "@Get('http://a/*b*/😀')".encode_utf16().count() as i64
    );
    assert_eq!(output.references[0].reference_name, "send");
}

#[test]
fn mentions_obey_js_identifier_boundaries_and_stop_threshold() {
    assert_eq!(
        dead_code::mention_count("foobar $foo foo$ _foo foo_ éfoo foo9 foo foo", "foo", 2),
        2
    );
    assert_eq!(dead_code::mention_count("foo foo foo", "foo", 1), 1);
    assert_eq!(dead_code::mention_count("anything", "", 2), 0);
    // JS indexes the neighboring supplementary letter as a surrogate code unit.
    assert_eq!(dead_code::mention_count("𐐀foo", "foo", 2), 1);
}

#[test]
fn source_reader_refuses_escapes_and_oversized_sources() {
    let base = test_directory("reader");
    let root = base.join("project");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("ok.ts"), b"const ok = 1;\xff").unwrap();
    fs::write(base.join("outside.ts"), "secret fixture").unwrap();
    let big = fs::File::create(root.join("big.ts")).unwrap();
    big.set_len(dead_code::MAX_CORROBORATION_BYTES + 1).unwrap();
    assert_eq!(
        dead_code::default_source_reader(&root, "./nested/../ok.ts"),
        Some("const ok = 1;�".into())
    );
    assert_eq!(
        dead_code::default_source_reader(&root, "../outside.ts"),
        None
    );
    assert_eq!(dead_code::default_source_reader(&root, "big.ts"), None);
    assert_eq!(dead_code::default_source_reader(&root, "."), None);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dead_code_folds_unused_members_and_counts_exclusions() {
    let base = test_directory("report");
    let database = base.join("graph.db");
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.execute_batch("CREATE TABLE nodes (id TEXT PRIMARY KEY,kind TEXT,name TEXT,qualified_name TEXT,file_path TEXT,language TEXT,start_line INTEGER,end_line INTEGER,start_column INTEGER,end_column INTEGER,updated_at INTEGER,docstring TEXT,signature TEXT,visibility TEXT,is_exported INTEGER,is_async INTEGER,is_static INTEGER,is_abstract INTEGER,decorators TEXT,type_parameters TEXT,return_type TEXT);
        CREATE TABLE edges (source TEXT,target TEXT,kind TEXT);
        CREATE TABLE files (path TEXT PRIMARY KEY,generated INTEGER);
        CREATE TABLE unresolved_refs (from_node_id TEXT,reference_name TEXT,name_tail TEXT,reference_kind TEXT);").unwrap();
    for (id, kind, name, file, start, end, exported) in [
        ("live", "function", "live", "live.ts", 1, 3, 1),
        ("anchor", "function", "anchor", "dead.ts", 1, 1, 0),
        ("class", "class", "Unused", "dead.ts", 2, 20, 0),
        ("method", "method", "forgotten", "dead.ts", 4, 10, 0),
        ("test", "function", "helper", "tests/thing.ts", 1, 4, 0),
    ] {
        conn.execute("INSERT INTO nodes (id,kind,name,qualified_name,file_path,language,start_line,end_line,start_column,end_column,updated_at,is_exported,is_async,is_static,is_abstract) VALUES (?,?,?,? ,?,'typescript',?,?,0,0,123,?,0,0,0)",
            rusqlite::params![id,kind,name,format!("{file}::{name}"),file,start,end,exported]).unwrap();
    }
    conn.execute_batch("INSERT INTO edges VALUES ('live','anchor','calls'),('anchor','live','calls'),('class','method','contains');").unwrap();
    drop(conn);
    let store = Store::open(&database).unwrap();
    let report =
        dead_code::build_with_reader(&store, &dead_code::DeadCodeQuery::default(), None).unwrap();
    assert_eq!(report.total, 1);
    assert_eq!(report.entries[0].node.name, "Unused");
    assert_eq!(report.entries[0].members[0].name, "forgotten");
    assert_eq!(report.excluded["nested"], 1);
    assert_eq!(report.excluded["tests"], 1);
    assert!(!report.corroborated);
    let query = serde_json::from_str(r#"{"kinds":["banana"]}"#).unwrap();
    let fallback = dead_code::build_with_reader(&store, &query, None).unwrap();
    assert_eq!(fallback.kinds, report.kinds);
    drop(store);
    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dependency_detection_cache_invalidates_on_file_count_change() {
    let mut ctx: codegraph_rust_pilot::types::MemoryContext = serde_json::from_str(
        r#"{"files":{},"manifests":{"package.json":"{}"},"nodes":[],"refs":[],"now":1}"#,
    )
    .unwrap();
    assert!(!nestjs::detect(&ctx));
    ctx.manifests.insert(
        "package.json".into(),
        r#"{"dependencies":{"@nestjs/core":"*"}}"#.into(),
    );
    assert!(!nestjs::detect(&ctx));
    ctx.files
        .insert("src/plain.ts".into(), "const x = 1;".into());
    assert!(nestjs::detect(&ctx));
}
