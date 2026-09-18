//! 惰性目录遍历：按需出条目，绝不先收集全量路径（架构文档 §5.1 第 2 步）。
//!
//! 规则：层级上限 64；默认不跟随符号链接/reparse point；跳过隐藏与临时文件；
//! 只保留音频扩展名；目录读取失败产出 [`WalkItem::Error`]，由协调器决定
//! 本次扫描是否算“完整成功”（不完整的扫描不得把未见条目判为删除）。

use std::collections::HashSet;
use std::fs::ReadDir;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const MAX_DEPTH: usize = 64;

/// 支持入库的音频扩展名（小写，不含点）。
pub fn is_audio_extension(path: &Path) -> bool {
    match path.extension().and_then(|value| value.to_str()) {
        Some(extension) => AUDIO_EXTENSIONS
            .iter()
            .any(|known| extension.eq_ignore_ascii_case(known)),
        None => false,
    }
}

const AUDIO_EXTENSIONS: [&str; 8] = ["mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "wav"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    /// 相对根目录、始终以 `/` 分隔的路径。
    pub relative_path: String,
    pub absolute: PathBuf,
    pub size_bytes: i64,
    pub mtime_ns: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkItem {
    File(WalkEntry),
    /// 链接被跳过：记录明细供管理员查看，但不算扫描失败。
    SkippedLink {
        relative_path: String,
    },
    /// 遍历失败（目录不可读、超深、stat 失败）。
    Error {
        relative_path: String,
        code: String,
    },
}

struct PendingDir {
    relative_prefix: String,
    depth: usize,
    entries: ReadDir,
}

/// 深度优先的惰性迭代器；构造时只读根目录一层。
pub struct DirectoryWalker {
    stack: Vec<PendingDir>,
    seen_dirs: HashSet<PathBuf>,
    pending_error: Option<WalkItem>,
}

impl DirectoryWalker {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref();
        let absolute = root.to_path_buf();
        match walk_read_dir(&absolute) {
            Ok(entries) => Self {
                stack: vec![PendingDir {
                    relative_prefix: String::new(),
                    depth: 0,
                    entries,
                }],
                seen_dirs: HashSet::from([absolute]),
                pending_error: None,
            },
            Err(error) => {
                tracing::warn!(path = %root.display(), error = %error, "根目录不可读");
                Self {
                    stack: Vec::new(),
                    seen_dirs: HashSet::new(),
                    pending_error: Some(WalkItem::Error {
                        relative_path: String::new(),
                        code: classify_io_error(&error).to_owned(),
                    }),
                }
            }
        }
    }
}

impl Iterator for DirectoryWalker {
    type Item = WalkItem;

    fn next(&mut self) -> Option<WalkItem> {
        if let Some(error) = self.pending_error.take() {
            return Some(error);
        }
        while !self.stack.is_empty() {
            let index = self.stack.len() - 1;
            let depth = self.stack[index].depth;
            let prefix = self.stack[index].relative_prefix.clone();
            let next_entry = self.stack[index].entries.next();
            let entry = match next_entry {
                Some(Ok(entry)) => entry,
                Some(Err(error)) => {
                    // 目录中途读失败：整个根目录遍历视为不完整。
                    let code = classify_io_error(&error).to_owned();
                    return Some(WalkItem::Error {
                        relative_path: prefix.trim_end_matches('/').to_owned(),
                        code,
                    });
                }
                None => {
                    self.stack.pop();
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().to_string();
            if is_hidden_or_temp(&name) {
                continue;
            }
            let relative = format!("{prefix}{name}");
            let file_type = match walk_file_type(&entry) {
                Ok(file_type) => file_type,
                Err(error) => {
                    return Some(WalkItem::Error {
                        relative_path: relative,
                        code: classify_io_error(&error).to_owned(),
                    });
                }
            };
            if file_type.is_symlink() {
                return Some(WalkItem::SkippedLink {
                    relative_path: relative,
                });
            }
            if file_type.is_dir() {
                let child_prefix = format!("{relative}/");
                if depth + 1 > MAX_DEPTH {
                    return Some(WalkItem::Error {
                        relative_path: relative,
                        code: "DEPTH_LIMIT".to_owned(),
                    });
                }
                let absolute = entry.path();
                // 链接已排除，仍防御硬链接图/绑定导致的环。
                if !self.seen_dirs.insert(absolute.clone()) {
                    continue;
                }
                match walk_read_dir(&absolute) {
                    Ok(entries) => {
                        self.stack.push(PendingDir {
                            relative_prefix: child_prefix,
                            depth: depth + 1,
                            entries,
                        });
                    }
                    Err(error) => {
                        return Some(WalkItem::Error {
                            relative_path: relative,
                            code: classify_io_error(&error).to_owned(),
                        });
                    }
                }
                continue;
            }
            if !file_type.is_file() || !is_audio_extension(&entry.path()) {
                continue;
            }
            let absolute = entry.path();
            let metadata = match absolute.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    return Some(WalkItem::Error {
                        relative_path: relative,
                        code: classify_io_error(&error).to_owned(),
                    });
                }
            };
            let size_bytes = metadata.len() as i64;
            let mtime_ns = match metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|since| since.as_nanos() as i64)
            {
                Some(value) => value,
                None => {
                    return Some(WalkItem::Error {
                        relative_path: relative,
                        code: "MTIME_UNAVAILABLE".to_owned(),
                    });
                }
            };
            return Some(WalkItem::File(WalkEntry {
                relative_path: relative,
                absolute,
                size_bytes,
                mtime_ns,
            }));
        }
        None
    }
}

