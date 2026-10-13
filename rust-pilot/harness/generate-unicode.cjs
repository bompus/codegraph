// Node's Unicode categories are the reference for JavaScript identifier boundaries.
const fs = require('node:fs');
const path = require('node:path');
if (process.versions.unicode !== '17.0') throw Error(`Expected Unicode 17.0, got ${process.versions.unicode}`);
const table = Buffer.alloc(0x110000 / 8);
for (let cp = 0; cp < 0x110000; cp++) {
  if (/[\p{L}\p{N}]/u.test(String.fromCodePoint(cp))) table[cp >> 3] |= 1 << (cp & 7);
}
const target = path.join(__dirname, '../src/unicode-ln.bin');
if (process.argv.includes('--check')) {
  if (!fs.readFileSync(target).equals(table)) throw Error('Unicode table differs from Node');
  console.log('Unicode table: all 1,114,112 code points match Node');
} else fs.writeFileSync(target, table);
