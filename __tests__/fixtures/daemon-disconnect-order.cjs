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
  // Time each shutdown step so a daemon that is slow to exit shows, in its log,
  // which step it was waiting on.
  let stopAt = 0;
  const mark = (step) => {
    process.stderr.write(`[disconnect-test] +${Date.now() - stopAt}ms ${step} resources=${process.getActiveResourcesInfo().join(',')}\n`);
  };
  const timed = (label, promise) => {
    mark(`${label} begin`);
    return Promise.resolve(promise).then(
      (value) => { mark(`${label} end`); return value; },
      (error) => { mark(`${label} threw ${error && error.message}`); throw error; },
    );
  };
  const net = require('node:net');
  const serverClose = net.Server.prototype.close;
  net.Server.prototype.close = function (cb) {
    if (!stopAt) return serverClose.call(this, cb);
    mark('server.close begin');
    return serverClose.call(this, (...args) => { mark('server.close end'); if (cb) cb(...args); });
  };
  const freshness = require('../../dist/mcp/index-freshness.js');
  const endFreshness = freshness.endFreshnessMeasurements;
  freshness.endFreshnessMeasurements = (...args) => timed('endFreshnessMeasurements', endFreshness(...args));
  const { MCPEngine } = require('../../dist/mcp/engine.js');
  const engineStop = MCPEngine.prototype.stop;
  MCPEngine.prototype.stop = function () {
    const wrap = (owner, method, label) => {
      if (!owner) return;
      const original = owner[method];
      owner[method] = (...args) => timed(label, original.apply(owner, args));
    };
    wrap(this.queryPool, 'destroy', 'queryPool.destroy');
    wrap(this.toolHandler, 'closeAll', 'toolHandler.closeAll');
    wrap(this.defaultLease, 'release', 'defaultLease.release');
    if (this.initPromise) timed('initPromise', this.initPromise).catch(() => {});
    return timed('engine.stop', engineStop.call(this));
  };
  process.on('exit', (code) => { if (stopAt) mark(`exit ${code}`); });
  const stop = Daemon.prototype.stop;
  Daemon.prototype.stop = function (reason) {
    state(this, `stop ${reason}`);
    if (!stopAt) {
      stopAt = Date.now();
      // Sample what keeps the process alive after shutdown begins.
      setInterval(() => mark('alive'), 500).unref();
    }
    return stop.call(this, reason);
  };
}
