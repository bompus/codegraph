// Only the detached daemon in the early-disconnect integration test loads this
// patch. Keep its initial idle deadline out of proxy startup; the real timer
// still arms when the last registered client disconnects.
if (process.env.CODEGRAPH_DAEMON_INTERNAL === '1') {
  const { Daemon } = require('../../dist/mcp/daemon.js');
  const started = new WeakSet();
  const arm = Daemon.prototype.armIdleTimer;
  Daemon.prototype.armIdleTimer = function () {
    if (!started.has(this)) {
      started.add(this);
      return;
    }
    return arm.call(this);
  };
  const state = (daemon, event) => {
    process.stderr.write(`[disconnect-test] ${event} clients=${daemon.getClientCount()} sockets=${daemon.sockets.size} stopping=${daemon.stopping} resources=${process.getActiveResourcesInfo().join(',')}\n`);
  };
  const connect = Daemon.prototype.handleConnection;
  Daemon.prototype.handleConnection = function (socket) {
    connect.call(this, socket);
    state(this, 'connection');
    socket.once('close', () => setImmediate(() => state(this, 'socket-close')));
  };
  const drop = Daemon.prototype.dropClient;
  Daemon.prototype.dropClient = function (session) {
    drop.call(this, session);
    state(this, 'client-close');
  };
  // Diagnostic: time each shutdown step and sample what keeps the process alive.
  const t0 = { at: 0 };
  const mark = (step) => process.stderr.write(`[stop-diag] +${Date.now() - t0.at}ms ${step} resources=${process.getActiveResourcesInfo().join(',')}\n`);
  const timed = (label, promise) => {
    mark(`${label} begin`);
    return Promise.resolve(promise).then((v) => { mark(`${label} end`); return v; }, (e) => { mark(`${label} threw ${e && e.message}`); throw e; });
  };
  const net = require('node:net');
  const serverClose = net.Server.prototype.close;
  net.Server.prototype.close = function (cb) {
    if (!t0.at) return serverClose.call(this, cb);
    mark('server.close begin');
    return serverClose.call(this, (...args) => { mark('server.close end'); if (cb) cb(...args); });
  };
  const freshness = require('../../dist/mcp/index-freshness.js');
  const endFresh = freshness.endFreshnessMeasurements;
  freshness.endFreshnessMeasurements = (...a) => timed('endFreshnessMeasurements', endFresh(...a));
  const { MCPEngine } = require('../../dist/mcp/engine.js');
  const engineStop = MCPEngine.prototype.stop;
  MCPEngine.prototype.stop = function () {
    if (this.queryPool) { const d = this.queryPool.destroy; const pool = this.queryPool; pool.destroy = () => timed('queryPool.destroy', d.call(pool)); }
    const th = this.toolHandler; const ca = th.closeAll; th.closeAll = () => timed('toolHandler.closeAll', ca.call(th));
    if (this.initPromise) timed('initPromise', this.initPromise).catch(() => {});
    if (this.defaultLease) { const lease = this.defaultLease; const r = lease.release; lease.release = () => timed('defaultLease.release', r.call(lease)); }
    mark(`engine.stop begin cg=${!!this.cg} lease=${!!this.defaultLease} indexing=${this.cg ? this.cg.isIndexing() : 'n/a'}`);
    return timed('engine.stop', engineStop.call(this));
  };
  process.on('exit', (code) => mark(`exit ${code}`));
  const stop = Daemon.prototype.stop;
  Daemon.prototype.stop = function (reason) {
    state(this, `stop ${reason}`);
    if (!t0.at) {
      t0.at = Date.now();
      const engine = this.engine;
      const sampler = setInterval(() => mark(`sample indexing=${engine.cg ? engine.cg.isIndexing() : 'n/a'}`), 500);
      sampler.unref();
    }
    return stop.call(this, reason);
  };
}
