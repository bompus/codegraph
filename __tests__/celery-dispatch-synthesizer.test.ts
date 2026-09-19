/**
 * Celery task-dispatch bridge (Python).
 *
 * Celery decouples a task's call site from its body: a `@shared_task` / `@app.task`
 * decorated `def` is invoked through `task.delay(...)` / `task.apply_async(...)`, a
 * dynamic hop with no static edge. This bridges each `.delay`/`.apply_async` site to
 * the task function, gated on the DECORATOR (read from the source above the `def`) so a
 * `.delay()` on a non-task object resolves to nothing. Covers both decorator dialects
 * (`@shared_task`, `@app.task(...)`), the module-qualified `mod.task.apply_async()` form,
 * and proves the precision gates: a plain function called with `.delay()` and a canvas
 * `group(...).delay()` (no single identifier before `.delay`) both contribute no edge.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

describe('celery-dispatch synthesizer', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'celery-dispatch-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  it('bridges .delay()/.apply_async() to decorated tasks, ignoring non-task and canvas dispatch', async () => {
    // Two decorator dialects: bare @shared_task and arg'd @app.task(...).
    fs.writeFileSync(
      path.join(dir, 'tasks.py'),
      `from celery import shared_task
from myapp.celery import app


@shared_task
def send_email(to):
    return to


@app.task(bind=True, max_retries=3)
def crunch(self, n):
    return n * 2
`
    );
    fs.mkdirSync(path.join(dir, 'services'), { recursive: true });
    fs.writeFileSync(
      path.join(dir, 'services', 'tickets.py'),
      `from celery import shared_task


@shared_task
def invalidate_cache():
    return None
`
    );
    // A plain function — NOT a celery task — that nonetheless has .delay() called on it.
    fs.writeFileSync(
      path.join(dir, 'utils.py'),
      `def process_data(x):
    return x
`
    );
    // Dispatch sites, all inside one enclosing function.
    fs.writeFileSync(
      path.join(dir, 'views.py'),
      `from tasks import send_email, crunch
from services import tickets
from utils import process_data
from celery import group


def handle_request(req):
    send_email.delay(req.addr)                 # → send_email task (cross-file)
    crunch.apply_async(args=[5])               # → crunch task (@app.task dialect)
    tickets.invalidate_cache.apply_async()     # module-qualified → invalidate_cache
    process_data.delay(req.x)                  # NOT a task → no edge
    group([send_email.s(a) for a in req.addrs]).delay()  # canvas → no edge
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, t.file_path tf, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'celery-dispatch'`
      )
      .all();

    const targets = (src: string) => edges.filter((r: any) => r.source === src).map((r: any) => r.target).sort();
    // handle_request dispatches exactly the three real tasks (both dialects + module-qualified).
    expect(targets('handle_request')).toEqual(['crunch', 'invalidate_cache', 'send_email']);
    // The @app.task target resolved to the task def, not anything else.
    const crunchEdge = edges.find((r: any) => r.target === 'crunch');
    expect(crunchEdge.tf).toMatch(/tasks\.py$/);
    // Module-qualified `tickets.invalidate_cache.apply_async()` resolved by the last identifier.
    const cacheEdge = edges.find((r: any) => r.target === 'invalidate_cache');
    expect(cacheEdge.tf).toMatch(/services[\\/]tickets\.py$/);
    expect(cacheEdge.via).toBe('invalidate_cache');
    // PRECISION: a plain function called with .delay() is never targeted (no decorator).
    expect(edges.some((r: any) => r.target === 'process_data')).toBe(false);

    cg.close?.();
  });

  it('resolves aliased decorators, import aliases, task-object bindings, Task subclasses, and send_task names', async () => {
    fs.writeFileSync(
      path.join(dir, 'myapp.py'),
      `from celery import Celery
app = Celery('myapp')
`
    );
    fs.writeFileSync(
      path.join(dir, 'tasks.py'),
      `from celery import shared_task as st
from myapp import app
from celery import Task

t = app.task()
factory = app.task

@st
def by_alias(x):
    return x

@t
def by_factory_alias(x):
    return x

def bound_body(x):
    return x
bound = app.task(bound_body)

class CrunchTask(Task):
    def run(self, n):
        return n * 2

crunch = CrunchTask()

@shared_task(name='custom.named.task')
def custom_named(x):
    return x
`
    );
    fs.writeFileSync(
      path.join(dir, 'views.py'),
      `from tasks import by_alias, bound, CrunchTask, crunch
from tasks import by_factory_alias as aliased
import tasks
from myapp import app


def handle():
    by_alias.delay(1)                    # @st aliased decorator
    aliased.apply_async(args=[2])        # from-import alias of the task
    bound.delay(3)                       # task-object binding -> bound_body
    crunch.apply_async(args=[5])         # Task-subclass instance -> run
    tasks.custom_named.delay(6)          # custom-registered-name task


def enqueue():
    CrunchTask.delay(4)                  # Task subclass by class name -> run


def kickoff():
    app.send_task('tasks.by_alias')      # dotted registered name
    app.send_task('custom.named.task')   # name='…' decorator arg
    app.send_task('tasks.nonexistent')   # unknown name -> nothing
`
    );

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, t.file_path tf, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'celery-dispatch'`
      )
      .all();
    const targets = (src: string) => edges.filter((r: any) => r.source === src).map((r: any) => r.target).sort();
    const via = (t: string) => edges.filter((r: any) => r.target === t).map((r: any) => r.via).sort();

    // @st / @t aliases, the import alias, and the task-object binding all land.
    expect(via('by_alias').sort()).toEqual(['by_alias', 'tasks.by_alias']); // .delay + send_task
    expect(targets('handle')).toContain('by_factory_alias');
    expect(targets('handle')).toContain('bound_body');
    // Task subclass: both `CrunchTask.delay()` and the `crunch` instance hit `run`.
    const runs = edges.filter((r: any) => r.target === 'run');
    expect(runs.map((r: any) => r.via).sort()).toEqual(['CrunchTask', 'crunch']);
    expect(runs.map((r: any) => r.source).sort()).toEqual(['enqueue', 'handle']);
    // Custom-registered name: both the direct dispatch and send_task resolve.
    expect(via('custom_named').sort()).toEqual(['custom.named.task', 'custom_named']);
    // PRECISION: unknown task names contribute nothing.
    expect(edges.some((r: any) => r.via === 'tasks.nonexistent')).toBe(false);

    cg.close?.();
  });

  it('produces no edges in a Celery-free project (clean control)', async () => {
    fs.writeFileSync(
      path.join(dir, 'app.py'),
      `def schedule(job):
    job.delay()          # a .delay() that has nothing to do with Celery
    return job


def run():
    schedule(make_job())
`
    );
    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const count = db
      .prepare(
        `SELECT count(*) c FROM edges WHERE json_extract(metadata,'$.synthesizedBy') = 'celery-dispatch'`
      )
      .get();
    expect(count.c).toBe(0);
    cg.close?.();
  });
});
