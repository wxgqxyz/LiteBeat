const fs = require('fs');
const path = require('path');
const out = path.join(__dirname, '..', 'fixtures', 'media');
fs.mkdirSync(out, { recursive: true });

const bin = Buffer.alloc(10000);
for (let i = 0; i < 10000; i++) bin[i] = i % 256;
fs.writeFileSync(path.join(out, 'range_10000.bin'), bin);

const rate = 8000;
const n = rate;
const samples = Buffer.alloc(n * 2);
for (let i = 0; i < n; i++) {
  const v = Math.round(Math.sin(2 * Math.PI * 440 * i / rate) * 12000);
  samples.writeInt16LE(v, i * 2);
}
const hdr = Buffer.alloc(44);
hdr.write('RIFF', 0);
hdr.writeUInt32LE(36 + samples.length, 4);
hdr.write('WAVE', 8);
hdr.write('fmt ', 12);
hdr.writeUInt32LE(16, 16);
hdr.writeUInt16LE(1, 20);
hdr.writeUInt16LE(1, 22);
hdr.writeUInt32LE(rate, 24);
hdr.writeUInt32LE(rate * 2, 28);
hdr.writeUInt16LE(2, 32);
hdr.writeUInt16LE(16, 34);
hdr.write('data', 36);
hdr.writeUInt32LE(samples.length, 40);
fs.writeFileSync(path.join(out, 'tone_1s.wav'), Buffer.concat([hdr, samples]));
console.log('fixtures written to', out);
