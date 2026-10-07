import { afterEach, expect, it } from 'vitest';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { CodeGraph } from '../src';
let root: string;
let graph: CodeGraph | undefined;
afterEach(() => { graph?.close(); if(root) fs.rmSync(root,{recursive:true,force:true}); });
it('follows class-scoped C++ aliases to included owners and refuses primitive aliases', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'serializer.hpp'),'namespace detail { template<typename T> class Writer { public: Writer(int x) {} void dump() {} }; }\n');
  fs.writeFileSync(path.join(root,'use.cpp'),'#include "serializer.hpp"\nclass Json { using serializer = detail::Writer<Json>; public: void run() { serializer s(1); s.dump(); } };\nnamespace other { using Writer=int; void run() { Writer x(1); } }\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName).sort();
  expect(targets).toEqual(['detail::Writer::Writer','detail::Writer::dump']);
});
it('uses transitive angle includes and rejects an unrelated qualified owner', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'writer.hpp'),'class Writer { public: void dump() {} };\n');
  fs.writeFileSync(path.join(root,'bridge.hpp'),'#include <writer.hpp>\n');
  fs.writeFileSync(path.join(root,'use.cpp'),'#include <bridge.hpp>\nusing Good=Writer; using Bad=missing::Writer; void good(){Good a; a.dump();} void bad(){Bad b; b.dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  const callers=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.source)!.name);
  expect(callers).toEqual(['good']);
});
it('does not crash while declining a non-ASCII C++ alias', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct 名 { void f() {} };\nusing Alias = 名;\nvoid use(){Alias x; x.f();}\n');
  graph=await CodeGraph.init(root,{index:true});
  expect(graph.getNodesInFile('use.cpp').some(n=>n.name==='use')).toBe(true);
});
it('does not construct a class through a pointer alias', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct Writer { Writer(int) {} }; using Ptr=Writer*; using Other=Ptr; void run(){ Other p(0); }\n');
  graph=await CodeGraph.init(root,{index:true});
  expect(graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls')).toEqual([]);
});
it('keeps subscript and typed-field receivers away from unrelated method names', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),`class Json { public: using reference=Json&; reference operator[](int) { return *this; } void dump() {} void get_to(int&) {} };
using json=Json;
struct Holder { json value; };
class Other { public: void dump() {} void get_to(int,int,int) {} };
void run(){ json j; int out; j[0].dump(); j[0].get_to(out); Holder h; h.value.dump(); }
void range(){ std::vector<Holder> cases; for(const auto& c: cases) { c.value.dump(); } }
void unknown(){ external[0].dump(); }
`);
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName).filter(n=>n.endsWith('::dump')||n.endsWith('::get_to')).sort();
  expect(targets).toEqual(['Json::dump','Json::dump','Json::dump','Json::get_to']);
});

it('does not take a range variable type from another function', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'class Json {public: void dump(){} };\nvoid old(){std::vector<Json> items;}\nvoid use(){for(auto items : external()){items[0].dump();}}\n');
  graph=await CodeGraph.init(root,{index:true});
  expect(graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls')).toEqual([]);
});
it('keeps an inner free call separate from an outer same-named method', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'class Json {public: using reference=Json&; reference operator[](int){return *this;} void dump(){} };\nint dump(){return 0;}\nvoid run(){Json j; j[dump()].dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName).sort();
  expect(targets).toEqual(['Json::dump','dump']);
});
it('refuses an ambiguous C++ method overload set on a subscript result', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'class Json {public: using reference=Json&; reference operator[](int){return *this;} void dump(){} void dump(int){} };\nvoid run(){Json j; j[0].dump(); j[0].dump(1);}\n');
  graph=await CodeGraph.init(root,{index:true});
  expect(graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls')).toEqual([]);
});
it('does not resolve an unsubstituted generic field to a same-named global class', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct T {void dump(){}}; struct Other {void dump(){}}; template<class T> struct Holder {T value;}; void run(){Holder<Other> h; h.value.dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  expect(graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls')).toEqual([]);
});
it('does not resolve an unsubstituted operator result to a same-named global class', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct T {void dump(){}}; struct Other {void dump(){}}; template<class T> struct Holder {T operator[](int);}; void run(){Holder<Other> h; h[0].dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  expect(graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls')).toEqual([]);
});

it('preserves ordinary C++ this receivers alongside complex receiver evidence', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct Buffer {\n void reserve(int){}\n void run(){\n  this->reserve(1);\n }\n};\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Buffer::reserve']);
});

it('leaves constructor and operator expression receivers with the ordinary C++ resolver', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct Message { Message& operator<<(int); void send(){} }; void run(){Message().send(); (Message()<<1).send();}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets.filter(n=>n==='Message::send')).toHaveLength(2);
});
it('follows an include-visible namespace macro alias through a subscript receiver', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'defs.hpp'),'#define LIB_BEGIN namespace lib {\n#define LIB_END }\n');
  fs.writeFileSync(path.join(root,'fwd.hpp'),'#include "defs.hpp"\nLIB_BEGIN\nclass Json;\nusing Value=Json;\nLIB_END\n');
  fs.writeFileSync(path.join(root,'json.hpp'),'#include "fwd.hpp"\nLIB_BEGIN\nclass Json { public: using reference=Json&; reference operator[](int){return *this;} void dump(){} };\nLIB_END\n');
  fs.writeFileSync(path.join(root,'use.cpp'),'#include "json.hpp"\nusing Local=lib::Value; void run(){Local j; j[0].dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets.filter(n=>n.endsWith('Json::dump'))).toHaveLength(1);
});

