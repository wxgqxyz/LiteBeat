# 媒体测试夹具

由 `node scripts/gen_media_fixtures.js` 与 `ffmpeg` 确定性生成，字节内容可从脚本复现。

| 文件 | 大小 | SHA-256 | 用途 |
| --- | --- | --- | --- |
| `range_10000.bin` | 10000 B | `3421d9aa928a94decb191ab8e8b76c1d8434bf602c5b3ba10ad42f54c8199c34` | Range 协议向量：第 i 字节值为 `i % 256` |
| `tone_1s.wav` | 16044 B | `edfdbd36a8301a85c984026c4d27d31f5c86a83dce18b90cbe6c490067cb6c6d` | 播放兼容：8 kHz / 16-bit / 单声道 / 1 s 440 Hz 正弦 |
| `tone_1s.mp3` | 5117 B | `fddf5ac2b0f1409767829b209d5f37d97a1c578b58c5a48ba17298bac3feef0f` | 播放兼容：LAME 440 Hz 1 s（`-q:a 4`，44.1 kHz） |
| `tone_1s.m4a` | 13414 B | `17ccfccea6002e6d1c839a4f0e79a0abaa0f3412fa199f0cd792a807454c37e6` | 播放兼容：AAC 96 kbps 440 Hz 1 s（44.1 kHz） |

音频再生成命令：

```text
ffmpeg -f lavfi -i "sine=frequency=440:duration=1:sample_rate=44100" -codec:a libmp3lame -q:a 4 tone_1s.mp3
ffmpeg -f lavfi -i "sine=frequency=440:duration=1:sample_rate=44100" -c:a aac -b:a 96k tone_1s.m4a
```
