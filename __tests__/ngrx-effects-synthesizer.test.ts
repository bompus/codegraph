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

  it('links an effect\'s store.select reads to the selector nodes (concatLatestFrom/withLatestFrom)', async () => {
    write('src/user.actions.ts', `import { createActionGroup, emptyProps } from '@ngrx/store';
export const UsersActions = createActionGroup({
  source: 'Users',
  events: { loadUsers: emptyProps(), audit: emptyProps() },
});
`);
    write('src/user/user.selectors.ts', `import { createSelector, createFeatureSelector } from '@ngrx/store';
export const selectUserState = createFeatureSelector<any>('user');
export const selectCurrentUser = createSelector(selectUserState, (s: any) => s.current);
export const selectUserIds = createSelector(selectUserState, (s: any) => s.ids);
export const selectShared = createSelector(selectUserState, (s: any) => s);
`);
    // A same-named selector in a DIFFERENT feature dir — unpinned resolution
    // must prefer the effect's own feature dir.
    write('src/other/other.selectors.ts', `import { createSelector } from '@ngrx/store';
export const selectShared = createSelector((s: any) => s, (s: any) => s);
`);
    // Same-named selector twice in the effect's own feature dir — a genuine
    // tie declines rather than guess.
    write('src/user/more.selectors.ts', `import { createSelector } from '@ngrx/store';
export const selectTied = createSelector((s: any) => s, (s: any) => s);
`);
    write('src/user/even-more.selectors.ts', `import { createSelector } from '@ngrx/store';
export const selectTied = createSelector((s: any) => s, (s: any) => s);
`);
    write('src/user/user.effects.ts', `import { Injectable, inject } from '@angular/core';
import { Actions, createEffect, ofType } from '@ngrx/effects';
import { concatLatestFrom, withLatestFrom } from '@ngrx/operators';
import { Store } from '@ngrx/store';
import { map } from 'rxjs/operators';
import { UsersActions } from '../user.actions';
import * as UserSelectors from './user.selectors';
import { selectUserIds } from './user.selectors';

@Injectable()
export class UserEffects {
  constructor(private actions$: Actions, private store: Store) {}

  loadUsers$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.loadUsers),
      concatLatestFrom(() => this.store.select(UserSelectors.selectCurrentUser)),
      map(([action, user]) => user)
    )
  );

  audit$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.audit),
      withLatestFrom(this.store.select(selectUserIds)),
      map(([action, ids]) => ids)
    )
  );

  feature$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.audit),
      concatLatestFrom(() => this.store.select(selectShared)),
      map(() => null)
    )
  );

  tied$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.audit),
      concatLatestFrom(() => this.store.select(selectTied)),
      map(() => null)
    )
  );

  notStore$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.audit),
      concatLatestFrom(() => this.facadeThing.select(UserSelectors.selectCurrentUser)),
      map(() => null)
    )
  );

  gone$ = createEffect(() =>
    this.actions$.pipe(
      ofType(UsersActions.audit),
      concatLatestFrom(() => this.store.select(selectMissing)),
      map(() => null)
    )
  );
}
`);
    write('src/user/user.service.ts', `import { Injectable } from '@angular/core';
@Injectable()
export class UserService { all() { return []; } }
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, t.file_path targetFile, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'ngrx-select'`
      )
      .all();

    const pairs = edges.map((r: any) => `${r.source}>${r.target}`).sort();
    // Namespace-pinned (UserSelectors.selectCurrentUser), named-import pinned
    // (selectUserIds), and unpinned-but-dir-nearest (selectShared) reads all
    // resolve; the dispatch edges stay a separate 'ngrx-dispatch' family.
    expect(pairs).toEqual([
      'audit$>selectUserIds',
      'feature$>selectShared',
      'loadUsers$>selectCurrentUser',
    ]);
    expect(edges.find((r: any) => r.source === 'feature$')!.targetFile).toContain('user.selectors.ts');
    // PRECISION: a tied same-name selector declines; a non-store receiver and
    // an unresolvable selector name emit nothing.
    expect(edges.some((r: any) => r.source === 'tied$')).toBe(false);
    expect(edges.some((r: any) => r.source === 'notStore$')).toBe(false);
    expect(edges.some((r: any) => r.source === 'gone$')).toBe(false);

    cg.close?.();
  });

  it('links component/guard store.select + selectSignal reads the same way', async () => {
    write('src/user/user.selectors.ts', `import { createSelector, createFeatureSelector } from '@ngrx/store';
export const selectUserState = createFeatureSelector<any>('user');
export const selectCurrentUser = createSelector(selectUserState, (s: any) => s.current);
export const selectUserIds = createSelector(selectUserState, (s: any) => s.ids);
`);
    write('src/user/user.component.ts', `import { Component } from '@angular/core';
import { Store } from '@ngrx/store';
import * as UserSelectors from './user.selectors';
import { selectUserIds } from './user.selectors';

@Component({ selector: 'app-users', template: '' })
export class UsersComponent {
  user$ = this.store.select(UserSelectors.selectCurrentUser);
  ids$ = this.store.selectSignal(selectUserIds);

  constructor(private store: Store) {}

  reload(): void {
    this.users = this.store.select(selectUserIds);
  }
}
`);
    write('src/user/user.guard.ts', `import { inject } from '@angular/core';
import { Store } from '@ngrx/store';
import { map } from 'rxjs/operators';
import { selectCurrentUser } from './user.selectors';

export const userGuard = () => {
  const store = inject(Store);
  return store.select(selectCurrentUser).pipe(map((u) => !!u));
};
`);
    // A .select( on a store-ish receiver in a file with NO @ngrx import is a
    // different framework's protocol (Akita & friends) — never bridged.
    write('src/user/akita-ish.ts', `export class OtherStore {
  select(x: string) { return x; }
}
export class AkitaComponent {
  constructor(private store: OtherStore) {}
  load(): void {
    this.store.select(selectCurrentUser);
  }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, json_extract(e.metadata,'$.via') via,
                json_extract(e.metadata,'$.readAt') readAt
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'ngrx-select'
         ORDER BY s.name, t.name`
      )
      .all();

    const pairs = edges.map((r: any) => `${r.source}>${r.target}`).sort();
    // Class-field reads, a selectSignal read, an in-method read, and a
    // functional guard's `store.select` all resolve to the same selectors.
    expect(pairs).toEqual([
      'ids$>selectUserIds',
      'reload>selectUserIds',
      'user$>selectCurrentUser',
      'userGuard>selectCurrentUser',
    ]);
    // PRECISION: the no-@ngrx file's .select( is never bridged.
    expect(edges.some((r: any) => r.source === 'load')).toBe(false);

    cg.close?.();
  });

  it('resolves a selector destructured off getRouterSelectors() (platform example-app shape)', async () => {
    // The real site: `export const { selectRouteData } = getRouterSelectors()`
    // in reducers/index.ts — a factory-destructure binding, not a
    // createSelector() declarator, so the selector node only exists via the
    // exported-factory-binding extraction arm.
    write('src/reducers/index.ts', `import { getRouterSelectors } from '@ngrx/router-store';
export const { selectRouteData, selectQueryParams } = getRouterSelectors();
const { notExported } = getRouterSelectors();
`);
    write('src/effects/router.effects.ts', `import { createEffect } from '@ngrx/effects';
import { concatLatestFrom } from '@ngrx/operators';
import { inject } from '@angular/core';
import { Store } from '@ngrx/store';
import { selectRouteData } from '../reducers';

export const routeEffect = createEffect(() => {
  const store = inject(Store);
  return store.someStream$.pipe(concatLatestFrom(() => store.select(selectRouteData)));
});
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    // The destructured exported bindings mint constant nodes; the unexported
    // destructure stays unextracted.
    const nodes = db
      .prepare(`SELECT name, kind, signature FROM nodes WHERE name IN ('selectRouteData','selectQueryParams','notExported')`)
      .all();
    expect(nodes.map((n: any) => `${n.name}:${n.kind}`).sort()).toEqual([
      'selectQueryParams:constant',
      'selectRouteData:constant',
    ]);
    expect(nodes.find((n: any) => n.name === 'selectRouteData')!.signature).toContain('getRouterSelectors');

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'ngrx-select'`
      )
      .all();
    expect(edges.map((r: any) => `${r.source}>${r.target}`)).toEqual(['routeEffect>selectRouteData']);

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
