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

#[test]
fn unicode_categories_match_node_over_entire_code_space() {
    let result = std::process::Command::new("fnm")
        .args(["exec", "--using", "codegraph", "node"])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("harness/generate-unicode.cjs"))
        .arg("--check")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for c in [
        '\u{088f}', '\u{0c5c}', '\u{0cdc}', '\u{a7ce}', '\u{a7cf}', '\u{a7d2}', '\u{a7d4}',
        '\u{a7f1}',
    ] {
        assert_eq!(
            dead_code::mention_count(&format!("{c}foo foo{c}"), "foo", 2),
            0
        );
    }
}

#[test]
fn collation_matches_node_for_every_unicode_scalar() {
    let result = std::process::Command::new("fnm")
        .args(["exec", "--using", "codegraph", "node"])
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("harness/collation-oracle.cjs"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout.len(), (0x110000 - 0x800) * 5);
    let collator = codegraph_rust_pilot::collation::Collator::default();
    let mut previous: Option<char> = None;
    for entry in result.stdout.as_chunks::<5>().0 {
        let c = char::from_u32(u32::from_le_bytes(entry[..4].try_into().unwrap())).unwrap();
        if let Some(p) = previous {
            assert_eq!(
                collator.compare(p.encode_utf8(&mut [0; 4]), c.encode_utf8(&mut [0; 4])),
                (entry[4] as i32 - 1).cmp(&0),
                "U+{:04X}, U+{:04X}",
                p as u32,
                c as u32
            );
        }
        previous = Some(c);
    }
}

#[test]
fn relative_root_rejects_parent_escape() {
    // Resolving a relative root must not turn it into an empty containment prefix.
    assert!(dead_code::default_source_reader(std::path::Path::new("."), "../Cargo.toml").is_none());
}

#[test]
fn large_regex_scans_do_not_collect_unneeded_tail_matches() {
    let text = format!("NestFactory.create {}", "class A {} ".repeat(200_000));
    assert!(codegraph_rust_pilot::text::test(
        r"\bNestFactory\s*\.\s*create",
        &text
    ));
    assert_eq!(
        codegraph_rust_pilot::text::first(r"\bclass\b", &text)
            .unwrap()
            .start,
        19
    );
    assert!(!codegraph_rust_pilot::text::test(
        r"\bRouterModule\b",
        &text
    ));
}

#[test]
fn stored_json_uses_js_numbers_keys_and_length_coercion() {
    use codegraph_rust_pilot::json::{canonical, has_positive_length};
    let value =
        serde_json::from_str(r#"{"10":1.0,"9":2e0,"small":0.0000001,"large":1e21,"negzero":-0.0}"#)
            .unwrap();
    assert_eq!(
        canonical(&value),
        r#"{"9":2,"10":1,"large":1e+21,"negzero":0,"small":1e-7}"#
    );
    for text in [
        r#"{"length":2}"#,
        r#"{"length":"2"}"#,
        r#"{"length":true}"#,
        r#"{"length":[2]}"#,
    ] {
        assert!(
            has_positive_length(&serde_json::from_str(text).unwrap()),
            "{text}"
        );
    }
    for text in [
        r#"{"length":0}"#,
        r#"{"length":null}"#,
        r#"{"length":"no"}"#,
        r#"{"length":{}}"#,
    ] {
        assert!(
            !has_positive_length(&serde_json::from_str(text).unwrap()),
            "{text}"
        );
    }
}

#[test]
fn reviewer_nest_fixtures_match_real_typescript() {
    let crate_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for name in ["review-ordering.json", "review-nest-regressions.json"] {
        let input = crate_path.join("harness").join(name);
        let ts = std::process::Command::new("fnm")
            .args(["exec", "--using", "codegraph", "node"])
            .arg(crate_path.join("harness/run-ts.cjs"))
            .arg(crate_path.join("harness/nest.ts"))
            .arg(&input)
            .output()
            .unwrap();
        let rust = std::process::Command::new(env!("CARGO_BIN_EXE_codegraph-rust-pilot"))
            .arg("nestjs")
            .arg(&input)
            .output()
            .unwrap();
        assert!(
            ts.status.success(),
            "{}",
            String::from_utf8_lossy(&ts.stderr)
        );
        assert!(
            rust.status.success(),
            "{}",
            String::from_utf8_lossy(&rust.stderr)
        );
        assert!(ts.stdout == rust.stdout, "differential fixture {name}");
    }
}

#[test]
fn json_metadata_retains_surrogates_and_number_overflow() {
    use codegraph_rust_pilot::json::{canonical_text, raw_has_positive_length};
    assert_eq!(
        canonical_text(r#"{"10":1.0,"9":2e0,"\ud800":"\udc00","text":"𝄞\u2028","n":1e999}"#)
            .unwrap(),
        "{\"9\":2,\"10\":1,\"n\":null,\"text\":\"𝄞\u{2028}\",\"\\ud800\":\"\\udc00\"}"
    );
    assert!(raw_has_positive_length(r#"{"length":2,"other":"\ud800"}"#));
    assert!(raw_has_positive_length(r#"{"length":1e999}"#));
    assert!(raw_has_positive_length(r#""\ud800""#));
    assert!(!raw_has_positive_length(r#"{"length":"\ud800"}"#));
}

#[test]
fn raw_json_matches_node_for_nested_metadata_and_utf16_keys() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let deep = format!("{}1e999{}", "[".repeat(200), "]".repeat(200));
    let input = format!(
        r#"[{{"\ud800":"\udfff","😀":0,"ｚ":0,"10":2e0,"9":1.0,"huge":9007199254740993,"tiny":5e-324,"decimal":0.100000000000000005,"pair":"\ud834\udd1e","repeat":1,"repeat":2}},{}]"#,
        deep
    );
    let crate_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new("fnm")
        .args(["exec", "--using", "codegraph", "node"])
        .arg(crate_path.join("harness/run-ts.cjs"))
        .arg(crate_path.join("harness/json-oracle.ts"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        format!(
            "{}\n",
            codegraph_rust_pilot::json::canonical_text(&input).unwrap()
        )
        .as_bytes(),
        result.stdout
    );
}
