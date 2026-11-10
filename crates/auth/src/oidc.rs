use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use openidconnect::{
    AccessTokenHash, AdditionalProviderMetadata, AsyncHttpClient, AuthorizationCode, ClientId,
    ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet, EndpointSet, HttpRequest,
    HttpResponse, IssuerUrl, Nonce, OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier,
    ProviderMetadata, RedirectUrl, RequestTokenError, Scope, TokenResponse,
    core::{
        CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreClaimName, CoreClaimType,
        CoreClient, CoreClientAuthMethod, CoreErrorResponseType, CoreGrantType, CoreJsonWebKey,
        CoreJweContentEncryptionAlgorithm, CoreJweKeyManagementAlgorithm, CoreJwsSigningAlgorithm,
        CoreResponseMode, CoreResponseType, CoreSubjectIdentifierType, CoreTokenType,
    },
    http::StatusCode,
    reqwest,
    url::{Host, Origin, Url},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use subtle::ConstantTimeEq;

const ROLE_SCOPE: &str = "arcdos";
const ROLES: [&str; 2] = ["drive-users", "drive-admins"];
const SIGNING_ALGORITHM: CoreJwsSigningAlgorithm = CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256;
const MAX_TOKEN_BYTES: usize = 16 * 1024;
const MAX_PROVIDER_RESPONSE: usize = 256 * 1024;

pub struct OidcConfig {
    pub issuer: String,
    pub public_origin: String,
    pub client_id: String,
    pub client_secret: String,
    pub android: Option<AndroidClientConfig>,
}

pub struct AndroidClientConfig {
    pub issuer: String,
    pub client_id: String,
}

pub struct LoginFlow {
    pub authorization_url: String,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub display_name: String,
    pub roles: Vec<String>,
}

pub struct Authenticated {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Duration,
    pub identity: Identity,
}

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("invalid OIDC configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("identity provider unavailable")]
    Unavailable,
    #[error("invalid or expired OIDC credential")]
    InvalidCredential,
    #[error("identity is not authorized for ArcDOS")]
    Forbidden,
}

type Result<T> = std::result::Result<T, OidcError>;

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AdditionalEndpoints {
    revocation_endpoint: Option<String>,
    introspection_endpoint: Option<String>,
}

impl AdditionalProviderMetadata for AdditionalEndpoints {}

type Metadata = ProviderMetadata<
    AdditionalEndpoints,
    CoreAuthDisplay,
    CoreClientAuthMethod,
    CoreClaimName,
    CoreClaimType,
    CoreGrantType,
    CoreJweContentEncryptionAlgorithm,
    CoreJweKeyManagementAlgorithm,
    CoreJsonWebKey,
    CoreResponseMode,
    CoreResponseType,
    CoreSubjectIdentifierType,
>;

pub struct OidcProvider {
    client: Client,
    http: ProviderHttp,
    public_origin: String,
    userinfo: Url,
    revocation: Url,
    client_id: ClientId,
    client_secret: ClientSecret,
    android: Option<AndroidProvider>,
}

struct AndroidProvider {
    issuer: String,
    client_id: String,
    introspection: Url,
}

impl OidcProvider {
    pub async fn discover(config: OidcConfig) -> Result<Self> {
        let issuer = checked_url(&config.issuer, true)?;
        let public_origin = checked_url(&config.public_origin, false)?;
        if public_origin.path() != "/" {
            return Err(OidcError::InvalidConfig(
                "public origin must not include a path",
            ));
        }
        if !valid_credential(&config.client_id, 255)
            || !valid_credential(&config.client_secret, 4096)
        {
            return Err(OidcError::InvalidConfig("invalid client ID or secret"));
        }
        let client_id = ClientId::new(config.client_id);
        let client_secret = ClientSecret::new(config.client_secret);
        let issuer_url = IssuerUrl::new(config.issuer)
            .map_err(|_| OidcError::InvalidConfig("invalid issuer"))?;
        let redirect = public_origin
            .join("/auth/callback")
            .map_err(|_| OidcError::InvalidConfig("invalid callback URL"))?;
        let redirect = RedirectUrl::new(redirect.to_string())
            .map_err(|_| OidcError::InvalidConfig("invalid callback URL"))?;
        let http = ProviderHttp::new(issuer.origin())?;
        let metadata = Metadata::discover_async(issuer_url, &http)
            .await
            .map_err(|_| OidcError::Unavailable)?;
        endpoint(metadata.authorization_endpoint().as_str(), &http.origin)?;
        endpoint(
            metadata
                .token_endpoint()
                .ok_or(OidcError::InvalidConfig("missing token endpoint"))?
                .as_str(),
            &http.origin,
        )?;
        let userinfo = endpoint(
            metadata
                .userinfo_endpoint()
                .ok_or(OidcError::InvalidConfig("missing UserInfo endpoint"))?
                .as_str(),
            &http.origin,
        )?;
        let revocation = endpoint(
            metadata
                .additional_metadata()
                .revocation_endpoint
                .as_deref()
                .ok_or(OidcError::InvalidConfig("missing revocation endpoint"))?,
            &http.origin,
        )?;
        if !metadata
            .id_token_signing_alg_values_supported()
            .contains(&SIGNING_ALGORITHM)
        {
            return Err(OidcError::InvalidConfig(
                "select an RS256 signing key in Authentik",
            ));
        }
        let android = config
            .android
            .map(|android| {
                endpoint(&android.issuer, &http.origin)?;
                if !valid_credential(&android.client_id, 255)
                    || android.client_id == client_id.as_str()
                {
                    return Err(OidcError::InvalidConfig(
                        "Android must have a separate client ID",
                    ));
                }
                let introspection = endpoint(
                    metadata
                        .additional_metadata()
                        .introspection_endpoint
                        .as_deref()
                        .ok_or(OidcError::InvalidConfig("missing introspection endpoint"))?,
                    &http.origin,
                )?;
                Ok(AndroidProvider {
                    issuer: android.issuer,
                    client_id: android.client_id,
                    introspection,
                })
            })
            .transpose()?;
        let client = CoreClient::from_provider_metadata(
            metadata,
            client_id.clone(),
            Some(client_secret.clone()),
        )
        .set_redirect_uri(redirect);
        Ok(Self {
            client,
            http,
            public_origin: public_origin.origin().ascii_serialization(),
            userinfo,
            revocation,
            client_id,
            client_secret,
            android,
        })
    }

    pub fn public_origin(&self) -> &str {
        &self.public_origin
    }

    pub fn start_login(&self) -> LoginFlow {
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = self
            .client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new(ROLE_SCOPE.to_owned()))
            .add_prompt(CoreAuthPrompt::Login)
            .set_pkce_challenge(challenge)
            .url();
        LoginFlow {
            authorization_url: url.to_string(),
            state: state.into_secret(),
            nonce: nonce.into_secret(),
            pkce_verifier: verifier.into_secret(),
        }
    }

    /// Exchange the one-time code after the caller has consumed the matching login state.
    pub async fn exchange(
        &self,
        code: &str,
        nonce: &str,
        pkce_verifier: &str,
    ) -> Result<Authenticated> {
        if !valid_credential(code, MAX_TOKEN_BYTES) {
            return Err(OidcError::InvalidCredential);
        }
        let exchange_started = Instant::now();
        let tokens = self
            .client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .map_err(|_| OidcError::Unavailable)?
            .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier.to_owned()))
            .request_async(&self.http)
            .await
            .map_err(|error| match error {
                RequestTokenError::ServerResponse(response)
                    if *response.error() == CoreErrorResponseType::InvalidGrant =>
                {
                    OidcError::InvalidCredential
                }
                _ => OidcError::Unavailable,
            })?;
        if tokens.token_type() != &CoreTokenType::Bearer {
            return Err(OidcError::InvalidCredential);
        }
        let id_token = tokens.id_token().ok_or(OidcError::InvalidCredential)?;
        let verifier = self
            .client
            .id_token_verifier()
            .set_allowed_algs([SIGNING_ALGORITHM]);
        let claims = id_token
            .claims(&verifier, &Nonce::new(nonce.to_owned()))
            .map_err(|_| OidcError::InvalidCredential)?;
        if claims.audiences().len() != 1
            || claims.audiences()[0].as_str() != self.client_id.as_str()
            || claims
                .authorized_party()
                .is_some_and(|party| party != &self.client_id)
        {
            return Err(OidcError::InvalidCredential);
        }
        if let Some(expected) = claims.access_token_hash() {
            let algorithm = id_token
                .signing_alg()
                .map_err(|_| OidcError::InvalidCredential)?;
            let key = id_token
                .signing_key(&verifier)
                .map_err(|_| OidcError::InvalidCredential)?;
            let actual = AccessTokenHash::from_token(tokens.access_token(), algorithm, key)
                .map_err(|_| OidcError::InvalidCredential)?;
            if !bool::from(actual.as_bytes().ct_eq(expected.as_bytes())) {
                return Err(OidcError::InvalidCredential);
            }
        }
        let identity = self
            .identity(tokens.access_token().secret(), claims.subject().as_str())
            .await?;
        let expiration = u64::try_from(claims.expiration().timestamp())
            .map_err(|_| OidcError::InvalidCredential)?;
        let id_token_remaining = Duration::from_secs(expiration.saturating_sub(unix_time()?));
        let expires_in = tokens
            .expires_in()
            .and_then(|duration| duration.checked_sub(exchange_started.elapsed()))
            .map(|duration| duration.min(id_token_remaining))
            .filter(|duration| !duration.is_zero())
            .ok_or(OidcError::InvalidCredential)?;
        Ok(Authenticated {
            access_token: tokens.access_token().secret().clone(),
            refresh_token: tokens.refresh_token().map(|token| token.secret().clone()),
            expires_in,
            identity,
        })
    }

    /// Re-read authorization state from the provider on every protected request.
    pub async fn identity(&self, access_token: &str, expected_subject: &str) -> Result<Identity> {
        if !valid_credential(access_token, MAX_TOKEN_BYTES) || !valid_subject(expected_subject) {
            return Err(OidcError::InvalidCredential);
        }
        let response = self
            .http
            .client
            .get(self.userinfo.clone())
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|_| OidcError::Unavailable)?;
        if matches!(
            response.status(),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
        ) {
            return Err(OidcError::InvalidCredential);
        }
        let claims: UserInfo = parse_response(response).await?;
        claims.authorize(expected_subject)
    }

    /// Validate an Android public-client token with the confidential web client.
    pub async fn android_identity(&self, access_token: &str) -> Result<Identity> {
        let android = self.android.as_ref().ok_or(OidcError::InvalidCredential)?;
        if !valid_credential(access_token, MAX_TOKEN_BYTES) {
            return Err(OidcError::InvalidCredential);
        }
        let response = self
            .http
            .client
            .post(android.introspection.clone())
            .form(&[
                ("token", access_token),
                ("token_type_hint", "access_token"),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.secret().as_str()),
            ])
            .send()
            .await
            .map_err(|_| OidcError::Unavailable)?;
        let claims: Introspection = parse_response(response).await?;
        let subject = claims.android_subject(android, unix_time()?)?;
        // Authentik introspects refresh tokens too. UserInfo accepts only access tokens.
        self.identity(access_token, &subject).await
    }

    pub async fn revoke(&self, access_token: &str, refresh_token: Option<&str>) -> Result<()> {
        let access = self.revoke_token(access_token, "access_token").await;
        let refresh = match refresh_token {
            Some(token) => self.revoke_token(token, "refresh_token").await,
            None => Ok(()),
        };
        access.and(refresh)
    }

    async fn revoke_token(&self, token: &str, hint: &str) -> Result<()> {
        if !valid_credential(token, MAX_TOKEN_BYTES) {
            return Err(OidcError::InvalidCredential);
        }
        let response = self
            .http
            .client
            .post(self.revocation.clone())
            .form(&[
                ("token", token),
                ("token_type_hint", hint),
                ("client_id", self.client_id.as_str()),
                ("client_secret", self.client_secret.secret().as_str()),
            ])
            .send()
            .await
            .map_err(|_| OidcError::Unavailable)?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(OidcError::Unavailable)
        }
    }
}

