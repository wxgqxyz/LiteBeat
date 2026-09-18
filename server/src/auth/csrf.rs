//! CSRF 与来源边界：修改类请求既要带 `X-CSRF-Token`，也要 Origin 与 Host 一致。
//! 登录请求此刻还没有会话，因此只校验 Origin 与 JSON Content-Type。

use axum::http::{HeaderMap, HeaderName, StatusCode, header};
use axum::response::Response;

use crate::auth::Session;
use crate::auth::session::decode_hex;
use crate::error::json_error;

pub const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");

#[derive(Debug, thiserror::Error)]
pub enum CsrfError {
    #[error("缺少 CSRF 令牌或不匹配")]
    Token,
    #[error("Origin 与 Host 不一致")]
    Origin,
}

impl CsrfError {
    /// 两类失败都回 403，且不带出期望值，避免探测。
    pub fn to_response(&self, request_id: &str) -> Response {
        json_error(
            StatusCode::FORBIDDEN,
            "CSRF_FAILED",
            "请求来源或 CSRF 令牌校验未通过",
            request_id,
        )
    }
}

/// 会话里的 CSRF 令牌必须与请求头十六进制逐字节相等。
pub fn verify_token(session: &Session, headers: &HeaderMap) -> Result<(), CsrfError> {
    let provided = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let decoded = decode_hex(provided).ok_or(CsrfError::Token)?;
    if constant_time_eq(&decoded, &session.csrf_token) {
        Ok(())
    } else {
        Err(CsrfError::Token)
    }
}

/// 无 Origin 的请求视为非浏览器客户端放行；一旦带上就必须与 Host 完全一致，
/// `null` Origin、跨站 Origin 与带路径的 Origin 都拒绝。
pub fn verify_origin(headers: &HeaderMap) -> Result<(), CsrfError> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };
    let origin = origin.to_str().map_err(|_| CsrfError::Origin)?;
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or(CsrfError::Origin)?;
    let Some((scheme, authority)) = origin.split_once("://") else {
        return Err(CsrfError::Origin);
    };
    if !matches!(scheme, "http" | "https") {
        return Err(CsrfError::Origin);
    }
    let mut segments = authority.split('/');
    let authority = segments.next().unwrap_or_default();
    if segments.next().is_some() || !authority.eq_ignore_ascii_case(host) {
        return Err(CsrfError::Origin);
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::session::encode_hex;
    use axum::http::HeaderValue;

    fn session() -> Session {
        Session {
            user_id: 1,
            csrf_token: vec![0xab; 4],
        }
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                HeaderName::from_lowercase(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    #[test]
    fn matching_csrf_header_passes_and_others_fail() {
        let good = headers(&[("x-csrf-token", "abababab")]);
        assert!(verify_token(&session(), &good).is_ok());
        for bad in [
            headers(&[]),
            headers(&[("x-csrf-token", "")]),
            headers(&[("x-csrf-token", "abab")]),
            headers(&[("x-csrf-token", "deadbeef")]),
            headers(&[("x-csrf-token", "not-hex")]),
        ] {
            assert!(matches!(
                verify_token(&session(), &bad),
                Err(CsrfError::Token)
            ));
        }
    }

    #[test]
    fn origin_must_equal_host_when_present() {
        let same_host = headers(&[
            ("origin", "http://localhost:8090"),
            ("host", "localhost:8090"),
        ]);
        assert!(verify_origin(&same_host).is_ok());
        for bad in [
            headers(&[
                ("origin", "http://evil.example"),
                ("host", "localhost:8090"),
            ]),
            headers(&[("origin", "null"), ("host", "localhost:8090")]),
            headers(&[
                ("origin", "http://localhost:8090/path"),
                ("host", "localhost:8090"),
            ]),
            headers(&[("origin", "http://localhost:8090")]),
        ] {
            assert!(matches!(verify_origin(&bad), Err(CsrfError::Origin)));
        }
        assert!(verify_origin(&headers(&[("host", "localhost:8090")])).is_ok());
    }

    #[test]
    fn hex_form_is_case_insensitive_for_tokens() {
        let upper = encode_hex(&[0xabu8; 4]).to_uppercase();
        let with_header = headers(&[("x-csrf-token", upper.as_str())]);
        assert!(verify_token(&session(), &with_header).is_ok());
    }
}
