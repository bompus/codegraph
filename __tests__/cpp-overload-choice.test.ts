/**
 * `lib::format("{}", x)` among overloads opened by a namespace macro picks the
 * one the arguments fit: fmt's 1,400 `fmt::format("{}", …)` calls all went to
 * color.h's `format(const text_style&, …)` — the first of the set indexed.
 * A narrow literal fits a `format_string`, a wide one (`L"{}"`) the
 * `wformat_string` overload, and a style argument the style overload.
 */
import { describe, it, expect, afterAll, beforeAll } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';

let root = '';
let cg: CodeGraph;

beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cpp-overload-'));
  const files: Record<string, string> = {
    'include/lib/base.h': `#pragma once
#define LIB_BEGIN_NAMESPACE namespace lib {
#define LIB_END_NAMESPACE }

LIB_BEGIN_NAMESPACE
template <typename... T> struct format_string {};
template <typename... T> struct wformat_string {};
LIB_END_NAMESPACE
`,
    'include/lib/color.h': `#pragma once
#include "base.h"

LIB_BEGIN_NAMESPACE
struct text_style {};

template <typename... T>
inline auto format(const text_style& ts, format_string<T...> fmt, T&&... args) -> int {
  return 0;
}
LIB_END_NAMESPACE
`,
    'include/lib/format.h': `#pragma once
#include "base.h"

LIB_BEGIN_NAMESPACE
template <typename... T>
inline auto format(format_string<T...> fmt, T&&... args) -> int {
  return 1;
}
LIB_END_NAMESPACE
`,
    'include/lib/xchar.h': `#pragma once
#include "base.h"

LIB_BEGIN_NAMESPACE
template <typename... T>
inline auto format(wformat_string<T...> fmt, T&&... args) -> int {
  return 2;
}
LIB_END_NAMESPACE
`,
    'class-default.cpp': `struct JsonPrinter {
  static void write_json(int value, bool comma = true);
};
void JsonPrinter::write_json(int value, bool comma) {}
void use_printer_default() { JsonPrinter::write_json(1); }
`,
    'export-default.hpp': `#define EXPORT_API
namespace std { struct string {}; }
namespace api { EXPORT_API std::string exported_default(int value, int extra = 2); }
`,
    'export-default.cpp': `#include "export-default.hpp"
namespace api { std::string exported_default(int value, int extra) { return {}; } }
void use_exported_default() { api::exported_default(1); }
`,
    'namespace-default.hpp': `#define LIB_OPEN_NAMESPACE namespace library {
#define LIB_CLOSE_NAMESPACE }
LIB_OPEN_NAMESPACE
namespace detail {
template<class T> struct Buffer {};
EXPORT_API void scoped_buffer_default(Buffer<char>& buffer, int extra = 2);
}
LIB_CLOSE_NAMESPACE
`,
    'namespace-default.cpp': `#include "namespace-default.hpp"
LIB_OPEN_NAMESPACE
namespace detail {
void scoped_buffer_default(Buffer<char>& buffer, int extra) {}
void use_scoped_buffer_default() { Buffer<char> buffer; scoped_buffer_default(buffer); }
}
LIB_CLOSE_NAMESPACE
`,
    'default-header.hpp': 'auto from_header(int value, int extra = 2) -> int;\n',
    'default-impl.cpp': '#include "default-header.hpp"\nint from_header(int renamed, int extra) { return renamed + extra; }\n',
    'default-use.cpp': '#include "default-header.hpp"\nint use_header_default() { return from_header(1); }\n',
    'late-header-use.cpp': 'int from_header(int value, int extra);\nint use_late_header() { return from_header(1); }\n#include "default-header.hpp"\nint use_available_header() { return from_header(1); }\n',
    'macro-first.hpp': '#define FIRST_BEGIN_NAMESPACE namespace first {\n#define FIRST_END_NAMESPACE }\nFIRST_BEGIN_NAMESPACE\nvoid scoped_default(int value = 1);\nFIRST_END_NAMESPACE\n',
    'macro-second.hpp': '#define SECOND_BEGIN_NAMESPACE namespace second {\n#define SECOND_END_NAMESPACE }\nSECOND_BEGIN_NAMESPACE\nvoid scoped_default(int value) {}\nSECOND_END_NAMESPACE\n',
    'macro-default-use.cpp': '#include "macro-first.hpp"\n#include "macro-second.hpp"\nvoid use_macro_default() { second::scoped_default(); }\nvoid use_macro_explicit() { second::scoped_default(1); }\n',
    'defaults.cpp': `void forwarded(int original, int extra = 2);
void forwarded(int renamed, const int extra) {}
void use_forwarded() { forwarded(1); }
void varargs(const char* format, ...) {}
void use_varargs() { varargs("%d", 1); }
void arity_choice(int value) {}
void arity_choice(int value, int extra) {}
void use_two_args() { arity_choice(1, 2); }
void wrong_defaults(double value, int extra = 2);
void wrong_defaults(int value, int extra) {}
void use_wrong_defaults() { wrong_defaults(1); }
namespace other { void wrong_defaults(int value, int extra = 2); }
struct locale_ref {};
struct ostream {};
int compiled_format();
int compiled_only(locale_ref loc, const char* format) { return 0; }
int compiled_only(ostream& stream, const char* format) { return 0; }
void use_compiled_wrong() { compiled_only(FMT_COMPILE("{}"), "value"); }
#define WRAP(T) const T&
template<class L, class R> void macro_args(WRAP(L) lhs, const char* op, WRAP(R) rhs) {}
void use_macro_args() { macro_args(1, "==", 2); }
void late_defaults(int value, int extra) {}
void use_late_default() { late_defaults(1); }
void late_defaults(int value, int extra = 2);
void added_defaults(int value, int extra) {}
void added_defaults(int value, int extra = 2);
void use_added_default() { added_defaults(1); }
`,
    'test/use.cc': `#include "lib/color.h"
#include "lib/format.h"
#include "lib/xchar.h"

int narrow() { return lib::format("{}", 1); }
int wide() { return lib::format(L"{}", 2); }
int styled() { return lib::format(lib::text_style{}, "{}", 3); }
`,
  };
  for (const [variant, opening] of Object.entries({
    raw: 'namespace detail {\nconst char* sample = R"raw(\n}\n)raw";',
    multiline: 'namespace detail\n{',
    unicode: 'const char* sample = R"raw(é🙂})raw"; namespace detail {',
  })) {
    const header = `${variant}-namespace-default.hpp`;
    files[header] = files['namespace-default.hpp'].replace('namespace detail {', opening)
      .replaceAll('scoped_buffer_default', `${variant}_buffer_default`);
    files[`${variant}-namespace-default.cpp`] = files['namespace-default.cpp']
      .replace('namespace-default.hpp', header)
      .replaceAll('scoped_buffer_default', `${variant}_buffer_default`);
  }
  files['aggregate-namespace-default.hpp'] = files['namespace-default.hpp']
    .replace('namespace detail {', 'namespace detail {\nstruct Locale {}; struct StringView {}; struct Args {};')
    .replace('int extra = 2', 'StringView fmt, Args args, Locale loc = {}')
    .replaceAll('scoped_buffer_default', 'aggregate_buffer_default');
  files['aggregate-namespace-default.cpp'] = files['namespace-default.cpp']
    .replace('namespace-default.hpp', 'aggregate-namespace-default.hpp')
    .replace('int extra', 'StringView fmt, Args args, Locale loc')
    .replace('scoped_buffer_default(buffer)', 'scoped_buffer_default(buffer, StringView{}, Args{})')
    .replaceAll('scoped_buffer_default', 'aggregate_buffer_default');
  for (const [variant, malformed] of Object.entries({
    missingtype: 'StringView fmt, Args args, = {}',
    brokenlist: 'StringView fmt, Args args, Locale loc = {};',
  })) {
    const header = `${variant}-namespace-default.hpp`;
    files[header] = files['aggregate-namespace-default.hpp']
      .replace('StringView fmt, Args args, Locale loc = {}' + (variant === 'brokenlist' ? ')' : ''), malformed)
      .replaceAll('aggregate_buffer_default', `${variant}_buffer_default`);
    files[`${variant}-namespace-default.cpp`] = files['aggregate-namespace-default.cpp']
      .replace('aggregate-namespace-default.hpp', header)
      .replaceAll('aggregate_buffer_default', `${variant}_buffer_default`);
  }
  files['separator-default.hpp'] = files['export-default.hpp']
    .replace('namespace api {', "namespace api { namespace detail { constexpr int count = 1'000; }")
    .replaceAll('exported_default', 'separator_default');
  files['separator-default.cpp'] = files['export-default.cpp']
    .replace('export-default.hpp', 'separator-default.hpp')
    .replaceAll('exported_default', 'separator_default');
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

