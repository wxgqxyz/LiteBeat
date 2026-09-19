//! 不透明游标：base64url(JSON)，携带版本、过滤器摘要与排序键值；
//! 编解码上限 512 字节，非法/串用一律 400，绝不把用户输入拼进 SQL。

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const CURSOR_VERSION: u8 = 1;
pub const MAX_CURSOR_BYTES: usize = 512;

#[derive(Debug, Serialize, Deserialize)]
struct CursorPayload {
    v: u8,
    f: String,
    k: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CursorError {
    #[error("游标格式非法")]
    Malformed,
    #[error("游标版本不受支持")]
    Version,
    #[error("游标与当前过滤器不匹配")]
    FilterMismatch,
    #[error("游标超出长度上限")]
    TooLong,
}

/// 过滤器摘要：对端点与全部过滤参数的规范串取 SHA-256 前 8 字节十六进制。
pub fn filter_digest(spec: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(spec.as_bytes());
    let sum = hasher.finalize();
    crate::auth::session::encode_hex(&sum[..8])
}

pub fn encode(digest: &str, keys: &[String]) -> Result<String, CursorError> {
    let json = serde_json::to_string(&CursorPayload {
        v: CURSOR_VERSION,
        f: digest.to_owned(),
        k: keys.to_vec(),
    })
    .map_err(|_| CursorError::Malformed)?;
    let encoded = URL_SAFE_NO_PAD.encode(json.as_bytes());
    if encoded.len() > MAX_CURSOR_BYTES {
        return Err(CursorError::TooLong);
    }
    Ok(encoded)
}

pub fn decode(raw: &str, expect_digest: &str) -> Result<Vec<String>, CursorError> {
    if raw.len() > MAX_CURSOR_BYTES {
        return Err(CursorError::TooLong);
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| CursorError::Malformed)?;
    // 拒绝未知字段，防止游标被篡改成携带额外键。
    let payload: CursorPayload = match serde_json::from_slice(&bytes) {
        Ok(payload) => payload,
        Err(_) => return Err(CursorError::Malformed),
    };
    if payload.v != CURSOR_VERSION {
        return Err(CursorError::Version);
    }
    if payload.f != expect_digest {
        return Err(CursorError::FilterMismatch);
    }
    if payload.k.is_empty() {
        return Err(CursorError::Malformed);
    }
    Ok(payload.k)
}

#[cfg(test)]
mod tests {
    use super::{decode, encode, filter_digest};

    #[test]
    fn roundtrip_and_filter_mismatch() {
        let digest_a = filter_digest("tracks|artist=周杰伦");
        let digest_b = filter_digest("tracks|artist=林俊杰");
        let cursor = encode(&digest_a, &["晴天".into(), "7".into()]).unwrap();
        let keys = decode(&cursor, &digest_a).unwrap();
        assert_eq!(keys, vec!["晴天".to_string(), "7".to_string()]);
        assert!(matches!(
            decode(&cursor, &digest_b),
            Err(super::CursorError::FilterMismatch)
        ));
    }

    #[test]
    fn garbage_and_oversize_rejected() {
        let digest = filter_digest("x");
        assert!(decode("!!not base64!!", &digest).is_err());
        assert!(decode(&"A".repeat(600), &digest).is_err());
        assert!(encode(&digest, &["y".repeat(600)]).is_err());
    }
}
