//! T04 媒体传输：`GET/HEAD /media/tracks/{id}` 的鉴权、受限路径、配额与 Range 语义。
//!
//! 契约（对齐架构文档第 6 节）：
//! - 单范围 206 精确切片；语法有效的多范围或带 `If-Range` 的请求忽略范围回 200 完整体。
//! - 不可满足或畸形范围回 416，附 `Content-Range: bytes */总长`。
//! - HEAD 忽略 Range，元数据与完整 GET 一致且无响应体。
//! - 全局并发 `media_inflight`（默认 32）超额 503 + `Retry-After`；
//!   单会话 `media_per_session`（默认 4）超额 429；配额持有到响应体结束或丢弃。
//! - 读取块 `media_chunk_bytes`（默认 32 KiB）；连续无进展 `no_progress_timeout`（默认 30s）中止。
//! - 音频响应 `Cache-Control: private, no-store`，不做 gzip/br 二次压缩。

pub mod body_guard;
pub mod path;
mod routes;
pub mod stream;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub use routes::media_router;

use crate::config::LimitsConfig;
use body_guard::MediaQuota;

#[derive(Debug)]
pub struct MediaEnv {
    pub quota: Arc<MediaQuota>,
    pub chunk_bytes: usize,
    pub no_progress_timeout: Duration,
    /// 配置库根的规范化绝对路径；DB 中的根必须逐等于其中之一。
    pub allowed_roots: Vec<PathBuf>,
}

impl MediaEnv {
    pub fn new(limits: &LimitsConfig, roots: impl IntoIterator<Item = impl AsRef<Path>>) -> Self {
        let allowed_roots = roots
            .into_iter()
            .filter_map(|root| std::fs::canonicalize(root).ok())
            .map(|path| strip_verbatim(&path))
            .collect();
        Self {
            quota: Arc::new(MediaQuota::new(
                limits.media_inflight,
                limits.media_per_session,
            )),
            chunk_bytes: limits.media_chunk_bytes,
            no_progress_timeout: Duration::from_secs(30),
            allowed_roots,
        }
    }
}

impl Default for MediaEnv {
    fn default() -> Self {
        Self {
            quota: Arc::new(MediaQuota::new(32, 4)),
            chunk_bytes: 32 * 1024,
            no_progress_timeout: Duration::from_secs(30),
            allowed_roots: Vec::new(),
        }
    }
}

/// Windows 规范化路径带 `\\?\` 前缀；比较与拼接前统一剥掉。
pub(crate) fn strip_verbatim(path: &Path) -> PathBuf {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}
