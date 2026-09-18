//! Argon2id 口令哈希：参数固定为计划要求的 m=32 MiB、t=3、p=1，
//! 随机盐与参数一起编进 PHC 字符串，因此数据库只需保存一个字段。

use std::sync::OnceLock;

use argon2::password_hash::{Encoding, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};

/// 内存开销（KiB）：32 MiB。不得为了通过内存指标下调，那会等比削弱离线破解成本。
pub const MEMORY_KIB: u32 = 32 * 1024;
pub const TIME_COST: u32 = 3;
pub const PARALLELISM: u32 = 1;
pub const OUTPUT_BYTES: usize = 32;
const SALT_BYTES: usize = 16;

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("随机数生成失败")]
    Random(#[from] getrandom::Error),
    #[error("Argon2 参数不合法")]
    Engine(#[from] argon2::Error),
    #[error("口令哈希串无法解析")]
    Format(#[from] argon2::password_hash::Error),
}

fn engine() -> Result<Argon2<'static>, PasswordError> {
    let params = Params::new(MEMORY_KIB, TIME_COST, PARALLELISM, Some(OUTPUT_BYTES))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

fn random_bytes<const N: usize>() -> Result<[u8; N], PasswordError> {
    let mut buffer = [0u8; N];
    getrandom::fill(&mut buffer)?;
    Ok(buffer)
}

/// 生成带独立随机盐的 Argon2id 哈希串；调用方需放到阻塞线程或专用线程上执行。
pub fn hash_password(plain: &str) -> Result<String, PasswordError> {
    let salt = SaltString::encode_b64(&random_bytes::<SALT_BYTES>()?)?;
    let hash = engine()?.hash_password(plain.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// 校验口令。参数与盐都取自哈希串本身，因此改参数只会让旧哈希校验失败而不会静默降级。
pub fn verify_password(stored: &str, plain: &str) -> Result<bool, PasswordError> {
    let parsed = PasswordHash::parse(stored, Encoding::B64)?;
    let verified = engine()?.verify_password(plain.as_bytes(), &parsed).is_ok();
    Ok(verified)
}

static DUMMY: OnceLock<Option<String>> = OnceLock::new();

/// 用户名不存在时也做一次同参数校验，避免用响应耗时枚举账号。
/// 哑哈希首次调用时在校验线程内生成；返回 `None` 表示内存不足，调用方按普通失败处理。
pub fn dummy_hash() -> Option<&'static str> {
    DUMMY
        .get_or_init(|| {
            let seed = random_bytes::<SALT_BYTES>().ok()?;
            hash_password(&crate::auth::session::encode_hex(&seed)).ok()
        })
        .as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_carries_algorithm_and_parameters_and_rejects_wrong_password() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(hash.starts_with("$argon2id$v=19$"));
        assert!(hash.contains("m=32768,t=3,p=1"));
        assert!(verify_password(&hash, "correct horse battery staple").unwrap());
        assert!(!verify_password(&hash, "wrong password").unwrap());
    }

    #[test]
    fn distinct_salts_produce_distinct_hashes() {
        let first = hash_password("same secret").unwrap();
        let second = hash_password("same secret").unwrap();
        assert_ne!(first, second);
        assert!(verify_password(&second, "same secret").unwrap());
    }

    #[test]
    fn malformed_hash_is_reported_not_verified() {
        assert!(verify_password("$argon2id$", "x").is_err());
    }

    #[test]
    fn dummy_hash_is_stable_and_verifiable() {
        let dummy = dummy_hash().unwrap();
        assert_eq!(Some(dummy), dummy_hash());
        assert!(verify_password(dummy, "not-the-dummy-password").is_ok());
    }
}
