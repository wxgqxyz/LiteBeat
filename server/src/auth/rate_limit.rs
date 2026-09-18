//! 登录防护：限流与 Argon2 校验闸门都在真正哈希之前完成判定，
//! 限流键表规模与在途校验数量都有硬上限，避免登录洪水把 32 MiB 级别的哈希
//! 放大成内存峰值，也避免防护表自身无限增长。

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use crate::auth::password::{self, PasswordError};

/// 固定窗口计数策略。
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub max: u32,
    pub window: Duration,
}

/// 每来源 5 次/分钟。
pub const IP_POLICY: Policy = Policy {
    max: 5,
    window: Duration::from_secs(60),
};
/// 每账号 10 次/10 分钟。
pub const USERNAME_POLICY: Policy = Policy {
    max: 10,
    window: Duration::from_secs(600),
};
/// 限流键表上限；超出后先按窗口过期清理，仍超限时淘汰窗口最旧的键。
pub const MAX_KEYS: usize = 1024;
/// 在途口令校验上限：1 个正在执行 + 2 个排队。
pub const VERIFY_INFLIGHT: usize = 3;

#[derive(Debug, thiserror::Error)]
#[error("登录尝试过于频繁，请稍后重试")]
pub struct RateLimited {
    pub retry_after: Duration,
}

impl RateLimited {
    /// `Retry-After` 按协议要求是整秒，不足一秒的余量向上取整。
    pub fn retry_after_seconds(&self) -> u64 {
        let micros = self.retry_after.as_micros();
        u64::try_from(micros.div_ceil(1_000_000)).unwrap_or(u64::MAX)
    }
}

#[derive(Debug)]
struct Windowed {
    policy: Policy,
    hits: HashMap<String, (Instant, u32)>,
}

impl Windowed {
    fn new(policy: Policy) -> Self {
        Self {
            policy,
            hits: HashMap::new(),
        }
    }

    /// 该键当前窗口是否仍在生效。
    fn fresh(&self, started: Instant, now: Instant) -> bool {
        now.checked_duration_since(started)
            .is_some_and(|elapsed| elapsed < self.policy.window)
    }

    /// 返回 `Some(剩余窗口)` 表示该键已被限流。
    fn blocked_for(&self, key: &str, now: Instant) -> Option<Duration> {
        let (started, count) = *self.hits.get(key)?;
        if self.fresh(started, now) && count >= self.policy.max {
            let elapsed = now.duration_since(started);
            Some(self.policy.window - elapsed)
        } else {
            None
        }
    }

    fn record(&mut self, key: &str, now: Instant) {
        let window = self.policy.window;
        match self.hits.get_mut(key) {
            Some((started, count))
                if now
                    .checked_duration_since(*started)
                    .is_some_and(|elapsed| elapsed < window) =>
            {
                *count += 1;
            }
            _ => {
                self.hits.insert(key.to_string(), (now, 1));
            }
        }
    }

    /// TTL 清理 + 有界淘汰：键表长度始终不超过 `MAX_KEYS`。
    fn evict(&mut self, now: Instant) {
        let window = self.policy.window;
        self.hits
            .retain(|_, (started, _)| now.checked_duration_since(*started) < Some(window));
        while self.hits.len() >= MAX_KEYS {
            let oldest = self
                .hits
                .iter()
                .min_by_key(|(_, (started, _))| *started)
                .map(|(key, _)| key.clone());
            let Some(key) = oldest else { break };
            self.hits.remove(&key);
        }
    }
}

#[derive(Debug)]
struct Counters {
    ips: Windowed,
    usernames: Windowed,
}

