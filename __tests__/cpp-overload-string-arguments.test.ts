import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import { CodeGraph } from '../src';
let root = '';
let cg: CodeGraph;
beforeAll(async () => {
  root = fs.mkdtempSync(path.join(os.tmpdir(), 'cg-cpp-string-overload-'));
  fs.writeFileSync(path.join(root, 'values.cpp'), `namespace std { struct string {}; }
struct Printer {
  static void write(const std::string& value, bool comma = true);
  static void write(int value, bool comma = true);
};
void Printer::write(const std::string& value, bool comma) {}
void Printer::write(int value, bool comma) {}
std::string make_text() { return {}; }
void literal_text() { Printer::write("text"); }
void typed_text() { std::string value; Printer::write(value); }
void returned_text() { Printer::write(make_text()); }
void conditional_text(bool choose) { Printer::write(choose ? "yes" : "no"); }
void number() { int value = 2; { int* value = nullptr; } Printer::write(value); }
void unknown(auto value) { Printer::write(value); }
void missing() { Printer::write(); }
void shadowed_return(int (*make_text)()) { Printer::write(make_text()); }
struct string { operator int() const { return 1; } };
void custom_type() { string value; Printer::write(value); }
void loop_shadow() { std::string value; for (int value = 0; value < 1; ++value) { Printer::write(value); } }
void capture_shadow() { std::string value; auto f = [value = 1] { Printer::write(value); }; }
char** make_pointer() { return nullptr; }
void pointer_return() { Printer::write(make_pointer()); }
struct Pair { int first; int second; };
void structured_shadow(Pair pair) { std::string value; { auto [other, value] = pair; Printer::write(value); } }

struct Info {
  const char* name() const { return "name"; }
  int total_tests(void) const { return 2; }
};
void getter_text(const Info& info) { Printer::write(info.name()); }
void getter_number(const Info& info) { Printer::write(info.total_tests()); }
struct OtherInfo { int name() const { return 2; } };
void receiver_shadow(const Info& info, OtherInfo (&rows)[1]) { for (const OtherInfo& info : rows) { Printer::write(info.name()); } }
void shadowed_text() { std::string value; { int value = 2; Printer::write(value); } }
void default_number(int value = 1) { Printer::write(value); }
struct ChainedPrinter {
  static ChainedPrinter instance() { return {}; }
  void write(int value, bool comma = true);
  void write(const std::string& value, bool comma = true);
};
void ChainedPrinter::write(int value, bool comma) {}
void ChainedPrinter::write(const std::string& value, bool comma) {}
void chained_number() { ChainedPrinter::instance().write(1); }
void initializer_scope() { std::string value; { int value = 1, used = (Printer::write(value), 0); } }
void later_initializer() { std::string value; { int used = (Printer::write(value), 0), value = 1; } }
void commented_number() { Printer::write(/* value */ 1); }
void signed_number() { Printer::write(-1); }
void plus_number() { Printer::write(+1); }
void hexadecimal_number() { Printer::write(0x10); }
void binary_number() { Printer::write(0b10U); }
void octal_number() { Printer::write(017LL); }
void separated_number() { Printer::write(1'000UL); }
void commented_getter(const Info& info) { Printer::write(info.name(/* no args */)); }
void commented_concat() { Printer::write("a" /* comment */ "b"); }
void ambiguous_initializer() { std::string value; { int value = 1, used = (Printer::write(value), Printer::write("text"), 0); } }
void direct_pair() { int value = 1; Printer::write(value); Printer::write("text"); }
struct OtherPrinter { static void write(const std::string& value) {} };
void distinct_owner_initializer() { int value = 1, used = (OtherPrinter::write("text"), Printer::write(value), 0); }
#include "getter-types.hpp"
void out_of_line_number(const model::Info& info) { Printer::write(info.count()); }
void out_of_line_text(const model::Info& info) { Printer::write(info.label()); }
void foreign_owner_number(const foreign::Info& info) { Printer::write(info.label()); }
void foreign_owner_text(const foreign::Info& info) { Printer::write(info.count()); }
void char_pointer(const char* value) { Printer::write(value); }
void double_pointer(const char** value) { Printer::write(value); }
class PrivateInfo { int count(void) const { return 2; } public: void submit(const PrivateInfo& info) { Printer::write(info.count()); } };


`);
  fs.writeFileSync(path.join(root, 'macro.cpp'), `#define FIRST_BEGIN_NAMESPACE namespace first {
#define FIRST_END_NAMESPACE }
#define SECOND_BEGIN_NAMESPACE namespace second {
#define SECOND_END_NAMESPACE }
namespace std { struct string {}; }
FIRST_BEGIN_NAMESPACE
void write(int value) {}
FIRST_END_NAMESPACE
SECOND_BEGIN_NAMESPACE
void write(const std::string& value) {}
SECOND_END_NAMESPACE
void macro_number() { first::write(1); }
void macro_invalid() { first::write("x"); }
`);
  fs.writeFileSync(path.join(root, 'getter-types.hpp'), `#pragma once
#define GTEST_API_
namespace model { class GTEST_API_ Info { public: int count(void) const; const char* label() const; }; }
namespace foreign { struct Info { const char* count() const; int label() const; }; }
`);
  fs.writeFileSync(path.join(root, 'getter-definitions.cpp'), `#include "getter-types.hpp"
int model::Info::count() const { return 1; }
const char* model::Info::label() const { return "label"; }
const char* foreign::Info::count() const { return "foreign"; }
int foreign::Info::label() const { return 2; }
`);
  cg = await CodeGraph.init(root, { index: true });

});
afterAll(() => { cg?.close(); if (root) fs.rmSync(root, { recursive: true, force: true }); });
describe('C++ string and integral overload arguments with visible defaults', () => {
  const targets = (name: string) => {
    const caller = cg.getNodesInFile('values.cpp').find(n => n.name === name)!;
    return cg.getOutgoingEdgesFrom([caller.id]).filter(e => e.kind === 'calls')
      .map(e => cg.getNode(e.target)!).filter(n => n.name === 'write');
  };
  it('selects the string overload for grounded string arguments', () => {
    for (const name of ['literal_text', 'typed_text', 'returned_text', 'conditional_text', 'getter_text', 'commented_getter', 'later_initializer', 'commented_concat', 'out_of_line_text', 'foreign_owner_text', 'char_pointer']) {
      expect.soft(targets(name).map(n => n.startLine), name).toEqual([6]);
    }
  });
  it('preserves the integral overload with its declaration default', () => {
    for (const name of ['number', 'getter_number', 'shadowed_text', 'default_number', 'initializer_scope', 'commented_number', 'signed_number', 'plus_number', 'hexadecimal_number', 'binary_number', 'octal_number', 'separated_number', 'out_of_line_number', 'foreign_owner_number', 'submit']) {
      expect.soft(targets(name).map(n => n.startLine), name).toEqual([7]);
    }
  });
  it('uses the outer matched invocation for a chained receiver', () => {
    const callTargets = targets('chained_number');
    expect(callTargets).toHaveLength(1);
    expect(callTargets[0].qualifiedName).toBe('ChainedPrinter::write');
    expect(callTargets[0].startLine).toBe(41);
  });
  it('does not retarget another macro namespace with the same extracted name', () => {
    const caller = cg.getNodesInFile('macro.cpp').find(n => n.name === 'macro_invalid')!;
    const actual = cg.getOutgoingEdgesFrom([caller.id]).filter(e => e.kind === 'calls').map(e => cg.getNode(e.target)!);
    expect(actual.some(n => n.filePath === 'macro.cpp' && n.startLine === 10)).toBe(false);
    const positive = cg.getNodesInFile('macro.cpp').find(n => n.name === 'macro_number')!;
    expect(cg.getOutgoingEdgesFrom([positive.id]).some(e => e.kind === 'calls' && cg.getNode(e.target)?.startLine === 7)).toBe(true);
  });
  it('locates each callee at its interior across nested expression wrappers', () => {
    expect.soft(targets('ambiguous_initializer').map(n => n.startLine).sort()).toEqual([6, 7]);
    const caller = cg.getNodesInFile('values.cpp').find(n => n.name === 'ambiguous_initializer')!;
    const calls = cg.getOutgoingEdgesFrom([caller.id]).filter(e => e.kind === 'calls');
    expect.soft(calls.map(e => e.column).sort((a, b) => a! - b!)).toEqual([75, 98]);
    expect.soft(targets('direct_pair').map(n => n.startLine).sort()).toEqual([6, 7]);
    expect.soft(targets('distinct_owner_initializer').filter(n => n.qualifiedName === 'Printer::write').map(n => n.startLine)).toEqual([7]);
  });
  it('declines unknown competing argument types and missing required arguments', () => {
    expect.soft(targets('unknown')).toEqual([]);
    expect.soft(targets('missing')).toEqual([]);
    expect.soft(targets('shadowed_return').some(n => n.startLine === 6)).toBe(false);
    expect.soft(targets('custom_type').some(n => n.startLine === 6)).toBe(false);
    expect.soft(targets('loop_shadow').some(n => n.startLine === 6)).toBe(false);
    expect.soft(targets('capture_shadow').some(n => n.startLine === 6)).toBe(false);
    expect.soft(targets('pointer_return').some(n => n.startLine === 6)).toBe(false);
    expect.soft(targets('double_pointer')).toEqual([]);
    expect.soft(targets('structured_shadow').some(n => n.startLine === 6)).toBe(false);
    expect.soft(targets('receiver_shadow').some(n => n.startLine === 6)).toBe(false);
  });
});
