import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src/index';

describe('python call-receiver fabrication (kernel arm)', () => {
  let tempDir: string;
  let cg: CodeGraph | undefined;


  beforeEach(() => {
    tempDir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-py-fab-'));
  });

  afterEach(() => {
    cg?.close();
    cg = undefined;
    fs.rmSync(tempDir, { recursive: true, force: true });
  });

  it('resolves a module-qualified call and fabricates none from chained receivers (#66)', async () => {
    fs.writeFileSync(
      path.join(tempDir, 'ledger.py'),
      'def append(row):\n    return True\n\n\ndef path():\n    return "ledger.jsonl"\n'
    );
    fs.writeFileSync(
      path.join(tempDir, 'record.py'),
      'from . import ledger\n\n\ndef add_outcome(row):\n    if not ledger.append(row):\n        return None\n    return ledger.path()\n'
    );
    fs.writeFileSync(
      path.join(tempDir, 'unrelated.py'),
      'class Box:\n    def __init__(self):\n        self.data = []\n\n    def build_map(self):\n        self.data.append({"x": 1})\n        rows = {}\n        rows["k"] = []\n        rows["k"].append(2)\n        return rows\n'
    );

    cg = await CodeGraph.init(tempDir, { index: true });

    const ledgerAppend = cg
      .getNodesByKind('function')
      .find((n) => n.name === 'append' && n.filePath.replace(/\\/g, '/').endsWith('ledger.py'));
    expect(ledgerAppend).toBeDefined();

    const addOutcome = cg.getNodesByKind('function').find((n) => n.name === 'add_outcome');
    expect(addOutcome).toBeDefined();
    expect(
      cg
        .getOutgoingEdges(addOutcome!.id)
        .filter((e) => e.kind === 'calls')
        .map((e) => e.target)
    ).toContain(ledgerAppend!.id);

    // Both non-identifier receivers live in build_map; neither may reach
    // ledger.py's append.
    const buildMap = cg.getNodesByKind('method').find((n) => n.name === 'build_map');
    expect(buildMap).toBeDefined();
    expect(
      cg
        .getOutgoingEdges(buildMap!.id)
        .filter((e) => e.kind === 'calls')
        .map((e) => e.target)
    ).not.toContain(ledgerAppend!.id);
  });
});