/// 登录限流状态。键来自请求头，因此必须限制长度并由 `MAX_KEYS` 兜底。
#[derive(Debug)]
pub struct LoginRateLimiter {
    inner: Mutex<Counters>,
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginRateLimiter {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Counters {
                ips: Windowed::new(IP_POLICY),
                usernames: Windowed::new(USERNAME_POLICY),
            }),
        }
    }

    /// 判定并记账；任一维度已限流时两个维度都不记账，
    /// 这样单个洪水来源不会顺带耗尽合法账号的配额。
    pub fn check(&self, ip_key: &str, username_key: &str, now: Instant) -> Result<(), RateLimited> {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        guard.ips.evict(now);
        guard.usernames.evict(now);
        let blocked = guard
            .ips
            .blocked_for(ip_key, now)
            .or_else(|| guard.usernames.blocked_for(username_key, now));
        if let Some(retry_after) = blocked {
            return Err(RateLimited { retry_after });
        }
        guard.ips.record(ip_key, now);
        guard.usernames.record(username_key, now);
        Ok(())
    }
}

type VerifyJob = Box<dyn FnOnce() -> Result<bool, PasswordError> + Send>;

struct Job {
    work: VerifyJob,
    reply: mpsc::Sender<Result<bool, PasswordError>>,
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("口令校验排队已满")]
    Busy,
    #[error("口令校验线程不可用")]
    Unavailable,
    #[error("口令校验结果丢失")]
    Dropped,
    #[error(transparent)]
    Password(#[from] PasswordError),
}

/// 单线程口令校验池：Argon2 串行执行，排队位用尽时立即拒绝而不是继续占内存。
pub struct VerifyPool {
    tx: Arc<Option<SyncSender<Job>>>,
}

impl Clone for VerifyPool {
    fn clone(&self) -> Self {
        Self {
            tx: Arc::clone(&self.tx),
        }
    }
}

impl Default for VerifyPool {
    fn default() -> Self {
        Self::new()
    }
}

impl VerifyPool {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<Job>(VERIFY_INFLIGHT);
        let started = std::thread::Builder::new()
            .name("argon2-verify".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let result = (job.work)();
                    let _ = job.reply.send(result);
                }
            })
            .is_ok();
        if started {
            Self {
                tx: Arc::new(Some(tx)),
            }
        } else {
            // 线程起不来时通道必须关闭，否则调用方会永远等不到结果。
            drop(tx);
            Self { tx: Arc::new(None) }
        }
    }

    fn submit(
        &self,
        work: VerifyJob,
    ) -> Result<Receiver<Result<bool, PasswordError>>, VerifyError> {
        let Some(tx) = self.tx.as_ref().as_ref() else {
            return Err(VerifyError::Unavailable);
        };
        let (reply_tx, reply_rx) = mpsc::channel();
        match tx.try_send(Job {
            work,
            reply: reply_tx,
        }) {
            Ok(()) => Ok(reply_rx),
            Err(TrySendError::Full(job)) => {
                drop(job);
                Err(VerifyError::Busy)
            }
            Err(TrySendError::Disconnected(_)) => Err(VerifyError::Unavailable),
        }
    }

    /// 执行 Argon2 校验。调用方在得到 `Busy` 时不得已经读过口令哈希。
    pub async fn verify(&self, stored: String, plain: String) -> Result<bool, VerifyError> {
        let reply = self.submit(Box::new(move || password::verify_password(&stored, &plain)))?;
        // 等待结果占用一个阻塞线程；在途作业已被 try_send 限制在 VERIFY_INFLIGHT 内。
        let verified = tokio::task::spawn_blocking(move || reply.recv())
            .await
            .map_err(|_| VerifyError::Dropped)?
            .map_err(|_| VerifyError::Dropped)??;
        Ok(verified)
    }
}

/// 单个服务实例的登录防护状态，经 `Extension` 注入 auth 路由。
pub struct AuthLimits {
    pub logins: LoginRateLimiter,
    pub verifies: VerifyPool,
}

impl AuthLimits {
    pub fn new() -> Self {
        Self {
            logins: LoginRateLimiter::new(),
            verifies: VerifyPool::new(),
        }
    }
}

