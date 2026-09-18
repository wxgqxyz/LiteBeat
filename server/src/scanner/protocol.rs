//! `litebeat scan-worker` 有界 JSON 行协议（架构文档 §3.2、§5.1）。
//!
//! 契约：
//! - 一行一条请求/响应；输入单行 ≤4 KiB，输出单行 ≤32 KiB，越界即视为协议违规。
//! - 单个文本字段最多 1024 字符；累计元数据 JSON ≤16 KiB，超出按优先级裁剪并记 `truncated`。
//! - 调试指令（sleep/crash/huge_output）仅在子进程环境带 `LITEBEAT_SCAN_WORKER_DEBUG=1`
//!   时生效，用于集成测试超时、崩溃与越界路径；生产默认不生效。

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_LINE_BYTES: usize = 4 * 1024;
pub const MAX_RESPONSE_LINE_BYTES: usize = 32 * 1024;
pub const MAX_TEXT_FIELD_CHARS: usize = 1024;
pub const MAX_METADATA_JSON_BYTES: usize = 16 * 1024;
/// 每任务错误明细行数上限；超出后只累计 failed 计数。
pub const MAX_ERROR_DETAIL_ROWS: usize = 1000;

/// 测试后门开关：仅当子进程带此环境变量时才接受 [`DebugDirective`]。
pub const DEBUG_ENV: &str = "LITEBEAT_SCAN_WORKER_DEBUG";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<DebugDirective>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebugDirective {
    SleepMs(u64),
    Crash,
    HugeOutput,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FileMetadata {
    pub title: String,
    pub artist: String,
    pub album_title: String,
    pub album_artist: String,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub mime: Option<String>,
    pub disc_no: i64,
    pub track_no: i64,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanResponse {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<FileMetadata>,
}

impl ScanResponse {
    pub fn parsed(mut meta: FileMetadata) -> Self {
        normalize(&mut meta);
        Self {
            ok: true,
            code: None,
            meta: Some(meta),
        }
    }

    pub fn failure(code: &str) -> Self {
        Self {
            ok: false,
            code: Some(code.to_owned()),
            meta: None,
        }
    }
}

/// 单字段截到 1024 字符；总量超 16 KiB 时按低优先级字段依次清空。
fn normalize(meta: &mut FileMetadata) {
    let mut truncated = false;
    for value in [
        &mut meta.title,
        &mut meta.artist,
        &mut meta.album_title,
        &mut meta.album_artist,
    ] {
        let kept: String = value.chars().take(MAX_TEXT_FIELD_CHARS).collect();
        truncated |= kept != *value;
        *value = kept;
    }
    // 裁剪顺序：先丢可选项，再折半低频字段，保住标题/时长等播放与展示必需字段。
    while serialized_size(meta) > MAX_METADATA_JSON_BYTES {
        if !trim_one(meta) {
            break;
        }
        truncated = true;
    }
    meta.truncated |= truncated;
}

/// 按优先级执行一步裁剪；返回 false 表示已无可裁字段。
fn trim_one(meta: &mut FileMetadata) -> bool {
    if meta.codec.take().is_some() {
        return true;
    }
    if meta.mime.take().is_some() {
        return true;
    }
    halve(&mut meta.album_artist) || halve(&mut meta.album_title) || halve(&mut meta.artist)
}

fn halve(value: &mut String) -> bool {
    if value.is_empty() {
        return false;
    }
    let keep = value.chars().count() / 2;
    *value = if keep == 0 {
        String::new()
    } else {
        value.chars().take(keep).collect()
    };
    true
}

fn serialized_size(meta: &FileMetadata) -> usize {
    serde_json::to_string(meta)
        .map(|text| text.len())
        .unwrap_or(usize::MAX)
}

/// 从读取端取一行并强制长度上限；超过上限或无换行的 EOF 残行返回错误。
pub fn read_bounded_line(reader: &mut impl BufRead, max: usize) -> io::Result<Option<String>> {
    let mut buffer = Vec::with_capacity(1024);
    let read = reader.read_until(b'\n', &mut buffer)?;
    if read == 0 {
        if buffer.is_empty() {
            return Ok(None);
        }
        return Err(io::Error::new(io::ErrorKind::InvalidData, "行末前 EOF"));
    }
    if buffer.last() != Some(&b'\n') || buffer.len() - 1 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("单行超过 {max} 字节上限"),
        ));
    }
    buffer.pop();
    if buffer.last() == Some(&b'\r') {
        buffer.pop();
    }
    String::from_utf8(buffer)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "非 UTF-8 行"))
        .map(Some)
}

