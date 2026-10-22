import { describe, it, expect, beforeAll, afterAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

// `Logger.log()` must bind to Logger's own `log`, not to the first method in
// the file whose owner's name merely CONTAINS "Logger" (`FileLogger::log`).
// The method lookup matched the class name as a substring of the qualified
// name, so a class declared earlier in the same file with a longer name
// ending in the receiver's won every call.

let dir: string;
let cg: CodeGraph;

beforeAll(async () => {
  dir = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-owner-class-'));
  fs.writeFileSync(path.join(dir, 'log.py'), `class FileLogger:
    def log(self):
        pass

class Logger:
    def log(self):
        pass

def via_class():
    Logger.log()

def via_instance(logger: Logger):
    logger.log()
`);
  fs.writeFileSync(path.join(dir, 'log.ts'), `export class FileLogger {
  static log(): void {}
}

export class Logger {
  static log(): void {}
}

export function viaStatic(): void {
  Logger.log();
}
`);
  // A same-named NESTED class must not take a call that names the top-level
  // class — whether it is declared first, or is the only one with the method.
  fs.writeFileSync(path.join(dir, 'nested.py'), `class Outer:
    class Logger:
        def log(self):
            pass

class Logger:
    def log(self):
        pass

def nested_first():
    Logger.log()
`);
  fs.writeFileSync(path.join(dir, 'nested_only.py'), `class Plain:
    pass

class Box:
    class Plain:
        def log(self):
            pass

def top_has_none():
    Plain.log()
`);
  // A class that declares no method of the name reaches the one it inherits,
  // as Django's `form.is_valid()` on `Form(BaseForm)` does — not a method of
  // another class whose name merely contains the receiver's (`AdminForm`,
  // declared first so a name test would pick it).
  fs.writeFileSync(path.join(dir, 'forms.py'), `class AdminForm:
    def is_valid(self):
        pass

class BaseForm:
    def is_valid(self):
        pass

class Form(BaseForm):
    pass

def submit(form: Form):
    form.is_valid()

def submit_class():
    Form.is_valid()
`);
  fs.writeFileSync(path.join(dir, 'stores.ts'), `export class AdminStore {
  static create(): void {}
}

export class BaseStore {
  static create(): void {}
}

export class Store extends BaseStore {}

export function makeStore(): void {
  Store.create();
}
`);
  // A class declared inside a function is named only in there (Django's
  // serializer test declares a `File` with a `write`): a call elsewhere is
  // not on it, through the class name or a receiver named after it.
  fs.writeFileSync(path.join(dir, 'streams.py'), `class SerializerTests:
    def test_stream(self):
        class File:
            def write(self, line):
                pass
        f = File()
        File.write(f, "x")
`);
  fs.writeFileSync(path.join(dir, 'storage.py'), `class File:
    pass

def save(file):
    file.write("data")

def save_class(f):
    File.write(f, "data")
`);
  // An untyped parameter does not identify either the test app's Manager or
  // the library's computed Manager base. Neither supplies a guessed call.
  fs.mkdirSync(path.join(dir, 'tests'));
  fs.writeFileSync(path.join(dir, 'tests', 'models.py'), `class Model:
    def check(self):
        pass

class Manager(Model):
    pass
`);
  fs.writeFileSync(path.join(dir, 'managers.py'), `class QuerySet:
    def filter(self):
        pass

class CurrentSiteManager:
    def get_queryset(self):
        pass

class BaseManager:
    def check(self):
        pass

    def get_queryset(self):
        pass

    @classmethod
    def from_queryset(cls, queryset_class):
        return cls

class Manager(BaseManager.from_queryset(QuerySet)):
    pass

def check_managers(manager):
    manager.check()
    manager.get_queryset()
    manager.filter()
`);
  // A nested class and an unrelated class cannot supply methods for an
  // untyped parameter merely because their names share a word.
  fs.writeFileSync(path.join(dir, 'cachelib.py'), `class BaseCache:
    def aadd(self):
        pass

class BadModelWithSplit:
    def split(self):
        pass
`);
  fs.writeFileSync(path.join(dir, 'local_fakes.py'), `def make_fakes():
    class Fake:
        def aadd(self):
            pass

        def split(self):
            pass
    return Fake()
`);
  fs.writeFileSync(path.join(dir, 'cache_use.py'), `def use_cache(cache, value):
    cache.aadd()
    value.split()
`);
  // A Kotlin companion object's members are the class's own: `Foo.create()`
  // is Foo's, not that of `FooBar`, declared first.
  fs.writeFileSync(path.join(dir, 'Factory.kt'), `package app

class FooBar {
    companion object {
        fun create(): FooBar = FooBar()
    }
}

class Foo {
    companion object {
        fun create(): Foo = Foo()
    }
}

fun build() {
    Foo.create()
}
`);
  // R allows dots in a class name: the owner is `Bank.Account`, whole.
  fs.writeFileSync(path.join(dir, 'account.R'), `Bank.Account <- R6::R6Class("Bank.Account",
  public = list(
    deposit = function(x) { x }
  )
)
`);
  fs.writeFileSync(path.join(dir, 'main.R'), `BankAccount <- R6::R6Class("BankAccount",
  public = list(
    deposit = function(x) { x }
  )
)

run_it <- function() {
  Bank.Account$deposit(1)
}
`);
  cg = await CodeGraph.init(dir, { index: true });
});

afterAll(() => {
  cg?.destroy();
  fs.rmSync(dir, { recursive: true, force: true });
});

/** `file#qualifiedName` of every callee — the file matters: several fixtures share names. */
function calleesOf(name: string, file: string): string[] {
  const fn = cg.getNodesByName(name).find((n) => (n.kind === 'function' || n.kind === 'method') && n.filePath === file);
  expect(fn).toBeDefined();
  return cg.getCallees(fn!.id).map(({ node }) => `${node.filePath}#${node.qualifiedName}`);
}

describe('method call binds to the receiver class itself, not a class whose name contains it', () => {
  it('Python: Logger.log() → Logger::log', () => {
    const callees = calleesOf('via_class', 'log.py');
    expect(callees).toContain('log.py#Logger::log');
    expect(callees).not.toContain('log.py#FileLogger::log');
  });

  it('Python: a typed logger.log() reaches Logger::log', () => {
    const callees = calleesOf('via_instance', 'log.py');
    expect(callees).toContain('log.py#Logger::log');
    expect(callees).not.toContain('log.py#FileLogger::log');
  });

  it('TypeScript: Logger.log() → Logger::log', () => {
    const callees = calleesOf('viaStatic', 'log.ts');
    expect(callees).toContain('log.ts#Logger::log');
    expect(callees).not.toContain('log.ts#FileLogger::log');
  });

  it('Python: a method the class inherits is reached through its base, not a namesake class', () => {
    for (const fn of ['submit', 'submit_class']) {
      const callees = calleesOf(fn, 'forms.py');
      expect(callees).toContain('forms.py#BaseForm::is_valid');
      expect(callees).not.toContain('forms.py#AdminForm::is_valid');
    }
  });

  it('TypeScript: a static method the class inherits is reached through its base', () => {
    const callees = calleesOf('makeStore', 'stores.ts');
    expect(callees).toContain('stores.ts#BaseStore::create');
    expect(callees).not.toContain('stores.ts#AdminStore::create');
  });

  it('Python: a class another function declares takes no call from outside it', () => {
    expect(calleesOf('save', 'storage.py')).not.toContain('streams.py#SerializerTests::test_stream::File::write');
    expect(calleesOf('save_class', 'storage.py')).not.toContain('streams.py#SerializerTests::test_stream::File::write');
    // Inside the function that declares it, the name is that class.
    expect(calleesOf('test_stream', 'streams.py')).toContain('streams.py#SerializerTests::test_stream::File::write');
  });

  it("Python: an untyped parameter does not borrow a test class or a computed base", () => {
    const callees = calleesOf('check_managers', 'managers.py');
    expect(callees).toEqual([]);
    expect(callees).not.toContain('tests/models.py#Model::check');
  });

  it('Python: an untyped receiver does not borrow a method from another class', () => {
    const callees = calleesOf('use_cache', 'cache_use.py');
    expect(callees).toEqual([]);
    expect(callees).not.toContain('cachelib.py#BadModelWithSplit::split');
  });

  it("Kotlin: a companion object's method is its own class's", () => {
    const callees = calleesOf('build', 'Factory.kt');
    expect(callees).toContain('Factory.kt#app::Foo::create');
    expect(callees).not.toContain('Factory.kt#app::FooBar::create');
  });

  it('R: a dotted class name is the owner as a whole', () => {
    const callees = calleesOf('run_it', 'main.R');
    expect(callees).toContain('account.R#Bank.Account::deposit');
    expect(callees).not.toContain('main.R#BankAccount::deposit');
  });

  // The fork distinguishes the top-level class from its nested namesake.
  // Check the declarations independently of the call edges they may supply.
  it('Python nested-class fixtures are indexed and resolved', () => {
    expect(cg.getNodesInFile('nested_only.py').filter((n) => n.kind === 'class' && n.name === 'Plain')).toHaveLength(2);
    expect(calleesOf('top_has_none', 'nested_only.py')).toEqual([]);
    expect(calleesOf('nested_first', 'nested.py').length).toBeGreaterThan(0);
  });
  it("Python: a nested class's method is not the top-level class's own", () => {
    expect(calleesOf('top_has_none', 'nested_only.py')).not.toContain('nested_only.py#Box::Plain::log');
  });
  it('Python: a nested class declared first does not take a top-level call', () => {
    const callees = calleesOf('nested_first', 'nested.py');
    expect(callees).toContain('nested.py#Logger::log');
    expect(callees).not.toContain('nested.py#Outer::Logger::log');
  });
});
