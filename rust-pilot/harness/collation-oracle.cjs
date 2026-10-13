// Every Unicode scalar, plus strings which exercise normalization and collation levels.
const collator = new Intl.Collator('en-US');
if (process.versions.cldr !== '48.0' || process.versions.unicode !== '17.0') throw Error('Oracle requires CLDR 48 / Unicode 17');
const chars = [];
for (let cp = 0; cp < 0x110000; cp++) {
  if (cp < 0xd800 || cp > 0xdfff) chars.push(String.fromCodePoint(cp));
}
chars.sort(collator.compare);
const output = Buffer.alloc(chars.length * 5);
chars.forEach((c, i) => {
  output.writeUInt32LE(c.codePointAt(0), i * 5);
  output[i * 5 + 4] = i ? collator.compare(chars[i - 1], c) + 1 : 1;
});
process.stdout.write(output);
