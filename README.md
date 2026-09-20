# LiteBeat

可以部署在个人电脑、小型服务器或 NAS 上的轻量自托管音乐 Web 平台。在浏览器中管理和播放你自己的音乐库，桌面与移动共用一个响应式前端。

## 状态

当前完成 **T01 工程基线**、**T02 数据库层**（SQLite 模式、迁移命令、2 读/1 写有界工作队列）、**T03 登录与会话**（Argon2id、CSRF、限流）、**T04 原文件播放**（完整 HTTP Range 语义、全局/会话流配额、30s 无进展超时）、**T05 受限扫描**（惰性遍历、增量入库、有界 scan-worker 子进程、admin scans 端点）与 **T06 曲库检索**（tracks/albums/search/queue-ids 端点、不透明游标分页、中文安全搜索、002 索引迁移与可复现造数脚本）。**T07 前端外壳与全局播放器**（单实例 `<audio>` 跨路由存活、1000 上限队列且随机播放只打乱 ID、输入法安全的中文搜索、刷新恢复队列与位置但不自动播放）也已交付。歌单、性能与发布按 [实施路线](./03-delivery-and-validation.md) 的 T08–T11 逐步交付。

| 阶段 | 内容 | 状态 |
|---|---|---|
| T01 | 工程基线：服务、健康检查、前端构建 | ✅ 已完成并通过本地验证 |
| T02 | SQLite、迁移与有界工作队列 | ✅ 已完成并通过 12 项契约测试 |
| T03 | 管理员登录与会话 | ✅ 已完成并通过 17 项集成测试 |
| T04 | 原文件播放与 Range 语义 | ✅ 已完成并通过 11 项契约测试（含浏览器播放冒烟） |
| T05 | 受限扫描与增量入库 | ✅ 已完成并通过 14 项集成测试（服务启动即把 `[[library.roots]]` 登记进库并打印 `root_id`；Linux cgroup 分支待真机复验） |
| T06 | 曲库检索与中文搜索 API | ✅ 已完成并通过 9 项契约测试（另为索引命中追加 002 迁移，db 契约测试增至 13 项；深页仍是索引顺序扫描） |
| T07 | 前端外壳、音乐库与全局播放器 | ✅ 已完成并通过 37 项单测＋9 项端到端（系统 Edge 真实解码播放；未下载自带 Chromium，浏览器兼容矩阵待 T10；移动底部导航与内联图标并入 T08/T10）。交付后整轮走查修掉四处真缺陷：开发模式代理改写 Host 撞上 CSRF 同源判定导致无法登录、刷新恢复后进度滑块被 `max` 夹死在最左、全新安装未登记曲库根导致扫描报 `NOT_FOUND`、口令校验池在途上限比文档多 1（配套单测为竞态写法，已确定化） |
| T08–T11 | 歌单、性能、发布 | 计划中 |

## 规划文档

- [产品定位与前端体验](./01-product-and-experience.md)
- [技术架构与性能设计](./02-architecture-and-performance.md)
- [实施路线与验收计划](./03-delivery-and-validation.md)

## 技术栈

Rust + Axum + Tokio + SQLite（rusqlite, FTS5）｜Svelte 5 + TypeScript + Vite｜单进程部署，无 Redis / 独立搜索服务 / 实时转码。

## 快速开始

```powershell
# 后端（需要 Rust stable + MSVC 工具链）
cargo run -p litebeat -- serve --config config/litebeat.example.toml

# 前端构建
npm --prefix web ci
npm --prefix web run build
```

访问 `http://127.0.0.1:8090`。配置音乐目录：编辑 `config/litebeat.example.toml` 的 `[[library.roots]]`。服务启动时会把这些根登记进库并在日志里打印 `root_id`，创建扫描任务（`POST /api/v1/admin/scans`）要带的就是这个 id。

## 验证命令

```powershell
cargo fmt --all -- --check
cargo test -p litebeat --test health --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked
npm --prefix web run check
npm --prefix web run test:unit
npm --prefix web run build
```

前端端到端测试需要一个隔离后端和测试账号，凭据只写在本机环境变量里：

```powershell
$env:LITEBEAT_E2E_USER = 'e2eadmin'
$env:LITEBEAT_E2E_PASSWORD = '<只用于本机临时库的口令>'

# 终端 A：ffmpeg 造三条 30s 长音、临时数据目录、127.0.0.1:8090
bash scripts/e2e-backend.sh

# 终端 B：Playwright 自起 vite dev server；自带 Chromium 未下载时走系统 Edge
$env:LITEBEAT_E2E_CHANNEL = 'msedge'
npm --prefix web run test:e2e
```

## 设计目标（尚未测量）

内存预算：空库 RSS ≤24 MiB，20 路播放混合负载峰值 ≤96 MiB；前端首屏 JS gzip ≤80 KiB；普通列表 API p95 ≤100ms。完整指标与测量口径见规划文档，所有数字在真实实现与对照测试前仅为验收目标。

## 许可

MIT
