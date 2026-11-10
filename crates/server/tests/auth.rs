#![cfg(target_os = "linux")]

use std::{
    fs,
    sync::{Arc, Mutex, OnceLock},
    time::{SystemTime, UNIX_EPOCH},
};

use arcdos_auth::AndroidClientConfig;
use arcdos_server::{AuthentikConfig, app_with_authentik};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Request, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rsa::{
    RsaPrivateKey,
    pkcs1v15::SigningKey,
    rand_core::OsRng,
    sha2::{Digest, Sha256},
    signature::{SignatureEncoding, Signer},
    traits::PublicKeyParts,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const ORIGIN: &str = "https://arc.example.test";
const LIST: &str = "/arcdos.files.v1.FileService/ListEntries";
const REVOKE: &str = "/auth/admin/revoke";

struct Provider {
    base: String,
    login: Mutex<Option<(String, String)>>,
    account: Mutex<Account>,
}

struct Account {
    active: bool,
    role: &'static str,
    revoked: bool,
}

struct Fixture {
    app: Router,
    provider: Arc<Provider>,
    _root: TempDir,
    _task: tokio::task::JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self._task.abort();
    }
}

impl Fixture {
    async fn start() -> Self {
        // Generate the fixture key before starting the provider's request deadlines.
        signing_key();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = Arc::new(Provider {
            base: format!("http://{}", listener.local_addr().unwrap()),
            login: Mutex::new(None),
            account: Mutex::new(Account {
                active: true,
                role: "drive-users",
                revoked: false,
            }),
        });
        let upstream = Router::new()
            .route(
                "/application/o/web/.well-known/openid-configuration",
                get(discovery),
            )
            .route("/keys", get(jwks))
            .route("/token", post(exchange))
            .route("/userinfo", get(userinfo))
            .route("/introspect", post(introspect))
            .route("/revoke", post(revoke))
            .with_state(provider.clone());
        let task = tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        let secret = root.path().join("secret");
        fs::create_dir(&data).unwrap();
        fs::write(&secret, "synthetic-secret").unwrap();
        let app = app_with_authentik(
            &root.path().join("web"),
            &data,
            AuthentikConfig {
                issuer: provider.issuer(),
                public_origin: ORIGIN.to_owned(),
                client_id: "web".to_owned(),
                client_secret_file: secret,
                android: Some(AndroidClientConfig {
                    issuer: provider.android_issuer(),
                    client_id: "android".to_owned(),
                }),
            },
        )
        .await
        .unwrap();
        fs::write(data.join("files/note.txt"), "synthetic file").unwrap();
        Self {
            app,
            provider,
            _root: root,
            _task: task,
        }
    }

    async fn begin_login(&self) -> (String, String) {
        let reply = send(&self.app, Request::get("/auth/login"), "").await;
        assert_eq!(reply.0, StatusCode::SEE_OTHER);
        let location = reply.1[header::LOCATION].to_str().unwrap();
        let query = location.split_once('?').unwrap().1;
        assert_eq!(parameter(query, "response_type"), "code");
        assert_eq!(parameter(query, "client_id"), "web");
        assert_eq!(parameter(query, "prompt"), "login");
        assert_eq!(parameter(query, "code_challenge_method"), "S256");
        let nonce = parameter(query, "nonce").to_owned();
        let challenge = parameter(query, "code_challenge").to_owned();
        self.provider
            .login
            .lock()
            .unwrap()
            .replace((nonce, challenge));
        (
            cookie(&reply.1, "__Host-arcdos-login"),
            parameter(query, "state").to_owned(),
        )
    }

    async fn sign_in(&self, previous: Option<&str>) -> (String, String) {
        let (login, state) = self.begin_login().await;
        let cookies = match previous {
            Some(previous) => format!("{login}; {previous}"),
            None => login,
        };
        let reply = self.callback(&cookies, &state, "valid").await;
        assert_eq!(
            reply.0,
            StatusCode::SEE_OTHER,
            "{}",
            String::from_utf8_lossy(&reply.2)
        );
        assert_eq!(reply.1[header::LOCATION], "/files/");
        assert_eq!(reply.1[header::REFERRER_POLICY], "no-referrer");
        let session = cookie(&reply.1, "__Host-arcdos-session");
        let status = send(
            &self.app,
            Request::get("/auth/session").header(header::COOKIE, &session),
            "",
        )
        .await;
        assert_eq!(status.0, StatusCode::OK);
        let status = decode(&status.2);
        assert_eq!(status["mode"], "oidc");
        assert_eq!(status["user"]["subject"], "fixture-subject");
        assert_eq!(status["user"]["name"], "Fixture User");
        assert!(status.get("access_token").is_none());
        (session, status["csrf_token"].as_str().unwrap().to_owned())
    }

