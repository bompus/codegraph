//! Internal extraction/parity probe, not a distributed CodeGraph CLI.
use codegraph_kernel::{contract_info, extract_native, NativeExtractBuffers};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::Path, time::Instant};

fn tables(output: &NativeExtractBuffers) -> [(&str, &[u8]); 6] {
    [
        ("meta", &output.meta),
        ("nodes", &output.nodes),
        ("edges", &output.edges),
        ("refs", &output.refs),
        ("bindings", &output.bindings),
        ("arena", &output.arena),
    ]
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--bench") && args.len() == 2 {
        // TSV rows are source path, language, and logical graph path. Load
        // once before timing, just as the production-addon benchmark does.
        let manifest = fs::read_to_string(&args[1])?;
        let inputs = manifest
            .lines()
            .map(|line| {
                let columns: Vec<_> = line.split('\t').collect();
                if columns.len() != 3 {
                    return Err("expected source, language, path".into());
                }
                Ok((
                    columns[2].to_owned(),
                    fs::read_to_string(columns[0])?,
                    columns[1].to_owned(),
                ))
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        if inputs.is_empty() {
            return Err("empty input manifest".into());
        }
        let mut last: Vec<Option<NativeExtractBuffers>> = (0..inputs.len()).map(|_| None).collect();
        let iterations = 100;
        let mut bytes: usize = 0;
        let start = Instant::now();
        for _ in 0..iterations {
            for (index, (path, content, language)) in inputs.iter().enumerate() {
                let output = extract_native(path, content, language)?;
                bytes += tables(&output)
                    .iter()
                    .map(|(_, table)| table.len())
                    .sum::<usize>();
                last[index] = Some(output);
            }
        }
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut hash = Sha256::new();
        for output in last.into_iter().flatten() {
            for (name, table) in tables(&output) {
                if name == "meta" {
                    if table.len() != 40 {
                        return Err("unexpected metadata layout".into());
                    }
                    let duration = f64::from_le_bytes(table[28..36].try_into()?);
                    if !duration.is_finite() || duration < 0.0 {
                        return Err("invalid duration".into());
                    }
                    hash.update(&table[..28]);
                    hash.update(&table[36..]);
                } else {
                    hash.update(table);
                }
            }
        }
        let contract = contract_info();
        println!("{{\"files\":{},\"iterations\":{},\"bytes\":{},\"elapsedMs\":{},\"outputHash\":\"{:x}\",\"contract\":{{\"abiVersion\":{},\"kernelVersion\":{:?},\"nodeKinds\":{:?},\"edgeKinds\":{:?},\"languages\":{:?}}}}}",
            inputs.len() * iterations, iterations, bytes, elapsed_ms, hash.finalize(),
            contract.abi_version, contract.kernel_version, contract.node_kinds, contract.edge_kinds, contract.languages);
        return Ok(());
    }
    if !(args.len() == 3 || args.len() == 4) {
        return Err("usage: extract-native <source> <language> <output-dir> [logical-path]; or --bench <manifest.tsv>".into());
    }
    let content = fs::read_to_string(&args[0])?;
    let output = extract_native(args.get(3).unwrap_or(&args[0]), &content, &args[1])?;
    fs::create_dir_all(&args[2])?;
    for (name, table) in tables(&output) {
        fs::write(Path::new(&args[2]).join(format!("{name}.bin")), table)?;
    }
    Ok(())
}
