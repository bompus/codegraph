/**
 * Known-wrong and known-right edges on pinned real repositories
 * (docs/design/resolution-binding-model-plan.md, Phase 0).
 *
 * Every `absent` case is an edge a resolution PR measured and removed on a
 * real corpus; the endpoints are the ones named in that PR's body. They must
 * stay gone. The `present` cases are controls: edges the same PRs reported
 * unchanged, so a change that silently kills resolution fails here rather
 * than looking like a precision win.
 *
 * Run with `npm run eval:precision -- <corpus>` (precision-runner.ts), which
 * fetches the pinned commit, indexes it, scores every case, and records the
 * resolved-edge histogram for LOST/GAINED comparison across builds.
 */
import type { EdgeCase } from './types.js';

export interface PrecisionCorpus {
  key: string;
  repo: string;
  /** Full SHA: GitHub only serves an arbitrary commit to `git fetch` by its full id. */
  commit: string;
  note: string;
}

export const PRECISION_CORPORA: Record<string, PrecisionCorpus> = {
  vite: {
    key: 'vite',
    repo: 'https://github.com/vitejs/vite.git',
    commit: '8492422b8f110625a90c702f42f30784e8cf19dc',
    note: 'The corpus #1713, #1718 and #1746 measured (LOST 4 / 12 / 157, GAINED 0).',
  },
  vitest: {
    key: 'vitest',
    repo: 'https://github.com/vitest-dev/vitest.git',
    commit: '7c818153add03b0bca54453a54e76961dd5be18d',
    note: '#1713: 34 fuzzy edges from bare imports removed, GAINED 0.',
  },
  svelte: {
    key: 'svelte',
    repo: 'https://github.com/sveltejs/svelte.git',
    commit: '5895c637b04dc8667020c8d326807c3f3a984472',
    note: '#1713: 5 fuzzy edges removed, GAINED 0.',
  },
  flask: {
    key: 'flask',
    repo: 'https://github.com/pallets/flask.git',
    commit: 'd73fa1cdcbd8b1465c151db8924ba58b1dd14e35',
    note: 'Python corpus for binding-model Phase 3 (import rows replace the Python import regex).',
  },
  gin: {
    key: 'gin',
    repo: 'https://github.com/gin-gonic/gin.git',
    commit: 'dcaa4296d111981ffb31ac3eba90bb63e1eb5ab9',
    note: 'Go corpus for binding-model Phase 3 (import rows replace the Go import regex).',
  },
  petclinic: {
    key: 'petclinic',
    repo: 'https://github.com/spring-projects/spring-petclinic.git',
    commit: '818c4136ea971c21674525f9053de0d9c7ad8cfe',
    note: 'Java corpus for binding-model Phase 3 (import rows replace the JVM import regex).',
  },
  exposed: {
    key: 'exposed',
    repo: 'https://github.com/JetBrains/Exposed.git',
    commit: '2155404863401e0257f89c301509cde59e7becd1',
    note: 'Kotlin corpus for binding-model Phase 3 (import rows replace the JVM import regex).',
  },
  javalin: {
    key: 'javalin',
    repo: 'https://github.com/javalin/javalin.git',
    commit: '48bf31c3e0fc9f218470dde17de67499fbf549eb',
    note: 'Mixed Java/Kotlin: Kotlin tests import static members of Java classes (ApiBuilder).',
  },
  slim: {
    key: 'slim',
    repo: 'https://github.com/slimphp/Slim.git',
    commit: '3675bf6baac66b07032575b7bef4200b60b7974b',
    note: 'PHP corpus for binding-model Phase 3 (use rows replace the PHP import regex).',
  },
  jq: {
    key: 'jq',
    repo: 'https://github.com/jqlang/jq.git',
    commit: '9d241e277204b83c4a7ddc7d733e5c72f99ef500',
    note: 'C corpus for binding-model Phase 3 (include rows and static storage replace the C/C++ regexes).',
  },
  json: {
    key: 'json',
    repo: 'https://github.com/nlohmann/json.git',
    commit: 'aa391dc0a56f8409e2e7aca6e7c9a9d766d44ce8',
    note: 'C++ corpus for binding-model Phase 3.',
  },
};

