#![cfg(target_os = "linux")]

use std::{fs, os::unix::fs::symlink, path::PathBuf, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use serde_json::{Value, json};
use tempfile::{TempDir, tempdir};
use tower::ServiceExt;

const TOKEN: &str = "arcdos-search-fixture-token-0000000000000000";
const SEARCH: &str = "/arcdos.search.v1.SearchService/SearchFiles";
const TAGS: &str = "/arcdos.search.v1.SearchService/SetTags";

struct Fixture {
    root: TempDir,
    data: PathBuf,
    files: PathBuf,
    token: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempdir().unwrap();
        let data = root.path().join("data");
        let files = data.join("files");
        let token = root.path().join("token");
        fs::create_dir_all(&files).unwrap();
        fs::write(&token, TOKEN).unwrap();
        Self {
            root,
            data,
            files,
            token,
        }
    }

    fn app(&self) -> Router {
        arcdos_server::app_with_files(&self.root.path().join("web"), &self.data, &self.token)
            .unwrap()
    }
}

async fn rpc(app: &Router, path: &str, payload: Value, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header("connect-protocol-version", "1");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(payload.to_string())).unwrap())
        .await
        .unwrap();
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, 1024 * 1024).await.unwrap();
    let response = serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!(
            "could not decode {path} HTTP {}: {error}; {}",
            parts.status,
            String::from_utf8_lossy(&bytes)
        );
    });
    (parts.status, response)
}

async fn ok(app: &Router, path: &str, payload: Value) -> Value {
    let (status, response) = rpc(app, path, payload, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK, "{path}: {response}");
    response
}

async fn search(app: &Router, query: &str) -> Value {
    ok(app, SEARCH, json!({"query": query})).await
}

fn hits(response: &Value) -> &[Value] {
    response.get("hits").map_or(&[], |hits| {
        hits.as_array().expect("search hits must be an array")
    })
}

async fn wait_for_hits(app: &Router, query: &str, count: usize) -> Value {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let response = search(app, query).await;
            if hits(&response).len() == count {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("search did not return {count} hits for {query:?}"))
}

fn tag_request(hit: &Value, tags: Value) -> Value {
    json!({
        "entryId": hit["entry"]["entryId"],
        "expectedRevision": hit["entry"]["revision"],
        "tags": tags,
    })
}

#[tokio::test]
async fn search_and_tags_require_authorization_and_validate_requests() {
    let fixture = Fixture::new();
    let app = fixture.app();
    for path in [SEARCH, TAGS] {
        for token in [None, Some("wrong-development-token-000000000000000000")] {
            let (status, response) = rpc(&app, path, json!({}), token).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}: {response}");
            assert_eq!(response["code"], "unauthenticated");
        }
    }
    for payload in [
        json!({"query": "record", "limit": 101}),
        json!({"query": "a".repeat(257)}),
        json!({"query": "one two three four five six seven eight nine ten eleven twelve thirteen"}),
        json!({"query": {"invalid": "type"}}),
    ] {
        let (status, response) = rpc(&app, SEARCH, payload, Some(TOKEN)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_eq!(response["code"], "invalid_argument");
    }
    assert!(hits(&search(&app, "\") (*").await).is_empty());
}

