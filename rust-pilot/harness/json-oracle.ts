import fs from 'node:fs';
import { canonical } from './canonical';
process.stdout.write(canonical(JSON.parse(fs.readFileSync(0, 'utf8'))));
