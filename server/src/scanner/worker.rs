//! 扫描子进程两端：`litebeat scan-worker` 的解析主循环（stdin/stdout JSON 行），
//! 以及协调端的托管结构 [`ParseWorker`]（拉起、逐文件超时、杀死重建、平台资源限额）。
//!
//! 契约（架构文档 §5.1 第 4–5 步、§3.2 扫描行）：
//! - 同时只有一个子进程、一个在途文件；解析标准模式只读标签与属性，不解码音频、不存封面。
//! - 单文件超时（默认 5s）即杀死进程并重建，记当前文件失败后继续下一文件。
//! - Windows 用 Job Object 限制子进程提交内存；Linux 用 cgroup v2 子组限制 memory.max；
//!   限额能力不可用时如实上报 `unavailable`，不谎称完成隔离。

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::timeout;

use crate::scanner::protocol::{
    DEBUG_ENV, FileMetadata, MAX_REQUEST_LINE_BYTES, MAX_RESPONSE_LINE_BYTES, ScanResponse,
    WorkerRequest, write_bounded_line,
};

/// 协调端拉起 worker 所需的固定配置；`program` 默认取当前可执行文件。
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub program: PathBuf,
    pub memory_limit_bytes: u64,
    /// 允许子进程响应调试指令（仅测试注入）。
    pub debug: bool,
}

impl WorkerConfig {
    pub fn for_current_exe() -> Self {
        Self {
            program: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("litebeat")),
            memory_limit_bytes: 96 * 1024 * 1024,
            debug: false,
        }
    }
}

/// 子进程侧故障类别；任何一类都要求杀死并重建。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseFailure {
    /// 超过单文件时限（含读写阶段整体超时）。
    Timeout,
    /// 越界行、非法 JSON 等协议违规。
    Protocol,
    /// 进程崩溃或管道断裂。
    Crashed,
}

impl ParseFailure {
    pub fn error_code(self) -> &'static str {
        match self {
            Self::Timeout => "PARSE_TIMEOUT",
            Self::Protocol => "WORKER_PROTOCOL_VIOLATION",
            Self::Crashed => "WORKER_CRASHED",
        }
    }
}

pub struct ParseWorker {
    child: Child,
    stdin: ChildStdin,
    lines: BufReader<ChildStdout>,
    isolation: Option<IsolationGuard>,
}

impl ParseWorker {
    pub async fn spawn(config: &WorkerConfig) -> io::Result<Self> {
        let mut command = tokio::process::Command::new(&config.program);
        command
            .arg("scan-worker")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            // 协调器异常退出也不能留下孤儿进程。
            .kill_on_drop(true);
        if config.debug {
            command.env(DEBUG_ENV, "1");
        } else {
            command.env_remove(DEBUG_ENV);
        }
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let isolation = child
            .id()
            .and_then(|pid| attach_isolation(pid, config.memory_limit_bytes));
        Ok(Self {
            child,
            stdin,
            lines: BufReader::new(stdout),
            isolation,
        })
    }

    /// 资源限额是否真正生效；供状态端点如实上报。
    pub fn isolation_active(&self) -> bool {
        self.isolation.is_some()
    }

    /// 处理一个文件；超时或协议失败时本进程已被终止，调用方须重建。
    pub async fn parse(
        &mut self,
        path: &Path,
        limit: Duration,
    ) -> Result<ScanResponse, ParseFailure> {
        self.parse_with_debug(path, limit, None).await
    }

    /// 带调试指令的解析（仅测试注入；子进程需以 debug 配置拉起）。
    pub async fn parse_with_debug(
        &mut self,
        path: &Path,
        limit: Duration,
        debug: Option<crate::scanner::protocol::DebugDirective>,
    ) -> Result<ScanResponse, ParseFailure> {
        let request = WorkerRequest {
            path: path.to_string_lossy().into_owned(),
            debug,
        };
        let text = match serde_json::to_string(&request) {
            Ok(text) if text.len() <= MAX_REQUEST_LINE_BYTES => text,
            _ => {
                self.kill().await;
                return Err(ParseFailure::Protocol);
            }
        };
        match timeout(limit, self.exchange(&text)).await {
            Err(_elapsed) => {
                self.kill().await;
                Err(ParseFailure::Timeout)
            }
            Ok(Err(failure)) => {
                self.kill().await;
                Err(failure)
            }
            Ok(Ok(response)) => Ok(response),
        }
    }