export const edgeCases: EdgeCase[] = [
  // --- javalin: a Kotlin file importing a Java class's static member ---
  {
    id: 'javalin-kotlin-imports-java-member', corpus: 'javalin', kind: 'calls',
    from: { file: 'javalin/src/test/java/io/javalin/TestApiBuilder.kt', name: '`ApiBuilder prefixes paths with slash`' },
    to: { file: 'javalin/src/main/java/io/javalin/apibuilder/ApiBuilder.java', name: 'get' }, expect: 'present',
    source: 'jvm member imports', why: 'The file imports io.javalin.apibuilder.ApiBuilder.get and calls get(...) bare.',
  },
  {
    id: 'javalin-bare-get-not-test-client', corpus: 'javalin', kind: 'calls',
    from: { file: 'javalin/src/test/java/io/javalin/TestApiBuilder.kt', name: '`ApiBuilder prefixes paths with slash`' },
    to: { file: 'javalin/src/test/java/io/javalin/testing/HttpUtil.kt', name: 'get' }, expect: 'absent',
    source: 'jvm member imports', why: 'HttpUtil.get is the test HTTP client; the bare get(...) is the imported ApiBuilder route.',
  },

  {
    id: 'slim-guarded-exception-receiver', corpus: 'slim', kind: 'calls',
    from: { file: 'Slim/Error/AbstractErrorRenderer.php', name: 'getErrorTitle' },
    to: { file: 'Slim/Exception/HttpException.php', name: 'getTitle' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'A positive instanceof HttpException branch narrows the receiver.',
  },
  {
    id: 'json-value-not-internal-serializer', corpus: 'json', kind: 'calls',
    from: { file: 'docs/mkdocs/docs/examples/dump.cpp', name: 'main' },
    to: { file: 'include/nlohmann/detail/output/serializer.hpp', name: 'dump' }, expect: 'absent',
    source: 'binding-model Phase 2b', why: 'Calls on JSON values do not directly call the internal serializer method.',
  },
  {
    id: 'flask-package-reexport-receiver', corpus: 'flask', kind: 'calls',
    from: { file: 'examples/tutorial/flaskr/auth.py', name: 'auth.py' },
    to: { file: 'src/flask/sansio/scaffold.py', name: 'route' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'The module-level Blueprint instance inherits route through the public flask package export.',
  },
  {
    id: 'gin-declared-factory-receiver', corpus: 'gin', kind: 'calls',
    from: { file: 'auth.go', name: 'BasicAuthForRealm' },
    to: { file: 'auth.go', name: 'searchCredential' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'processAccounts declares authPairs as its result type.',
  },
  {
    id: 'petclinic-bounded-type-parameter', corpus: 'petclinic', kind: 'calls',
    from: { file: 'src/test/java/org/springframework/samples/petclinic/service/EntityUtils.java', name: 'getById' },
    to: { file: 'src/main/java/org/springframework/samples/petclinic/model/BaseEntity.java', name: 'getId' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'The receiver type parameter explicitly extends BaseEntity.',
  },
  {
    id: 'exposed-wildcard-package-receiver', corpus: 'exposed', kind: 'calls',
    from: { file: 'exposed-java-time/src/test/kotlin/org/jetbrains/exposed/v1/javatime/JavaTimeTests.kt', name: 'testTimestampWithTimeZoneThrowsExceptionForUnsupportedDialects' },
    to: { file: 'exposed-jdbc/src/main/kotlin/org/jetbrains/exposed/v1/jdbc/SchemaUtils.kt', name: 'create' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'The wildcard import names the JDBC package, not the same-named R2DBC object.',
  },
  {
    id: 'exposed-wildcard-not-other-package', corpus: 'exposed', kind: 'calls',
    from: { file: 'exposed-java-time/src/test/kotlin/org/jetbrains/exposed/v1/javatime/JavaTimeTests.kt', name: 'testTimestampWithTimeZoneThrowsExceptionForUnsupportedDialects' },
    to: { file: 'exposed-r2dbc/src/main/kotlin/org/jetbrains/exposed/v1/r2dbc/SchemaUtils.kt', name: 'create' }, expect: 'absent',
    source: 'binding-model Phase 2b', why: 'No R2DBC SchemaUtils import binds this receiver.',
  },
  {
    id: 'vite-factory-receiver-control', corpus: 'vite', kind: 'calls',
    from: { file: 'packages/vite/scripts/benchCircularImport.ts', name: 'runBenchmark' },
    to: { file: 'packages/vite/src/module-runner/runner.ts', name: 'import' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'createServerModuleRunner declares ModuleRunner as its return type.',
  },
  {
    id: 'vite-external-receiver-not-project-method', corpus: 'vite', kind: 'calls',
    from: { file: 'packages/create-vite/src/index.ts', name: 'init' },
    to: { file: 'packages/vite/src/client/overlay.ts', name: 'text' }, expect: 'absent',
    source: 'binding-model Phase 2b', why: 'prompts.text belongs to the imported prompt library, not ErrorOverlay.',
  },

  {
    id: 'gin-composite-receiver-json-control', corpus: 'gin', kind: 'calls',
    from: { file: 'binding/json_test.go', name: 'TestJSONBindingBindBody' },
    to: { file: 'binding/json.go', name: 'BindBody' }, expect: 'present',
    source: 'binding-model Phase 2b', why: 'jsonBinding{}.BindBody names the JSON implementation explicitly.',
  },
  {
    id: 'gin-composite-receiver-not-bson', corpus: 'gin', kind: 'calls',
    from: { file: 'binding/json_test.go', name: 'TestJSONBindingBindBody' },
    to: { file: 'binding/bson.go', name: 'BindBody' }, expect: 'absent',
    source: 'binding-model Phase 2b', why: 'A JSON composite literal cannot invoke bsonBinding.BindBody.',
  },

  // --- cpp-begin-truth-set: same-name `begin` ties on nlohmann/json ---
  // Phase 3 re-broke 319 same-name ties toward file-level (exported) entities
  // and away from class members. These cases pin the ties that are correct
  // today (member calls on the receiver's own type) and the cross-TU member
  // picks that must stay gone. Bare `begin(x)` in one TU can never resolve
  // onto a test-local member in another TU; `using std::begin` / ADL /
  // std::vector-member sites have no in-project callee. Full per-site
  // accounting is in docs/benchmarks/cpp-begin-truth-set-2026-09-14.md;
  // the wrong-present edges it records are NOT encoded here (an `absent`
  // case for an edge the engine still draws would fail the gate).
  {
    id: 'json-begin-fuzzer-contains-word', corpus: 'json', kind: 'calls',
    from: { file: 'tests/thirdparty/Fuzzer/FuzzerDictionary.h', name: 'ContainsWord' },
    to: { file: 'tests/thirdparty/Fuzzer/FuzzerDictionary.h', name: 'begin' }, expect: 'present',
    source: 'cpp-begin-truth-set', why: 'ContainsWord calls begin() on its own Dictionary; the same-file member is the callee.',
  },
  {
    id: 'json-begin-fuzzer-end-calls-begin', corpus: 'json', kind: 'calls',
    from: { file: 'tests/thirdparty/Fuzzer/FuzzerDictionary.h', name: 'end' },
    to: { file: 'tests/thirdparty/Fuzzer/FuzzerDictionary.h', name: 'begin' }, expect: 'present',
    source: 'cpp-begin-truth-set', why: '`return begin() + Size` is a member call on the same Dictionary.',
  },
  {
    id: 'json-begin-front-member', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/json.hpp', name: 'front' },
    to: { file: 'include/nlohmann/json.hpp', name: 'begin' }, expect: 'present',
    source: 'cpp-begin-truth-set', why: '`return *begin()` calls the basic_json member (recorded as a function node).',
  },
  {
    id: 'json-begin-rend-member', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/json.hpp', name: 'rend' },
    to: { file: 'include/nlohmann/json.hpp', name: 'begin' }, expect: 'present',
    source: 'cpp-begin-truth-set', why: '`return reverse_iterator(begin())` calls the basic_json member.',
  },
  {
    id: 'json-begin-subscript-member', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/json.hpp', name: 'operator[]' },
    to: { file: 'include/nlohmann/json.hpp', name: 'begin' }, expect: 'present',
    source: 'cpp-begin-truth-set', why: '`set_parents(begin() + ...)` calls the basic_json member.',
  },
  {
    id: 'json-begin-emplace-member', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/json.hpp', name: 'emplace' },
    to: { file: 'include/nlohmann/json.hpp', name: 'begin' }, expect: 'present',
    source: 'cpp-begin-truth-set', why: '`auto it = begin()` calls the basic_json member.',
  },
  {
    id: 'json-begin-no-cross-test-member', corpus: 'json', kind: 'calls',
    from: { file: 'tests/src/unit-algorithms.cpp', name: 'unit-algorithms.cpp' },
    to: { file: 'tests/src/unit-convenience.cpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: 'A bare begin() on a const json in one TU must not resolve onto another test file-local alt_string_iter member (the Phase 3 old pick).',
  },
  {
    id: 'json-begin-no-eigen-member', corpus: 'json', kind: 'calls',
    from: { file: 'tests/src/unit-class_iterator.cpp', name: 'unit-class_iterator.cpp' },
    to: { file: 'tests/src/unit-regression3.cpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: 'A bare begin() in one TU must not resolve onto the issue_4320_eigen::vector3 test member in another TU.',
  },
  {
    id: 'json-begin-no-udt-member', corpus: 'json', kind: 'calls',
    from: { file: 'tests/src/unit-bjdata.cpp', name: 'unit-bjdata.cpp' },
    to: { file: 'tests/src/unit-udt.cpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: 'A bare begin() in one TU must not resolve onto the no_iterator_type test member in another TU.',
  },
  {
    id: 'json-begin-no-fifo-member', corpus: 'json', kind: 'calls',
    from: { file: 'tests/src/unit-diagnostics.cpp', name: 'unit-diagnostics.cpp' },
    to: { file: 'tests/thirdparty/fifo_map/fifo_map.hpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: 'A bare begin() in a test TU must not resolve onto the thirdparty fifo_map member.',
  },
  {
    id: 'json-begin-no-array-member', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/json.hpp', name: 'destroy' },
    to: { file: 'include/nlohmann/json.hpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: '`array->begin()` in destroy is std::vector::begin, not the same-file basic_json member.',
  },
  {
    id: 'json-begin-no-ordered-map-this', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/ordered_map.hpp', name: 'at' },
    to: { file: 'include/nlohmann/json.hpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: '`this->begin()` on ordered_map (a std::vector subclass) must not resolve onto json.hpp begin.',
  },
  {
    id: 'json-begin-no-adl-to-helper', corpus: 'json', kind: 'calls',
    from: { file: 'tests/src/unit-algorithms.cpp', name: 'unit-algorithms.cpp' },
    to: { file: 'tests/src/unit-user_defined_input.cpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: '`begin(expected)` is ADL / std::begin, not the other TU\'s test-helper free begin.',
  },
  {
    id: 'json-begin-no-using-std-to-proxy', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/detail/conversions/to_json.hpp', name: 'construct' },
    to: { file: 'include/nlohmann/detail/iterators/iteration_proxy.hpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: '`using std::begin; begin(arr)` must not resolve onto iteration_proxy::begin.',
  },
  {
    id: 'json-begin-no-doctest-to-dictionary', corpus: 'json', kind: 'calls',
    from: { file: 'tests/thirdparty/doctest/doctest.h', name: 'run' },
    to: { file: 'tests/thirdparty/Fuzzer/FuzzerDictionary.h', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: '`reporters_currently_used.begin()` is a vector member, not Dictionary::begin.',
  },
  {
    id: 'json-begin-no-abi-adl-to-member', corpus: 'json', kind: 'calls',
    from: { file: 'tests/abi/include/nlohmann/json_v3_10_5.hpp', name: 'construct' },
    to: { file: 'tests/abi/include/nlohmann/json_v3_10_5.hpp', name: 'begin' }, expect: 'absent',
    source: 'cpp-begin-truth-set', why: '`using std::begin; begin(arr)` in the vendored ABI header must not resolve onto that file\'s basic_json begin overloads.',
  },
  {
    id: 'json-items-calls-proxy-ctor', corpus: 'json', kind: 'calls',
    from: { file: 'include/nlohmann/json.hpp', name: 'items' },
    to: { file: 'include/nlohmann/detail/iterators/iteration_proxy.hpp', name: 'iteration_proxy' },
    expect: 'present',
    source: 'cpp-items-ctor',
    why: 'Main-header items() returns iteration_proxy(*this); that must stay a calls edge to the constructor, not only instantiates to the class.',
  },

  // --- #1713: a bare (npm / builtin) import must never fuzzy-match a project symbol ---
  {
    id: 'vite-bare-import-self-edge',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'scan.ts' },
    to: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'scan' },
    expect: 'absent',
    source: '#1713',
    why: "`import { scan } from 'rolldown/experimental'` resolved onto the importing file's OWN `scan` — a self-edge.",
  },
  {
    id: 'vite-bare-import-getEnv',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'packages/vite/src/node/config.ts', name: 'config.ts' },
    to: { file: 'packages/vite/src/node/plugins/importAnalysis.ts', name: 'getEnv' },
    expect: 'absent',
    source: '#1713',
    why: "`import { getEnv } from '@vitejs/devtools/config'` resolved onto an unrelated project `getEnv`.",
  },
  {
    id: 'vitest-bare-import-evaluatedModules',
    corpus: 'vitest',
    kind: 'imports',
    to: { file: '.ts', name: 'evaluatedModules' },
    expect: 'absent',
    source: '#1713',
    why: "`import type { EvaluatedModules } from 'vite/module-runner'` resolved (case-insensitively) onto the method VitestMocker::evaluatedModules in 17 files.",
  },
  {
    id: 'svelte-bare-import-bundle',
    corpus: 'svelte',
    kind: 'imports',
    to: { file: 'scripts/generate-browser-support.ts', name: 'bundle' },
    expect: 'absent',
    source: '#1713',
    why: "`import MagicString, { Bundle } from 'magic-string'` resolved onto the script function `bundle` in 5 files.",
  },

  // --- #1718 (supersedes #1709): fuzzy may reject a unique guess, never manufacture one ---
  {
    id: 'vite-fuzzy-nested-resolveConfig-getEnv',
    corpus: 'vite',
    kind: 'calls',
    from: { file: 'packages/vite/src/node/config.ts', name: 'resolveConfig' },
    to: { file: 'packages/vite/src/node/plugins/importAnalysis.ts', name: 'getEnv' },
    expect: 'absent',
    source: '#1718',
    why: 'A fuzzy call edge onto a nested function the call site cannot lexically reach.',
  },
  {
    id: 'vite-fuzzy-nested-build-scan',
    corpus: 'vite',
    kind: 'calls',
    from: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'build' },
    to: { file: 'packages/vite/src/node/optimizer/scan.ts', name: 'scan' },
    expect: 'absent',
    source: '#1718',
    why: 'The `calls` edge that followed the self-import above.',
  },

  // --- #1746 / #1719: a module that imports but exports nothing is sealed ---
  {
    id: 'vite-sealed-module-defineConfig',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'vite.config.ts', name: 'vite.config.ts' },
    to: { file: 'playground/ssr-html/test-stacktrace.js', name: 'vite' },
    expect: 'absent',
    source: '#1746',
    why: "Every `import { defineConfig } from 'vite'` across the playground resolved onto a module-scope `const vite` in a file that exports nothing (157 edges).",
  },

  // --- Controls: edges the PRs reported unchanged ---
  {
    id: 'vite-control-relative-import',
    corpus: 'vite',
    kind: 'imports',
    from: { file: 'packages/vite/src/node/config.ts', name: 'config.ts' },
    to: { file: 'packages/vite/src/node/logger.ts', name: 'createLogger' },
    expect: 'present',
    source: 'control',
    why: "`import { createLogger } from './logger'` is a relative project import; the import resolver must still bind it.",
  },
  {
    id: 'flask-control-relative-from-import',
    corpus: 'flask',
    kind: 'imports',
    from: { file: 'src/flask/app.py', name: 'app.py' },
    to: { file: 'src/flask/helpers.py', name: 'get_debug_flag' },
    expect: 'present',
    source: 'control',
    why: '`from .helpers import get_debug_flag` is a relative package import; the Python import mappings must still bind it.',
  },
];
