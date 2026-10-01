/**
 * Calls on a library's own types and modules stay the library's, not a
 * same-named project method's:
 *
 * - Rust `Vec::new()` / `task::spawn(…)`: a type the project doesn't declare,
 *   and a module path (tokio's `task::spawn` went to TaskTracker's `spawn`
 *   300 times, `Vec::new` to a VecWithInitialized);
 * - a link further down a multi-line Rust chain is named after the chain's
 *   head: `bat()\n.stdout(…)` is assert_cmd's, `Arg::new("x")\n.value_hint(…)`
 *   the project's Arg;
 * - Go's `w.Header().Get(…)` is net/http's Header, not gin's `Context.Get`;
 * - R's bare `range(x)` is base R's, not a ggproto object's `range` method;
 * - JS `response.text()` is fetch's Response, not a project class's `text`.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-library-receivers-'));
  const files: Record<string, string> = {
    'src/lib.rs': `pub struct VecWithInitialized;
impl VecWithInitialized {
    pub fn new() -> Self { VecWithInitialized }
}

pub struct TaskTracker;
impl TaskTracker {
    pub fn spawn(&self) {}
}

pub struct OutputType;
impl OutputType {
    pub fn stdout(&self) {}
}

pub struct Arg;
impl Arg {
    pub fn new(name: &str) -> Arg { Arg }
    pub fn value_hint(self, hint: u8) -> Arg { self }
    pub fn stdout(self, text: &str) -> Arg { self }
}

pub struct Builder;
impl Builder {
    pub fn new() -> Self { Builder }
    pub fn stage(&mut self) -> &mut Self { self }
    pub fn current_dir(&mut self, text: &str) -> &mut Self { self }
    pub fn line_number(&mut self, value: bool) -> &mut Self { self }
}
pub struct WrongBuilder;
impl WrongBuilder {
    pub fn current_dir(&mut self, text: &str) -> &mut Self { self }
    pub fn line_number(&mut self, value: bool) -> &mut Self { self }
    pub fn filter(&self) {}
}
pub fn current_dir() {}
pub struct Format;
impl Format { pub fn is_empty(&self) -> bool { false } }
pub struct WrongFormat;
impl WrongFormat { pub fn is_empty(&self) -> bool { false } }
pub struct Config;
impl Config { pub fn format(&self) -> &Format { &Format } }
pub struct Container { config: Config }
impl Container { pub fn inspect(&self) { self.config.format().is_empty(); } }
#[derive(Clone)]
pub struct Selection;
impl Selection { pub fn map(self, f: fn(u8) -> u8) -> Self { self } }
pub fn selected(selections: &[Selection]) {
    for selection in selections.iter() { selection.clone().map(|v| v); }
}

mod inline_tests {
    use super::Builder;
    pub fn construct() { Builder::new().stage(); }
}
pub fn run() {
    let mut builder = Builder::new();
    builder
        .stage().stage().stage().stage().stage().stage().stage()
        .stage().stage().stage().stage().stage().stage().stage()
        .current_dir("x")
        .line_number(true);
    current_dir();
    [1, 2].iter()
        .filter(|x| true);
    let v: Vec<u8> = Vec::new();
    task::spawn(async {});
    bat(
        "x"
    )
        /* pipe assertion */
        .assert()
        .stdout("x");
    let a = Arg::new("cmd")
        .value_hint(1)
        .stdout("x");
}
`,
    'context.go': `package gin

type Context struct{}

func (c *Context) Get(key string) (any, bool) { return nil, false }

type Header struct{}
func (h *Header) Get(key string) string { return key }
func (c *Context) Header() *Header { return &Header{} }
func own(c *Context) string { return c.Header().Get("x") }

func handler(w Writer) string {
	return w.Header().Get("Content-Type")
}
`,
    'R/coord.R': `Coord <- ggproto("Coord",
  range = function(panel_params) {
    list(x = panel_params$x$dimension())
  }
)

scale_limits <- function(x) {
  range(x)
}
`,
    'src/file.js': `export class LazyFile {
  text() { return ''; }
}

export async function load(response) {
  return await response.text();
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

/** `Owner::member` of every call edge out of a file. */
function callsFrom(file: string): string[] {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids)
    .filter((e) => e.kind === 'calls')
    .map((e) => cg.getNode(e.target)!.qualifiedName)
    .sort();
}