    async fn exchange(&mut self, request_line: &str) -> Result<ScanResponse, ParseFailure> {
        if self.stdin.write_all(request_line.as_bytes()).await.is_err()
            || self.stdin.write_all(b"\n").await.is_err()
            || self.stdin.flush().await.is_err()
        {
            return Err(ParseFailure::Crashed);
        }
        let mut buffer = Vec::with_capacity(1024);
        match self.lines.read_until(b'\n', &mut buffer).await {
            Err(_) => return Err(ParseFailure::Crashed),
            Ok(0) => return Err(ParseFailure::Crashed),
            Ok(_) => {}
        }
        if buffer.last() != Some(&b'\n') || buffer.len() - 1 > MAX_RESPONSE_LINE_BYTES {
            return Err(ParseFailure::Protocol);
        }
        buffer.pop();
        let line = String::from_utf8(buffer).map_err(|_| ParseFailure::Protocol)?;
        serde_json::from_str(&line).map_err(|_| ParseFailure::Protocol)
    }

    async fn kill(&mut self) {
        let _ = self.child.start_kill();
        // 不阻塞 reap：kill_on_drop 兜底，这里只尽力回收。
        match timeout(Duration::from_millis(500), self.child.wait()).await {
            Ok(Ok(_)) | Ok(Err(_)) => {}
            Err(_) => tracing::warn!("扫描子进程未在 500ms 内退出，依赖 kill_on_drop 收尾"),
        }
    }

    /// 正常收尾：尽力优雅退出，最终强杀兜底（幂等）。
    pub async fn shutdown(mut self) {
        self.kill().await;
    }
}

impl std::fmt::Debug for ParseWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParseWorker")
            .field("pid", &self.child.id())
            .field("isolation", &self.isolation.is_some())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// 子进程侧
// ---------------------------------------------------------------------------

/// `litebeat scan-worker` 入口：stdin 读请求、stdout 写响应，直到 EOF。
pub fn worker_main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();
    let debug_enabled = std::env::var_os(DEBUG_ENV).is_some();
    loop {
        let line = match crate::scanner::protocol::read_bounded_line(
            &mut reader,
            MAX_REQUEST_LINE_BYTES,
        ) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(_) => {
                // 输入越界或非法 UTF-8：按协议违规直接退出，让协调端重建。
                return;
            }
        };
        let request: WorkerRequest = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(_) => {
                let response = ScanResponse::failure("INVALID_REQUEST");
                if write_bounded_line(&mut writer, &response).is_err() {
                    return;
                }
                continue;
            }
        };
        if let Some(debug) = request.debug.filter(|_| debug_enabled) {
            match debug {
                crate::scanner::protocol::DebugDirective::SleepMs(ms) => {
                    // 睡够再继续正常解析：验证“超时被杀 → 重建 → 后续文件照常”。
                    std::thread::sleep(Duration::from_millis(ms));
                }
                crate::scanner::protocol::DebugDirective::Crash => std::process::exit(7),
                crate::scanner::protocol::DebugDirective::HugeOutput => {
                    let junk = "x".repeat(MAX_RESPONSE_LINE_BYTES * 2);
                    let _ = writer
                        .write_all(junk.as_bytes())
                        .and_then(|()| writer.write_all(b"\n"))
                        .and_then(|()| writer.flush());
                    return;
                }
            }
        }
        let response = parse_file(Path::new(&request.path));
        if write_bounded_line(&mut writer, &response).is_err() {
            return;
        }
    }
}

/// 标准解析：只读标签与属性，不读取封面图片。
pub fn parse_file(path: &Path) -> ScanResponse {
    use lofty::config::{ParseOptions, ParsingMode};
    use lofty::file::{AudioFile as _, TaggedFileExt as _};
    use lofty::probe::Probe;
    use lofty::tag::ItemKey;

    let options = ParseOptions::new()
        .read_properties(true)
        .read_tags(true)
        .read_cover_art(false)
        .parsing_mode(ParsingMode::Strict);
    let tagged = match Probe::open(path).and_then(|probe| probe.options(options).read()) {
        Ok(tagged) => tagged,
        Err(_) => return ScanResponse::failure("PARSE_FAILED"),
    };
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let text = |key: ItemKey| -> String {
        tag.and_then(|tag| tag.get_string(key))
            .unwrap_or_default()
            .to_owned()
    };
    let number = |key: ItemKey| -> i64 {
        tag.and_then(|tag| tag.get_string(key))
            .unwrap_or_default()
            .split(['/', ' '])
            .next()
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0)
    };
    let title = text(ItemKey::TrackTitle);
    let fallback = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("未知曲目"));
    let (codec, mime) = codec_and_mime(path);
    let duration_ms = tagged.properties().duration().as_millis() as i64;
    ScanResponse::parsed(FileMetadata {
        title: if title.is_empty() { fallback } else { title },
        artist: text(ItemKey::TrackArtist),
        album_title: text(ItemKey::AlbumTitle),
        album_artist: text(ItemKey::AlbumArtist),
        duration_ms: Some(duration_ms),
        codec: Some(codec),
        mime: Some(mime),
        disc_no: number(ItemKey::DiscNumber),
        track_no: number(ItemKey::TrackNumber),
        truncated: false,
    })
}

