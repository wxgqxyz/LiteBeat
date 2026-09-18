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
    (
        StatusCode::NOT_FOUND,
        axum::Json(ErrorResponse {
            error: ErrorBody {
                code: "NOT_FOUND".into(),
                message: "资源不存在".into(),
                request_id: request_id.into(),
            },
        }),
    )
        .into_response()
}
