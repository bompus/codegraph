---
name: codegraph-release
description: Use when editing the changelog, preparing an upstream release, or considering publishing, tags, or release workflows.
---

The release procedures below describe upstream `colbymchenry/codegraph` only. Upstream publishes to npm and [GitHub Releases](https://github.com/colbymchenry/codegraph/releases). `CHANGELOG.md` remains the source of truth for change notes.

### Writing changelog entries

**Default: write entries under `## [Unreleased]`** — that's the section reserved for work landing between releases. **Don't pre-create a `## [X.Y.Z]` block** for the next release: the Release workflow's first step is `scripts/prepare-release.mjs`, which automatically promotes everything under `[Unreleased]` into a new `## [X.Y.Z] - <YYYY-MM-DD>` block at release time (or merges into a pre-existing `[X.Y.Z]` block if one exists — but you don't need one). A pre-created sparse `[X.Y.Z]` block is picked by the extractor over the larger `[Unreleased]` section, so don't pre-stage one.

Formatting rules for any entry (anywhere — `[Unreleased]` or otherwise):

1. **Write friendly, user-facing notes — not engineer-facing ones.** Group under `### New Features` and `### Fixes` (sentence-case). Surface `### Breaking Changes` and `### Security` as their own sections **only when the release has them**; fold improvement-flavored changes into New Features. Omit empty sections. (The GitHub Release page extracts each version block **verbatim** via `scripts/extract-release-notes.mjs`, so entries must read well to users as-is.)
2. **One plain-language sentence per bullet:** what changed and why it matters to a user. Lead with the capability, or with the symptom that's now fixed.
3. **Strip the internals.** No internal file paths (`src/...`), no internal symbol / function / class names, no benchmark numbers / percentages / node-or-edge counts. **Keep:** language & framework names (Go, Spring, NestJS, …), things a user types or sets (`codegraph install`, `codegraph_explore`, the `CODEGRAPH_*` env vars), agent / IDE names (Claude Code, Cursor, opencode, Kiro, …), and a brief `Thanks @user` when a contributor is credited.
4. Issue / PR references in entries are by number (`(#403)` etc.); the GitHub renderer auto-links them in the published release notes.
5. **Don't add a `[X.Y.Z]: https://...` link reference yourself** — `prepare-release.mjs` appends it automatically when it promotes the version (idempotent: a re-run is a no-op if it already exists).
6. **Every release opens with a `### Highlights` block — the only part most people read.** At most ~8 one-line bullets, in plain language for someone who doesn't read code, ordered by what a typical user notices first (new agent/IDE support and setup changes, then answer quality, then reliability), plus a one-sentence upgrade note when a re-index is needed. Write or refresh it in `[Unreleased]` when a release is being prepared — not per PR — and keep the detailed `### New Features` / `### Fixes` entries below it. When `### Fixes` grows past ~15 entries, group them under `####` sub-headings (`Better answers from codegraph_explore`, `Finding your project, live updates, and the CLI`, `Indexing reliability and disk usage`, `Language and framework accuracy`) so a skimmer can find their area.

Multi-word headings like `### New Features` are safe on the normal release path: `prepare-release.mjs` **Case A** moves the whole `[Unreleased]` body verbatim into `[X.Y.Z]`. (Only its rarely-used **Case B** *merge* splits sub-sections with a single-word `^### (\w+)$` regex that wouldn't match them — and Case B fires only if a `[X.Y.Z]` block was pre-created, which rule above already forbids.)

### Upstream release flow (reference only)

Upstream runs releases through `.github/workflows/release.yml`; do not publish the root package manually. Agents do not bump versions unless explicitly asked. A requested release normally needs only the target version in `package.json`, plus the same version in `ui/package.json` (the component package is versioned with the engine, `ui-package.test.ts` pins it, and the workflow does not sync it); the workflow synchronizes the lock file, promotes `[Unreleased]`, builds the platform bundles, creates the GitHub Release, and publishes through npm trusted publishing. Read the workflow before changing or describing this process.
