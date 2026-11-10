#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::symlink, path::PathBuf};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::{TempDir, tempdir};
use tower::ServiceExt;

const TOKEN: &str = "arcDOS-test-key-000000000000000000000000";

struct Fixture {
    _root: TempDir,
    data: PathBuf,
    web: PathBuf,
    token: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let data = root.path().join("data");
        let web = root.path().join("web");
        let token = root.path().join("token");
        fs::create_dir(&data).unwrap();
        fs::write(&token, TOKEN).unwrap();
        Self {
            _root: root,
            data,
            web,
            token,
        }
    }

    fn app(&self) -> Router {
        arcdos_server::app_with_files(&self.web, &self.data, &self.token).unwrap()
    }
}

async fn send(
    app: &Router,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    (parts.status, parts.headers, bytes.to_vec())
}

fn request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"))
}

async fn rpc(app: &Router, service: &str, method: &str, payload: Value) -> (StatusCode, Value) {
    let path = format!("/arcdos.files.v1.{service}/{method}");
    let request = request("POST", &path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("connect-protocol-version", "1")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let (status, _, bytes) = send(app, request).await;
    let decoded = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "could not decode {method} HTTP {status}: {error}; {}",
            String::from_utf8_lossy(&bytes)
        );
    });
    (status, decoded)
}

async fn ok(app: &Router, service: &str, method: &str, payload: Value) -> Value {
    let (status, response) = rpc(app, service, method, payload).await;
    assert_eq!(status, StatusCode::OK, "{method}: {response}");
    response
}

async fn file(app: &Router, method: &str, payload: Value) -> Value {
    ok(app, "FileService", method, payload).await
}

async fn root(app: &Router) -> Value {
    file(app, "GetEntry", json!({})).await["entry"].clone()
}