#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    arcdos_active: Option<bool>,
    #[serde(default)]
    arcdos_groups: Vec<String>,
    arcdos_display_name: Option<String>,
}

impl UserInfo {
    fn authorize(self, subject: &str) -> Result<Identity> {
        if self.sub != subject {
            return Err(OidcError::InvalidCredential);
        }
        if self.arcdos_active != Some(true) {
            return Err(OidcError::Forbidden);
        }
        let roles: Vec<_> = ROLES
            .into_iter()
            .filter(|role| self.arcdos_groups.iter().any(|group| group == role))
            .map(str::to_owned)
            .collect();
        if roles.is_empty() {
            return Err(OidcError::Forbidden);
        }
        let display_name: String = self
            .arcdos_display_name
            .unwrap_or_default()
            .trim()
            .chars()
            .filter(|character| !character.is_control())
            .take(128)
            .collect();
        Ok(Identity {
            display_name: if display_name.is_empty() {
                self.sub.clone()
            } else {
                display_name
            },
            subject: self.sub,
            roles,
        })
    }
}

#[derive(Deserialize)]
struct Introspection {
    active: bool,
    client_id: Option<String>,
    iss: Option<String>,
    aud: Option<Audience>,
    exp: Option<u64>,
    sub: Option<String>,
    scope: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

impl Audience {
    fn matches(&self, client_id: &str) -> bool {
        match self {
            Self::One(audience) => audience == client_id,
            Self::Many(audiences) => audiences.len() == 1 && audiences[0] == client_id,
        }
    }
}

impl Introspection {
    fn android_subject(self, android: &AndroidProvider, now: u64) -> Result<String> {
        if !self.active
            || self.client_id.as_deref() != Some(android.client_id.as_str())
            || self.iss.as_deref() != Some(android.issuer.as_str())
            || !self
                .aud
                .is_some_and(|audience| audience.matches(&android.client_id))
            || !self.exp.is_some_and(|expires| expires > now)
            || !self.scope.is_some_and(|scope| {
                let mut scopes = scope.split_ascii_whitespace();
                scopes.clone().any(|scope| scope == "openid")
                    && scopes.any(|scope| scope == ROLE_SCOPE)
            })
        {
            return Err(OidcError::InvalidCredential);
        }
        self.sub
            .filter(|subject| valid_subject(subject))
            .ok_or(OidcError::InvalidCredential)
    }
}

struct ProviderHttp {
    client: reqwest::Client,
    origin: Origin,
}

impl ProviderHttp {
    fn new(origin: Origin) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|_| OidcError::Unavailable)?;
        Ok(Self { client, origin })
    }
}

