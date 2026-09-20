//! T05 受限扫描：惰性遍历、增量判定、有界子进程协议与 admin scans 端点。
//!
//! 分工：
//! - [`walk`]：深度受限的惰性目录迭代（不跟链接、忽略隐藏/临时项）。
//! - [`protocol`]：scan-worker 的有界 JSON 行协议与字段上限。
//! - [`worker`]：子进程两侧——lofty 解析主循环与协调端托管（超时杀死重建、
//!   Windows Job Object / Linux cgroup v2 内存限额）。
//! - [`coordinator`]：单任务协调器、批量入库、扫描端点与重启 interrupted 标记。

pub mod coordinator;
pub mod protocol;
pub mod walk;
pub mod worker;

pub use coordinator::{
    ScanConfig, ScanEnv, mark_interrupted, scanner_router, sync_configured_roots,
};
