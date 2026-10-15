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
  const stop = Daemon.prototype.stop;
  Daemon.prototype.stop = function (reason) {
    state(this, `stop ${reason}`);
    return stop.call(this, reason);
  };
}