fn codec_and_mime(path: &Path) -> (String, String) {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mime = match extension.as_str() {
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    };
    (extension, mime.to_owned())
}

// ---------------------------------------------------------------------------
// 平台资源限额
// ---------------------------------------------------------------------------

#[cfg(windows)]
mod isolation_impl {
    //! Job Object：限制子进程提交内存并在句柄关闭时连带杀死，防止孤儿。
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOB_OBJECT_LIMIT_PROCESS_MEMORY, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    pub const MODE: &str = "windows-job-object";

    pub struct IsolationGuard {
        job: HANDLE,
    }

    // SAFETY: 句柄只是内核对象引用，跨线程移动不改变其有效性；
    // 关闭只发生在 Drop 中，且 guard 全程与子进程一一对应。
    unsafe impl Send for IsolationGuard {}

    impl Drop for IsolationGuard {
        fn drop(&mut self) {
            unsafe {
                // KILL_ON_JOB_CLOSE 保证即使主进程崩溃，子进程也会被回收。
                CloseHandle(self.job);
            }
        }
    }

    pub fn attach(pid: u32, memory_limit_bytes: u64) -> Option<IsolationGuard> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_PROCESS_MEMORY | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            info.ProcessMemoryLimit = memory_limit_bytes as usize;
            let applied = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if applied == 0 {
                CloseHandle(job);
                return None;
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                CloseHandle(job);
                return None;
            }
            let assigned = AssignProcessToJobObject(job, process);
            CloseHandle(process);
            if assigned == 0 {
                CloseHandle(job);
                return None;
            }
            Some(IsolationGuard { job })
        }
    }
}

#[cfg(target_os = "linux")]
mod isolation_impl {
    //! cgroup v2 子组：memory.max 限额；能力不可用时返回 None 并如实上报。
    use std::path::PathBuf;

    pub const MODE: &str = "linux-cgroup-v2";

    pub struct IsolationGuard {
        dir: PathBuf,
    }

    impl Drop for IsolationGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir(&self.dir);
        }
    }

    pub fn attach(pid: u32, memory_limit_bytes: u64) -> Option<IsolationGuard> {
        if !PathBuf::from("/sys/fs/cgroup/cgroup.controllers").exists() {
            return None;
        }
        let dir = PathBuf::from(format!("/sys/fs/cgroup/litebeat-scan-{pid}"));
        std::fs::create_dir(&dir).ok()?;
        if std::fs::write(dir.join("memory.max"), memory_limit_bytes.to_string()).is_err()
            || std::fs::write(dir.join("cgroup.procs"), pid.to_string()).is_err()
        {
            let _ = std::fs::remove_dir(&dir);
            return None;
        }
        Some(IsolationGuard { dir })
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod isolation_impl {
    pub const MODE: &str = "unavailable";

    pub struct IsolationGuard;

    pub fn attach(_pid: u32, _memory_limit_bytes: u64) -> Option<IsolationGuard> {
        None
    }
}

pub use isolation_impl::IsolationGuard;
pub use isolation_impl::MODE as ISOLATION_MODE;
use isolation_impl::attach as attach_isolation;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mp3_without_tags_falls_back_to_file_stem() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixtures/scanner/no_tags.mp3");
        if !path.exists() {
            // 生成脚本尚未运行时跳过（集成测试前必须先跑 scripts/gen_scanner_fixtures.js）。
            return;
        }
        let response = parse_file(&path);
        assert!(response.ok, "无标签 mp3 应解析成功: {:?}", response.code);
        let meta = response.meta.unwrap();
        assert_eq!(meta.title, "no_tags");
        assert!(meta.duration_ms.unwrap() > 900);
        assert_eq!(meta.mime.as_deref(), Some("audio/mpeg"));
    }

    #[test]
    fn garbage_file_reports_parse_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.mp3");
        std::fs::write(&path, [0u8; 512]).unwrap();
        let response = parse_file(&path);
        assert!(!response.ok);
        assert_eq!(response.code.as_deref(), Some("PARSE_FAILED"));
    }
}
