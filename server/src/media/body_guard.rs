//! 媒体流配额：全局与单会话两层限额，配额由响应体持有到流结束或被丢弃。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaError {
    /// 全局在途响应已满：调用方应回 503 + Retry-After。
    GlobalFull,
    /// 单会话并发流已满：调用方应回 429。
    SessionFull,
}

#[derive(Debug)]
struct State {
    global: usize,
    sessions: HashMap<Vec<u8>, usize>,
}

#[derive(Debug)]
pub struct MediaQuota {
    state: Mutex<State>,
    global_limit: usize,
    session_limit: usize,
}

/// RAII 守卫：Drop 即归还全局与会话名额，与响应体生死绑定。
#[derive(Debug)]
pub struct StreamGuard {
    quota: Arc<MediaQuota>,
    session_key: Vec<u8>,
}

impl MediaQuota {
    pub fn new(global_limit: usize, session_limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                global: 0,
                sessions: HashMap::new(),
            }),
            global_limit,
            session_limit,
        }
    }

    pub fn try_acquire(self: &Arc<Self>, session_key: Vec<u8>) -> Result<StreamGuard, QuotaError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.global >= self.global_limit {
            return Err(QuotaError::GlobalFull);
        }
        let session = state.sessions.entry(session_key.clone()).or_insert(0);
        if *session >= self.session_limit {
            if *session == 0 {
                state.sessions.remove(&session_key);
            }
            return Err(QuotaError::SessionFull);
        }
        *session += 1;
        state.global += 1;
        Ok(StreamGuard {
            quota: Arc::clone(self),
            session_key,
        })
    }

    fn release(&self, session_key: &[u8]) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.global = state.global.saturating_sub(1);
        if let Some(count) = state.sessions.get_mut(session_key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                state.sessions.remove(session_key);
            }
        }
    }

    /// 当前在途流数量；测试与运维观测用。
    pub fn in_flight(&self) -> usize {
        self.state.lock().unwrap().global
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.quota.release(&self.session_key);
    }
}
