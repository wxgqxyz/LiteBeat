//! 会话签发与撤销：32 字节随机令牌的十六进制串只经 `Set-Cookie` 出网一次，
//! 库里只存 `SHA-256(十六进制串)`；`expires_at` 由 SQLite 计算，避免应用与库时钟口径漂移。

use std::sync::OnceLock;

use crate::auth::{SESSION_COOKIE, hash_token};
use crate::db::{Db, DbError, Value};

/// 会话绝对有效期，与 `Max-Age` 一致：7 天。
pub const MAX_AGE_SECONDS: i64 = 7 * 24 * 60 * 60;
pub const MAX_ACTIVE_SESSIONS: usize = 64;
/// 令牌与 CSRF 种子的随机字节数，十六进制后长度为 64 字符。
pub const TOKEN_BYTES: usize = 32;
/// 打开 HTTPS 部署时显式设置该变量，让 Cookie 带上 `Secure`。
pub const COOKIE_SECURE_ENV: &str = "LB_AUTH_COOKIE_SECURE";
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("随机数生成失败")]
    Random(#[from] getrandom::Error),
    #[error(transparent)]
    Database(#[from] DbError),
}

/// 新会话：`raw_token` 只进 Cookie，`csrf_token` 只经 `/auth/session` 回给页面。
#[derive(Debug, Clone)]
pub struct Issued {
    pub raw_token: String,
    pub csrf_token: Vec<u8>,
}

pub fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        out.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }
    out
}

/// 严格解析小写或大写十六进制；长度为奇数或含非十六进制字符返回 `None`。
pub fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let text = text.trim();
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let hi = (pair[0] as char).to_digit(16)?;
            let lo = (pair[1] as char).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

fn random_bytes(len: usize) -> Result<Vec<u8>, getrandom::Error> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes)?;
    Ok(bytes)
}

/// 十六进制小写令牌，与 `auth::hash_token` 的入参口径一致。
pub fn random_hex(len_bytes: usize) -> Result<String, getrandom::Error> {
    Ok(encode_hex(&random_bytes(len_bytes)?))
}

/// `Secure` 位默认关闭以保证本机 HTTP 可登录；HTTPS 部署通过环境变量显式打开。
pub fn secure_cookie() -> bool {
    static SECURE: OnceLock<bool> = OnceLock::new();
    *SECURE.get_or_init(|| {
        std::env::var(COOKIE_SECURE_ENV)
            .ok()
            .is_some_and(|value| matches!(value.as_str(), "1" | "true" | "yes"))
    })
}

fn cookie(pair: String, max_age: i64) -> String {
    let secure = if secure_cookie() { "; Secure" } else { "" };
    format!("{pair}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}")
}

/// 建立会话 Cookie（不含 `Secure` 时本机 HTTP 仍可用）。
pub fn session_cookie(raw_token: &str) -> String {
    cookie(format!("{SESSION_COOKIE}={raw_token}"), MAX_AGE_SECONDS)
}

/// 清除会话 Cookie：立即过期且不带令牌值。
pub fn cleared_cookie() -> String {
    cookie(format!("{SESSION_COOKIE}="), 0)
}

/// 签发会话：先删到期行，再把活跃会话裁到上限内，最后插入新行；
/// 裁剪与插入放在同一事务里，避免并发登录把会话数留在上限之外。
pub async fn issue(db: &Db, user_id: i64) -> Result<Issued, SessionError> {
    let raw_token = random_hex(TOKEN_BYTES)?;
    let csrf_token = random_bytes(TOKEN_BYTES)?;
    db.execute(
        "DELETE FROM sessions WHERE expires_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        vec![],
    )
    .await?;
    db.transaction(vec![
        (
            "DELETE FROM sessions WHERE token_hash NOT IN
             (SELECT token_hash FROM sessions ORDER BY expires_at DESC LIMIT ?)"
                .to_string(),
            vec![Value::Integer(
                i64::try_from(MAX_ACTIVE_SESSIONS - 1).unwrap_or(i64::MAX),
            )],
        ),
        (
            "INSERT INTO sessions(token_hash, user_id, csrf_token, expires_at)
             VALUES (?, ?, ?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+7 days'))"
                .to_string(),
            vec![
                Value::Blob(hash_token(&raw_token)),
                Value::Integer(user_id),
                Value::Blob(csrf_token.clone()),
            ],
        ),
    ])
    .await?;
    Ok(Issued {
        raw_token,
        csrf_token,
    })
}

/// 撤销会话；行不存在也视为成功，调用方无论如何都要清除 Cookie。
pub async fn revoke(db: &Db, raw_token: &str) -> Result<u64, DbError> {
    let reply = db
        .execute(
            "DELETE FROM sessions WHERE token_hash = ?",
            vec![Value::Blob(hash_token(raw_token))],
        )
        .await?;
    Ok(reply.affected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip_is_lowercase_and_strict() {
        let bytes = vec![0u8, 15, 171, 255];
        let text = encode_hex(&bytes);
        assert_eq!(text, "000fabff");
        assert_eq!(decode_hex(&text.to_uppercase()), Some(bytes.clone()));
        assert_eq!(decode_hex("00 0b"), None);
        assert_eq!(decode_hex("0g"), None);
        assert_eq!(decode_hex("abc"), None);
        assert_eq!(decode_hex(""), Some(Vec::new()));
    }

    #[test]
    fn tokens_are_lowercase_hex_and_unpredictable() {
        let first = random_hex(TOKEN_BYTES).unwrap();
        let second = random_hex(TOKEN_BYTES).unwrap();
        assert_eq!(first.len(), TOKEN_BYTES * 2);
        assert_eq!(first, first.to_lowercase());
        assert_ne!(first, second);
        assert_eq!(
            decode_hex(&first).map(|bytes| bytes.len()),
            Some(TOKEN_BYTES)
        );
    }

    #[test]
    fn cookie_attributes_match_contract_without_secure_by_default() {
        let value = session_cookie("deadbeef");
        assert!(value.starts_with("lb_session=deadbeef; "));
        assert!(value.contains("HttpOnly"));
        assert!(value.contains("SameSite=Lax"));
        assert!(value.contains("Path=/"));
        assert!(value.contains("Max-Age=604800"));
        assert!(!value.contains("Secure"));
        assert_eq!(
            cleared_cookie(),
            "lb_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"
        );
    }
}
