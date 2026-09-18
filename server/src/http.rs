use std::path::PathBuf;

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

use crate::error;

#[derive(Clone)]
pub struct AppState {
    pub web_dir: PathBuf,
    pub ready: bool,
}

pub fn router(state: AppState) -> Router {
    let assets_dir = state.web_dir.join("assets");
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route_service("/", ServeFile::new(state.web_dir.join("index.html")))
        .nest_service("/assets", ServeDir::new(assets_dir))
        .fallback(fallback)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn live() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"status":"ok"}"#,
    )
}

async fn ready(State(state): State<AppState>) -> Response {
    if state.ready {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"ready"}"#,
        )
            .into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "application/json")],
            r#"{"status":"not_ready"}"#,
        )
            .into_response()
    }
}

async fn fallback() -> Response {
    error::not_found("unassigned")
}
