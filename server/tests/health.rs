use axum::{
    Router,
    body::to_bytes,
    http::{Method, Request, StatusCode},
};
use std::sync::{Arc, atomic::AtomicBool};
use tower::ServiceExt;

use litebeat::http::{AppState, router};

fn app() -> (Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("index.html"), "<html></html>").unwrap();
    let app = router(AppState {
        web_dir: dir.path().to_path_buf(),
        ready: Arc::new(AtomicBool::new(true)),
        db: None,
        media: Arc::new(litebeat::media::MediaEnv::default()),
        scans: Arc::new(litebeat::scanner::ScanEnv::default()),
    });
    (app, dir)
}

#[tokio::test]
async fn live_health_is_json() {
    let (app, _dir) = app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/health/live")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
}

#[tokio::test]
async fn head_health_has_no_body() {
    let (app, _dir) = app();
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::HEAD)
                .uri("/health/live")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn api_missing_route_does_not_return_html() {
    let (app, _dir) = app();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/v1/missing")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
}