impl<'a> AsyncHttpClient<'a> for ProviderHttp {
    type Error = OidcError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse>> + Send + 'a>>;

    fn call(&'a self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let (parts, body) = request.into_parts();
            // Discovery follows jwks_uri. Restrict it before sending any request.
            let url = endpoint(&parts.uri.to_string(), &self.origin)?;
            let response = self
                .client
                .request(parts.method, url)
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(|_| OidcError::Unavailable)?;
            let status = response.status();
            let headers = response.headers().clone();
            let mut output = HttpResponse::new(read_body(response).await?);
            *output.status_mut() = status;
            *output.headers_mut() = headers;
            Ok(output)
        })
    }
}

fn checked_url(value: &str, allow_loopback: bool) -> Result<Url> {
    let url = Url::parse(value).map_err(|_| OidcError::InvalidConfig("invalid URL"))?;
    let loopback = match url.host() {
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || (allow_loopback && url.scheme() == "http" && loopback))
    {
        return Err(OidcError::InvalidConfig(
            "URL must use HTTPS and have no credentials, query, or fragment",
        ));
    }
    Ok(url)
}

fn endpoint(value: &str, origin: &Origin) -> Result<Url> {
    let url = checked_url(value, true)?;
    if &url.origin() != origin {
        return Err(OidcError::InvalidConfig(
            "provider endpoints must share the issuer origin",
        ));
    }
    Ok(url)
}