const formatFileCalledBy = (fn: string): string[] => {
  const node = cg.getNodesInFile('test/use.cc').find((n) => n.name === fn)!;
  return cg.getOutgoingEdges(node.id).filter((e) => e.kind === 'calls').map((e) => cg.getNode(e.target)!)
    .filter((t) => t.name === 'format').map((t) => t.filePath);
};

describe('C++ overloads in a macro-opened namespace', () => {
  it('a narrow literal picks the format_string overload', () => {
    expect(formatFileCalledBy('narrow')).toEqual(['include/lib/format.h']);
  });

  it('a wide literal picks the wformat_string overload', () => {
    expect(formatFileCalledBy('wide')).toEqual(['include/lib/xchar.h']);
  });

  it('a style argument picks the style overload', () => {
    expect(formatFileCalledBy('styled')).toEqual(['include/lib/color.h']);
  });
});


describe('C++ declaration arity', () => {
  it('uses defaults from the matching forward declaration', () => {
    const caller = cg.getNodesInFile('defaults.cpp').find(n => n.name === 'use_forwarded')!;
    const targets = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    expect(targets.map(n => n.name)).toEqual(['forwarded']);
  });

  it('uses visible header defaults for a separately defined function', () => {
    const caller = cg.getNodesInFile('default-use.cpp').find(n => n.name === 'use_header_default')!;
    const targets = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    expect(targets.filter(n => n.name === 'from_header').map(n => n.filePath)).toEqual(['default-impl.cpp']);
  });

  it('makes header defaults available only after their include', () => {
    const targets = (name: string) => {
      const caller = cg.getNodesInFile('late-header-use.cpp').find(n => n.name === name)!;
      return cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    };
    expect(targets('use_late_header').filter(n => n.name === 'from_header')).toEqual([]);
    expect(targets('use_available_header').filter(n => n.name === 'from_header').map(n => n.filePath)).toEqual(['default-impl.cpp']);
  });

  it('keeps defaults inside their macro-opened namespace', () => {
    const caller = cg.getNodesInFile('macro-default-use.cpp').find(n => n.name === 'use_macro_default')!;
    const targets = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    expect(targets.filter(n => n.name === 'scoped_default')).toEqual([]);
    const validCaller = cg.getNodesInFile('macro-default-use.cpp').find(n => n.name === 'use_macro_explicit')!;
    const validTargets = cg.getOutgoingEdges(validCaller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    expect(validTargets.filter(n => n.name === 'scoped_default').map(n => n.filePath)).toEqual(['macro-second.hpp']);
  });

  it('accepts anonymous C variadic arguments', () => {
    const caller = cg.getNodesInFile('defaults.cpp').find(n => n.name === 'use_varargs')!;
    const targets = cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    expect(targets.map(n => n.name)).toEqual(['varargs']);
  });
});


describe('C++ overload proof controls', () => {
  const targets = (name: string) => {
    const caller = cg.getNodesInFile('defaults.cpp').find(n => n.name === name)!;
    return cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
  };

  it('selects the uniquely fitting sibling for a nonrecursive call', () => {
    const hit = targets('use_two_args').filter(n => n.name === 'arity_choice');
    const expected = cg.getNodesInFile('defaults.cpp').find(n => n.name === 'arity_choice' && n.startLine === 7)!;
    expect(hit.map(n => n.id)).toEqual([expected.id]);
  });

  it('does not borrow defaults from another overload or namespace', () => {
    expect(targets('use_wrong_defaults').filter(n => n.name === 'wrong_defaults' && n.startLine === 10)).toEqual([]);
  });

  it('preserves calls with macro-wrapped parameter types', () => {
    expect(targets('use_macro_args').filter(n => n.name === 'macro_args').map(n => n.name)).toEqual(['macro_args']);
  });

  it('uses defaults added after the definition but before the call', () => {
    expect(targets('use_added_default').filter(n => n.name === 'added_defaults').map(n => n.name)).toEqual(['added_defaults']);
  });

  it('does not use defaults declared after the call site', () => {
    expect(targets('use_late_default').filter(n => n.name === 'late_defaults')).toEqual([]);
  });

  it('does not pass a compiled format as a locale or stream receiver', () => {
    expect(targets('use_compiled_wrong').filter(n => n.name === 'compiled_only')).toEqual([]);
  });
});


describe('C++ defaults in class and macro-prefixed declarations', () => {
  const targets = (file: string, name: string) => {
    const caller = cg.getNodesInFile(file).find(n => n.name === name)!;
    return cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
  };

  it('uses a class static method prototype default for its out-of-line definition', () => {
    expect(targets('class-default.cpp', 'use_printer_default').filter(n => n.name === 'write_json').map(n => n.startLine)).toEqual([4]);
  });

  it('separates a macro-prefixed return type from the declaration name and owner', () => {
    expect(targets('export-default.cpp', 'use_exported_default').filter(n => n.name === 'exported_default').map(n => n.filePath)).toEqual(['export-default.cpp']);
  });

  it('retains explicit namespace ownership when an opening macro distorts the AST', () => {
    expect(targets('namespace-default.cpp', 'use_scoped_buffer_default').filter(n => n.name === 'scoped_buffer_default').map(n => n.startLine)).toEqual([4]);
  });
});


describe('C++ namespace scope token boundaries', () => {
  const targets = (file: string, name: string) => {
    const caller = cg.getNodesInFile(file).find(n => n.name === name)!;
    return cg.getOutgoingEdges(caller.id).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
  };

  it.each(['raw', 'multiline', 'unicode', 'aggregate'])('preserves a header default through %s namespace source', variant => {
    expect(targets(`${variant}-namespace-default.cpp`, `use_${variant}_buffer_default`)
      .filter(n => n.name === `${variant}_buffer_default`).map(n => n.startLine)).toEqual([4]);
  });

  it.each(['missingtype', 'brokenlist'])('refuses a default with %s parameter shape damage', variant => {
    expect(targets(`${variant}-namespace-default.cpp`, `use_${variant}_buffer_default`)
      .filter(n => n.name === `${variant}_buffer_default`)).toEqual([]);
  });

  it('keeps digit separators from extending an unrelated nested namespace', () => {
    expect(targets('separator-default.cpp', 'use_separator_default')
      .filter(n => n.name === 'separator_default').map(n => n.filePath)).toEqual(['separator-default.cpp']);
  });
});
