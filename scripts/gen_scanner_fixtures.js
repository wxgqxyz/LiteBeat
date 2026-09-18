// 生成 server/tests/scanner.rs 与 worker 单元测试依赖的音频夹具。
// 运行：node scripts/gen_scanner_fixtures.js
const fs = require('fs');
const path = require('path');
const out = path.join(__dirname, '..', 'fixtures', 'scanner');
fs.mkdirSync(out, { recursive: true });

// MPEG1 Layer III 帧：128kbps、44.1kHz、单声道 → 417 字节/帧、~26.12ms/帧。
function mp3Frame() {
  const frame = Buffer.alloc(417);
  frame[0] = 0xff;
  frame[1] = 0xfb; // MPEG1 Layer3, 无 CRC
  frame[2] = 0x90; // bitrate index=9 (128k), samplerate=0 (44.1k), 无 padding
  frame[3] = 0xc0; // mono
  for (let i = 4; i < frame.length; i++) frame[i] = (i * 37) % 251;
  return frame;
}
function bareMp3(seconds) {
  const frames = Math.round((seconds * 44100) / 1152);
  return Buffer.concat(Array.from({ length: frames }, mp3Frame));
}

function synchsafe(n) {
  return Buffer.from([(n >> 21) & 0x7f, (n >> 14) & 0x7f, (n >> 7) & 0x7f, n & 0x7f]);
}
// ID3v2.4 文本帧：UTF-8 编码字节 0x03。
function textFrame(id, value) {
  const body = Buffer.concat([Buffer.from([0x03]), Buffer.from(value, 'utf8')]);
  return Buffer.concat([Buffer.from(id, 'latin1'), synchsafe(body.length), Buffer.from([0, 0]), body]);
}
function apicFrame(pictureBytes) {
  const body = Buffer.concat([
    Buffer.from([0x03]),
    Buffer.from('image/png\0', 'latin1'),
    Buffer.from([0x03]), // 封面类型：Front cover
    Buffer.from('cover\0', 'latin1'),
    pictureBytes,
  ]);
  return Buffer.concat([Buffer.from('APIC', 'latin1'), synchsafe(body.length), Buffer.from([0, 0]), body]);
}
function id3v24(frames) {
  const body = Buffer.concat(frames);
  return Buffer.concat([Buffer.from('ID3'), Buffer.from([0x04, 0x00, 0x00]), synchsafe(body.length), body]);
}
function taggedMp3(frames, seconds) {
  return Buffer.concat([id3v24(frames), bareMp3(seconds ?? 1)]);
}

function write(rel, buf) {
  const target = path.join(out, rel);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, buf);
  console.log('写入', rel, buf.length, '字节');
}

// 无标签：仅裸 MPEG 帧，解析应回退到文件名。
write('no_tags.mp3', bareMp3(1));

// 中文路径 + 中文标签。
write(
  path.join('中文专辑', '中文歌曲.mp3'),
  taggedMp3([
    textFrame('TIT2', '中文歌曲标题'),
    textFrame('TPE1', '中文歌手'),
    textFrame('TALB', '中文专辑名'),
    textFrame('TPE2', '中文专辑歌手'),
    textFrame('TRCK', '3/12'),
  ])
);

// 损坏标签：ID3 头声明 1 MiB 体长，实际却只有几字节乱码。
const corrupt = Buffer.alloc(64);
Buffer.from('ID3').copy(corrupt, 0);
corrupt[3] = 0x04;
corrupt[4] = 0x00;
corrupt[5] = 0x00;
corrupt.writeUInt8(0x7f, 6);
corrupt.writeUInt8(0x7f, 7);
corrupt.writeUInt8(0x7f, 8); // 声明 ~268MB 却截断
for (let i = 9; i < corrupt.length; i++) corrupt[i] = 0xff;
write('corrupt_tag.mp3', corrupt);

// 超大封面标签：APIC ~2MiB，解析须成功且忽略封面内容。
const junkPng = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  Buffer.alloc(2 * 1024 * 1024, 0x41),
]);
write(
  'huge_cover.mp3',
  taggedMp3([textFrame('TIT2', '带超大封面的歌'), apicFrame(junkPng)])
);

// 损坏音频但无标签结构：期望 PARSE_FAILED（与 no_tags 对照）。
write('broken_audio.mp3', Buffer.from('not an audio file at all'.repeat(4), 'latin1'));

console.log('scanner 夹具生成完成 →', out);
