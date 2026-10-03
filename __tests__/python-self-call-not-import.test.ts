/**
 * A Python call written on the instance, `self.get_ip(request)`, is a method
 * call even when the file also imports a function named `get_ip`. The
 * extractor records it by its bare name, and the import strategy claimed it:
 * django-allauth's `DefaultAccountAdapter.send_notification_mail` calling
 * `self.get_client_ip(self.request)` was linked to `httpkit.get_client_ip`,
 * which `adapter.py` imports, instead of the adapter's own method.
 */
import { describe, it, expect, afterEach } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

describe('Python self-calls do not resolve through a same-named import', () => {
  const dirs: string[] = [];
  let cg: CodeGraph | undefined;

  afterEach(() => {
    cg?.close();
    cg = undefined;
    for (const dir of dirs.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
  });

  async function callsIn(files: Record<string, string>): Promise<string[]> {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'codegraph-py-self-'));
    dirs.push(dir);
    for (const [rel, content] of Object.entries(files)) {
      fs.mkdirSync(path.dirname(path.join(dir, rel)), { recursive: true });
      fs.writeFileSync(path.join(dir, rel), content);
    }
    cg = await CodeGraph.init(dir, { index: true });
    const rows = (cg as any).db.db
      .prepare(
        `SELECT s.qualified_name s, t.qualified_name t, t.file_path f FROM edges e
         JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target WHERE e.kind IN ('calls', 'instantiates')`,
      )
      .all() as { s: string; t: string; f: string }[];
    return rows.map((r) => `${r.s} -> ${r.t} (${r.f})`).sort();
  }

  const httpkit = 'def get_ip(request):\n    return "1.2.3.4"\n';

  it('a method calling its own same-named method through self', async () => {
    expect(
      await callsIn({
        'pkg/__init__.py': '',
        'pkg/httpkit.py': httpkit,
        'pkg/adapter.py': [
          'from pkg.httpkit import get_ip',
          '',
          '',
          'class Adapter:',
          '    def get_ip(self, request):',
          '        return get_ip(request)',
          '',
          '    def notify(self, request):',
          '        return self.get_ip(request)',
          '',
        ].join('\n'),
      }),
    ).toEqual([
      // The bare call is still the imported function.
      'Adapter::get_ip -> get_ip (pkg/httpkit.py)',
      'Adapter::notify -> Adapter::get_ip (pkg/adapter.py)',
    ]);
  });

  it('a method a base class in another file declares', async () => {
    expect(
      await callsIn({
        'pkg/__init__.py': '',
        'pkg/httpkit.py': httpkit,
        'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
        'pkg/adapter.py': [
          'from pkg.base import Base',
          'from pkg.httpkit import get_ip',
          '',
          '',
          'class Adapter(Base):',
          '    def notify(self, request):',
          '        return self.get_ip(request)',
          '',
          '    def raw(self, request):',
          '        return get_ip(request)',
          '',
        ].join('\n'),
      }),
    ).toEqual([
      'Adapter::notify -> Base::get_ip (pkg/base.py)',
      'Adapter::raw -> get_ip (pkg/httpkit.py)',
    ]);
  });
  it('a same-named local in another method rebinds nothing', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/httpkit.py': httpkit,
      'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        'from pkg.httpkit import get_ip',
        '',
        '',
        'class Adapter(Base):',
        '    def cached(self, request):',
        '        get_ip = self.__dict__.get("ip")',
        '        return get_ip',
        '',
        '    def notify(self, request):',
        '        return self.get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::notify'))).toEqual([
      'Adapter::notify -> Base::get_ip (pkg/base.py)',
    ]);
  });

  // The class body or an instance attribute rebinding the name to the
  // import makes `self.get_ip()` the imported function at runtime.
  it.each([
    ['a class attribute', ['    get_ip = staticmethod(get_ip)', '']],
    ['an instance attribute', ['    def __init__(self):', '        self.get_ip = get_ip', '']],
  ])('a name %s rebinds to the import', async (_label, body) => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/httpkit.py': httpkit,
      'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        'from pkg.httpkit import get_ip',
        '',
        '',
        'class Adapter(Base):',
        ...body,
        '    def notify(self, request):',
        '        return self.get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::notify'))).toEqual([
      'Adapter::notify -> get_ip (pkg/httpkit.py)',
    ]);
  });

  // Python's MRO for Adapter(Left, Right) with Left(Root) reaches Root
  // before Right; a breadth-first ancestor search would pick Right.
  it('a method two bases could supply', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': [
        'class Root:',
        '    def get_ip(self, request):',
        '        return "root"',
        '',
        '',
        'class Left(Root):',
        '    pass',
        '',
        '',
        'class Right:',
        '    def get_ip(self, request):',
        '        return "right"',
        '',
      ].join('\n'),
      'pkg/adapter.py': [
        'from pkg.base import Left, Right',
        '',
        '',
        'class Adapter(Left, Right):',
        '    def notify(self, request):',
        '        return self.get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls).toEqual(['Adapter::notify -> Root::get_ip (pkg/base.py)']);
  });
  // A class with more than one base leaves the choice to node order; one
  // that unpacks its bases (`*bases`) has more than its header shows.
  it('a method behind unpacked bases', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': [
        'class Root:',
        '    def get_ip(self, request):',
        '        return "root"',
        '',
        '',
        'class Left(Root):',
        '    pass',
        '',
        '',
        'class Right:',
        '    def get_ip(self, request):',
        '        return "right"',
        '',
      ].join('\n'),
      'pkg/adapter.py': [
        'from pkg.base import Left, Right',
        '',
        'bases = (Left,)',
        '',
        '',
        'class Adapter(*bases, Right):',
        '    def notify(self, request):',
        '        return self.get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls).toEqual(['Adapter::notify -> Root::get_ip (pkg/base.py)']);
  });

  // A header over several lines still ends at its colon; the body's
  // rebinding counts.
  it('a class header over several lines', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/httpkit.py': httpkit,
      'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        'from pkg.httpkit import get_ip',
        '',
        '',
        'class Adapter(',
        '  Base,',
        '):',
        '    get_ip = staticmethod(get_ip)',
        '',
        '    def notify(self, request):',
        '        return self.get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::notify'))).toEqual([
      'Adapter::notify -> get_ip (pkg/httpkit.py)',
    ]);
  });

  // cpython's email.message.Message imports `walk` in its class body, so
  // `self.walk()` is that function, not the base's method it shadows.
  it('a function the class body imports', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/iterators.py': 'def unused():\n    pass\n\n\ndef walk(self):\n    yield self\n',
      'pkg/base.py': 'class Base:\n    def walk(self):\n        return []\n',
      'pkg/message.py': [
        'from pkg.base import Base',
        '',
        '',
        'class Message(Base):',
        '    from pkg.iterators import (',
        '        unused,  # a comment ends at its line',
        '        walk,',
        '    )',
        '',
        '    def get_charsets(self):',
        '        return [part for part in self.walk()]',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Message::get_charsets'))).toEqual([
      'Message::get_charsets -> walk (pkg/iterators.py)',
    ]);
  });

  // `self` outside any class is whatever the function is later bound to;
  // the import still names it.
  it('a self-call outside any class', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/httpkit.py': httpkit,
      'pkg/other.py': 'class Other:\n    def lookup(self, request):\n        return "other"\n',
      'pkg/factory.py': [
        'from pkg.httpkit import get_ip as lookup',
        '',
        '',
        'def notify(self, request):',
        '    return self.lookup(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('notify'))).toEqual(['notify -> get_ip (pkg/httpkit.py)']);
  });
  // imaplib's `raise self.error(...)` instantiates the class IMAP4 nests.
  it('a class the calling class nests', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/helpers.py': 'def error(message):\n    return message\n',
      'pkg/other.py': 'class Other:\n    def error(self, message):\n        return message\n',
      'pkg/imap.py': [
        'class IMAP4:',
        '    class error(Exception):',
        '        pass',
        '',
        '    def expunge(self):',
        '        raise self.error("no")',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('IMAP4::expunge'))).toEqual([
      'IMAP4::expunge -> IMAP4::error (pkg/imap.py)',
    ]);
  });
  it('a class the calling class nests, nearer than an inherited method', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': 'class Base:\n    def error(self, message):\n        return message\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        '',
        '',
        'class Adapter(Base):',
        '    class error(Exception):',
        '        pass',
        '',
        '    def expunge(self):',
        '        raise self.error("no")',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::expunge'))).toEqual([
      'Adapter::expunge -> Adapter::error (pkg/adapter.py)',
    ]);
  });
  it('super() reaches the base method an override extends, past a same-named import', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/httpkit.py': httpkit,
      'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        'from pkg.httpkit import get_ip',
        '',
        '',
        'class Adapter(Base):',
        '    def get_ip(self, request):',
        '        return super().get_ip(request)',
        '',
        '    def legacy(self, request):',
        '        return super(Adapter, self).get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::'))).toEqual([
      'Adapter::get_ip -> Base::get_ip (pkg/base.py)',
      'Adapter::legacy -> Base::get_ip (pkg/base.py)',
    ]);
  });
  it('super() skips an intermediate class that does not declare the method', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': [
        'class Base(object):',
        '    def get_ip(self, request):',
        '        return "base"',
        '',
        '',
        'class Middle(Base):',
        '    def other(self):',
        '        return 1',
        '',
      ].join('\n'),
      'pkg/adapter.py': [
        'from pkg.base import Middle',
        '',
        '',
        'class Adapter(Middle):',
        '    def get_ip(self, request):',
        '        return super().get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::'))).toEqual([
      'Adapter::get_ip -> Base::get_ip (pkg/base.py)',
    ]);
  });
  it('super() follows the method resolution order across several bases', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': [
        'class Root:',
        '    def get_ip(self, request):',
        '        return "root"',
        '',
        '',
        'class Left(Root):',
        '    def other(self):',
        '        return 1',
        '',
        '',
        'class Right(Root):',
        '    def get_ip(self, request):',
        '        return "right"',
        '',
      ].join('\n'),
      'pkg/adapter.py': [
        'from pkg.base import Left, Right',
        '',
        '',
        'class Adapter(Left, Right):',
        '    def get_ip(self, request):',
        '        return super().get_ip(request)',
        '',
      ].join('\n'),
    });
    // Adapter, Left, Right, Root: Right comes before the Root that Left extends.
    expect(calls.filter((c) => c.startsWith('Adapter::'))).toEqual([
      'Adapter::get_ip -> Right::get_ip (pkg/base.py)',
    ]);
  });
  it('super(Cls, self) starts after the class it names', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        '',
        '',
        'class Adapter(Base):',
        '    def get_ip(self, request):',
        '        return "adapter"',
        '',
        '',
        'class Child(Adapter):',
        '    def get_ip(self, request):',
        '        return super(Adapter, self).get_ip(request)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Child::'))).toEqual([
      'Child::get_ip -> Base::get_ip (pkg/base.py)',
    ]);
  });
  it('super().__init__() reaches the base constructor', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': 'class Base:\n    def __init__(self, name):\n        self.name = name\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        '',
        '',
        'class Adapter(Base):',
        '    def __init__(self, name):',
        '        super().__init__(name)',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => c.startsWith('Adapter::'))).toEqual([
      'Adapter::__init__ -> Base::__init__ (pkg/base.py)',
    ]);
  });
  it('super() past a base the index does not hold', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': 'class Base:\n    def get_ip(self, request):\n        return "base"\n',
      'pkg/adapter.py': [
        'from pkg.base import Base',
        'from vendor.mixins import ForwardedMixin',
        '',
        '',
        'class Adapter(ForwardedMixin, Base):',
        '    def get_ip(self, request):',
        '        return super().get_ip(request)',
        '',
      ].join('\n'),
    });
    // ForwardedMixin comes first at runtime and may define get_ip itself.
    expect(calls.filter((c) => c.startsWith('Adapter::'))).toEqual([]);
  });
  it('super() reaches an in-repo base whose own base the index does not hold', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': [
        'from vendor.forms import Form',
        '',
        '',
        'class Base(Form):',
        '    def clean(self):',
        '        return "base"',
        '',
      ].join('\n'),
      'pkg/adapter.py': [
        'from pkg.base import Base',
        '',
        '',
        'class Adapter(Base):',
        '    def clean(self):',
        '        return super().clean()',
        '',
      ].join('\n'),
    });
    // Form comes after Base, which declares clean first.
    expect(calls.filter((c) => c.startsWith('Adapter::'))).toEqual([
      'Adapter::clean -> Base::clean (pkg/base.py)',
    ]);
  });

  // Each runs where Python's super() would raise, call something else, or
  // reach a method the index can't see first; none may link to Base.m/Mid.m.
  const base = 'class Base:\n    def m(self):\n        return "base"\n';
  const mid = (body: string) => `from pkg.base import Base\n\n\nclass Mid(Base):\n${body}\n`;
  it.each([
    ['in a nested function', 'class Child(Base):\n    def run(self):\n        def inner():\n            return super().m()\n        return inner()\n'],
    ['in a lambda', 'class Child(Base):\n    def run(self):\n        f = lambda: super().m()\n        return f()\n'],
    ['in a staticmethod', 'class Child(Base):\n    @staticmethod\n    def run(obj):\n        return super().m()\n'],
    ['in the class body', 'class Child(Base):\n    value = super().m()\n'],
    ['with `super` a parameter', 'class Child(Base):\n    def run(self, super):\n        return super().m()\n'],
    ['with `super` a parameter on a later line', 'class Child(Base):\n    def run(\n        self,\n        super,\n    ):\n        return super().m()\n'],
    ['with `super` rebound in the file', 'super = print\n\n\nclass Child(Base):\n    def run(self):\n        return super().m()\n'],
    ['with a receiver other than the first parameter', 'class Child(Base):\n    def run(self):\n        return super(Child, object()).m()\n'],
    ['with a receiver named other than the first parameter', 'class Child(Base):\n    def run(self, other):\n        return super(Child, other).m()\n'],
    ['with the named class shadowed by a parameter', 'from pkg.mid import Mid\n\n\nclass Child(Mid):\n    def m(self, Child=Mid):\n        return super(Child, self).m()\n', mid('    def m(self):\n        return "mid"')],
    ['under a metaclass that defines mro', 'class Meta(type):\n    def mro(cls):\n        return [cls, object]\n\n\nclass Child(Base, metaclass=Meta):\n    def m(self):\n        return super().m()\n'],
    ['under a metaclass the index does not hold', 'from vendor.meta import Meta\n\n\nclass Child(Base, metaclass=Meta):\n    def m(self):\n        return super().m()\n'],
    ['past a tuple assignment that rebinds the name', 'from pkg.mid import Mid\n\n\nclass Child(Mid):\n    def m(self):\n        return super().m()\n', mid('    m, other = (lambda self: "mid"), None')],
    ['past a class-body import that rebinds the name', 'from pkg.mid import Mid\n\n\nclass Child(Mid):\n    def m(self):\n        return super().m()\n', mid('    from builtins import len as m')],
    ['in a generator expression', 'class Child(Base):\n    def run(self):\n        return next(super().m() for _ in range(1))\n'],
    ['with the named class redefined', 'class Child(Base):\n    def m(self):\n        return super(Child, self).m()\n\n\nSaved = Child\n\n\nclass Child:\n    pass\n'],
    ['under a parenthesized metaclass that defines mro', 'class Meta(type):\n    def mro(cls):\n        return [cls, object]\n\n\nclass Child(Base, metaclass=(Meta)):\n    def m(self):\n        return super().m()\n'],
    ['under a metaclass of its own named ABCMeta', 'class ABCMeta(type):\n    def mro(cls):\n        return [cls, object]\n\n\nclass Child(Base, metaclass=ABCMeta):\n    def m(self):\n        return super().m()\n'],
    ['under keywords that may pass a metaclass', 'opts = {}\n\n\nclass Child(Base, **opts):\n    def m(self):\n        return super().m()\n'],
    ['past a multi-line class-body import that rebinds the name', 'from pkg.mid import Mid\n\n\nclass Child(Mid):\n    def m(self):\n        return super().m()\n', mid('    from builtins import (\n        len as m,\n    )')],
    ['past a multi-line tuple assignment that rebinds the name', 'from pkg.mid import Mid\n\n\nclass Child(Mid):\n    def m(self):\n        return super().m()\n', mid('    (m,\n     other) = (lambda self: "mid"), None')],
    ['to a property', 'from pkg.mid import Mid\n\n\nclass Child(Mid):\n    def m(self):\n        return super().m()\n', mid('    @property\n    def m(self):\n        return "mid"')],
  ])('super() links nothing %s', async (_, child, midFile = mid('    def other(self):\n        return 1')) => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': base,
      'pkg/mid.py': midFile,
      'pkg/child.py': `from pkg.base import Base\n${child}`,
    });
    expect(calls.filter((c) => /^Child\S* -> (Base|Mid)::m /.test(c))).toEqual([]);
  });
  it('super() still links in a classmethod, a one-line method, a nested block or function, a conditional method, a comprehension, under a plain metaclass', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': [
        'class Base:',
        '    def m(self):',
        '        return "base"',
        '',
        '    m.alters_data = True',
        '',
        '    class Inner:',
        '        m = 1',
        '',
        '    @classmethod',
        '    def make(cls):',
        '        return cls()',
        '',
      ].join('\n'),
      'pkg/child.py': [
        'import abc',
        'from abc import ABCMeta',
        'from pkg.base import Base',
        '',
        'FLAG = True',
        '',
        '',
        'def consume(**kw):',
        '    return kw',
        '',
        '',
        'consume(super=1)',
        '',
        '',
        'def nest(ob, super=None):',
        '    from builtins import super',
        '    return ob',
        '',
        '',
        'class Block:',
        '    def super(self):',
        '        return 1',
        '',
        '',
        'class Meta(type):',
        '    def __call__(cls, *args):',
        '        return type.__call__(cls, *args)',
        '',
        '',
        'class Child(Base, metaclass=Meta):',
        '    """Calls super().m() in a docstring; super = nothing here."""',
        '',
        '    def m(self): return super().m()',
        '',
        '    @classmethod',
        '    def make(cls):',
        '        return super().make()',
        '',
        '    def run(self, flag):',
        '        if flag:',
        '            for _ in range(2):',
        '                return super(Child, self).m()',
        '',
        '    def wrapped(self):',
        '        def inner(item):',
        '            return super().m()',
        '        return inner(self)',
        '',
        '    if FLAG:',
        '        def cond(self):',
        '            return super().m()',
        '',
        '    def listed(self):',
        '        return [super().m() for _ in range(1)]',
        '',
        '    def first(self):',
        '        return list(x for x in super().make())',
        '',
        '',
        'class Abstract(Base, metaclass=ABCMeta):',
        '    def m(self):',
        '        return super().m()',
        '',
        '',
        'class Dotted(Base, metaclass=abc.ABCMeta):',
        '    def m(self):',
        '        return super().m()',
        '',
      ].join('\n'),
    });
    expect(calls.filter((c) => / -> Base::/.test(c))).toEqual([
      'Abstract::m -> Base::m (pkg/base.py)',
      'Child::cond -> Base::m (pkg/base.py)',
      'Child::first -> Base::make (pkg/base.py)',
      'Child::listed -> Base::m (pkg/base.py)',
      'Child::m -> Base::m (pkg/base.py)',
      'Child::make -> Base::make (pkg/base.py)',
      'Child::run -> Base::m (pkg/base.py)',
      'Child::wrapped::inner -> Base::m (pkg/base.py)',
      'Dotted::m -> Base::m (pkg/base.py)',
    ]);
  });
  it('super() starts at its own class when a base shares its name', async () => {
    const calls = await callsIn({
      'pkg/__init__.py': '',
      'pkg/base.py': 'class Adapter:\n    def m(self):\n        return "base"\n',
      'pkg/child.py': [
        'from pkg.base import Adapter as Parent',
        '',
        '',
        'class Adapter(Parent):',
        '    def m(self):',
        '        return super().m()',
        '',
      ].join('\n'),
    });
    expect(calls).toContain('Adapter::m -> Adapter::m (pkg/base.py)');
  });
});
