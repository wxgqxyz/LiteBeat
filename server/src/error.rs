use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
    pub request_id: String,
}

pub fn not_found(request_id: impl Into<String>) -> Response {
    json_error(StatusCode::NOT_FOUND, "NOT_FOUND", "资源不存在", request_id)
}

pub fn json_error(
    status: StatusCode,
    code: &str,
    message: &str,
    request_id: impl Into<String>,
) -> Response {
    (
        status,
        axum::Json(ErrorResponse {
            error: ErrorBody {
                code: code.into(),
                message: message.into(),
                request_id: request_id.into(),
            },
        }),
    )
        .into_response()
}