async fn parse_response<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    if !response.status().is_success() {
        return Err(OidcError::Unavailable);
    }
    serde_json::from_slice(&read_body(response).await?).map_err(|_| OidcError::Unavailable)
}

async fn read_body(mut response: reqwest::Response) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|size| size > MAX_PROVIDER_RESPONSE as u64)
    {
        return Err(OidcError::Unavailable);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| OidcError::Unavailable)? {
        if chunk.len() > MAX_PROVIDER_RESPONSE - body.len() {
            return Err(OidcError::Unavailable);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn unix_time() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| OidcError::Unavailable)
}

fn valid_credential(value: &str, maximum: usize) -> bool {
    !value.is_empty() && value.len() <= maximum && value.bytes().all(|byte| byte.is_ascii_graphic())
}

fn valid_subject(value: &str) -> bool {
    valid_credential(value, 255)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn provider_urls_require_https_and_match_the_issuer_origin() {
        let issuer = checked_url("https://auth.example.test/application/o/web/", true).unwrap();
        let origin = issuer.origin();
        assert!(endpoint("https://auth.example.test/application/o/token/", &origin).is_ok());
        for url in [
            "http://auth.example.test/",
            "https://auth.example.test:8443/",
            "https://auth.example.test.other/",
            "https://auth.example.test@other.example/",
            "https://user:secret@auth.example.test/",
            "https://auth.example.test/?next=https://other.example/",
            "https://auth.example.test/#fragment",
        ] {
            assert!(endpoint(url, &origin).is_err(), "{url}");
        }
        for host in ["127.0.0.1", "[::1]"] {
            let url = format!("http://{host}:8080/");
            assert!(checked_url(&url, true).is_ok());
            assert!(checked_url(&url, false).is_err());
        }
        assert!(checked_url("http://localhost:8080/", true).is_err());
    }

    #[test]
    fn live_userinfo_requires_matching_subject_active_account_and_arc_groups() {
        let mut claims = json!({
            "sub": "subject",
            "arcdos_active": true,
            "arcdos_groups": ["drive-admins", "unrelated", "drive-users", "drive-users"],
            "arcdos_display_name": " Alice\n ",
        });
        let parse = |claims| {
            serde_json::from_value::<UserInfo>(claims)
                .unwrap()
                .authorize("subject")
        };
        let identity = parse(claims.clone()).unwrap();
        assert_eq!(identity.subject, "subject");
        assert_eq!(identity.display_name, "Alice");
        assert_eq!(identity.roles, ["drive-users", "drive-admins"]);

        claims["sub"] = json!("different");
        assert!(matches!(
            parse(claims.clone()),
            Err(OidcError::InvalidCredential)
        ));
        claims["sub"] = json!("subject");
        claims["arcdos_active"] = json!(false);
        assert!(matches!(parse(claims.clone()), Err(OidcError::Forbidden)));
        claims["arcdos_active"] = json!(true);
        claims["arcdos_groups"] = json!(["Drive-Admins", "drive-users-other"]);
        assert!(matches!(parse(claims), Err(OidcError::Forbidden)));
        assert!(matches!(
            parse(json!({"sub": "subject"})),
            Err(OidcError::Forbidden)
        ));
    }

    #[test]
    fn android_introspection_binds_the_token_to_the_public_client() {
        let android = AndroidProvider {
            issuer: "https://auth.example.test/application/o/android/".into(),
            client_id: "arc-android".into(),
            introspection: Url::parse("https://auth.example.test/application/o/introspect/")
                .unwrap(),
        };
        let claims = json!({
            "active": true,
            "client_id": android.client_id,
            "iss": android.issuer,
            "aud": android.client_id,
            "exp": 123,
            "sub": "subject",
            "scope": "arcdos openid",
        });
        let parse = |claims| {
            serde_json::from_value::<Introspection>(claims)
                .unwrap()
                .android_subject(&android, 122)
        };
        assert_eq!(parse(claims.clone()).unwrap(), "subject");
        let mut array_audience = claims.clone();
        array_audience["aud"] = json!([android.client_id]);
        assert_eq!(parse(array_audience).unwrap(), "subject");
        for (field, value) in [
            ("active", json!(false)),
            ("client_id", json!("arc-web")),
            ("iss", json!("https://auth.example.test/application/o/web/")),
            ("aud", json!("arc-web")),
            ("aud", json!(["arc-web", "arc-android"])),
            ("exp", json!(122)),
            ("scope", json!("openid arcdos-other")),
            ("scope", json!("arcdos")),
            ("sub", json!("")),
        ] {
            let mut invalid = claims.clone();
            invalid[field] = value;
            assert!(
                matches!(parse(invalid), Err(OidcError::InvalidCredential)),
                "{field}"
            );
        }
    }
}