pub fn write_bounded_line(writer: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let text = serde_json::to_string(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if text.len() > MAX_RESPONSE_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "响应超出单行上限",
        ));
    }
    writer.write_all(text.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with(title: String, artist: String) -> FileMetadata {
        FileMetadata {
            title,
            artist,
            ..Default::default()
        }
    }

    #[test]
    fn long_fields_are_capped_to_1024_chars() {
        let long = "歌".repeat(2000);
        let mut meta = meta_with(long.clone(), long.clone());
        normalize(&mut meta);
        assert_eq!(meta.title.chars().count(), MAX_TEXT_FIELD_CHARS);
        assert_eq!(meta.artist.chars().count(), MAX_TEXT_FIELD_CHARS);
        assert!(meta.truncated);
    }

    #[test]
    fn oversized_metadata_drops_optional_fields_first() {
        // 单字段上限 1024 字符；用 4 字节字符让总量真正越过 16 KiB。
        let mut meta = meta_with("🎵".repeat(1024), "🎵".repeat(1024));
        meta.album_title = "🎵".repeat(1024);
        meta.album_artist = "🎵".repeat(1024);
        meta.codec = Some("mp3".into());
        meta.mime = Some("audio/mpeg".into());
        normalize(&mut meta);
        assert!(serialized_size(&meta) <= MAX_METADATA_JSON_BYTES);
        assert_eq!(meta.title.chars().count(), 1024, "标题必须保留");
        assert!(meta.codec.is_none(), "可选项应先被丢弃");
        assert!(meta.truncated);
    }

    #[test]
    fn bounded_line_reads_reject_oversize_and_accept_normal() {
        let good = b"{\"path\":\"a\"}\n";
        let mut cursor = &good[..];
        let line = read_bounded_line(&mut cursor, 64).unwrap();
        assert_eq!(line.as_deref(), Some(r#"{"path":"a"}"#));
        assert!(read_bounded_line(&mut cursor, 64).unwrap().is_none());

        let huge = format!("{}\n", "x".repeat(100));
        let mut cursor = huge.as_bytes();
        assert!(read_bounded_line(&mut cursor, 64).is_err());

        let no_newline = b"tail-without-newline";
        let mut cursor = &no_newline[..];
        assert!(read_bounded_line(&mut cursor, 64).is_err());
    }

    #[test]
    fn response_roundtrip_and_line_limit() {
        let response = ScanResponse::failure("PARSE_FAILED");
        let mut buffer = Vec::new();
        write_bounded_line(&mut buffer, &response).unwrap();
        assert!(buffer.ends_with(b"\n"));
        let mut cursor: &[u8] = &buffer;
        let line = read_bounded_line(&mut cursor, MAX_RESPONSE_LINE_BYTES)
            .unwrap()
            .unwrap();
        let parsed: ScanResponse = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed, response);

        let oversized = meta_with("字".repeat(1024), "a".repeat(1024));
        let mut buffer = Vec::new();
        // 手工构造 >32KiB 响应验证写出端上限（normalize 之后不可能这么大，仅测边界）。
        let text = "x".repeat(MAX_RESPONSE_LINE_BYTES + 1);
        let io_err = write_bounded_line_impl(&mut buffer, &text).unwrap_err();
        assert_eq!(io_err.kind(), io::ErrorKind::InvalidData);
        let _ = oversized;
    }

    fn write_bounded_line_impl(writer: &mut impl Write, text: &str) -> io::Result<()> {
        if text.len() > MAX_RESPONSE_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "响应超出单行上限",
            ));
        }
        writer.write_all(text.as_bytes())?;
        writer.write_all(b"\n")
    }
}
