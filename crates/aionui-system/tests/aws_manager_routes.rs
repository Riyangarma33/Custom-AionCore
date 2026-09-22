//! Integration tests for AWS CLI manager routes.

use std::sync::Arc;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tempfile::tempdir;
use tower::ServiceExt;

use aionui_auth::CurrentUser;
use aionui_db::{UserStatus, UserType};
use aionui_system::{AwsManagerService, AwsRouterState, aws_routes};

fn setup_test_app() -> (axum::Router, tempfile::TempDir) {
    let tmp = tempdir().unwrap();
    let service = Arc::new(AwsManagerService::new(tmp.path().to_path_buf()));
    let state = AwsRouterState { service };
    let app = aws_routes(state);
    (app, tmp)
}

fn authed_request(method: &str, uri: &str, body: Body) -> Request<Body> {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body)
        .unwrap();

    req.extensions_mut().insert(CurrentUser {
        id: "admin".to_string(),
        username: "admin".to_string(),
        user_type: UserType::Local,
        status: UserStatus::Active,
    });
    req
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn test_get_runtime_info_returns_ok() {
    let (app, _tmp) = setup_test_app();
    let req = authed_request("GET", "/api/aws/runtime", Body::empty());
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"]["config_path"].as_str().is_some());
    assert!(json["data"]["user"].as_str().is_some());
}

#[tokio::test]
async fn test_list_profiles_returns_ok() {
    let (app, _tmp) = setup_test_app();
    let req = authed_request("GET", "/api/aws/profiles", Body::empty());
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn test_test_identity_rejects_empty_profile() {
    let (app, _tmp) = setup_test_app();
    let req = authed_request(
        "POST",
        "/api/aws/profiles/test-identity",
        Body::from(r#"{"profile": ""}"#),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_sso_sessions_list_returns_ok() {
    let (app, _tmp) = setup_test_app();
    let req = authed_request("GET", "/api/aws/sso-sessions", Body::empty());
    let resp = app.oneshot(req).await.unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["success"], true);
    assert!(json["data"].is_array());
}

#[tokio::test]
async fn test_sso_sessions_save_rejects_empty_name() {
    let (app, _tmp) = setup_test_app();
    let req = authed_request(
        "POST",
        "/api/aws/sso-sessions",
        Body::from(r#"{"name": "", "sso_start_url": "https://test.awsapps.com/start", "sso_region": "us-east-1"}"#),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