#[tokio::test]
async fn searches_filenames_paths_contents_metadata_and_tags_with_a_result_limit() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.files.join("research-silo")).unwrap();
    fs::write(
        fixture.files.join("research-silo/contract.txt"),
        "copperfield survey material",
    )
    .unwrap();
    fs::write(
        fixture.files.join("contract-draft.txt"),
        "copperfield draft memo",
    )
    .unwrap();
    let app = fixture.app();

    let contents = wait_for_hits(&app, "copperfield", 2).await;
    let nested = hits(&contents)
        .iter()
        .find(|hit| hit["path"] == "research-silo/contract.txt")
        .unwrap();
    assert_eq!(nested["entry"]["name"], "contract.txt");
    let by_name = search(&app, "contract draft").await;
    assert_eq!(hits(&by_name)[0]["path"], "contract-draft.txt");
    let by_path = search(&app, "research silo").await;
    assert!(
        hits(&by_path)
            .iter()
            .any(|hit| hit["path"] == nested["path"])
    );
    assert_eq!(hits(&search(&app, "file txt").await).len(), 2);
    let limited = ok(&app, SEARCH, json!({"query": "copperfield", "limit": 1})).await;
    assert_eq!(hits(&limited).len(), 1);
    assert_eq!(limited["hasMore"], true);

    let mut stale = tag_request(nested, json!(["archival dossier"]));
    stale["expectedRevision"] = json!("stale");
    let (status, response) = rpc(&app, TAGS, stale, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert_eq!(response["code"], "aborted");
    let invalid = tag_request(nested, json!(["   "]));
    let (status, response) = rpc(&app, TAGS, invalid, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");

    let tags = tag_request(nested, json!([" archival dossier ", "archival dossier"]));
    assert_eq!(
        ok(&app, TAGS, tags).await["tags"],
        json!(["archival dossier"])
    );
    let response = wait_for_hits(&app, "archival dossier", 1).await;
    let tagged = &hits(&response)[0];
    assert_eq!(tagged["entry"]["entryId"], nested["entry"]["entryId"]);
    assert_eq!(tagged["tags"], json!(["archival dossier"]));

    ok(&app, TAGS, tag_request(tagged, json!([]))).await;
    wait_for_hits(&app, "archival dossier", 0).await;
}

#[tokio::test]
async fn matches_canonically_equivalent_unicode_queries_and_paths() {
    let fixture = Fixture::new();
    let name = "\u{1112}\u{1161}\u{11ab}\u{1100}\u{1173}\u{11af}.txt";
    fs::write(fixture.files.join(name), "résumé").unwrap();
    let app = fixture.app();
    let by_name = wait_for_hits(&app, "한글", 1).await;
    assert_eq!(hits(&by_name)[0]["path"], name);
    let by_content = search(&app, "re\u{301}sume\u{301}").await;
    assert_eq!(hits(&by_content)[0]["path"], name);
}

#[tokio::test]
async fn reindexes_external_edits_and_renames_without_following_symlinks() {
    let fixture = Fixture::new();
    let original = fixture.files.join("original-note.txt");
    let renamed = fixture.files.join("renamed-note.txt");
    fs::write(&original, "pruneword").unwrap();
    let app = fixture.app();
    let first = wait_for_hits(&app, "pruneword", 1).await;

    fs::write(&original, "refreshword with longer bytes").unwrap();
    let changed = wait_for_hits(&app, "refreshword", 1).await;
    assert_ne!(
        hits(&first)[0]["entry"]["revision"],
        hits(&changed)[0]["entry"]["revision"]
    );
    assert!(hits(&search(&app, "pruneword").await).is_empty());

    fs::rename(&original, &renamed).unwrap();
    let moved = wait_for_hits(&app, "renamed note", 1).await;
    assert_eq!(hits(&moved)[0]["path"], "renamed-note.txt");
    assert!(hits(&search(&app, "original note").await).is_empty());

    fs::remove_file(&renamed).unwrap();
    let outside = fixture.root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let outside_file = outside.join("outside-unique.txt");
    fs::write(&outside_file, "outsidefixturecontent").unwrap();
    symlink(&outside_file, &renamed).unwrap();
    symlink(&outside, fixture.files.join("outside-directory")).unwrap();
    fs::write(fixture.files.join("barrier.txt"), "scanbarrier").unwrap();
    wait_for_hits(&app, "scanbarrier", 1).await;
    for query in [
        "refreshword",
        "renamed note",
        "outsidefixturecontent",
        "outside directory",
    ] {
        assert!(hits(&search(&app, query).await).is_empty(), "{query}");
    }
}

#[tokio::test]
async fn offline_rebuild_resumes_indexing_and_preserves_catalog_metadata() {
    let fixture = Fixture::new();
    let keep = fixture.files.join("restart-note.md");
    let gone = fixture.files.join("obsolete.txt");
    fs::write(&keep, "retainedseed").unwrap();
    fs::write(&gone, "obsoletecontent").unwrap();
    let app = fixture.app();
    let before = wait_for_hits(&app, "retainedseed", 1).await;
    let entry = &hits(&before)[0];
    ok(&app, TAGS, tag_request(entry, json!(["persisttag"]))).await;
    wait_for_hits(&app, "persisttag", 1).await;
    wait_for_hits(&app, "obsoletecontent", 1).await;
    assert!(arcdos_server::rebuild_search(&fixture.data).is_err());
    drop(app);

    fs::write(&keep, "reconciledcontent edited while stopped").unwrap();
    fs::remove_file(gone).unwrap();
    fs::write(fixture.files.join("offline-added.txt"), "offlinecontent").unwrap();
    arcdos_server::rebuild_search(&fixture.data).unwrap();

    let app = fixture.app();
    let response = wait_for_hits(&app, "reconciledcontent", 1).await;
    let rebuilt = &hits(&response)[0];
    assert_eq!(rebuilt["entry"]["entryId"], entry["entry"]["entryId"]);
    assert_ne!(rebuilt["entry"]["revision"], entry["entry"]["revision"]);
    assert_eq!(rebuilt["tags"], json!(["persisttag"]));
    assert_eq!(hits(&search(&app, "persisttag").await).len(), 1);
    let added = wait_for_hits(&app, "offlinecontent", 1).await;
    assert_eq!(hits(&added)[0]["path"], "offline-added.txt");
    assert!(hits(&search(&app, "obsoletecontent").await).is_empty());
    assert!(hits(&search(&app, "retainedseed").await).is_empty());
}
