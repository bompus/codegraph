# Call-edge precision: repowise's graded rows replayed on the fork (2026-09-25)

**Question.** repowise's benchmark graded 280 call edges emitted by CodeGraph 1.5.0 and found 58.6% correct (TypeScript 7/30, Kotlin 13/30, Python 19/30, C# 20/30 were the four languages that separated from repowise). The fork has since landed the binding model and the kernel resolver. Which of those graded edges does the fork still emit, and to what? This is step 1 of [the strategy doc](../design/strategy-2026-09.md) §6.

**Answer.** On the same 120 rows for those four languages, the fork's retained edges are **46 correct of 66 (70%)**, against 59 of 120 (49%) for 1.5.0 on the same rows. It got there mostly by declining rather than by resolving better. TypeScript dropped 28 of its 30 rows, including 6 of the 7 correct ones, so the fork's TypeScript member-call recall is now the larger problem. Kotlin is now the least precise language: 12 of its 23 retained edges are still wrong.

## Method

- **Rows.** `repowise-bench/graph/experiments/g1-edge-precision/rows/<language>-codegraph.json`: the call site, the target 1.5.0 bound, the stratum (resolution strategy), and the verdict and reason as graded by repowise from source.
- **Checkouts.** repowise-bench records no commit pins for these repositories, so each was checked out at the weekly commit (May–Aug 2026) where the most rows' source text sits on its recorded line: zod `912f0f51`, celery `4f159543`, Ocelot `1a2d8290` (best matches 19/30, 24/30 and 29/30); javalin `48bf31c3`, ktor `41b6ef93` and Exposed `c419caaa` at the last commit before 2026-08-10 (Kotlin rows carry no source text). A row whose line drifted is located at the nearest line within ±40 that holds its source fragment, or its callee name for Kotlin.
- **Index.** Fork at `0e2701d3` (#117), `codegraph init -y`, kernel resolver on (the default).
- **Replay.** For each located call site, the edge the fork emits there for the same callee name: `same` target as 1.5.0 (verdict carries over), `changed` target (graded by hand below), `dropped` (the ref is left unresolved) or `absent` (no edge and no ref at that line). Scripts: [`scripts/precision/`](../../scripts/precision/).

## Results

| language | 1.5.0 on these rows | fork keeps | of which correct | correct rows the fork drops | wrong rows the fork drops or fixes |
|---|---|---|---|---|---|
| typescript (zod) | 7 / 30 | 1 | **1 (100%)** | 6 of 7 | 23 of 23 |
| python (celery) | 19 / 30 | 18 | **16 (89%)** | 3 of 17 located | 7 of 9 located |
| csharp (Ocelot) | 20 / 30 | 24 | **18 (75%)** | 1 of 19 located | 4 of 10 |
| kotlin (javalin, ktor, Exposed) | 13 / 30 | 23 | **11 (48%)** | 4 of 13 | 5 of 17 |
| **four languages** | **59 / 120 (49%)** | **66** | **46 (70%)** | **14 of 56** | **39 of 59** |

Unlocated rows (2 Python correct, 2 Python wrong, 1 C# correct) are excluded from the fork's columns.

Six rows changed target and were graded by reading the source:

| row | 1.5.0 bound | fork binds | verdict |
|---|---|---|---|
| celery `t/integration/test_canvas.py:509` `group(...)` | `chunks.group` | `celery.canvas.group` class through `from celery import group` | correct |
| celery `t/unit/tasks/test_canvas.py:599` `signature('t')` | `Celery.signature` | `celery.canvas.signature` through its import | correct |
| javalin `TestRequest.kt:408` `TestUtil.test {…}` | `VueTestUtil::test` | `io.javalin.testing.TestUtil::test` | correct |
| ktor `SessionsBackwardCompatibleEncoder.kt:86` `parametersBuilder.append(…)` | `UrlDecodedParametersBuilder::append` | `StringValuesBuilder::append` (the interface `ParametersBuilder` extends) | correct |
| Ocelot `IOcelotBuilder.cs:97` (a declaration, not a call) | an `OcelotBuilder` overload | another `OcelotBuilder` overload | wrong |
| ktor `OutgoingContent.kt:65` (a declaration, not a call) | `PreCompressedResponse::getProperty` | `ObservableContent::getProperty` | wrong |

## What is still wrong

18 of the 20 retained wrong edges bind a call to a same-named member of an unrelated type; the other two are the declaration-line edges above.

- **Kotlin, exact-match (11).** DSL, builder and base-class receivers: the router's `get`/`path` bound to the test HTTP client's `HttpUtil::get` or `Context::path`, a request builder's `contentType` bound to an unrelated class, `Table.reference` bound to a val in an unrelated test object, the `GMTDate` constructor bound to a one-argument factory function, a test-base helper and an operator-invoked builder bound to same-named members in other modules, and a deprecated overload chosen over the live one.
- **C#, instance-method and exact-match (5).** Library receivers bound to repo methods that share the name (Moq's `Mock<T>.Verify`, xunit's `Assert.Null`, `StringValues.Value`), a field's own class method missed for a same-named one elsewhere, and a one-argument call bound to a three-argument overload.
- **Python, exact-match (2).** A class attribute (`_unpack_args = itemgetter(…)`) and a wrapper method, each bound to a different declaration of the same name.

The shared shape: a member call whose receiver type is unknown, or external, reaches a strategy that accepts any same-named method in the project. The fix direction is the one the strategy doc names: decline a member call whose receiver cannot be typed to a project type, instead of letting exact-match or instance-method pick by name, and record how sure each edge is.

## What the fork gave up

- **TypeScript member-call recall.** zod's fork index holds 2,366 `calls` edges; repowise's frozen 1.5.0 index of zod (a nearby commit) held 12,884 distinct calls. 13,980 TypeScript and JavaScript call refs fail as `unknown-receiver`, and 16,579 more fail without a reason. The six correct rows lost are chained or local-variable receivers (`schema.parse`, `outer.parse`, `z.string().ulid`) that the fork now leaves unresolved. The largest single class in the wrong rows, `z.string()` with `z` from `import * as z from "zod/v4"`, is now unresolved rather than resolved to the right export: a namespace import through the package's own `exports` map, which the import resolver does not follow yet.
- **Kotlin, 4 correct rows** now fail where 1.5.0 was right.

## Limits

- **This measures only edges 1.5.0 emitted.** Edges the fork emits that 1.5.0 did not are not sampled, so their precision is unknown; a fresh stratified sample of the fork's own output is the complete measurement.
- **Recall is measured only on the graded-correct rows**, a small sample; the edge counts above are the broader signal.
- **Commits are inferred, not pinned.** Line drift was absorbed by nearest-fragment matching; a few rows may sit on a neighbouring call of the same name.
- **Six verdicts are ours.** The rest are repowise's, carried over unchanged when the fork binds the same target.
- Go, Java, Swift, C++ and Rust, which tied in repowise's table, were not replayed.
