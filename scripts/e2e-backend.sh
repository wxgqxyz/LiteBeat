#!/usr/bin/env bash
# 为 e2e 起一个隔离后端：临时数据目录 + 真实可解码音频，绝不碰管理员自己的曲库。
#
#   LITEBEAT_E2E_USER=e2eadmin LITEBEAT_E2E_PASSWORD='…' bash scripts/e2e-backend.sh
#
# 跑完后在另一个终端执行：
#   LITEBEAT_E2E_USER=… LITEBEAT_E2E_PASSWORD=… npm --prefix web run test:e2e
#
# 依赖：ffmpeg（生成 30 秒长音；1 秒夹具播完就 ended，会让播放断言变竞态）。
# 曲目行由 server/examples/seed_smoke.rs 直接入库，省掉等扫描任务跑完的轮询；
# 服务启动时仍会把 [[library.roots]] 登记进 library_roots（扫描端点按 root_id 派活）。
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
env_dir="${LITEBEAT_E2E_ENV:-${TEMP:-/tmp}/litebeat-e2e}"
bind="${LITEBEAT_E2E_BIND:-127.0.0.1:8090}"
user="${LITEBEAT_E2E_USER:?需要先设置 LITEBEAT_E2E_USER}"
password="${LITEBEAT_E2E_PASSWORD:?需要先设置 LITEBEAT_E2E_PASSWORD}"
litebeat="$repo/target/debug/litebeat.exe"

[[ -x "$litebeat" ]] || { echo "缺少 $litebeat，先 cargo build"; exit 1; }
[[ -d "$repo/web/dist" ]] || { echo "缺少 web/dist，先 npm --prefix web run build"; exit 1; }
command -v ffmpeg >/dev/null || { echo "需要 ffmpeg 生成长音夹具"; exit 1; }

music="$env_dir/music"
data="$env_dir/data"
config="$env_dir/config.toml"
mkdir -p "$music" "$data"

# 三个不同编码 + 一个中文标题，覆盖 codec 分发与 CJK 检索两条路径。
for spec in "tone-01.mp3:libmp3lame" "tone-02.m4a:aac" "中文歌曲.mp3:libmp3lame"; do
  name="${spec%%:*}"
  codec="${spec##*:}"
  if [[ ! -f "$music/$name" ]]; then
    ffmpeg -hide_banner -loglevel error -y -f lavfi \
      -i "sine=frequency=440:duration=30:sample_rate=44100" \
      -c:a "$codec" -b:a 96k "$music/$name"
  fi
done

cat >"$config" <<EOF
[server]
bind = "$bind"
web_dir = "$(cygpath -m -a "$repo/web/dist")"

[storage]
data_dir = "$(cygpath -m -a "$data")"

[[library.roots]]
name = "e2e-music"
path = "$(cygpath -m -a "$music")"
EOF

db="$data/litebeat.db"
if [[ ! -f "$db" ]]; then
  printf '%s\n%s\n' "$user" "$password" | "$litebeat" admin create --config "$config"
else
  echo "沿用已存在的 $db（口令可能与当前环境变量不同）"
fi

cargo run --quiet --manifest-path "$repo/server/Cargo.toml" --example seed_smoke -- \
  "$(cygpath -m -a "$db")" "$(cygpath -m -a "$music")" \
  "$(cygpath -m -a "$music/tone-01.mp3")" \
  "$(cygpath -m -a "$music/tone-02.m4a")" \
  "$(cygpath -m -a "$music/中文歌曲.mp3")"

echo "e2e 后端就绪：http://$bind（配置 $config，Ctrl-C 结束）"
exec "$litebeat" serve --config "$config"
