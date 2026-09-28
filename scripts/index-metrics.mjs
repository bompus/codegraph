#!/usr/bin/env node
// Metrics over a CodeGraph index, and a line-insensitive diff of two of them.
//
//   node scripts/index-metrics.mjs <a.db>                 # one index's metrics as JSON
//   node scripts/index-metrics.mjs <a.db> <b.db> [--labels a,b]
//
// Verifying a change that moves the graph (an upstream PR, a resolver fix)
// asks two questions of the index it produces: did the counts it claims to
// move actually move, and did any real call edge disappear. Raw edge rows
// cannot answer the second one: an unrelated edit above a call shifts its line
// number, so a row-for-row diff reports hundreds of false losses. The diff here
// keys calls on (source, target, refName) by qualified name, with
// multiplicities, which survives the shift and works across two different
// trees. `agent-eval/diff-index-drift.mjs` answers a different question (does
// an incrementally synced index match a rebuild of the SAME tree) and keys on
// node ids instead.
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const TOTALS_SQL =
  'SELECT (SELECT count(*) FROM nodes) nodes, (SELECT count(*) FROM edges) edges,' +
  ' (SELECT count(*) FROM files) files';

/** Self-call edges. Recursion is legitimate; a wrapper calling itself through a host API is not. */
const SELF_CALLS_SQL = "SELECT count(*) n FROM edges WHERE source=target AND kind='calls'";

/** Interface members: properties and methods qualified under an interface in the same file. */
const INTERFACE_MEMBERS_SQL =
  "SELECT count(*) n FROM nodes p JOIN nodes i ON i.kind='interface' AND p.file_path=i.file_path" +
  " AND p.qualified_name LIKE i.name || '::%' WHERE p.kind IN ('property','method')";

/** `imports` edges landing on a property or method rather than the module symbol. */
const IMPORT_TO_MEMBER_SQL =
  'SELECT count(*) n FROM edges e JOIN nodes t ON t.id=e.target' +
  " WHERE e.kind='imports' AND t.kind IN ('property','method')";

/** Calls whose source is a field or module-scope initializer rather than a function body. */
const INITIALIZER_CALLS_SQL =
  'SELECT count(*) n FROM edges e JOIN nodes s ON s.id=e.source' +
  " WHERE e.kind='calls' AND s.kind IN ('property','variable','constant')";

const RESOLVED_BY_SQL =
  "SELECT json_extract(metadata,'$.resolvedBy') r, count(*) c FROM edges" +
  " WHERE kind='calls' GROUP BY r ORDER BY c DESC";

export const CALL_EDGES_SQL =
  'SELECT s.qualified_name src, s.file_path srcFile, t.qualified_name dst, t.file_path dstFile,' +
  " json_extract(e.metadata,'$.refName') refName," +
  " json_extract(e.metadata,'$.resolvedBy') resolvedBy," +
  " json_extract(e.metadata,'$.confidence') confidence" +
  " FROM edges e JOIN nodes s ON s.id=e.source JOIN nodes t ON t.id=e.target WHERE e.kind='calls'";

/** `db` is anything with `prepare(sql).get()/.all()`: node:sqlite's DatabaseSync or bun:sqlite's Database. */
export function readIndexMetrics(db, label) {
  const count = (sql) => db.prepare(sql).get().n;
  const resolvedBy = {};
  for (const row of db.prepare(RESOLVED_BY_SQL).all()) {
    resolvedBy[row.r ?? 'unresolved'] = row.c;
  }
  return {
    label,
    ...db.prepare(TOTALS_SQL).get(),
    selfCallEdges: count(SELF_CALLS_SQL),
    interfaces: count("SELECT count(*) n FROM nodes WHERE kind='interface'"),
    interfaceMembers: count(INTERFACE_MEMBERS_SQL),
    importToMemberEdges: count(IMPORT_TO_MEMBER_SQL),
    initializerCallEdges: count(INITIALIZER_CALLS_SQL),
    resolvedBy,
  };
}

/** Identity of a call edge that survives an unrelated edit moving its line. */
export function callEdgeKey(r) {
  return [r.src, r.srcFile, r.dst, r.dstFile, r.refName ?? ''].join('|');
}

