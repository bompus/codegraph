use codegraph_rust_pilot::json::{canonical, canonical_text, enumeration_order};
use codegraph_rust_pilot::{nestjs, types::MemoryContext};
use serde_json::{Value, json};
use std::{env, fs};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("dead-code") {
        let root = std::path::Path::new(&args[2]);
        let store =
            codegraph_rust_pilot::store::Store::open(&root.join(".codegraph/codegraph.db"))?;
        let queries: Vec<Value> = serde_json::from_slice(&fs::read(&args[3])?)?;
        #[derive(serde::Serialize)]
        struct Outcome {
            query: Value,
            report: codegraph_rust_pilot::dead_code::Report,
        }
        let mut results = vec![];
        for query in queries {
            let parsed = serde_json::from_value(query.clone())?;
            let report =
                codegraph_rust_pilot::dead_code::build_dead_code_report(&store, root, &parsed)?;
            results.push(Outcome { query, report });
        }
        println!("{}", canonical_text(&serde_json::to_string(&results)?)?);
        return Ok(());
    }
    if args.get(1).map(String::as_str) != Some("nestjs") {
        return Err("usage: codegraph-rust-pilot nestjs INPUT.json".into());
    }
    let mut input: Value = serde_json::from_slice(&fs::read(&args[2])?)?;
    enumeration_order(&mut input);
    let mut results = vec![];
    for case in input.as_array().ok_or("expected cases")? {
        let mut ctx: MemoryContext = serde_json::from_value(case["context"].clone())?;
        let mut extracts = serde_json::Map::new();
        for (path, content) in &ctx.files {
            let out = nestjs::extract(path, content, ctx.now);
            ctx.nodes.extend(out.nodes.clone());
            extracts.insert(path.clone(), serde_json::to_value(out)?);
        }
        let detected = nestjs::detect(&ctx);
        let resolved: Vec<_> = ctx.refs.iter().map(|r| nestjs::resolve(r, &ctx)).collect();
        let post = nestjs::post_extract(&ctx);
        for update in &post {
            if let Some(node) = ctx.nodes.iter_mut().find(|n| n.id == update.id) {
                *node = update.clone();
            }
        }
        let post_again = nestjs::post_extract(&ctx);
        results.push(json!({"name": case["name"], "extract": extracts, "detect": detected, "resolve": resolved, "postExtract": post, "postAgain": post_again}));
    }
    println!("{}", canonical(&serde_json::to_value(results)?));
    Ok(())
}
