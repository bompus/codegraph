/**
 * Outbound HTTP calls to hosts no route in the index serves become `endpoint`
 * nodes, linked from the calling function, so "what external services does
 * this call?" is answered by the graph.
 */

import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as path from 'path';
import * as os from 'os';
import CodeGraph from '../src/index';
import { ToolHandler } from '../src/mcp/tools';

const API = [
  "import axios from 'axios';",
  '',
  'export async function loadRepo(owner: string) {',
  '  const r = await fetch(`https://api.github.com/repos/${owner}`);',
  '  return r.json();',
  '}',
  '',
  'export async function createUser(user: object) {',
  "  return axios.post('https://API.example.com/v1/users', user);",
  '}',
  '',
  "const billing = axios.create({ baseURL: 'https://billing.example.com/api' });",
  '',
  'export function invoices() {',
  "  return billing.get('/invoices');",
  '}',
  '',
  'export function offline() {',
  "  return fetch('fake://offline/x');",
  '}',
  '',
  'export function relative() {',
  "  // fetch('https://comment.example.com/x')",
  "  return fetch('/api/items');",
  '}',
  '',
].join('\n');

describe('external HTTP endpoints', () => {
  let dir: string;
  let cg: CodeGraph;

  const endpoints = () =>
    cg.getNodesByKind('endpoint').map((n) => n.name).sort();
  const calledEndpoints = (fn: string) => {
    const node = cg.getNodesByName(fn).find((n) => n.kind === 'function')!;
    return cg.getCallees(node.id).filter((c) => c.node.kind === 'endpoint').map((c) => c.node.name);
  };

  beforeEach(async () => {
    dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-endpoints-'));
    fs.mkdirSync(path.join(dir, 'src'));
    fs.writeFileSync(path.join(dir, 'src/api.ts'), API);
    cg = CodeGraph.initSync(dir, { config: { include: ['**/*.ts'], exclude: [] } });
    await cg.indexAll();
  });

  afterEach(() => {
    try { cg.close(); } catch { /* ignore */ }
    fs.rmSync(dir, { recursive: true, force: true });
  });

  it('creates one endpoint per external request and links the caller', () => {
    expect(endpoints()).toEqual([
      'GET https://api.github.com/repos/${…}',
      'GET https://billing.example.com/api/invoices',
      'POST https://api.example.com/v1/users',
    ]);
    expect(calledEndpoints('loadRepo')).toEqual(['GET https://api.github.com/repos/${…}']);
    expect(calledEndpoints('invoices')).toEqual(['GET https://billing.example.com/api/invoices']);
  });

  it('ignores non-http schemes, comments and relative paths', () => {
    expect(calledEndpoints('offline')).toEqual([]);
    expect(calledEndpoints('relative')).toEqual([]);
  });

  it('shows the endpoint in codegraph_node', async () => {
    const result = await new ToolHandler(cg).execute('codegraph_node', { symbol: 'createUser', includeCode: false });
    const text = result.content?.[0]?.type === 'text' ? result.content[0].text : '';
    expect(text).toMatch(/\*\*Calls →\*\* .*POST https:\/\/api\.example\.com\/v1\/users/);
  });

  // Synthesis runs on a full index, not an incremental sync, for every
  // synthesized edge (docs/design/callback-edge-synthesis.md, remaining work).
  it('drops an endpoint on re-index once its last call is removed', async () => {
    fs.writeFileSync(path.join(dir, 'src/api.ts'), API.replace("axios.post('https://API.example.com/v1/users', user)", 'Promise.resolve(user)'));
    await cg.indexAll();
    expect(endpoints()).toEqual([
      'GET https://api.github.com/repos/${…}',
      'GET https://billing.example.com/api/invoices',
    ]);
  });
});
