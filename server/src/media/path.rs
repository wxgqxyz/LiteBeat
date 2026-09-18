//! 曲目定位与受限路径解析：只允许落在配置库根内的普通文件被打开。

use std::path::{Component, Path, PathBuf};

use tokio::fs::File;

use crate::db::{Db, Value};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// 曲目不存在、不可用，或记录的路径越出配置库根——一律按 404 处理，不泄露原因。
    #[error("曲目不可用")]
    NotFound,
    #[error("数据库错误: {0}")]
    Database(#[from] crate::db::DbError),
    #[error("文件读取失败: {0}")]
    Io(std::io::Error),
}

pub struct ResolvedTrack {
    pub file: File,
    pub len: u64,
    pub mime: String,
}

/// relative_path 只接受纯相对路径分量；绝对路径、盘符、`..` 直接拒绝。
fn is_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(':')
        && std::path::Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

pub async fn resolve_track(
    db: &Db,
    allowed_roots: &[PathBuf],
    track_id: i64,
) -> Result<ResolvedTrack, ResolveError> {
    let rows = db
        .query_rows(
            "SELECT t.relative_path, t.mime, r.canonical_path
             FROM tracks t
             JOIN library_roots r ON r.id = t.root_id
             WHERE t.id = ? AND t.available = 1 AND r.enabled = 1",
            vec![Value::Integer(track_id)],
        )
        .await?;
    let Some(row) = rows.into_iter().next() else {
        return Err(ResolveError::NotFound);
    };
    let mut iter = row.into_iter();
    let (Some(rel), Some(mime), Some(root)) = (iter.next(), iter.next(), iter.next()) else {
        return Err(ResolveError::NotFound);
    };
    let Some(relative_path) = rel.as_text() else {
        return Err(ResolveError::NotFound);
    };
    let relative_path = relative_path.to_string();
    let mime = match mime {
        Value::Text(s) if !s.is_empty() => s,
        _ => "application/octet-stream".into(),
    };
    let Some(root_text) = root.as_text().map(str::to_string) else {
        return Err(ResolveError::NotFound);
    };
    if !is_safe_relative_path(&relative_path) {
        return Err(ResolveError::NotFound);
    }
    // 数据库里的根必须逐等于配置里的某个根，防止 DB 记录被改写后越权。
    let db_root = canonical_root(&root_text)?;
    if !allowed_roots
        .iter()
        .map(|configured| super::strip_verbatim(configured))
        .any(|configured| configured == db_root)
    {
        return Err(ResolveError::NotFound);
    }
    let joined = db_root.join(&relative_path);
    // canonicalize 会跟随符号链接；解析结果仍必须在根内。
    let canonical = tokio::fs::canonicalize(&joined)
        .await
        .map_err(|_| ResolveError::NotFound)
        .map(|path| super::strip_verbatim(&path))?;
    if !canonical.starts_with(&db_root) || canonical == db_root {
        return Err(ResolveError::NotFound);
    }
    let file = File::open(&canonical).await.map_err(ResolveError::Io)?;
    let metadata = file.metadata().await.map_err(ResolveError::Io)?;
    // 句柄元数据决定真实长度；与文件大小不一致（并发截断等）按不可用处理。
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(ResolveError::NotFound);
    }
    Ok(ResolvedTrack {
        file,
        len: metadata.len(),
        mime,
    })
}

fn canonical_root(text: &str) -> Result<PathBuf, ResolveError> {
    let path = Path::new(text);
    if !path.is_absolute() {
        return Err(ResolveError::NotFound);
    }
    // DB 存的是扫描时写入的规范化路径；这里再规范化一次以消除大小写/分隔符差异。
    let canonical = std::fs::canonicalize(path).map_err(|_| ResolveError::NotFound)?;
    Ok(super::strip_verbatim(&canonical))
}
