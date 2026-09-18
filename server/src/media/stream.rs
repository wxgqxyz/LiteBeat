//! 范围解析与有界分块流：单范围精确切片，多范围/If-Range 回落完整 200。

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes};
use futures_core::Stream;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::mpsc;
use tokio::time::timeout;

use super::body_guard::StreamGuard;

#[derive(Debug, PartialEq, Eq)]
pub enum RangeDecision {
    /// 完整 200 响应。
    Full,
    /// 206：闭区间 [start, end]。
    Partial { start: u64, end: u64 },
    /// 416：语法无效或不可满足。
    Unsatisfiable,
}

/// 解析单个 `Range` 头。多范围与携带 `If-Range` 的请求按策略忽略范围，
/// 返回完整 200；不生成 multipart 大缓冲，也不做无法验证的强校验。
pub fn decide_range(range: Option<&str>, if_range: Option<&str>, len: u64) -> RangeDecision {
    let Some(spec) = range else {
        return RangeDecision::Full;
    };
    if if_range.is_some() || len == 0 {
        return RangeDecision::Full;
    }
    let Some(rest) = spec.strip_prefix("bytes=") else {
        return RangeDecision::Unsatisfiable;
    };
    if rest.contains(',') {
        return RangeDecision::Full;
    }
    let unit = rest.trim();
    let Some((first, last)) = unit.split_once('-') else {
        return RangeDecision::Unsatisfiable;
    };
    match (first.is_empty(), last.is_empty()) {
        (false, false) => {
            let (Ok(start), Ok(end)) = (first.parse::<u64>(), last.parse::<u64>()) else {
                return RangeDecision::Unsatisfiable;
            };
            if start > end || start >= len {
                return RangeDecision::Unsatisfiable;
            }
            RangeDecision::Partial {
                start,
                end: end.min(len - 1),
            }
        }
        (false, true) => {
            let Ok(start) = first.parse::<u64>() else {
                return RangeDecision::Unsatisfiable;
            };
            if start >= len {
                return RangeDecision::Unsatisfiable;
            }
            RangeDecision::Partial {
                start,
                end: len - 1,
            }
        }
        (true, false) => {
            let Ok(suffix) = last.parse::<u64>() else {
                return RangeDecision::Unsatisfiable;
            };
            if suffix == 0 {
                return RangeDecision::Unsatisfiable;
            }
            let take = suffix.min(len);
            RangeDecision::Partial {
                start: len - take,
                end: len - 1,
            }
        }
        (true, true) => RangeDecision::Unsatisfiable,
    }
}

/// 从 `start` 起读取 `len` 字节，按 `chunk_bytes` 分块送入响应体。
/// 读取或投递连续 `no_progress` 无进展即中止；守卫随任务结束释放配额。
pub fn spawn_stream_body(
    file: File,
    start: u64,
    len: u64,
    chunk_bytes: usize,
    no_progress: Duration,
    guard: StreamGuard,
) -> Body {
    let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(1);
    tokio::spawn(async move {
        stream_to_completion(file, start, len, chunk_bytes, no_progress, tx).await;
        // 发送端已在上面函数返回时丢弃，接收端随即见到流结束，再释放守卫。
        drop(guard);
    });
    Body::from_stream(ReceiverStream { rx })
}

struct ReceiverStream {
    rx: mpsc::Receiver<Result<Bytes, io::Error>>,
}

impl Stream for ReceiverStream {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.rx.poll_recv(cx)
    }
}

async fn stream_to_completion(
    mut file: File,
    start: u64,
    len: u64,
    chunk_bytes: usize,
    no_progress: Duration,
    tx: mpsc::Sender<Result<Bytes, io::Error>>,
) {
    if len == 0 {
        return;
    }
    let mut buf = vec![0u8; chunk_bytes.min(len as usize)];
    let mut remaining = len;
    let seeked = timeout(no_progress, file.seek(io::SeekFrom::Start(start))).await;
    if !matches!(seeked, Ok(Ok(_))) {
        return;
    }
    while remaining > 0 {
        let want = (chunk_bytes as u64).min(remaining) as usize;
        let read = timeout(no_progress, file.read(&mut buf[..want])).await;
        let n = match read {
            Err(_) => return,
            Ok(Ok(0)) => return,
            Ok(Err(err)) => {
                let _ = tx.send(Err(err)).await;
                return;
            }
            Ok(Ok(n)) => n,
        };
        remaining -= n as u64;
        let payload = Bytes::copy_from_slice(&buf[..n]);
        match timeout(no_progress, tx.send(Ok(payload))).await {
            Err(_) | Ok(Err(_)) => return,
            Ok(Ok(())) => {}
        }
    }
}
