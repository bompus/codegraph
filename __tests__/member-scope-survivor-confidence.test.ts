/**
 * The C#, VB.NET and Objective-C member rules remove candidates a bare name
 * cannot mean. Whatever one of them leaves is trusted (0.9) only when the
 * scope binds it: a member of the class the call is written in or inherits.
 * A survivor the rules merely did not rule out, such as an Objective-C
 * function kept because its file is named after a superclass, stays below
 * the 0.8 trust line.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-member-survivor-'));
  const files: Record<string, string> = {
    'src/Specs/PageBase.cs': `namespace App.Specs;

public class PageBase
{
    protected void Render() { }
}
`,
    'src/Other/Widget.cs': `namespace App.Other;

public class Widget
{
    public void Render() { }
}
`,
    'src/Specs/Page.cs': `namespace App.Specs;

public class Page : PageBase
{
    public void Run()
    {
        Render();
    }
}
`,
    'Core/SDBase.h': `@interface SDBase : NSObject
@end
`,
    'Core/SDBase.m': `#import "SDBase.h"
void sd_reset(void) {
}
`,
    'Core/SDOther.m': `@implementation SDOther
- (void)sd_reset {
}
@end
`,
    'Core/SDCache.h': `#import "SDBase.h"
@interface SDCache : SDBase
@end
`,
    'Core/SDCache.m': `#import "SDCache.h"
@implementation SDCache
- (void)go {
    [self sd_reset];
}
@end
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

describe('member-rule survivors', () => {
  it('trust a C# base-class member the scope binds', () => {
    expect(callsFrom('src/Specs/Page.cs')).toEqual([{ target: 'App.Specs::PageBase::Render', confidence: 0.9 }]);
  });

  it('keep an Objective-C function the rule only failed to rule out below the trust line', () => {
    const calls = callsFrom('Core/SDCache.m');
    expect(calls.map((c) => c.target)).toEqual(['sd_reset']);
    expect(calls[0]!.confidence).toBeLessThan(0.8);
  });
});