describe('calls on a library’s types and modules', () => {
  it('Rust: outside types, module paths and multi-line chains', () => {
    const calls = callsFrom('src/lib.rs');
    expect(calls).not.toContain('VecWithInitialized::new');
    expect(calls).not.toContain('TaskTracker::spawn');
    expect(calls).not.toContain('OutputType::stdout');
    expect(calls).toContain('Arg::value_hint');
    expect(calls).toContain('Arg::stdout');
    expect(calls).toContain('Builder::current_dir');
    expect(calls).toContain('Builder::line_number');
    expect(calls).toContain('current_dir');
    expect(calls).not.toContain('WrongBuilder::current_dir');
    expect(calls).not.toContain('WrongBuilder::line_number');
    expect(calls).not.toContain('WrongBuilder::filter');
    expect(calls).toContain('Selection::map');
    expect(calls).toContain('Format::is_empty');
    expect(calls).not.toContain('WrongFormat::is_empty');
    const construct = cg.getNodesInFile('src/lib.rs').find(n => n.name === 'construct')!;
    expect(cg.getOutgoingEdges(construct.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)?.qualifiedName)).toContain('Builder::stage');
  });

  it('Go: net/http Header methods', () => {
    expect(callsFrom('context.go')).not.toContain('Context::Get');
    const ids = cg.getNodesInFile('context.go').map(n => n.id);
    const calls = cg.getOutgoingEdgesFrom(ids).filter(e => e.kind === 'calls');
    expect(calls.filter(e => cg.getNode(e.source)?.name === 'own').map(e => cg.getNode(e.target)?.qualifiedName)).toContain('Header::Get');
    expect(calls.filter(e => cg.getNode(e.source)?.name === 'handler').map(e => cg.getNode(e.target)?.qualifiedName)).not.toContain('Header::Get');
  });

  it('R: a bare call is a function, not an object’s method', () => {
    expect(callsFrom('R/coord.R').filter((q) => q.endsWith('range'))).toEqual([]);
  });

  it('JS: fetch Response bodies', () => {
    expect(callsFrom('src/file.js')).not.toContain('LazyFile::text');
  });
});

it('resolves Rust factory return types in their declaration scope', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rust-factory-scope-'));
  let graph: CodeGraph | undefined;
  try {
    fs.mkdirSync(path.join(dir, 'src'));
    fs.writeFileSync(path.join(dir, 'Cargo.toml'), '[package]\nname="app"\nversion="0.1.0"\n');
    fs.writeFileSync(path.join(dir, 'src/factory.rs'), `pub struct Factory;
impl Factory { pub fn new() -> Product { Product } }
pub struct Product;
impl Product { pub fn stdout(&self) {} }
`);
    fs.writeFileSync(path.join(dir, 'src/lib.rs'), `mod factory;
use factory::Factory;
struct Product;
impl Product { fn stdout(&self) {} }
fn run() { Factory::new().stdout(); }
`);
    graph = await CodeGraph.init(dir, { index: true });
    const run = graph.getNodesInFile('src/lib.rs').find(n => n.name === 'run')!;
    const targets = graph.getOutgoingEdges(run.id).filter(e => e.kind === 'calls').map(e => graph!.getNode(e.target)!);
    expect(targets.filter(n => n.name === 'stdout').map(n => n.filePath)).toEqual(['src/factory.rs']);
  } finally { graph?.close(); fs.rmSync(dir, { recursive: true, force: true }); }
});


it('keeps duplicate constructor names only when their requested member is unique', async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-rust-duplicate-builder-'));
  let graph: CodeGraph | undefined;
  try {
    fs.mkdirSync(path.join(dir, 'src'));
    fs.writeFileSync(path.join(dir, 'src/lib.rs'), `mod first; mod second;
use external::RegexMatcherBuilder;
pub fn use_builder() { RegexMatcherBuilder::new().line_terminator(true); RegexMatcherBuilder::new().ambiguous(); }
`);
    fs.writeFileSync(path.join(dir, 'src/first.rs'), `pub struct RegexMatcherBuilder;
impl RegexMatcherBuilder { pub fn new() -> Self { Self } pub fn line_terminator(&mut self, value: bool) -> &mut Self { self } pub fn ambiguous(&self) {} }
`);
    fs.writeFileSync(path.join(dir, 'src/second.rs'), `pub struct RegexMatcherBuilder;
impl RegexMatcherBuilder { pub fn new() -> Self { Self } pub fn ambiguous(&self) {} }
`);
    graph = await CodeGraph.init(dir, { index: true });
    const edges = graph.getOutgoingEdgesFrom(graph.getNodesInFile('src/lib.rs').map(n => n.id)).filter(e => e.kind === 'calls');
    const calls = edges.filter(e => graph!.getNode(e.target)?.name === 'line_terminator');
    expect(calls.map(e => graph!.getNode(e.target)?.filePath)).toEqual(['src/first.rs']);
    expect(Number(calls[0]!.metadata?.confidence)).toBeLessThanOrEqual(0.7);
    expect(edges.map(e => graph!.getNode(e.target)?.name)).not.toContain('ambiguous');
  } finally { graph?.close(); fs.rmSync(dir, { recursive: true, force: true }); }
});
