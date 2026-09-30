/**
 * A `global using static` applies to the project that declares it: the files
 * under the nearest `.csproj` directory. A bare name in one project reaches the
 * static type another project's global using brings in only as a guess, never
 * as a trusted edge.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-csharp-global-static-'));
  const files: Record<string, string> = {
    'ProjA/ProjA.csproj': `<Project Sdk="Microsoft.NET.Sdk"></Project>
`,
    'ProjA/GlobalUsings.cs': `global using static ProjA.Helpers.Guard;
`,
    'ProjA/Helpers/Guard.cs': `namespace ProjA.Helpers;

public static class Guard
{
    public static void Check(object value) { }
}
`,
    'ProjA/Service.cs': `namespace ProjA;

public class Service
{
    public void Run()
    {
        Check(this);
    }
}
`,
    'ProjB/ProjB.csproj': `<Project Sdk="Microsoft.NET.Sdk"></Project>
`,
    'ProjB/Worker.cs': `namespace ProjB;

public class Worker
{
    public void Run()
    {
        Check(this);
    }
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

function callsFrom(file: string): Array<{ target: string; confidence: number }> {
  const ids = cg.getNodesInFile(file).map((n) => n.id);
  return cg
    .getOutgoingEdgesFrom(ids)
    .filter((e) => e.kind === 'calls')
    .map((e) => ({ target: cg.getNode(e.target)!.qualifiedName, confidence: Number(e.metadata?.confidence) }));
}

describe('global using static', () => {
  it('binds a bare name in its own project', () => {
    expect(callsFrom('ProjA/Service.cs')).toEqual([{ target: 'ProjA.Helpers::Guard::Check', confidence: 0.9 }]);
  });

  it('does not reach into another project', () => {
    expect(callsFrom('ProjB/Worker.cs').filter((c) => c.confidence >= 0.8)).toEqual([]);
  });
});
