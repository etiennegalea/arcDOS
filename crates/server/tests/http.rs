use std::fs;

use arcdos_protocol::proto::arcdos::account::v1::{CheckRequest, CheckResponse};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header::CONTENT_TYPE},
};
use buffa::Message;
use tempfile::tempdir;
use tower::ServiceExt;

const HEALTH_RPC: &str = "/arcdos.account.v1.HealthService/Check";

fn health_request(content_type: &str, body: impl Into<Body>) -> Request<Body> {
    Request::post(HEALTH_RPC)
        .header(CONTENT_TYPE, content_type)
        .header("connect-protocol-version", "1")
        .body(body.into())
        .unwrap()
}

#[tokio::test]
async fn checks_health_over_connect_binary() {
    let web_dir = tempdir().unwrap();
    let response = arcdos_server::app(web_dir.path())
        .oneshot(health_request(
            "application/proto",
            CheckRequest::default().encode_to_vec(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "application/proto");
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert!(CheckResponse::decode_from_slice(&body).unwrap().ready);
}

#[tokio::test]
async fn checks_health_over_connect_json() {
    let web_dir = tempdir().unwrap();
    let response = arcdos_server::app(web_dir.path())
        .oneshot(health_request("application/json", "{}"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "application/json");
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(body, r#"{"ready":true}"#);
}

#[tokio::test]
async fn rejects_malformed_protobuf() {
    let web_dir = tempdir().unwrap();
    let response = arcdos_server::app(web_dir.path())
        .oneshot(health_request("application/proto", &[0x80][..]))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn rejects_oversized_health_requests() {
    let web_dir = tempdir().unwrap();
    let response = arcdos_server::app(web_dir.path())
        .oneshot(health_request("application/proto", vec![0; 4097]))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn serves_the_static_index_and_returns_not_found_for_missing_assets() {
    let web_dir = tempdir().unwrap();
    let index = "<!doctype html><title>ArcDOS</title>";
    fs::write(web_dir.path().join("index.html"), index).unwrap();
    let app = arcdos_server::app(web_dir.path());

    let response = app
        .clone()
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], "text/html");
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), index);

    let response = app
        .oneshot(
            Request::get("/_app/missing.js")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
