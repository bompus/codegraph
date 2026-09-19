/**
 * Spring application-event bridge (Java).
 *
 * Spring decouples an event publisher from its listener(s) through the application
 * event bus, linked by the EVENT TYPE: `eventPublisher.publishEvent(new XEvent(...))`
 * has no static edge to the `@EventListener void on(XEvent e)` that handles it (usually
 * in a different file). This bridges each `publishEvent(new XEvent(...))` site to every
 * listener of XEvent. Covers all four listener forms — param-typed `@EventListener`,
 * annotation-typed `@EventListener(XEvent.class)`, `@TransactionalEventListener`, and the
 * older `implements ApplicationListener<XEvent>` / `onApplicationEvent` — fans out to
 * multiple listeners of the same event, and proves precision: a published event with no
 * listener, and a same-file non-annotated method, both produce no edge.
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';
import * as os from 'node:os';
import { CodeGraph } from '../src';

describe('spring-event synthesizer', () => {
  let dir: string;
  beforeEach(() => { dir = fs.mkdtempSync(path.join(os.tmpdir(), 'spring-event-')); });
  afterEach(() => { fs.rmSync(dir, { recursive: true, force: true }); });

  const write = (rel: string, body: string) => {
    const p = path.join(dir, rel);
    fs.mkdirSync(path.dirname(p), { recursive: true });
    fs.writeFileSync(p, body);
  };

  it('bridges publishEvent(new X) to every listener form of X, ignoring unheard events and non-listeners', async () => {
    write('shop/OrderEvents.java', `package shop;
class OrderShippedEvent { }
class OrderCancelledEvent { }
class UnheardEvent { }
`);
    // Publisher — two events, one of them (UnheardEvent) has no listener.
    write('shop/OrderService.java', `package shop;
import org.springframework.context.ApplicationEventPublisher;
class OrderService {
    private ApplicationEventPublisher publisher;
    void ship() {
        publisher.publishEvent(new OrderShippedEvent());
        publisher.publishEvent(new UnheardEvent());
    }
    void cancel() {
        publisher.publishEvent(new OrderCancelledEvent());
    }
}
`);
    // Form 1: param-typed @EventListener — plus a same-file NON-listener (no annotation).
    write('shop/ShippingListener.java', `package shop;
import org.springframework.context.event.EventListener;
class ShippingListener {
    @EventListener
    public void onShipped(OrderShippedEvent event) { }

    public void helper(OrderShippedEvent event) { }
}
`);
    // Form 2: annotation-typed @EventListener(X.class) — fan-out, a 2nd OrderShipped listener.
    write('shop/AuditListener.java', `package shop;
import org.springframework.context.event.EventListener;
class AuditListener {
    @EventListener(OrderShippedEvent.class)
    public void audit(OrderShippedEvent event) { }
}
`);
    // Form 3: @TransactionalEventListener — a 3rd OrderShipped listener.
    write('shop/TxListener.java', `package shop;
import org.springframework.transaction.event.TransactionalEventListener;
class TxListener {
    @TransactionalEventListener
    public void afterShipped(OrderShippedEvent event) { }
}
`);
    // Form 4: older implements ApplicationListener<X> / onApplicationEvent.
    write('shop/LegacyListener.java', `package shop;
import org.springframework.context.ApplicationListener;
class LegacyListener implements ApplicationListener<OrderCancelledEvent> {
    @Override
    public void onApplicationEvent(OrderCancelledEvent event) { }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;

    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'spring-event'`
      )
      .all();

    const targets = (src: string) =>
      edges.filter((r: any) => r.source === src).map((r: any) => r.target).sort();
    // ship() → all three OrderShippedEvent listeners (param-typed, annotation-typed, transactional).
    expect(targets('ship')).toEqual(['afterShipped', 'audit', 'onShipped']);
    // cancel() → the ApplicationListener<X> form.
    expect(targets('cancel')).toEqual(['onApplicationEvent']);
    // Every shipped edge is keyed by the event type.
    expect(edges.filter((r: any) => r.source === 'ship').every((r: any) => r.via === 'OrderShippedEvent')).toBe(true);
    // PRECISION: UnheardEvent has no listener → no edge; the non-annotated helper is never a target.
    expect(edges.some((r: any) => r.via === 'UnheardEvent')).toBe(false);
    expect(edges.some((r: any) => r.target === 'helper')).toBe(false);

    cg.close?.();
  });

  it('bridges publishEvent(bareVar), AbstractAggregateRoot.registerEvent, and @DomainEvents', async () => {
    write('shop/Events.java', `package shop;
class OrderShippedEvent { }
class OrderCancelledEvent { }
class OrderPlacedEvent { }
class OrderPricedEvent { }
class CartCheckedEvent { }
class MutedEvent { }
`);
    // publishEvent of a bare identifier — the type is inferred inside the
    // enclosing method: a `XEvent ev` param, a `XEvent ev = new XEvent()` local,
    // or an `ev = new XEvent()` assignment. An untyped arg resolves nothing.
    write('shop/OrderService.java', `package shop;
import org.springframework.context.ApplicationEventPublisher;
class OrderService {
    private ApplicationEventPublisher publisher;
    void ship(OrderShippedEvent ev) {
        publisher.publishEvent(ev);
    }
    void cancel() {
        OrderCancelledEvent ev = new OrderCancelledEvent();
        publisher.publishEvent(ev);
    }
    void mute(Object raw) {
        publisher.publishEvent(raw);
    }
}
`);
    // Spring Data domain events: registerEvent inside an AbstractAggregateRoot
    // (published on save) — both inline `new` and a typed local.
    write('shop/Order.java', `package shop;
import org.springframework.data.domain.AbstractAggregateRoot;
class Order extends AbstractAggregateRoot {
    void place() {
        registerEvent(new OrderPlacedEvent());
    }
    void reprice() {
        OrderPricedEvent ev = new OrderPricedEvent();
        registerEvent(ev);
    }
}
`);
    // @DomainEvents: the RETURNED event objects are published on save.
    write('shop/Cart.java', `package shop;
import org.springframework.data.domain.AbstractAggregateRoot;
import org.springframework.domainEvents;
import java.util.List;
class Cart extends AbstractAggregateRoot {
    @DomainEvents
    List<Object> collect() {
        return List.of(new CartCheckedEvent());
    }
}
`);
    // PRECISION: a same-named registerEvent in a file with NO aggregate-root
    // reference is not Spring Data's hook — never a publisher.
    write('shop/PlainRecorder.java', `package shop;
class PlainRecorder {
    void registerEvent(Object ev) { }
    void touch() {
        registerEvent(new MutedEvent());
    }
}
`);
    write('shop/Listeners.java', `package shop;
import org.springframework.context.event.EventListener;
class Listeners {
    @EventListener public void onShipped(OrderShippedEvent e) { }
    @EventListener public void onCancelled(OrderCancelledEvent e) { }
    @EventListener public void onPlaced(OrderPlacedEvent e) { }
    @EventListener public void onPriced(OrderPricedEvent e) { }
    @EventListener public void onCartChecked(CartCheckedEvent e) { }
    @EventListener public void onMuted(MutedEvent e) { }
}
`);

    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const edges = db
      .prepare(
        `SELECT s.name source, t.name target, json_extract(e.metadata,'$.via') via
         FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'spring-event'`
      )
      .all();
    cg.close?.();

    const pairs = edges.map((r: any) => `${r.source}>${r.target}:${r.via}`).sort();
    expect(pairs).toEqual([
      'cancel>onCancelled:OrderCancelledEvent',
      'collect>onCartChecked:CartCheckedEvent',
      'place>onPlaced:OrderPlacedEvent',
      'reprice>onPriced:OrderPricedEvent',
      'ship>onShipped:OrderShippedEvent',
    ]);
    // PRECISION: publishEvent(raw) couldn't be typed → nothing; the non-DDD
    // registerEvent is not a Spring publisher → MutedEvent stays silent even
    // though a listener exists.
    expect(edges.some((r: any) => r.source === 'mute')).toBe(false);
    expect(edges.some((r: any) => r.via === 'MutedEvent')).toBe(false);
  });

  it('bridges a listener-body re-publish of an erased delegate arg to every listener', async () => {
    // halo shape: @EventListener(X.class) void on(X e) { publisher.publishEvent(e.getDelegate()) }
    // — the delegate's runtime type is erased, so the edge set is all listeners.
    write('shop/Delegator.java', `package shop;
import org.springframework.context.ApplicationEvent;
class DelegatorEvent extends ApplicationEvent {
    private final ApplicationEvent delegate;
    DelegatorEvent(Object source, ApplicationEvent delegate) { super(source); this.delegate = delegate; }
    ApplicationEvent getDelegate() { return delegate; }
}
class AlphaEvent { }
class BetaEvent { }
`);
    write('shop/Dispatcher.java', `package shop;
import org.springframework.context.ApplicationEventPublisher;
import org.springframework.context.event.EventListener;
class Dispatcher {
    private ApplicationEventPublisher publisher;
    @EventListener(DelegatorEvent.class)
    void onDelegator(DelegatorEvent event) {
        publisher.publishEvent(event.getDelegate());
    }
}
`);
    write('shop/Listeners.java', `package shop;
import org.springframework.context.event.EventListener;
class Listeners {
    @EventListener
    public void onAlpha(AlphaEvent event) { }
    @EventListener
    public void onBeta(BetaEvent event) { }
}
`);
    // PRECISION: a call-expression arg in a NON-listener method fans out to nothing.
    write('shop/Helper.java', `package shop;
import org.springframework.context.ApplicationEventPublisher;
class Helper {
    private ApplicationEventPublisher publisher;
    void notAListener(DelegatorEvent event) {
        publisher.publishEvent(event.getDelegate());
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
         WHERE json_extract(e.metadata,'$.synthesizedBy') = 'spring-event'`
      )
      .all();
    cg.close?.();

    const pairs = edges.map((r: any) => `${r.source}>${r.target}:${r.via}`).sort();
    expect(pairs).toEqual([
      'onDelegator>onAlpha:delegate:*',
      'onDelegator>onBeta:delegate:*',
      // self-loop to its own DelegatorEvent listener slot is skipped
    ]);
    expect(edges.some((r: any) => r.source === 'notAListener')).toBe(false);
  });

  it('produces no edges in a Spring app with no event bus (clean control)', async () => {
    write('shop/PlainService.java', `package shop;
import org.springframework.stereotype.Service;
@Service
class PlainService {
    private final Repo repo;
    PlainService(Repo repo) { this.repo = repo; }
    String find(String id) { return repo.get(id); }
}
`);
    const cg = await CodeGraph.init(dir, { silent: true });
    await cg.indexAll();
    const db = (cg as any).db.db;
    const count = db
      .prepare(`SELECT count(*) c FROM edges WHERE json_extract(metadata,'$.synthesizedBy') = 'spring-event'`)
      .get();
    expect(count.c).toBe(0);
    cg.close?.();
  });
});
