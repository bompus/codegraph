/**
 * Object-literal registry dispatch synthesizer.
 *
 * A command registry maps keys → handler classes/functions in an object literal, then
 * dispatches by a RUNTIME key (`new registry[command]().execute()`) that static parsing
 * can't follow. The synthesizer links each dispatching method → each registered handler's
 * callable entry. Validates: a class registry resolves to the handler's `.execute` method;
 * the field-initializer form (`commands = {…}` matched against a `this.commands[k]` dispatch);
 * and the dispatch GATE — a look-alike object literal that is only ever accessed statically
 * (never `registry[var]`) yields no edges.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

describe('object-registry synthesizer', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'obj-registry-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  it('links a dispatcher to each registered command class’s execute method, gated on dynamic dispatch', async () => {
    fs.writeFileSync(
      path.join(dir, 'commands.ts'),
      `export class AddCommand { execute() { return 'add'; } }
export class RemoveCommand { execute() { return 'remove'; } }
export class MoveCommand { execute() { return 'move'; } }
`
    );
    fs.writeFileSync(
      path.join(dir, 'manager.ts'),
      `import { AddCommand, RemoveCommand, MoveCommand } from './commands';

const Cmd = { ADD: 'add', REMOVE: 'remove', MOVE: 'move' };

class CommandManager {
  commands = {
    [Cmd.ADD]: AddCommand,
    [Cmd.REMOVE]: RemoveCommand,
    [Cmd.MOVE]: MoveCommand,
  };

  executeCommand(command: string) {
    return new this.commands[command]().execute();
  }
}
`
    );
    // A look-alike registry that is NEVER dynamically dispatched (only a static `.add`
    // member access) — must yield NO edges. The dynamic `registry[var]` dispatch is the gate.
    fs.writeFileSync(
      path.join(dir, 'static.ts'),
      `import { AddCommand, RemoveCommand } from './commands';
const table = { add: AddCommand, remove: RemoveCommand };
export function direct() { return new table.add().execute(); }
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();

    const db = (cg as any).db.db;
    const rows = db
      .prepare(
        `SELECT s.name source_name, t.name target_name, t.kind target_kind, t.file_path target_file
         FROM edges e
         JOIN nodes s ON s.id = e.source
         JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'object-registry'`
      )
      .all();
    cg.close?.();

    // Exactly the 3 dispatcher→handler-entry edges: executeCommand → {Add,Remove,Move}Command.execute.
    expect(rows.length).toBe(3);
    expect(rows.every((r: any) => r.source_name === 'executeCommand')).toBe(true);
    expect(rows.every((r: any) => r.target_kind === 'method' && r.target_name === 'execute')).toBe(true);
    expect(rows.every((r: any) => /commands\.ts$/.test(r.target_file))).toBe(true);
    // The statically-accessed look-alike registry contributed nothing.
    expect(rows.some((r: any) => /static\.ts$/.test(r.target_file))).toBe(false);
  });

  it('follows a const alias of the registry — `const r = registry; r[k](…)` dispatches', async () => {
    fs.writeFileSync(
      path.join(dir, 'handlers.ts'),
      `export function alphaHandler() { return 'a'; }
export function betaHandler() { return 'b'; }
`
    );
    fs.writeFileSync(
      path.join(dir, 'dispatch.ts'),
      `import { alphaHandler, betaHandler } from './handlers';

const registry = {
  alpha: alphaHandler,
  beta: betaHandler,
};

export function dispatchByAlias(key: string) {
  const r = registry;        // alias-through-const — r[k] hits registry's entries
  return r[key]();
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const rows = db
      .prepare(
        `SELECT s.name source_name, t.name target_name
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'object-registry'
         ORDER BY target_name`
      )
      .all();
    cg.close?.();

    expect(rows.map((r: any) => `${r.source_name}>${r.target_name}`)).toEqual([
      'dispatchByAlias>alphaHandler',
      'dispatchByAlias>betaHandler',
    ]);
  });

  it('includes post-declaration augmentation — `registry["k"] = fn` and `registry[KEY] = fn`', async () => {
    fs.writeFileSync(
      path.join(dir, 'handlers.ts'),
      `export function alphaHandler() { return 'a'; }
export function betaHandler() { return 'b'; }
export function gammaHandler() { return 'g'; }
export function deltaHandler() { return 'd'; }
`
    );
    fs.writeFileSync(
      path.join(dir, 'dispatch.ts'),
      `import { alphaHandler, betaHandler, gammaHandler, deltaHandler } from './handlers';

const registry = {
  alpha: alphaHandler,
  beta: betaHandler,
};

const GAMMA_KEY = 'gamma';
registry['gamma'] = gammaHandler;   // literal-key augmentation
registry[GAMMA_KEY] = deltaHandler; // computed-key augmentation

export function dispatchAll(key: string) {
  return registry[key]();
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const rows = db
      .prepare(
        `SELECT t.name target_name
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'object-registry'
           AND s.name = 'dispatchAll'
         ORDER BY target_name`
      )
      .all();
    cg.close?.();

    // Both literal AND computed augmentation contribute to the fan-out.
    expect(rows.map((r: any) => r.target_name)).toEqual([
      'alphaHandler',
      'betaHandler',
      'deltaHandler',
      'gammaHandler',
    ]);
  });

  it('links a literal-key call through the alias precisely — `r["k"](…)` reaches only that entry', async () => {
    fs.writeFileSync(
      path.join(dir, 'handlers.ts'),
      `export function alphaHandler() { return 'a'; }
export function betaHandler() { return 'b'; }
export function gammaHandler() { return 'g'; }
`
    );
    fs.writeFileSync(
      path.join(dir, 'dispatch.ts'),
      `import { alphaHandler, betaHandler, gammaHandler } from './handlers';

const registry = {
  alpha: alphaHandler,
  beta: betaHandler,
};
registry['gamma'] = gammaHandler;

export function directByAlias() {
  const r = registry;
  return r['alpha']();          // literal key — precisely the alpha entry
}

export function augmentedByAlias() {
  const r = registry;
  return r['gamma']();          // literal key added by augmentation
}

export function unknownByAlias() {
  const r = registry;
  return r['nope']();           // 'nope' is not a registered key — skip
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const rows = db
      .prepare(
        `SELECT s.name source_name, t.name target_name
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'object-registry'
         ORDER BY source_name`
      )
      .all();
    cg.close?.();

    // Literal-key access yields a SINGLE precise edge each — alpha and the
    // augmented gamma. 'nope' matches no key → nothing. (The two dispatch
    // functions also happen to be var-dispatch-free, so no fan-out edges.)
    expect(rows.map((r: any) => `${r.source_name}>${r.target_name}`)).toEqual([
      'augmentedByAlias>gammaHandler',
      'directByAlias>alphaHandler',
    ]);
  });

  it('does not treat an element grab (`const h = reg[k]`) as a registry alias', async () => {
    fs.writeFileSync(
      path.join(dir, 'handlers.ts'),
      `export function alphaHandler() { return 'a'; }
export function betaHandler() { return 'b'; }
`
    );
    fs.writeFileSync(
      path.join(dir, 'dispatch.ts'),
      `import { alphaHandler, betaHandler } from './handlers';

const registry = {
  alpha: alphaHandler,
  beta: betaHandler,
};

export function weird(key: string) {
  const h = registry[key];   // element grab — h is a handler, not the registry
  return h[key]();           // indexing a function — must NOT fan out
}
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const rows = db
      .prepare(
        `SELECT 1 FROM edges e JOIN nodes s ON s.id = e.source
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'object-registry'`
      )
      .all();
    cg.close?.();

    // `h` is `registry[key]` — never aliased to the registry itself, so its
    // `[key]` access produces no registry edges at all.
    expect(rows.length).toBe(0);
  });

  it('bridges assign-then-call — `const v = reg[k]; v(…)` fans out like `reg[k](…)`', async () => {
    fs.writeFileSync(
      path.join(dir, 'handlers.ts'),
      `export function printHelpDocs() { return 'h'; }
export function printAbout() { return 'a'; }
export function executeReleaseNoteGeneration() { return 'r'; }
`
    );
    fs.writeFileSync(
      path.join(dir, 'index.ts'),
      `import { printHelpDocs, printAbout, executeReleaseNoteGeneration } from './handlers';

const COMMANDS = {
  help: printHelpDocs,
  about: printAbout,
  release_notes: executeReleaseNoteGeneration,
};

export async function main(cmdString: string) {
  const cmd = COMMANDS[cmdString];   // assign-then-call (warp-drive shape)
  await cmd(['--verbose']);
}

export async function execSub(arg: string) {
  const command = COMMANDS['help'];  // literal-key RHS — single precise edge
  if (command) {
    await command(arg);
  }
}

export function shadowed(k: string, cmd: () => void) {
  cmd();                              // cmd here is the PARAM, not the alias —
}                                     // different function, must not bridge

export function memberTail(k: string) {
  const v = COMMANDS[k].load;         // reg[k].member IS the pre-existing
  return v();                          // chained-dispatch shape (section 3) —
}                                      // it fans out independently of v()
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const rows = db
      .prepare(
        `SELECT s.name source_name, t.name target_name, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'object-registry'
         ORDER BY source_name, target_name`
      )
      .all();
    cg.close?.();

    expect(rows.map((r: any) => `${r.source_name}>${r.target_name}`)).toEqual([
      // Literal-key RHS → precisely the 'help' entry.
      'execSub>printHelpDocs',
      // Dynamic key → fan-out to all three handlers, source = the calling fn.
      'main>executeReleaseNoteGeneration',
      'main>printAbout',
      'main>printHelpDocs',
      // `COMMANDS[k].load` — the pre-existing chained-access dispatch arm
      // (unchanged by this work), source = memberTail.
      'memberTail>executeReleaseNoteGeneration',
      'memberTail>printAbout',
      'memberTail>printHelpDocs',
    ]);
  });
});