fn classify_io_error(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "PERMISSION_DENIED",
        io::ErrorKind::NotFound => "NOT_FOUND",
        _ => "IO_ERROR",
    }
}

fn is_hidden_or_temp(name: &str) -> bool {
    name.starts_with('.')
        || name.starts_with('~')
        || name.ends_with(".tmp")
        || name.ends_with(".part")
        || name.ends_with(".crdownload")
}

fn walk_read_dir(path: &Path) -> io::Result<ReadDir> {
    std::fs::read_dir(path)
}

fn walk_file_type(entry: &std::fs::DirEntry) -> io::Result<std::fs::FileType> {
    entry.file_type()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, rel: &str) -> PathBuf {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, b"x").unwrap();
        path
    }

    fn files_of(walker: DirectoryWalker) -> Vec<String> {
        walker
            .filter_map(|item| match item {
                WalkItem::File(entry) => Some(entry.relative_path),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn collects_audio_files_with_slash_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "top.mp3");
        touch(dir.path(), "album_a/song.m4a");
        touch(dir.path(), "album_a/deep/song2.flac");
        touch(dir.path(), "notes.txt");
        let mut files = files_of(DirectoryWalker::new(dir.path()));
        files.sort();
        assert_eq!(
            files,
            vec!["album_a/deep/song2.flac", "album_a/song.m4a", "top.mp3"]
        );
    }

    #[test]
    fn skips_hidden_and_temp_entries() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), ".hidden.mp3");
        touch(dir.path(), ".cache/inner.mp3");
        touch(dir.path(), "~draft.mp3");
        touch(dir.path(), "video.mp3.tmp");
        touch(dir.path(), "ok.wav");
        assert_eq!(files_of(DirectoryWalker::new(dir.path())), vec!["ok.wav"]);
    }

    #[test]
    fn unreadable_root_yields_single_error_not_panic() {
        // 指向一个“文件”而不是目录：read_dir 必然失败。
        let dir = tempfile::tempdir().unwrap();
        let file = touch(dir.path(), "not-a-dir");
        let items: Vec<_> = DirectoryWalker::new(&file).collect();
        assert!(matches!(
            items.first(),
            Some(WalkItem::Error { relative_path, .. }) if relative_path.is_empty()
        ));
        assert_eq!(items.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "real.mp3");
        std::os::unix::fs::symlink(dir.path().join("real.mp3"), dir.path().join("link.mp3"))
            .unwrap();
        std::os::unix::fs::symlink(dir.path().join(""), dir.path().join("loopdir")).unwrap();
        let items: Vec<_> = DirectoryWalker::new(dir.path()).collect();
        let skipped: Vec<_> = items
            .iter()
            .filter(|item| matches!(item, WalkItem::SkippedLink { .. }))
            .collect();
        assert_eq!(skipped.len(), 2, "文件与目录链接都应跳过");
        assert_eq!(files_of(DirectoryWalker::new(dir.path())), vec!["real.mp3"]);
    }

    #[test]
    fn entry_carries_size_and_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metered.mp3");
        std::fs::write(&path, b"12345").unwrap();
        let items: Vec<_> = DirectoryWalker::new(dir.path()).collect();
        let WalkItem::File(entry) = &items[0] else {
            panic!("应为文件条目: {items:?}");
        };
        assert_eq!(entry.size_bytes, 5);
        assert!(entry.mtime_ns > 1_600_000_000_000_000_000);
        assert_eq!(entry.absolute, path);
    }
}