impl Default for AuthLimits {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Instant {
        Instant::now()
    }

    #[test]
    fn ip_policy_allows_five_per_window_then_resets() {
        let limiter = LoginRateLimiter::new();
        let now = base();
        for attempt in 1..=5 {
            assert!(
                limiter
                    .check("ip-a", &format!("user{attempt}"), now)
                    .is_ok(),
                "第 {attempt} 次应放行"
            );
        }
        let limited = limiter.check("ip-a", "user6", now).unwrap_err();
        assert!(limited.retry_after_seconds() <= 60);
        // 同账号不同来源互不影响。
        assert!(limiter.check("ip-b", "someone", now).is_ok());
        // 窗口过后恢复。
        let later = now + IP_POLICY.window;
        assert!(limiter.check("ip-a", "user7", later).is_ok());
    }

    #[test]
    fn username_policy_is_ten_per_ten_minutes() {
        let limiter = LoginRateLimiter::new();
        let now = base();
        for attempt in 1..=USERNAME_POLICY.max {
            assert!(
                limiter
                    .check(&format!("ip{attempt}"), "victim", now)
                    .is_ok()
            );
        }
        assert!(limiter.check("ip-fresh", "victim", now).is_err());
        assert!(limiter.check("ip-fresh", "other", now).is_ok());
        assert!(
            limiter
                .check("ip-fresh", "victim", now + USERNAME_POLICY.window)
                .is_ok()
        );
    }

    #[test]
    fn key_table_stays_bounded_under_flood() {
        let limiter = LoginRateLimiter::new();
        let now = base();
        for attempt in 0..(MAX_KEYS * 3) {
            // 来源与账号同时变化，两个键表都被压满。
            let _ = limiter.check(&format!("ip-{attempt}"), &format!("user-{attempt}"), now);
        }
        let guard = limiter.inner.lock().unwrap();
        assert!(guard.ips.hits.len() <= MAX_KEYS);
        assert!(guard.usernames.hits.len() <= MAX_KEYS);
    }

    #[test]
    fn expired_keys_are_evicted_before_reinsert() {
        let limiter = LoginRateLimiter::new();
        let now = base();
        for attempt in 0..MAX_KEYS {
            let _ = limiter.check(&format!("old-{attempt}"), &format!("user-{attempt}"), now);
        }
        let later = now + IP_POLICY.window;
        assert!(limiter.check("brand-new", "u2", later).is_ok());
        let guard = limiter.inner.lock().unwrap();
        assert!(guard.ips.hits.len() <= MAX_KEYS);
        assert!(guard.ips.hits.contains_key("brand-new"));
        assert!(!guard.ips.hits.contains_key("old-0"));
    }

    #[test]
    fn verify_pool_admits_one_running_and_two_queued() {
        let pool = VerifyPool::new();
        let mut gates = Vec::new();
        let mut replies = Vec::new();
        for _ in 0..VERIFY_INFLIGHT {
            let (gate_tx, gate_rx) = mpsc::channel::<()>();
            let reply = pool
                .submit(Box::new(move || {
                    let _ = gate_rx.recv();
                    Ok(true)
                }))
                .expect("在途上限内应放行");
            gates.push(gate_tx);
            replies.push(reply);
        }
        let overflow = pool.submit(Box::new(|| Ok(true)));
        assert!(matches!(overflow, Err(VerifyError::Busy)));
        drop(gates);
        for reply in replies {
            assert!(matches!(reply.recv().unwrap(), Ok(true)));
        }
    }

    #[test]
    fn verify_pool_without_worker_is_unavailable_not_hanging() {
        let (tx, rx) = mpsc::sync_channel::<Job>(VERIFY_INFLIGHT);
        drop(rx);
        let pool = VerifyPool {
            tx: Arc::new(Some(tx)),
        };
        assert!(matches!(
            pool.submit(Box::new(|| Ok(true))),
            Err(VerifyError::Unavailable)
        ));
    }
}
