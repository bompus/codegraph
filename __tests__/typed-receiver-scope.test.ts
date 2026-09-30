/**
 * A receiver's type decides which class's method a call reaches, and a type
 * name means the class its scope names:
 * - Python `self.cache = Store()` in `__init__` types `self.cache.fetch()`,
 *   in the class and its subclasses;
 *   a nested def reads an outer function's annotated parameter.
 * - PHP names a type through its namespace or `use … as Alias`, and a
 *   class's parent is the one its declaration names, not every same-named
 *   class in another namespace.
 * - Ruby finds a constant lexically (`Sub` in top-level `App` is `::Sub`).
 * - A C# or Ruby receiver typed as a project class that lacks the method
 *   is not a call on whichever class has one (a C# extension method is).
 * Each case pairs the trigger with a control that differs by one line.
 */
import { describe, it, expect, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

const roots: string[] = [];
afterAll(() => {
  for (const r of roots.splice(0)) fs.rmSync(r, { recursive: true, force: true });
});

async function edges(files: Record<string, string>, kinds: string[] = ['calls', 'references', 'instantiates']): Promise<string[]> {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-typed-receiver-'));
  roots.push(root);
  for (const [rel, content] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), content);
  }
  const cg = await CodeGraph.init(root, { index: true });
  try {
    const out: string[] = [];
    for (const kind of ['function', 'method'] as const) {
      for (const n of cg.getNodesByKind(kind)) {
        for (const e of cg.getOutgoingEdgesFrom([n.id], kinds)) {
          const t = cg.getNode(e.target)!;
          out.push(`${n.qualifiedName} -${e.kind}-> ${t.qualifiedName}@${t.filePath}`);
        }
      }
    }
    return out.sort();
  } finally {
    cg.close();
  }
}

const PY_ATTR = `class Store:
    def fetch(self):
        return 1


class Other:
    def fetch(self):
        return 2


class App:
    def __init__(self):
        self.cache = Store()

    def run(self):
        return self.cache.fetch()


class Child(App):
    def again(self):
        return self.cache.fetch()
`;

const PY_CLOSURE = (annotation: string) => `class Data:
    fetch = 1


class Other:
    def load(self):
        return 2


class Store:
    def fetch(self):
        return 1


def outer(obj: ${annotation}, pool):
    def inner():
        pool.submit(obj.fetch)
    return inner
`;

const PHP_APP = `<?php
class App {
  public function __construct(private Sub $s) {}
  public function run() { return $this->s->baseMethod(); }
}
`;

const RB_DECOY = (sub: string) => `module Other
  class DecoyBase
    def base_method
      1
    end
  end

  class ${sub} < DecoyBase
  end
end
`;

const CS_DECOY = (sub: string) => `namespace Other {
  class DecoyBase { public int BaseMethod() { return 1; } }
  class ${sub} : DecoyBase {}
}
`;

describe('typed receivers and scoped type names', () => {
  it('Python: a field assigned a constructor in __init__ types its method calls', async () => {
    const got = await edges({ 'main.py': PY_ATTR });
    expect(got).toContain('App::run -calls-> Store::fetch@main.py');
    expect(got).not.toContain('App::run -calls-> Other::fetch@main.py');
    // A subclass reads the field its base's __init__ typed.
    expect(got).toContain('Child::again -calls-> Store::fetch@main.py');
  });

  it("Python: a nested def reads the outer function's annotated parameter", async () => {
    // `Data.fetch` is data, so passing it links nothing.
    const data = await edges({ 'main.py': PY_CLOSURE('Data') });
    expect(data.filter((e) => e.startsWith('outer::inner -references->'))).toEqual([]);
    const store = await edges({ 'main.py': PY_CLOSURE('Store') });
    expect(store).toContain('outer::inner -references-> Store::fetch@main.py');
  });

  it("PHP: an inherited member is looked up on the parent the class names, not a namesake's", async () => {
    const files = (decoyMid: string) => ({
      'App.php': PHP_APP,
      'Sub.php': `<?php
class Mid { public function other() { return 2; } }
class Sub extends Mid {}
`,
      'Decoy.php': `<?php
namespace Other;
class DecoyBase { public function baseMethod() { return 1; } }
class ${decoyMid} extends DecoyBase {}
`,
    });
    expect((await edges(files('Mid'))).filter((e) => e.includes('baseMethod'))).toEqual([]);
    // Control: no same-named parent elsewhere, same verdict.
    expect((await edges(files('Mid2'))).filter((e) => e.includes('baseMethod'))).toEqual([]);
  });

  it('PHP: a class name in a file with no namespace is the global class', async () => {
    const got = await edges({
      'App.php': PHP_APP,
      'Sub.php': `<?php
class Sub { public function other() { return 2; } }
`,
      'Decoy.php': `<?php
namespace Other;
class Sub { public function baseMethod() { return 1; } }
`,
    });
    expect(got).toContain('App::__construct -references-> Sub@Sub.php');
    expect(got).not.toContain('App::__construct -references-> Other::Sub@Decoy.php');
    expect(got.filter((e) => e.includes('baseMethod'))).toEqual([]);
  });

  it('PHP: a typed property whose type is imported under an alias', async () => {
    const lib = `<?php
namespace Lib;
class Store { public function fetch() { return 1; } }
class Other { public function fetch() { return 2; } }
`;
    const app = (use: string, type: string) => `<?php
namespace App;
use ${use};
class App {
  private ${type} $c;
  public function run() { return $this->c->fetch(); }
}
`;
    const aliased = await edges({ 'Lib/Store.php': lib, 'App.php': app('Lib\\Store as Cache', 'Cache') });
    expect(aliased).toContain('App::App::run -calls-> Lib::Store::fetch@Lib/Store.php');
    const plain = await edges({ 'Lib/Store.php': lib, 'App.php': app('Lib\\Store', 'Store') });
    expect(plain).toContain('App::App::run -calls-> Lib::Store::fetch@Lib/Store.php');
  });

  it('Ruby: a constant is the one lexical lookup finds', async () => {
    const files = (sub: string) => ({
      'sub.rb': `class Sub
  def other
    2
  end
end
`,
      'decoy.rb': RB_DECOY(sub),
      'app.rb': `class App
  def run
    base = Sub.new
    base.base_method
  end
end
`,
    });
    const got = await edges(files('Sub'));
    expect(got).toContain('App::run -instantiates-> Sub@sub.rb');
    expect(got.filter((e) => e.includes('base_method'))).toEqual([]);
    // Control: the one `Sub` has no `base_method` either, whatever the receiver is called.
    expect((await edges(files('Sub2'))).filter((e) => e.includes('base_method'))).toEqual([]);
  });

  it('C#: a typed receiver whose class lacks the method is not a call on another class', async () => {
    const app = `class Sub { public int Other() { return 2; } }

class App {
  public int Run(Sub s) { return s.BaseMethod(); }
}
`;
    for (const sub of ['Sub', 'Sub2']) {
      const got = await edges({ 'App.cs': app, 'Decoy.cs': CS_DECOY(sub) });
      expect(got.filter((e) => e.includes('BaseMethod'))).toEqual([]);
    }
    const ext = await edges({
      'App.cs': app,
      'Decoy.cs': CS_DECOY('Sub'),
      'Ext.cs': `static class SubExtensions {
  public static int BaseMethod(this Sub s) { return 3; }
}
`,
    });
    expect(ext.filter((e) => e.includes('BaseMethod'))).toEqual(['App::Run -calls-> SubExtensions::BaseMethod@Ext.cs']);
  });
});