function tally(rows) {
  const m = new Map();
  for (const r of rows) {
    const k = callEdgeKey(r);
    const seen = m.get(k);
    if (seen) seen.n += 1;
    else m.set(k, { ...r, n: 1 });
  }
  return m;
}

/**
 * Call edges present in `a` more often than in `b`, and the reverse. An edge appearing twice in `a`
 * and once in `b` is one loss, not two: a call site duplicated in the source is a real second edge.
 */
export function diffCallEdges(a, b) {
  const A = tally(a);
  const B = tally(b);
  const excess = (x, y) =>
    [...x.entries()].flatMap(([k, d]) => {
      const other = y.get(k)?.n ?? 0;
      return d.n > other ? [{ ...d, n: d.n - other }] : [];
    });
  return { totalA: a.length, totalB: b.length, onlyInA: excess(A, B), onlyInB: excess(B, A) };
}

export function formatEdgeDelta(d) {
  const by = `${d.resolvedBy ?? 'unresolved'}@${d.confidence ?? '?'}`;
  return `  ${d.n}x ${d.src} (${d.srcFile}) -> ${d.dst} (${d.dstFile}) ref=${d.refName ?? ''} ${by}`;
}

export function formatEdgeDiff(diff, labelA = 'A', labelB = 'B') {
  const sum = (rows) => rows.reduce((s, r) => s + r.n, 0);
  const section = (label, rows) => [`only in ${label} (${sum(rows)}):`, ...rows.map(formatEdgeDelta)];
  return [
    `${labelA}: ${diff.totalA} calls edges; ${labelB}: ${diff.totalB}`,
    ...section(labelA, diff.onlyInA),
    ...section(labelB, diff.onlyInB),
  ].join('\n');
}

/** One line naming what moved between two metric sets: the row a verification comment quotes. */
export function summarizeMetrics(before, after) {
  const delta = (n, m) => (m === n ? `${n}` : `${n} -> ${m}`);
  return [
    `nodes ${delta(before.nodes, after.nodes)}`,
    `edges ${delta(before.edges, after.edges)}`,
    `self-calls ${delta(before.selfCallEdges, after.selfCallEdges)}`,
    `interface members ${delta(before.interfaceMembers, after.interfaceMembers)}`,
    `import-to-member ${delta(before.importToMemberEdges, after.importToMemberEdges)}`,
    `initializer calls ${delta(before.initializerCallEdges, after.initializerCallEdges)}`,
    `import-resolved ${delta(before.resolvedBy.import ?? 0, after.resolvedBy.import ?? 0)}`,
    `fuzzy ${delta(before.resolvedBy.fuzzy ?? 0, after.resolvedBy.fuzzy ?? 0)}`,
  ].join(', ');
}

async function main() {
  const args = process.argv.slice(2);
  const labelsAt = args.indexOf('--labels');
  const labels = labelsAt >= 0 ? args.splice(labelsAt, 2)[1].split(',') : ['a', 'b'];
  const [a, b] = args;
  if (!a) {
    console.error('usage: index-metrics.mjs <a.db> [b.db] [--labels a,b]');
    return 2;
  }
  for (const p of [a, b].filter(Boolean)) {
    // node:sqlite creates a missing file rather than failing; refuse first.
    if (!existsSync(p)) {
      console.error(`not found: ${p}`);
      return 2;
    }
  }
  const { DatabaseSync } = await import('node:sqlite');
  const measure = (path, label) => {
    const db = new DatabaseSync(path, { readOnly: true });
    try {
      return { metrics: readIndexMetrics(db, label), edges: db.prepare(CALL_EDGES_SQL).all() };
    } finally {
      db.close();
    }
  };
  const first = measure(a, labels[0]);
  console.log(JSON.stringify(first.metrics, null, 1));
  if (!b) return 0;
  const second = measure(b, labels[1]);
  console.log(JSON.stringify(second.metrics, null, 1));
  console.log(`[${labels[0]} -> ${labels[1]}] ${summarizeMetrics(first.metrics, second.metrics)}`);
  console.log(formatEdgeDiff(diffCallEdges(first.edges, second.edges), labels[0], labels[1]));
  return 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main().then((code) => process.exit(code));
}