    async fn callback(&self, cookies: &str, state: &str, code: &str) -> Reply {
        send(
            &self.app,
            Request::get(format!("/auth/callback?state={state}&code={code}"))
                .header(header::COOKIE, cookies),
            "",
        )
        .await
    }
}

impl Provider {
    fn issuer(&self) -> String {
        format!("{}/application/o/web/", self.base)
    }

    fn android_issuer(&self) -> String {
        format!("{}/application/o/android/", self.base)
    }
}

async fn discovery(State(provider): State<Arc<Provider>>) -> Json<Value> {
    Json(json!({
        "issuer": provider.issuer(),
        "authorization_endpoint": format!("{}/authorize", provider.base),
        "token_endpoint": format!("{}/token", provider.base),
        "jwks_uri": format!("{}/keys", provider.base),
        "userinfo_endpoint": format!("{}/userinfo", provider.base),
        "revocation_endpoint": format!("{}/revoke", provider.base),
        "introspection_endpoint": format!("{}/introspect", provider.base),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

fn signing_key() -> &'static RsaPrivateKey {
    static KEY: OnceLock<RsaPrivateKey> = OnceLock::new();
    KEY.get_or_init(|| RsaPrivateKey::new(&mut OsRng, 2048).unwrap())
}

async fn jwks() -> Json<Value> {
    let key = signing_key();
    Json(json!({"keys": [{
        "kty": "RSA", "alg": "RS256", "kid": "fixture",
        "n": URL_SAFE_NO_PAD.encode(key.n().to_bytes_be()),
        "e": URL_SAFE_NO_PAD.encode(key.e().to_bytes_be()),
    }]}))
}

async fn exchange(State(provider): State<Arc<Provider>>, form: String) -> Json<Value> {
    let (nonce, challenge) = provider.login.lock().unwrap().take().unwrap();
    assert_eq!(
        URL_SAFE_NO_PAD.encode(Sha256::digest(parameter(&form, "code_verifier").as_bytes())),
        challenge
    );
    let code = parameter(&form, "code");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({
        "iss": provider.issuer(),
        "sub": "fixture-subject",
        "aud": if code == "wrong-audience" { "foreign-client" } else { "web" },
        "iat": now.saturating_sub(120),
        "exp": if code == "expired" { now - 60 } else { now + 900 },
        "nonce": if code == "wrong-nonce" { "invalid" } else { &nonce },
    });
    let message = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","kid":"fixture"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    );
    let mut signature = SigningKey::<Sha256>::new(signing_key().clone())
        .sign(message.as_bytes())
        .to_vec();
    if code == "bad-signature" {
        signature[0] ^= 1;
    }
    Json(json!({
        "token_type": "Bearer",
        "access_token": "fixture-access",
        "expires_in": 900,
        "id_token": format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature)),
    }))
}

async fn userinfo(State(provider): State<Arc<Provider>>, headers: HeaderMap) -> impl IntoResponse {
    let account = provider.account.lock().unwrap();
    if account.revoked
        || !matches!(
            headers[header::AUTHORIZATION].to_str().unwrap(),
            "Bearer fixture-access" | "Bearer fixture-android-access"
        )
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(json!({
        "sub": "fixture-subject",
        "arcdos_active": account.active,
        "arcdos_groups": [account.role],
        "arcdos_display_name": "Fixture User",
    }))
    .into_response()
}

async fn introspect(State(provider): State<Arc<Provider>>, form: String) -> Json<Value> {
    assert_eq!(parameter(&form, "client_id"), "web");
    assert_eq!(parameter(&form, "client_secret"), "synthetic-secret");
    let token = parameter(&form, "token");
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let client = if token == "fixture-access" {
        "web"
    } else {
        "android"
    };
    Json(json!({
        "active": token != "unknown",
        "client_id": client,
        "aud": client,
        "iss": if client == "web" { provider.issuer() } else { provider.android_issuer() },
        "sub": "fixture-subject",
        "scope": "openid arcdos",
        "exp": if token == "expired" { now - 1 } else { now + 300 },
    }))
}

async fn revoke(State(provider): State<Arc<Provider>>, form: String) -> StatusCode {
    assert_eq!(parameter(&form, "token"), "fixture-access");
    assert_eq!(parameter(&form, "token_type_hint"), "access_token");
    provider.account.lock().unwrap().revoked = true;
    StatusCode::OK
}

fn parameter<'a>(query: &'a str, name: &str) -> &'a str {
    query
        .split('&')
        .filter_map(|field| field.split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
        .unwrap_or_else(|| panic!("missing fixture parameter {name}"))
}

