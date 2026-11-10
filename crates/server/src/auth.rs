use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use arcdos_auth::{
    AndroidClientConfig, Identity, OidcConfig, OidcProvider,
    session::{Credentials, LOGIN_TTL_SECONDS, PendingLogin, SESSION_TTL_SECONDS, Session, Store},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{
        HeaderMap, HeaderName, HeaderValue, StatusCode,
        header::{
            AUTHORIZATION, CACHE_CONTROL, COOKIE, LOCATION, ORIGIN, REFERRER_POLICY, SET_COOKIE,
            WWW_AUTHENTICATE,
        },
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use connectrpc::ConnectError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::BoxError;

const SESSION_COOKIE: &str = "__Host-arcdos-session";
const LOGIN_COOKIE: &str = "__Host-arcdos-login";
const CSRF_HEADER: HeaderName = HeaderName::from_static("x-csrf-token");
const MAX_DEV_TOKEN: usize = 256;
const FILES_PAGE: &str = "/files/";

/// Production file API settings. The confidential client secret is read at startup.
pub struct AuthentikConfig {
    pub issuer: String,
    pub public_origin: String,
    pub client_id: String,
    pub client_secret_file: PathBuf,
    pub android: Option<AndroidClientConfig>,
}

impl AuthentikConfig {
    pub(super) async fn discover(self, web: &Path, data: &Path) -> Result<OidcProvider, BoxError> {
        let client_secret = read_secret(web, data, &self.client_secret_file, 1, 4096)?;
        Ok(OidcProvider::discover(OidcConfig {
            issuer: self.issuer,
            public_origin: self.public_origin,
            client_id: self.client_id,
            client_secret,
            android: self.android,
        })
        .await?)
    }
}

#[derive(Clone)]
pub(super) enum Access {
    Development([u8; 32]),
    Oidc(Arc<OidcAccess>),
}

pub(super) struct OidcAccess {
    provider: OidcProvider,
    sessions: Arc<Store>,
}

impl Access {
    pub(super) fn development(web: &Path, data: &Path, secret: &Path) -> io::Result<Self> {
        let secret = read_secret(web, data, secret, 32, MAX_DEV_TOKEN)?;
        Ok(Self::Development(Sha256::digest(secret).into()))
    }

    pub(super) fn oidc(provider: OidcProvider, data: &Path) -> Result<Self, BoxError> {
        let sessions = Store::open(&data.join("state/auth.sqlite3"))?;
        Ok(Self::Oidc(Arc::new(OidcAccess {
            provider,
            sessions: Arc::new(sessions),
        })))
    }
}

pub(super) fn router(files: Router, access: Access) -> Router {
    let files = files.route_layer(middleware::from_fn_with_state(access.clone(), protect));
    let routes = Router::new()
        .route("/auth/session", get(session_status))
        .with_state(access.clone());
    let routes = match access {
        Access::Development(_) => routes,
        Access::Oidc(state) => routes.merge(
            Router::new()
                .route("/auth/login", get(login))
                .route("/auth/callback", get(callback))
                .route("/auth/logout", post(logout))
                .route(
                    "/auth/admin/revoke",
                    post(revoke_subject).layer(DefaultBodyLimit::max(4096)),
                )
                .with_state(state),
        ),
    };
    files.merge(routes.layer(middleware::from_fn(private_response)))
}

async fn private_response(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    set_private_headers(&mut response);
    response
}

async fn protect(State(access): State<Access>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let verified = match access {
        Access::Development(key) => bearer(headers)
            .filter(|token| (32..=MAX_DEV_TOKEN).contains(&token.len()))
            .filter(|token| bool::from(key.ct_eq(Sha256::digest(token).as_slice())))
            .map(|_| ())
            .ok_or(Denial::Unauthenticated),
        Access::Oidc(state) if headers.contains_key(AUTHORIZATION) => {
            if !origin_allowed(headers, state.provider.public_origin(), false) {
                Err(Denial::Forbidden)
            } else if let Some(token) = bearer(headers).filter(|token| token.len() <= 16_384) {
                state
                    .provider
                    .android_identity(token)
                    .await
                    .map(drop)
                    .map_err(Denial::from)
            } else {
                Err(Denial::Unauthenticated)
            }
        }
        Access::Oidc(state) => state
            .browser(headers, !request.method().is_safe())
            .await
            .map(drop),
    };
    let mut response = match verified {
        Ok(()) => next.run(request).await,
        Err(error) => error.for_api(headers),
    };
    set_private_headers(&mut response);
    response
}

#[derive(Debug, Clone, Copy)]
enum Denial {
    Unauthenticated,
    Forbidden,
    Unavailable,
    Internal,
}

impl From<arcdos_auth::OidcError> for Denial {
    fn from(error: arcdos_auth::OidcError) -> Self {
        use arcdos_auth::OidcError;
        match error {
            OidcError::InvalidCredential => Self::Unauthenticated,
            OidcError::Forbidden => Self::Forbidden,
            OidcError::Unavailable => Self::Unavailable,
            OidcError::InvalidConfig(_) => Self::Internal,
        }
    }
}

impl Denial {
    fn connect_error(self) -> ConnectError {
        match self {
            Self::Unauthenticated => ConnectError::unauthenticated("sign-in required"),
            Self::Forbidden => ConnectError::permission_denied("access denied"),
            Self::Unavailable => ConnectError::unavailable("identity provider unavailable"),
            Self::Internal => ConnectError::internal("could not verify access"),
        }
    }

    fn for_api(self, headers: &HeaderMap) -> Response {
        let mut response = self
            .connect_error()
            .into_http_response(headers)
            .map(Body::new);
        if matches!(self, Self::Unauthenticated) {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

impl IntoResponse for Denial {
    fn into_response(self) -> Response {
        self.for_api(&HeaderMap::new())
    }
}

impl OidcAccess {
    async fn db<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T, arcdos_auth::session::Error> + Send + 'static,
    ) -> Result<T, Denial> {
        let sessions = self.sessions.clone();
        tokio::task::spawn_blocking(move || operation(&sessions))
            .await
            .map_err(|_| Denial::Internal)?
            .map_err(|_| Denial::Internal)
    }

    async fn local_session(
        &self,
        headers: &HeaderMap,
        write: bool,
    ) -> Result<(String, Session), Denial> {
        if !origin_allowed(headers, self.provider.public_origin(), write) {
            return Err(Denial::Forbidden);
        }
        let id = cookie(headers, SESSION_COOKIE)
            .ok_or(Denial::Unauthenticated)?
            .to_owned();
        let key = id.clone();
        let session = self
            .db(move |store| store.get(&key))
            .await?
            .ok_or(Denial::Unauthenticated)?;
        if write
            && !single_header(headers, &CSRF_HEADER)
                .is_some_and(|token| session.matches_csrf(token))
        {
            return Err(Denial::Forbidden);
        }
        Ok((id, session))
    }

    async fn browser(
        &self,
        headers: &HeaderMap,
        write: bool,
    ) -> Result<(Identity, Session), Denial> {
        let (id, session) = self.local_session(headers, write).await?;
        let identity = self
            .provider
            .identity(
                &session.credentials.access_token,
                &session.credentials.subject,
            )
            .await;
        match identity {
            Ok(identity) => {
                if !self.db(move |store| store.is_active(&id)).await? {
                    return Err(Denial::Unauthenticated);
                }
                Ok((identity, session))
            }
            Err(arcdos_auth::OidcError::InvalidCredential | arcdos_auth::OidcError::Forbidden) => {
                self.db(move |store| store.remove(&id)).await?;
                Err(Denial::Unauthenticated)
            }
            Err(error) => Err(error.into()),
        }
    }
}

#[derive(Serialize)]
struct SessionStatus {
    mode: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<SessionUser>,
    #[serde(skip_serializing_if = "Option::is_none")]
    csrf_token: Option<String>,
}

#[derive(Serialize)]
struct SessionUser {
    subject: String,
    name: String,
    roles: Vec<String>,
}

async fn session_status(
    State(access): State<Access>,
    headers: HeaderMap,
) -> Result<Response, Denial> {
    let mut status = SessionStatus {
        mode: "development",
        user: None,
        csrf_token: None,
    };
    let mut expired = false;
    if let Access::Oidc(state) = access {
        status.mode = "oidc";
        match state.browser(&headers, false).await {
            Ok((identity, session)) => {
                status.user = Some(SessionUser {
                    subject: identity.subject,
                    name: identity.display_name,
                    roles: identity.roles,
                });
                status.csrf_token = Some(session.csrf_token);
            }
            Err(Denial::Unauthenticated) => expired = cookie(&headers, SESSION_COOKIE).is_some(),
            Err(error) => return Err(error),
        }
    }
    let mut response = Json(status).into_response();
    if expired {
        expire_cookie(&mut response, SESSION_COOKIE);
    }
    Ok(response)
}

async fn login(State(state): State<Arc<OidcAccess>>) -> Result<Response, Denial> {
    let flow = state.provider.start_login();
    let id = state
        .db(move |store| {
            store.begin_login(
                &flow.state,
                PendingLogin {
                    verifier: flow.pkce_verifier,
                    nonce: flow.nonce,
                    return_to: FILES_PAGE.to_owned(),
                },
            )
        })
        .await?;
    let mut response = redirect(&flow.authorization_url)?;
    set_cookie(&mut response, LOGIN_COOKIE, &id, LOGIN_TTL_SECONDS as u64);
    Ok(response)
}

#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

async fn callback(
    State(state): State<Arc<OidcAccess>>,
    headers: HeaderMap,
    Query(query): Query<Callback>,
) -> Result<Response, Denial> {
    let Some(cookie) = cookie(&headers, LOGIN_COOKIE).map(str::to_owned) else {
        return Ok(invalid_callback());
    };
    let Some(csrf_state) = query
        .state
        .filter(|value| !value.is_empty() && value.len() <= 512)
    else {
        return Ok(invalid_callback());
    };
    let Some(pending) = state
        .db(move |store| store.consume_login(&cookie, &csrf_state))
        .await?
    else {
        return Ok(invalid_callback());
    };
    let mut response = match (query.code, query.error) {
        (Some(code), None) if !code.is_empty() && code.len() <= 8192 => {
            match complete_login(&state, &headers, pending, code).await {
                Ok(response) => response,
                Err(error) => error.into_response(),
            }
        }
        _ => invalid_callback(),
    };
    expire_cookie(&mut response, LOGIN_COOKIE);
    Ok(response)
}

async fn complete_login(
    state: &OidcAccess,
    headers: &HeaderMap,
    pending: PendingLogin,
    code: String,
) -> Result<Response, Denial> {
    let tokens = state
        .provider
        .exchange(&code, &pending.nonce, &pending.verifier)
        .await?;
    let ttl = tokens
        .expires_in
        .min(Duration::from_secs(SESSION_TTL_SECONDS as u64));
    if ttl.as_secs() == 0 {
        return Err(Denial::Unauthenticated);
    }
    let previous = cookie(headers, SESSION_COOKIE).map(str::to_owned);
    let credentials = Credentials {
        subject: tokens.identity.subject,
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
    };
    let id = state
        .db(move |store| store.create(credentials, previous.as_deref(), ttl))
        .await?;
    let mut response = redirect(&pending.return_to)?;
    set_cookie(&mut response, SESSION_COOKIE, &id, ttl.as_secs());
    Ok(response)
}

async fn logout(
    State(state): State<Arc<OidcAccess>>,
    headers: HeaderMap,
) -> Result<Response, Denial> {
    let (id, _) = state.local_session(&headers, true).await?;
    if let Some(session) = state.db(move |store| store.remove(&id)).await?
        && state
            .provider
            .revoke(
                &session.credentials.access_token,
                session.credentials.refresh_token.as_deref(),
            )
            .await
            .is_err()
    {
        tracing::warn!("Authentik token revocation failed after local logout");
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    expire_cookie(&mut response, SESSION_COOKIE);
    Ok(response)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeSubject {
    subject: String,
}

async fn revoke_subject(
    State(state): State<Arc<OidcAccess>>,
    headers: HeaderMap,
    Json(request): Json<RevokeSubject>,
) -> Result<Response, Denial> {
    let (identity, _) = state.browser(&headers, true).await?;
    if !identity.roles.iter().any(|role| role == "drive-admins") {
        return Err(Denial::Forbidden);
    }
    if request.subject.is_empty() || request.subject.len() > 255 {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let self_revoked = identity.subject == request.subject;
    state
        .db(move |store| store.revoke_subject(&request.subject))
        .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    if self_revoked {
        expire_cookie(&mut response, SESSION_COOKIE);
    }
    Ok(response)
}

fn invalid_callback() -> Response {
    (
        StatusCode::BAD_REQUEST,
        "Sign-in could not be verified. Open /files/ and try again.",
    )
        .into_response()
}

fn redirect(location: &str) -> Result<Response, Denial> {
    let location = HeaderValue::from_str(location).map_err(|_| Denial::Internal)?;
    Ok((StatusCode::SEE_OTHER, [(LOCATION, location)]).into_response())
}

fn set_private_headers(response: &mut Response) {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut found = None;
    for header in headers.get_all(COOKIE) {
        for part in header.to_str().ok()?.split(';') {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            if key == name && found.replace(value).is_some() {
                return None;
            }
        }
    }
    found.filter(|value| {
        value.len() == 43
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn set_cookie(response: &mut Response, name: &str, value: &str, max_age: u64) {
    let value =
        format!("{name}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax");
    let value = HeaderValue::from_str(&value).expect("cookie values are URL-safe random tokens");
    response.headers_mut().append(SET_COOKIE, value);
}

fn expire_cookie(response: &mut Response, name: &str) {
    set_cookie(response, name, "", 0);
}

fn single_header<'a>(headers: &'a HeaderMap, name: &HeaderName) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    values.next().is_none().then_some(value)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let (scheme, value) = single_header(headers, &AUTHORIZATION)?.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("Bearer")
        && !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_graphic()))
    .then_some(value)
}

fn origin_allowed(headers: &HeaderMap, origin: &str, required: bool) -> bool {
    if headers
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site" || value == "same-site")
    {
        return false;
    }
    if required || headers.contains_key(ORIGIN) {
        single_header(headers, &ORIGIN) == Some(origin)
    } else {
        true
    }
}

pub(super) fn read_secret(
    web: &Path,
    data: &Path,
    file: &Path,
    min: usize,
    max: usize,
) -> io::Result<String> {
    let canonical = file.canonicalize()?;
    if canonical.starts_with(data.canonicalize()?)
        || web
            .canonicalize()
            .is_ok_and(|web| canonical.starts_with(web))
        || !fs::metadata(&canonical)?.is_file()
    {
        return Err(io::Error::other(
            "secret must be a regular file outside the web and data directories",
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(canonical)?
        .take((max + 3) as u64)
        .read_to_end(&mut bytes)?;
    let trimmed = bytes.trim_ascii_end();
    if !(min..=max).contains(&trimmed.len())
        || bytes.len() > max + 2
        || !trimmed.iter().all(u8::is_ascii_graphic)
    {
        return Err(io::Error::other(
            "secret file has an invalid size or format",
        ));
    }
    String::from_utf8(trimmed.to_vec()).map_err(io::Error::other)
}
