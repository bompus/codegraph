/**
 * MediatR request/notification dispatch bridge (C#/.NET).
 *
 * MediatR decouples a `_mediator.Send(x)` / `_mediator.Publish(x)` call from the `Handle`
 * method that runs it, linked by the request/notification TYPE (the `IRequestHandler<T,…>`
 * generic). This bridges each mediator dispatch → the `Handle` of the matching handler.
 * The sent type is resolved from the argument three ways — inline `new X(...)`, a local
 * `var v = new X(...)`, and a parameter/local declared `X v` — and precision rests on two
 * gates proven here: the receiver must be mediator-ish (a `MessagingCenter.Send` is ignored),
 * and the type must have a handler (an `IRequest` with no handler is never bridged). Covers
 * `IRequest<T>`, void `IRequest` (single-arg `IRequestHandler<T>`), and `INotification`.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

describe('mediatr-dispatch synthesizer', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'mediatr-dispatch-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  const write = (rel: string, body: string) => {
    const p = path.join(dir, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, body);
  };

  it('bridges Send/Publish to the matching Handle across inline, local, and param arg forms', async () => {
    write('Requests.cs', `namespace Shop;
using MediatR;
public record GetThingsQuery : IRequest<ThingsVm>;
public record CreateThingCommand(string Name) : IRequest<int>;
public record DeleteThingCommand(int Id) : IRequest;
public record ThingDeletedNotification(int Id) : INotification;
public class UnhandledCommand : IRequest<int> { }
`);
    write('Handlers.cs', `namespace Shop;
using MediatR;
using System.Threading;
using System.Threading.Tasks;
public class GetThingsQueryHandler : IRequestHandler<GetThingsQuery, ThingsVm> {
    public Task<ThingsVm> Handle(GetThingsQuery request, CancellationToken ct) => Task.FromResult(new ThingsVm());
}
public class CreateThingCommandHandler : IRequestHandler<CreateThingCommand, int> {
    public Task<int> Handle(CreateThingCommand request, CancellationToken ct) => Task.FromResult(1);
}
public class DeleteThingCommandHandler : IRequestHandler<DeleteThingCommand> {
    public Task Handle(DeleteThingCommand request, CancellationToken ct) => Task.CompletedTask;
}
public class ThingDeletedNotificationHandler : INotificationHandler<ThingDeletedNotification> {
    public Task Handle(ThingDeletedNotification notification, CancellationToken ct) => Task.CompletedTask;
}
`);
    write('ThingsController.cs', `namespace Shop;
using MediatR;
using System.Threading.Tasks;
public class ThingsController {
    private readonly ISender _mediator;
    public ThingsController(ISender mediator) { _mediator = mediator; }

    public async Task GetThings() {
        var vm = await _mediator.Send(new GetThingsQuery());
    }
    public async Task Create(CreateThingCommand command) {
        var id = await _mediator.Send(command);
    }
    public async Task Delete(int id) {
        var command = new DeleteThingCommand(id);
        await _mediator.Send(command);
    }
    public async Task Notify(int id) {
        await _mediator.Publish(new ThingDeletedNotification(id));
    }
    public async Task Bogus() {
        await _mediator.Send(new UnhandledCommand());
    }
    public void ViaMessagingCenter() {
        MessagingCenter.Send(this, "evt", new CreateThingCommand("x"));
    }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'mediatr-dispatch'`
      )
      .all();

    // Four bridged dispatches: inline (GetThings, Notify), param-typed (Create), local var (Delete).
    expect(edges.map((r: any) => r.source).sort()).toEqual(['Create', 'Delete', 'GetThings', 'Notify']);
    expect([...new Set(edges.map((r: any) => r.via))].sort()).toEqual([
      'CreateThingCommand', 'DeleteThingCommand', 'GetThingsQuery', 'ThingDeletedNotification',
    ]);
    // Every target is a Handle method.
    expect(edges.every((r: any) => r.target === 'Handle')).toBe(true);
    // PRECISION: an IRequest with no handler is never bridged; a non-mediator .Send is ignored.
    expect(edges.some((r: any) => r.via === 'UnhandledCommand')).toBe(false);
    expect(edges.some((r: any) => r.source === 'ViaMessagingCenter')).toBe(false);

    cg.close?.();
  });

  it('resolves generic-typed args by erased name and declines member-path args', async () => {
    write('Requests.cs', `namespace Shop;
using MediatR;
public record Envelope<T>(T Inner) : IRequest<int>;
public record GetThingsQuery : IRequest<ThingsVm>;
`);
    write('Handlers.cs', `namespace Shop;
using MediatR;
using System.Threading;
using System.Threading.Tasks;
public class EnvelopeHandler<T> : IRequestHandler<Envelope<T>, int> {
    public Task<int> Handle(Envelope<T> request, CancellationToken ct) => Task.FromResult(0);
}
public class GetThingsQueryHandler : IRequestHandler<GetThingsQuery, ThingsVm> {
    public Task<ThingsVm> Handle(GetThingsQuery request, CancellationToken ct) => Task.FromResult(new ThingsVm());
}
`);
    write('ThingsController.cs', `namespace Shop;
using MediatR;
using System.Threading.Tasks;
public class ThingsController {
    private readonly ISender _mediator;
    public ThingsController(ISender mediator) { _mediator = mediator; }

    public async Task Wrap(Envelope<int> envelope) {
        var id = await _mediator.Send(envelope);
    }
    public async Task WrapGeneric<T>(Envelope<T> generic) {
        await _mediator.Send<int>(generic);
    }
    public async Task Relay(GetThingsQuery holder) {
        // Sends holder.Command — the member's type isn't visible here. The head
        // ident's declared type must NOT be bridged (that would be a wrong edge).
        await _mediator.Send(holder.Command);
    }
    public async Task Erase(GetThingsQuery query) {
        IRequest<int> erased = query;
        await _mediator.Send(erased);
    }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'mediatr-dispatch'`
      )
      .all();

    // `Envelope<int> envelope` and `Envelope<T> generic` both resolve to the
    // erased `Envelope` key — the same erasure `new X<…>` args already use —
    // and the explicit `Send<int>` generic call form dispatches too.
    expect(edges.map((r: any) => r.source).sort()).toEqual(['Wrap', 'WrapGeneric']);
    expect(edges.every((r: any) => r.via === 'Envelope')).toBe(true);
    expect(edges.every((r: any) => r.target === 'Handle')).toBe(true);
    // PRECISION: `Send(holder.Command)` resolves nothing (member-path args are
    // declined — bridging `holder`'s GetThingsQuery type would be a wrong edge),
    // and `IRequest<int> erased` resolves to `IRequest`, which has no handler.
    expect(edges.some((r: any) => r.source === 'Relay')).toBe(false);
    expect(edges.some((r: any) => r.source === 'Erase')).toBe(false);

    cg.close?.();
  });

  it('fans out Publish of an erased/base-typed domain event to every notification handler', async () => {
    write('Events.cs', `namespace Shop;
using MediatR;
public record OrderShippedDomainEvent(int Id) : INotification;
public record BuyerVerifiedDomainEvent(int Id) : INotification;
public record OrderCancelledCommand(int Id) : IRequest;
`);
    write('Handlers.cs', `namespace Shop;
using MediatR;
using System.Threading;
using System.Threading.Tasks;
public class OrderShippedHandler : INotificationHandler<OrderShippedDomainEvent> {
    public Task Handle(OrderShippedDomainEvent notification, CancellationToken ct) => Task.CompletedTask;
}
public class BuyerVerifiedHandler : INotificationHandler<BuyerVerifiedDomainEvent> {
    public Task Handle(BuyerVerifiedDomainEvent notification, CancellationToken ct) => Task.CompletedTask;
}
public class OrderCancelledCommandHandler : IRequestHandler<OrderCancelledCommand> {
    public Task Handle(OrderCancelledCommand request, CancellationToken ct) => Task.CompletedTask;
}
`);
    write('MediatorExtension.cs', `namespace Shop;
using MediatR;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
public static class MediatorExtension {
    // eShop's canonical domain-event drain: elements are INotification, so the
    // var-erased loop arg must reach every INotificationHandler.
    public static async Task DispatchDomainEventsAsync(this IMediator mediator, Ctx ctx) {
        var domainEvents = ctx.ChangeTracker.Entries()
            .SelectMany(x => x.Entity.DomainEvents).ToList();
        foreach (var domainEvent in domainEvents)
            await mediator.Publish(domainEvent);
    }
    // A base-typed declared arg fans out the same way.
    public static async Task NotifyBase(this IMediator mediator, INotification notification) {
        await mediator.Publish(notification);
    }
}
public class Controller {
    private readonly ISender _mediator;
    public Controller(ISender mediator) { _mediator = mediator; }
    // PRECISION declines:
    public async Task Precise(int id) {
        await _mediator.Publish(new OrderShippedDomainEvent(id)); // concrete new → its handler only
    }
    public async Task SendLoop(List<Ctx> orders) {
        foreach (var order in orders)
            await _mediator.Send(order); // Send never fans out
    }
    public async Task NonEventLoop(List<OrderCancelledCommand> commands) {
        foreach (var cmd in commands)
            await _mediator.Publish(cmd); // non-event collection — no evidence
    }
    public async Task MemberArg(Request req) {
        await _mediator.Publish(req.Notification); // member-path arg — declined
    }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.file_path tfile, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'mediatr-dispatch'`
      )
      .all();

    // The erased loop publish reaches both notification handlers — and NOT the
    // command handler (Send/request types never join the fan-out).
    const loopEdges = edges.filter((r: any) => r.source === 'DispatchDomainEventsAsync');
    expect(loopEdges.map((r: any) => r.tfile)).toEqual(['Handlers.cs', 'Handlers.cs']);
    expect(loopEdges.every((r: any) => r.via === 'domainEvent:*')).toBe(true);
    // The declared INotification param fans out the same way.
    const baseEdges = edges.filter((r: any) => r.source === 'NotifyBase');
    expect(baseEdges).toHaveLength(2);
    expect(baseEdges.every((r: any) => r.via === 'INotification')).toBe(true);
    // Concrete `new X()` still takes the precise path — one handler only.
    const precise = edges.filter((r: any) => r.source === 'Precise');
    expect(precise).toHaveLength(1);
    expect(precise[0]!.via).toBe('OrderShippedDomainEvent');
    // PRECISION: no Send fan-out, no non-event collection, no member-path arg.
    expect(edges.some((r: any) => r.source === 'SendLoop')).toBe(false);
    expect(edges.some((r: any) => r.source === 'NonEventLoop')).toBe(false);
    expect(edges.some((r: any) => r.source === 'MemberArg')).toBe(false);

    cg.close?.();
  });

  it('produces no edges in a C# project with no MediatR (clean control)', async () => {
    write('Service.cs', `namespace Shop;
public class Service {
    private readonly IRepo _repo;
    public Service(IRepo repo) { _repo = repo; }
    public string Find(string id) => _repo.Get(id);
}
`);
    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const count = db
      .prepare(`SELECT count(*) c FROM edges WHERE json_extract(metadata,'$.synthesizedBy') = 'mediatr-dispatch'`)
      .get();
    expect(count.c).toBe(0);
    cg.close?.();
  });
});