it('reads a nested union field receiver without using the enclosing class fields', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct Payload {void Unref(){}};\nstruct Matcher {\n union Buffer {Payload* shared;};\n void Destroy(){buffer_.shared->Unref();}\n Buffer buffer_;\n};\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Payload::Unref']);
});
it('retains caller visibility while reading a field and subscript return declaration', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'fwd.hpp'),'struct Value;\n');
  fs.writeFileSync(path.join(root,'holder.hpp'),'#include "fwd.hpp"\nstruct Holder {Value* value; Value& operator[](int){return *value;}};\n');
  fs.writeFileSync(path.join(root,'value.hpp'),'struct Value {void dump(){}};\n');
  fs.writeFileSync(path.join(root,'use.cpp'),'#include "holder.hpp"\n#include "value.hpp"\nvoid run(){Holder h; h.value->dump(); h[0].dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Value::dump','Value::dump']);
});

it('honors range-variable shadows and unbraced range bodies', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'#include <vector>\nstruct Json {void dump(){}}; struct Other {void dump(){}}; struct Holder {Json value;}; struct OtherHolder {Other value;};\nvoid run(){\n std::vector<Holder> cases;\n for(const auto& c:cases){ {OtherHolder c; c.value.dump();} [&](OtherHolder c){c.value.dump();}(OtherHolder{}); }\n OtherHolder c;\n for(const auto& c:cases) c.value.dump();\n}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName).filter(n=>n.endsWith('::dump')).sort();
  expect(targets).toEqual(['Json::dump','Other::dump','Other::dump']);
});
it('preserves explicitly global types inside a same-named template parameter scope', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct T {void dump(){}};\ntemplate<class T> struct Holder {::T value; using Alias=::T; Alias other; ::T& operator[](int){return value;}};\nvoid run(){Holder<int> h; h.value.dump(); h.other.dump(); h[0].dump();}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['T::dump','T::dump','T::dump']);
});
it('reads later class fields with a concrete template primary owner', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct Primitive {void check(){}};\ntemplate<class T> struct Internal {Primitive primitive;};\ntemplate<class T> struct Iterator {\n void run(){m_it.primitive.check();}\n Internal<T> m_it{};\n};\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Primitive::check']);
});

it('preserves inferred auto locals while refusing an unknown auto shadow', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'#include <vector>\nstruct Json {void dump(){}}; struct Other {void dump(){}}; struct Holder {Json value;}; struct OtherHolder {Other value;};\nvoid run(){\n std::vector<Holder> cases;\n for(const auto& c:cases){\n  {auto c=OtherHolder(); c.value.dump();}\n  {auto c=unknown(); c.value.dump();}\n }\n}\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName).filter(n=>n.endsWith('::dump'));
  expect(targets).toEqual(['Other::dump']);
});
it('uses column containment for a later field beside another class on the same line', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'struct Payload {void dump(){}}; struct Holder {Payload inner;}; struct Real {void run(){value.inner.dump();} Holder value;}; struct Tiny {};\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Payload::dump']);
});

it('reads a recovered class field after an access macro splits the raw parse', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'),'#define JSON_PRIVATE_UNLESS_TESTED private\nstruct Primitive {void check(){}};\ntemplate<class T> struct Internal {Primitive primitive;};\ntemplate<class T> class Iterator {\npublic:\n void first(){}\nJSON_PRIVATE_UNLESS_TESTED:\n void extra(){}\npublic:\n void run(){m_it.primitive.check();}\nJSON_PRIVATE_UNLESS_TESTED:\n Internal<T> m_it{};\n};\n');
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Primitive::check']);
});

it('keeps reference alias method calls without inventing construction', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'), `struct Writer { Writer(int) {} void dump() {} };
using Ref=Writer&; using Other=Ref;
void run(Writer& original) { Other ref(original); ref.dump(); }
`);
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Writer::dump']);
});

it.each([
  '// using List = int;',
  'using Other = Writer; // using List = int;',
  '/*\nusing List = int;\n*/',
  'const char* text = "using List = int;";',
  'const char* text = R"(using List = int;)";',
])('ignores non-code local aliases: %s', async (noise) => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'), `struct Writer { void dump() {} };
void run() {
 using List = Writer;
 ${noise}
 List value;
 value.dump();
}
`);
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['Writer::dump']);
});

it('resolves base specifiers outside the derived class member scope', async () => {
  root=fs.mkdtempSync(path.join(os.tmpdir(),'cg-cpp-alias-'));
  fs.writeFileSync(path.join(root,'use.cpp'), `namespace outer {
struct Writer { void dump() {} };
struct ActualBase { using Field=Writer; };
struct Wrong {};
struct Derived : ActualBase {
 using ActualBase=Wrong;
 void run(Field value) { value.dump(); }
};
}
`);
  graph=await CodeGraph.init(root,{index:true});
  const targets=graph.getOutgoingEdgesFrom(graph.getNodesInFile('use.cpp').map(n=>n.id)).filter(e=>e.kind==='calls').map(e=>graph!.getNode(e.target)!.qualifiedName);
  expect(targets).toEqual(['outer::Writer::dump']);
});