#[tokio::test]
async fn file_routes_are_opt_in_and_require_the_token() {
    let fixture = Fixture::new();
    let get_entry = "/arcdos.files.v1.FileService/GetEntry";
    let contents = "/api/files/v1/entry/content";
    let upload = "/api/files/v1/uploads/id";
    let m1 = arcdos_server::app(&fixture.web);
    let (status, _, _) = send(&m1, Request::get(contents).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let app = fixture.app();
    for (method, path) in [
        ("POST", get_entry),
        ("GET", contents),
        ("HEAD", contents),
        ("PATCH", upload),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let (status, headers, _) = send(&app, request).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    }
    assert!(
        !root(&app)
            .await
            .get("entryId")
            .unwrap()
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert!(arcdos_server::app_with_files(&fixture.data, &fixture.data, &fixture.token).is_err());
}

#[tokio::test]
async fn file_operations_keep_opaque_ids_and_journal_across_restart() {
    let fixture = Fixture::new();
    let app = fixture.app();
    fs::write(fixture.data.join("files/é.txt"), b"synthetic").unwrap();
    symlink(&fixture.token, fixture.data.join("files/escape.txt")).unwrap();
    let initial = root(&app).await;
    let list = file(&app, "ListEntries", json!({})).await;
    let entries = list["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    let original = &entries[0];
    let folder = file(
        &app,
        "CreateDirectory",
        json!({
            "name": "資料", "expectedParentRevision": initial["revision"]
        }),
    )
    .await["entry"]
        .clone();
    let (status, conflict) = rpc(
        &app,
        "FileService",
        "CreateDirectory",
        json!({
            "name": "other", "expectedParentRevision": initial["revision"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(conflict["code"], "aborted");
    let copied = file(
        &app,
        "CopyEntry",
        json!({
            "entryId": original["entryId"], "expectedRevision": original["revision"],
            "destinationParentId": folder["entryId"], "name": "copy.txt",
            "expectedParentRevision": folder["revision"]
        }),
    )
    .await["entry"]
        .clone();
    assert_ne!(copied["entryId"], original["entryId"]);
    let folder =
        file(&app, "GetEntry", json!({"entryId": folder["entryId"]})).await["entry"].clone();
    let moved = file(
        &app,
        "MoveEntry",
        json!({
            "entryId": folder["entryId"], "expectedRevision": folder["revision"],
            "name": "renamed", "expectedParentRevision": root(&app).await["revision"]
        }),
    )
    .await["entry"]
        .clone();
    assert_eq!(moved["entryId"], folder["entryId"]);
    let child =
        file(&app, "GetEntry", json!({"entryId": copied["entryId"]})).await["entry"].clone();
    assert_eq!(child["parentId"], moved["entryId"]);

    let trashed = file(
        &app,
        "TrashEntry",
        json!({
            "entryId": moved["entryId"], "expectedRevision": moved["revision"]
        }),
    )
    .await["entry"]
        .clone();
    assert_eq!(trashed["trashed"], true);
    assert_eq!(
        file(&app, "ListTrash", json!({})).await["entries"][0]["name"],
        "renamed"
    );
    drop(app);
    let app = fixture.app();
    assert_eq!(root(&app).await["entryId"], initial["entryId"]);
    let restored = file(
        &app,
        "RestoreEntry",
        json!({
            "entryId": trashed["entryId"], "expectedRevision": trashed["revision"]
        }),
    )
    .await["entry"]
        .clone();
    assert_eq!(restored["entryId"], folder["entryId"]);
    assert_eq!(
        fs::read(fixture.data.join("files/renamed/copy.txt")).unwrap(),
        b"synthetic"
    );
    let changes = file(&app, "ListChanges", json!({"pageSize": 2})).await;
    assert_eq!(changes["changes"].as_array().unwrap().len(), 2);
    let remaining = file(
        &app,
        "ListChanges",
        json!({"afterSequence": changes["nextSequence"]}),
    )
    .await;
    let kinds: Vec<_> = changes["changes"]
        .as_array()
        .unwrap()
        .iter()
        .chain(remaining["changes"].as_array().unwrap())
        .filter(|change| change["entryId"] != original["entryId"])
        .map(|change| change["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "CHANGE_KIND_CREATED",
            "CHANGE_KIND_COPIED",
            "CHANGE_KIND_MOVED",
            "CHANGE_KIND_TRASHED",
            "CHANGE_KIND_RESTORED"
        ]
    );
}

#[tokio::test]
async fn pagination_and_simultaneous_mutations_detect_stale_revisions() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let files = fixture.data.join("files");
    fs::write(files.join("b"), b"b").unwrap();
    fs::write(files.join("a"), b"a").unwrap();
    let page = file(&app, "ListEntries", json!({"pageSize": 1})).await;
    assert_eq!(page["entries"][0]["name"], "a");
    let next = file(
        &app,
        "ListEntries",
        json!({
            "pageSize": 1, "pageToken": page["nextPageToken"]
        }),
    )
    .await;
    assert_eq!(next["entries"][0]["name"], "b");
    let revision = root(&app).await["revision"].clone();
    let (first, second) = tokio::join!(
        rpc(
            &app,
            "FileService",
            "CreateDirectory",
            json!({"name": "c", "expectedParentRevision": revision})
        ),
        rpc(
            &app,
            "FileService",
            "CreateDirectory",
            json!({"name": "d", "expectedParentRevision": revision})
        ),
    );
    assert!(matches!(
        (first.0, second.0),
        (StatusCode::OK, StatusCode::CONFLICT) | (StatusCode::CONFLICT, StatusCode::OK)
    ));
    let (status, response) = rpc(
        &app,
        "FileService",
        "ListEntries",
        json!({
            "pageSize": 1, "pageToken": page["nextPageToken"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(response["code"], "aborted");

    let original = &page["entries"][0];
    fs::write(files.join("a"), b"outside update").unwrap();
    let current = file(&app, "GetEntry", json!({"entryId": original["entryId"]})).await;
    assert_ne!(original["revision"], current["entry"]["revision"]);
}

async fn patch(
    app: &Router,
    path: &str,
    offset: u64,
    bytes: &'static [u8],
) -> (StatusCode, axum::http::HeaderMap) {
    let (status, headers, _) = send(
        app,
        request("PATCH", path)
            .header("upload-offset", offset)
            .header(header::CONTENT_LENGTH, bytes.len())
            .body(Body::from(bytes))
            .unwrap(),
    )
    .await;
    (status, headers)
}

#[tokio::test]
async fn resumable_uploads_survive_restart_and_publish_once_with_ranges() {
    let fixture = Fixture::new();
    let app = fixture.app();
    let payload = b"hello world";
    let root_entry = root(&app).await;
    let upload = ok(&app, "TransferService", "BeginUpload", json!({
        "name": "test.txt", "expectedParentRevision": root_entry["revision"],
        "sizeBytes": payload.len().to_string(), "sha256": STANDARD.encode(Sha256::digest(payload))
    })).await["upload"].clone();
    let path = format!(
        "/api/files/v1/uploads/{}",
        upload["uploadId"].as_str().unwrap()
    );
    let (status, headers) = patch(&app, &path, 0, b"hello ").await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(headers["upload-offset"], "6");
    drop(app);
    let app = fixture.app();
    let status = ok(
        &app,
        "TransferService",
        "GetUpload",
        json!({"uploadId": upload["uploadId"]}),
    )
    .await;
    assert_eq!(status["upload"]["committedOffset"], "6");
    let (status, headers) = patch(&app, &path, 0, b"hello ").await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(headers["upload-offset"], "6");
    assert_eq!(
        patch(&app, &path, 6, b"world").await.0,
        StatusCode::NO_CONTENT
    );
    file(
        &app,
        "CreateDirectory",
        json!({
            "name": "changed", "expectedParentRevision": root_entry["revision"]
        }),
    )
    .await;
    let (status, _) = rpc(
        &app,
        "TransferService",
        "CommitUpload",
        json!({
            "uploadId": upload["uploadId"]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let commit = ok(
        &app,
        "TransferService",
        "CommitUpload",
        json!({
            "uploadId": upload["uploadId"], "expectedParentRevision": root(&app).await["revision"]
        }),
    )
    .await;
    let retry = ok(
        &app,
        "TransferService",
        "CommitUpload",
        json!({"uploadId": upload["uploadId"]}),
    )
    .await;
    assert_eq!(commit["entry"]["entryId"], retry["entry"]["entryId"]);
    assert_eq!(
        file(&app, "ListChanges", json!({})).await["changes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let content = format!(
        "/api/files/v1/{}/content",
        commit["entry"]["entryId"].as_str().unwrap()
    );
    let (status, headers, body) = send(
        &app,
        request("GET", &content)
            .header(header::RANGE, "bytes=-5")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 6-10/11");
    assert_eq!(body, b"world");
    let (status, headers, body) =
        send(&app, request("HEAD", &content).body(Body::empty()).unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_LENGTH], "11");
    assert!(body.is_empty());
    assert_eq!(
        fs::read(fixture.data.join("files/test.txt")).unwrap(),
        payload
    );
}
