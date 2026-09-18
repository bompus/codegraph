/**
 * NgRx effects dispatch bridge (Angular/TypeScript).
 *
 * NgRx decouples a `store.dispatch(action)` call from the effect that reacts to it,
 * linked by the ACTION: effects subscribe via `ofType(...)` inside `createEffect(...)`
 * (class-field, functional `const`, or legacy `@Effect()` property) and components
 * dispatch onto the store. This bridges each `*.store.dispatch(x)` site → every effect
 * ofType-subscribed to x's action. Both sides normalize to the action creator's last
 * dot-segment (`UsersActions.loadUsers` → `loadUsers`) or the legacy action class name
 * (`new LoadUsers()` → `LoadUsers`). The dispatched arg resolves three ways — an inline
 * creator/class call, a local `const a = creator()`/`new X()`, and a param annotated
 * `a: X`. Precision gates proven here: the receiver must be store-ish (a
 * `dispatcher.dispatch` is ignored, bare `dispatch(` never matches), and the action must
 * have a registered ofType handler (an unhandled action is never bridged).
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

describe('ngrx-effects synthesizer', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'ngrx-effects-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  const write = (rel: string, body: string) => {
    const p = path.join(dir, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, body);
  };

  it('bridges store.dispatch to ofType effects across registration and dispatch forms', async () => {
    write('src/user.actions.ts', `import { createActionGroup, createAction, emptyProps } from '@ngrx/store';
export const UsersActions = createActionGroup({
  source: 'Users',
  events: {
    loadUsers: emptyProps(),
    loadUsersSuccess: emptyProps(),
    refresh: emptyProps(),
    refreshDone: emptyProps(),
    legacyLoad: emptyProps(),
    audit: emptyProps(),
    deleteUser: emptyProps(),
  },
});
export const restoreTask = createAction('[Tasks] Restore');
export class LoadThings {
  constructor(public id: number) {}
}
`);
    write('src/user.effects.ts', `import { Injectable } from '@angular/core';
import { Actions, createEffect, ofType, Effect } from '@ngrx/effects';
import { map, switchMap, tap } from 'rxjs/operators';
import { Store } from '@ngrx/store';
import { UsersActions, restoreTask, LoadThings } from './user.actions';
import { UserService } from './user.service';

@Injectable()
export class UserEffects {
  constructor(private actions$: Actions, private users: UserService, private store: Store) {}

  loadUsers$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.loadUsers),
      switchMap(() => this.users.all().pipe(map((users) => UsersActions.loadUsersSuccess({ users }))))
    )
  );

  refresh$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.refresh, restoreTask),
      map(() => UsersActions.refreshDone())
    )
  );

  @Effect()
  legacyLoad$ = this.actions$.pipe(
    ofType(UsersActions.legacyLoad),
    switchMap(() => this.users.all())
  );

  @Effect()
  things$ = this.actions$.pipe(
    ofType(LoadThings),
    map(() => UsersActions.audit())
  );

  audit$ = createEffect(
    () => this.actions$.pipe(ofType(UsersActions.audit), tap(() => console.log('audit'))),
    { dispatch: false }
  );

  chain$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.loadUsersSuccess),
      tap(() => this.store.dispatch(UsersActions.audit()))
    )
  );
}
`);
    write('src/functional.effects.ts', `import { inject } from '@angular/core';
import { Actions, createEffect, ofType } from '@ngrx/effects';
import { map } from 'rxjs/operators';
import { UsersActions } from './user.actions';

export const deleteUser$ = createEffect(
  (actions$ = inject(Actions)) =>
    actions$.pipe(
      ofType(UsersActions.deleteUser),
      map(() => UsersActions.audit())
    ),
  { functional: true }
);
`);
    write('src/users.component.ts', `import { Component } from '@angular/core';
import { Store } from '@ngrx/store';
import { UsersActions, restoreTask, LoadThings } from './user.actions';

@Component({ selector: 'app-users', template: '' })
export class UsersComponent {
  constructor(private store: Store) {}

  load(): void {
    this.store.dispatch(UsersActions.loadUsers());
  }
  del(): void {
    this.store.dispatch(UsersActions.deleteUser());
  }
  refresh(): void {
    this.store.dispatch(UsersActions.refresh());
  }
  restore(): void {
    this.store.dispatch(restoreTask());
  }
  legacy(): void {
    this.store.dispatch(UsersActions.legacyLoad());
  }
  legacyClass(): void {
    this.store.dispatch(new LoadThings(7));
  }
  viaVar(id: number): void {
    const a = new LoadThings(id);
    this.store.dispatch(a);
  }
  viaVarCreator(): void {
    const a = restoreTask();
    this.store.dispatch(a);
  }
  viaParam(action: LoadThings): void {
    this.store.dispatch(action);
  }
  viaParamDotted(action: SomeNs.LoadThings): void {
    this.store.dispatch(action);
  }
  unhandled(): void {
    this.store.dispatch(UsersActions.refreshDone());
  }
  notStore(): void {
    this.dispatcher.dispatch(UsersActions.loadUsers());
  }
  plainDispatch(): void {
    dispatch(UsersActions.loadUsers());
  }
}
`);
    write('src/user.service.ts', `import { Injectable } from '@angular/core';
@Injectable()
export class UserService {
  all() { return []; }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, t.kind targetKind, json_extract(e.metadata,'$.via') via,
                json_extract(e.metadata,'$.registeredAt') registeredAt
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'ngrx-dispatch'
         ORDER BY s.name, t.name`
      )
      .all();

    const pairs = edges.map((r: any) => `${r.source}>${r.target}`).sort();
    // Class-field createEffect, multi-ofType, functional const, @Effect property,
    // legacy class action, var/param arg resolution, and a free effect→effect hop.
    expect(pairs).toEqual([
      'chain$>audit$',
      'del>deleteUser$',
      'legacy>legacyLoad$',
      'legacyClass>things$',
      'load>loadUsers$',
      'refresh>refresh$',
      'restore>refresh$',
      'viaParam>things$',
      'viaParamDotted>things$',
      'viaVar>things$',
      'viaVarCreator>refresh$',
    ]);
    expect([...new Set(edges.map((r: any) => r.via))].sort()).toEqual([
      'LoadThings', 'audit', 'deleteUser', 'legacyLoad', 'loadUsers', 'refresh', 'restoreTask',
    ]);
    // Effect targets span all three registration shapes.
    expect(new Set(edges.map((r: any) => r.targetKind))).toEqual(new Set(['method', 'constant', 'property']));
    // registeredAt records the effect node that subscribed the action.
    for (const r of edges as any[]) {
      const tgt = db.prepare('SELECT id FROM nodes WHERE name = ?').get(r.target);
      expect(r.registeredAt).toBe(tgt.id);
    }
    // PRECISION: a dispatched action with no ofType handler is never bridged; a
    // non-store receiver and a bare `dispatch(` never match.
    expect(edges.some((r: any) => r.via === 'refreshDone')).toBe(false);
    expect(edges.some((r: any) => r.source === 'notStore')).toBe(false);
    expect(edges.some((r: any) => r.source === 'plainDispatch')).toBe(false);

    cg.close?.();
  });

  it('produces no edges in a TS project with no NgRx effects (clean control)', async () => {
    write('src/store.ts', `export class Store {
  dispatch(action: unknown) { return action; }
}
export class Cart {
  constructor(private store: Store) {}
  checkout(): void {
    this.store.dispatch({ type: 'checkout' });
  }
  dispatchIt(): void {
    dispatch({ type: 'bare' });
  }
}
`);
    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const count = db
      .prepare(`SELECT count(*) c FROM edges WHERE json_extract(metadata,'$.synthesizedBy') = 'ngrx-dispatch'`)
      .get();
    expect(count.c).toBe(0);
    cg.close?.();
  });
});
