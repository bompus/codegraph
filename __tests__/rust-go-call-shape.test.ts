/**
 * Rust and Go extractors keep one receiver level, so a chain's later links
 * (`sym.filename().map(…)`, `c.Flags().String(…)`) arrive as bare names. Read
 * at the call: a bare call never reaches a method (axum's routing `get(h)`
 * went to a cookie jar's `get`, tokio's `drop(x)` to a `Drop` impl); a chained
 * call with a standard-library name (`unwrap`, `map`, Go's `String`) needs a
 * receiver named after the owner, while a project-specific one keeps its
 * match (clap's `flag("n").short('n')` → `Arg::short`).
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rust-go-shape-'));
  const files: Record<string, string> = {
    'Cargo.toml': '[package]\nname = "app"\nversion = "0.1.0"\n',
    'src/guard.rs': `pub struct MutexGuard;
impl MutexGuard {
    pub fn map(self) -> Self { self }
}
pub struct CookieJar;
impl CookieJar {
    pub fn get(&self) -> Option<u8> { None }
}
pub struct Arg;
impl Arg {
    pub fn short(self, c: char) -> Self { self }
}
pub fn flag(name: &str) -> Arg { Arg }
pub struct Worker;
impl Worker {
    pub fn run_nested(&self) {
        fn nested_value() -> u8 { 1 }
        let _ = nested_value();
    }
}
`,
    'src/main.rs': `mod guard;
mod local;
use guard::flag;
fn get(handler: u8) -> u8 { handler }
fn run(sym: Option<&str>) {
    let _ = sym.map(|s| s.len());
    let _ = get(1);
    let _ = flag("n").short('n');
}
struct Other;
impl Other { fn short(self, c: char) -> Self { self } }
fn shadowed() {
    fn flag(name: &str) -> Other { Other }
    let _ = flag("local").short('x');
}
fn main() { run(None); }
`,
    'src/local.rs': `use crate::Other;
fn flag(name: &str) -> crate::guard::Arg { crate::guard::Arg }
fn shadowedLocal() {
    fn flag(name: &str) -> Other {
        let _ = name.len();
        Other
    }
    let _ = flag("local").short('x');
}
`,
    'go.mod': 'module example.com/app\n\ngo 1.21\n',
    'flags.go': `package app

type customString struct{}

func (c customString) String() string { return "" }

type FlagSet struct{}

func Flags() *FlagSet { return nil }
`,
    'cmd.go': `package app

func run() {
	Flags().String("name", "", "")
}
`,
  };
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  cg = await CodeGraph.init(root, { index: true });
});

afterAll(() => {
  cg?.close();
  if (root) fs.rmSync(root, { recursive: true, force: true });
});

const callsIn = (file: string, fn: string): string[] => {
  const node = cg.getNodesInFile(file).find((n) => n.name === fn)!;
  return cg.getOutgoingEdges(node.id).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!.qualifiedName);
};

describe('Rust and Go call shapes', () => {
  it('Rust: bare never a method, std chain names need their receiver, project chain names keep theirs', () => {
    const targets = callsIn('src/main.rs', 'run');
    expect(targets.some((t) => t.endsWith('MutexGuard::map'))).toBe(false);
    expect(targets.some((t) => t.endsWith('CookieJar::get'))).toBe(false);
    expect(targets.some((t) => t.endsWith('Arg::short'))).toBe(true);
  });

  it('Rust: a local factory shadows an imported factory', () => {
    for (const [file, caller, owner] of [['src/main.rs', 'shadowed', 'Other'], ['src/local.rs', 'shadowedLocal', 'Other']]) {
      const targets = callsIn(file!, caller!);
      expect(targets.some(t => t.endsWith('Arg::short'))).toBe(false);
      expect(targets.some(t => t.endsWith(`${owner}::short`))).toBe(true);
    }
  });

  it('Rust: keeps a nested function called without a receiver', () => {
    expect(callsIn('src/guard.rs', 'run_nested').some((t) => t.endsWith('nested_value'))).toBe(true);
  });

  it('Go: a std-named chained call is not a project type’s method', () => {
    expect(callsIn('cmd.go', 'run').some((t) => t.endsWith('customString::String'))).toBe(false);
  });
});