fn cookie(headers: &HeaderMap, name: &str) -> String {
    let cookie = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .find(|value| value.starts_with(&format!("{name}=")))
        .unwrap();
    for flag in ["Path=/", "HttpOnly", "Secure", "SameSite=Lax"] {
        assert!(cookie.split("; ").any(|field| field == flag), "{cookie}");
    }
    cookie.split_once(';').unwrap().0.to_owned()
}

type Reply = (StatusCode, HeaderMap, Vec<u8>);

async fn send(app: &Router, builder: axum::http::request::Builder, body: &str) -> Reply {
    let request = builder.body(Body::from(body.to_owned())).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    (
        parts.status,
        parts.headers,
        to_bytes(body, 1024 * 1024).await.unwrap().to_vec(),
    )
}

fn decode(body: &[u8]) -> Value {
    serde_json::from_slice(body).unwrap()
}

fn list() -> axum::http::request::Builder {
    Request::post(LIST)
        .header(header::CONTENT_TYPE, "application/json")
        .header("connect-protocol-version", "1")
}

#[tokio::test]
async fn login_rejects_invalid_state_and_signed_token_claims() {
    let fixture = Fixture::start().await;
    for code in ["wrong-audience", "wrong-nonce", "bad-signature", "expired"] {
        let (login, state) = fixture.begin_login().await;
        assert_eq!(
            fixture.callback("", &state, code).await.0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            fixture.callback(&login, "forged-state", code).await.0,
            StatusCode::BAD_REQUEST
        );
        let rejected = fixture.callback(&login, &state, code).await;
        assert_eq!(rejected.0, StatusCode::UNAUTHORIZED, "{code}");
        assert_eq!(rejected.1.get_all(header::SET_COOKIE).iter().count(), 1);
        assert_eq!(
            fixture.callback(&login, &state, code).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    let (old, _) = fixture.sign_in(None).await;
    let (new, _) = fixture.sign_in(Some(&old)).await;
    assert_ne!(old, new);
    let reply = send(
        &fixture.app,
        Request::get("/auth/session").header(header::COOKIE, old),
        "",
    )
    .await;
    assert!(decode(&reply.2).get("user").is_none());
}

#[tokio::test]
async fn file_access_checks_csrf_origin_and_live_account_state() {
    let fixture = Fixture::start().await;
    let app = &fixture.app;
    let (session, csrf) = fixture.sign_in(None).await;
    let reply = send(app, list().header(header::ORIGIN, ORIGIN), "{}").await;
    assert_eq!(reply.0, StatusCode::UNAUTHORIZED);
    assert_eq!(decode(&reply.2)["code"], "unauthenticated");
    for (origin, token) in [
        (ORIGIN, ""),
        (ORIGIN, "wrong"),
        ("https://other.example.test", csrf.as_str()),
    ] {
        let reply = send(
            app,
            list()
                .header(header::COOKIE, &session)
                .header(header::ORIGIN, origin)
                .header("x-csrf-token", token),
            "{}",
        )
        .await;
        assert_eq!(reply.0, StatusCode::FORBIDDEN);
        assert_eq!(decode(&reply.2)["code"], "permission_denied");
    }
    let reply = send(
        app,
        Request::patch("/api/files/v1/uploads/fake")
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, ORIGIN),
        "",
    )
    .await;
    assert_eq!(reply.0, StatusCode::FORBIDDEN);
    let reply = send(
        app,
        list()
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", &csrf),
        "{}",
    )
    .await;
    assert_eq!(reply.0, StatusCode::OK);
    let id = decode(&reply.2)["entries"][0]["entryId"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = format!("/api/files/v1/{id}/content");
    assert_eq!(
        send(app, Request::get(&path), "").await.0,
        StatusCode::UNAUTHORIZED
    );
    let reply = send(
        app,
        Request::get(&path)
            .header(header::COOKIE, &session)
            .header("sec-fetch-site", "same-site"),
        "",
    )
    .await;
    assert_eq!(reply.0, StatusCode::FORBIDDEN);
    let reply = send(
        app,
        Request::get(&path).header(header::COOKIE, &session),
        "",
    )
    .await;
    assert_eq!(reply.0, StatusCode::OK);
    assert_eq!(reply.1[header::CACHE_CONTROL], "no-store");
    assert_eq!(reply.2, b"synthetic file");

    fixture.provider.account.lock().unwrap().active = false;
    assert_eq!(
        send(
            app,
            Request::get(&path).header(header::COOKIE, &session),
            ""
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    {
        let mut account = fixture.provider.account.lock().unwrap();
        account.active = true;
        account.role = "unrelated-group";
    }
    let (login, state) = fixture.begin_login().await;
    assert_eq!(
        fixture.callback(&login, &state, "valid").await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn logout_and_admin_revocation_invalidate_existing_sessions() {
    let fixture = Fixture::start().await;
    let app = &fixture.app;
    let (session, csrf) = fixture.sign_in(None).await;
    let (other_session, _) = fixture.sign_in(None).await;
    let revoke_request = || {
        Request::post(REVOKE)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, ORIGIN)
            .header("x-csrf-token", &csrf)
    };
    let target = r#"{"subject":"fixture-subject"}"#;
    assert_eq!(
        send(app, revoke_request(), target).await.0,
        StatusCode::FORBIDDEN
    );
    fixture.provider.account.lock().unwrap().role = "drive-admins";
    assert_eq!(
        send(app, revoke_request(), target).await.0,
        StatusCode::NO_CONTENT
    );
    for cookie in [session, other_session] {
        let reply = send(
            app,
            Request::get("/auth/session").header(header::COOKIE, cookie),
            "",
        )
        .await;
        assert!(decode(&reply.2).get("user").is_none());
    }
    let (session, csrf) = fixture.sign_in(None).await;
    let logout_request = || {
        Request::post("/auth/logout")
            .header(header::COOKIE, &session)
            .header(header::ORIGIN, ORIGIN)
    };
    assert_eq!(
        send(app, logout_request(), "").await.0,
        StatusCode::FORBIDDEN
    );
    let reply = send(app, logout_request().header("x-csrf-token", csrf), "").await;
    assert_eq!(reply.0, StatusCode::NO_CONTENT);
    assert_eq!(
        cookie(&reply.1, "__Host-arcdos-session"),
        "__Host-arcdos-session="
    );
    assert!(fixture.provider.account.lock().unwrap().revoked);
    let reply = send(
        app,
        Request::get("/auth/session").header(header::COOKIE, session),
        "",
    )
    .await;
    assert!(decode(&reply.2).get("user").is_none());
}

#[tokio::test]
async fn android_bearer_requires_the_public_clients_active_access_token() {
    let fixture = Fixture::start().await;
    let request = |token| list().header(header::AUTHORIZATION, format!("Bearer {token}"));
    for token in ["fixture-access", "unknown", "expired", "fixture-refresh"] {
        assert_eq!(
            send(&fixture.app, request(token), "{}").await.0,
            StatusCode::UNAUTHORIZED,
            "{token}"
        );
    }
    assert_eq!(
        send(&fixture.app, request("fixture-android-access"), "{}")
            .await
            .0,
        StatusCode::OK
    );
    fixture.provider.account.lock().unwrap().role = "unrelated-group";
    assert_eq!(
        send(&fixture.app, request("fixture-android-access"), "{}")
            .await
            .0,
        StatusCode::FORBIDDEN
    );
}
