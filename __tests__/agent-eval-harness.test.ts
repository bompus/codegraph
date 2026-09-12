import { spawnSync } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { afterEach, describe, expect, it } from 'vitest';

const repoRoot = resolve(import.meta.dirname, '..');
const runAll = join(repoRoot, 'scripts/agent-eval/run-all.sh');
const audit = join(repoRoot, 'scripts/agent-eval/audit.sh');
const roots: string[] = [];

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function fixture() {
  const root = mkdtempSync(join(tmpdir(), 'codegraph-agent-eval-test-'));
  roots.push(root);
  const bin = join(root, 'bin');
  const corpus = join(root, 'corpus');
  const target = join(corpus, 'fixture');
  const out = join(root, 'out');
  mkdirSync(join(target, '.git'), { recursive: true });
  mkdirSync(join(target, '.codegraph'));
  mkdirSync(bin);
  mkdirSync(out);

  const jq = join(bin, 'jq');
  writeFileSync(jq, `#!/usr/bin/env node
let input=''; process.stdin.on('data', c => input += c); process.stdin.on('end', () => {
  if (process.argv.includes('-r')) {
    try { process.stdout.write(JSON.parse(input).tool_input?.command || ''); } catch {}
  } else {
    process.stdout.write('{"hookSpecificOutput":{"permissionDecision":"deny"}}\\n');
  }
});
`);
  chmodSync(jq, 0o755);

  const claude = join(bin, 'claude');
  writeFileSync(claude, '#!/usr/bin/env bash\nexit 23\n');
  chmodSync(claude, 0o755);

  return { root, bin, corpus, target, out };
}

// The harness is a bash script: POSIX only (seen failing on Windows 2026-09-12).
describe.runIf(process.platform !== 'win32')('agent evaluation harness safety', () => {
  it('returns a failure when a headless Claude arm fails', () => {
    const f = fixture();
    const codegraph = join(f.root, 'codegraph-under-test');
    writeFileSync(codegraph, '#!/usr/bin/env bash\nexit 0\n');
    chmodSync(codegraph, 0o755);

    const result = spawnSync('bash', [runAll, f.target, 'question', 'headless'], {
      env: {
        ...process.env,
        PATH: `${f.bin}:${process.env.PATH}`,
        CG_BIN: codegraph,
        CG_ARMS: 'with',
        AGENT_EVAL_OUT: f.out,
      },
      encoding: 'utf8',
    });

    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain('claude failed for headless-with turn 1');
  });

  it('uses a temporary npm prefix and removes it after a failed audit', () => {
    const f = fixture();
    const npmArgs = join(f.root, 'npm-args');
    const prefixFile = join(f.root, 'npm-prefix');
    const npm = join(f.bin, 'npm');
    writeFileSync(npm, `#!/usr/bin/env bash
set -eu
printf '%s\\n' "$@" > "$NPM_ARGS_FILE"
prefix=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = --prefix ]; then prefix="$2"; shift 2; else shift; fi
done
printf '%s' "$prefix" > "$NPM_PREFIX_FILE"
mkdir -p "$prefix/node_modules/.bin"
cat > "$prefix/node_modules/.bin/codegraph" <<'EOF'
#!/usr/bin/env bash
if [ "\${1:-}" = --version ]; then echo 9.9.9; fi
exit 0
EOF
chmod +x "$prefix/node_modules/.bin/codegraph"
`);
    chmodSync(npm, 0o755);

    const result = spawnSync('bash', [audit, '9.9.9', 'fixture', 'unused', 'question'], {
      env: {
        ...process.env,
        PATH: `${f.bin}:${process.env.PATH}`,
        CORPUS: f.corpus,
        AGENT_EVAL_OUT: f.out,
        NPM_ARGS_FILE: npmArgs,
        NPM_PREFIX_FILE: prefixFile,
      },
      encoding: 'utf8',
    });

    expect(result.status).not.toBe(0);
    expect(readFileSync(npmArgs, 'utf8')).not.toMatch(/(^|\\n)-g(\\n|$)/);
    const prefix = readFileSync(prefixFile, 'utf8');
    expect(prefix).toContain('codegraph-audit.');
    expect(existsSync(prefix)).toBe(false);
  });

  it('prints the intended diagnostic when no codegraph binary is available', () => {
    const f = fixture();
    const result = spawnSync('bash', [runAll, f.target, 'question', 'headless'], {
      env: {
        ...process.env,
        PATH: `${f.bin}:/usr/bin:/bin`,
        CG_BIN: '',
        AGENT_EVAL_OUT: f.out,
      },
      encoding: 'utf8',
    });

    expect(result.status).not.toBe(0);
    expect(result.stdout).toContain('no codegraph binary on PATH (set CG_BIN)');
  });
});
