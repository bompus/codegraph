const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');
const [root, socketPath, protocol, writerModule] = process.argv.slice(2);
const writer = require(writerModule);
writer.tryAcquireWriterLock(root, 'daemon');
writer.markWriterReady(root);
fs.writeFileSync(path.join(root, '.codegraph', 'daemon.pid'), JSON.stringify({pid:process.pid,version:'test-build',socketPath,startedAt:Date.now()}));
const server = net.createServer(socket => socket.end(JSON.stringify({protocol:1,pid:process.pid,codegraph:'test-build',...(protocol === 'legacy' ? {} : {writerProtocol:1})}) + '\n'));
server.listen(socketPath, () => process.send('ready'));
process.on('SIGTERM', () => {
  writer.releaseWriterLock(root);
  fs.rmSync(path.join(root, '.codegraph', 'daemon.pid'), {force:true});
  server.close(() => process.exit(0));
});
