use std::{fs, time::Duration};

use axum::{
    Json, Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
    routing::{get, post},
};
use serde_json::{Value, json};
use tempfile::tempdir;
use tokio::net::TcpListener;
use tower::ServiceExt;

use super::{ai_client::AiClient, router_with_worker};
use crate::auth::{Access, router};

const AI_TOKEN: &str = "fixture-embedding-worker-token-00000000000";
const FILE_TOKEN: &str = "fixture-file-access-token-000000000000000";
const SEARCH: &str = "/arcdos.search.v1.SearchService/SearchFiles";

async fn search(app: &Router, query: &str, authorized: bool) -> (StatusCode, Value) {
    let mut request = Request::post(SEARCH)
        .header(header::CONTENT_TYPE, "application/json")
        .header("connect-protocol-version", "1");
    if authorized {
        request = request.header(header::AUTHORIZATION, format!("Bearer {FILE_TOKEN}"));
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(json!({"query": query}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 64 * 1024).await.unwrap();
    (parts.status, serde_json::from_slice(&bytes).unwrap())
}

fn verify_worker_token(headers: &HeaderMap) {
    assert_eq!(headers[header::AUTHORIZATION], format!("Bearer {AI_TOKEN}"));
}

#[tokio::test]
async fn semantic_results_obey_access_and_revisions_and_keyword_search_survives_outage() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker = AiClient::new(
        &format!("http://{}", listener.local_addr().unwrap()),
        AI_TOKEN,
    )
    .unwrap();
    let fake = Router::new()
        .route(
            "/healthz",
            get(|headers: HeaderMap| async move {
                verify_worker_token(&headers);
                Json(json!({"ready": true, "embedding": {"model": "fixture@1", "dimensions": 2}}))
            }),
        )
        .route(
            "/v1/embed",
            post(|headers: HeaderMap, Json(input): Json<Value>| async move {
                verify_worker_token(&headers);
                let vectors: Vec<_> = input["texts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|text| match text.as_str().unwrap() {
                        "solar array output" | "photovoltaic performance" => [1.0, 0.0],
                        _ => [0.0, 1.0],
                    })
                    .collect();
                Json(json!({"model": "fixture@1", "vectors": vectors}))
            }),
        );
    let service = tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

    let fixture = tempdir().unwrap();
    let data = fixture.path().join("data");
    let web = fixture.path().join("web");
    let token_file = fixture.path().join("token");
    fs::create_dir_all(data.join("files")).unwrap();
    let source = data.join("files/measurements.txt");
    fs::write(&source, "solar array output").unwrap();
    fs::write(&token_file, FILE_TOKEN).unwrap();
    let access = Access::development(&web, &data, &token_file).unwrap();
    let app = router(
        router_with_worker(&web, &data, Some(worker)).unwrap(),
        access,
    );

    let query = "photovoltaic performance";
    assert_eq!(search(&app, query, false).await.0, StatusCode::UNAUTHORIZED);
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let (status, response) = search(&app, query, true).await;
            assert_eq!(status, StatusCode::OK);
            if response["hits"][0]["path"] == "measurements.txt" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    })
    .await
    .expect("expected an indexed semantic-only result");

    fs::write(&source, "unrelated laboratory notebook").unwrap();
    let (status, response) = search(&app, query, true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(response["hits"].as_array().is_none_or(Vec::is_empty));

    service.abort();
    let _ = service.await;
    let (status, response) = search(&app, "measurements", true).await;
    assert_eq!(status, StatusCode::OK);
    // The revised document may need one more observation before it reenters the keyword index.
    if response["hits"][0]["path"] != "measurements.txt" {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let (status, response) = search(&app, "measurements", true).await;
                assert_eq!(status, StatusCode::OK);
                if response["hits"][0]["path"] == "measurements.txt" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(80)).await;
            }
        })
        .await
        .expect("expected a keyword result while the worker is stopped");
    }
}
