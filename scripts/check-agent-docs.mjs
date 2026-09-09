import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const maxBytes = 32_768;
const agentsPath = fileURLToPath(new URL('../AGENTS.md', import.meta.url));
const bytes = Buffer.byteLength(readFileSync(agentsPath, 'utf8'), 'utf8');

console.log(`AGENTS.md: ${bytes}/${maxBytes} bytes`);
if (bytes > maxBytes) {
  console.error('AGENTS.md exceeds Codex\'s default project instruction limit.');
  process.exitCode = 1;
}
