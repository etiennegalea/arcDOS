use std::{env, path::Path, sync::Arc, time::Duration};

use reqwest::{Client, Response, Url, header};
use serde::{Deserialize, de::DeserializeOwned};
use tokio::sync::Semaphore;

const MAX_TEXTS: usize = 16;
const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_DIMENSIONS: usize = 4096;

#[derive(Debug, thiserror::Error)]
pub(super) enum AiError {
    #[error("invalid local AI configuration")]
    Configuration,
    #[error("local AI worker is unavailable")]
    Unavailable,
    #[error("local AI worker returned an invalid response")]
    InvalidResponse,
    #[error("invalid embedding input")]
    InvalidInput,
}

#[derive(Clone)]
pub(super) struct AiClient {
    http: Client,
    origin: Url,
    embed_slot: Arc<Semaphore>,
}

#[derive(Deserialize)]
pub(super) struct EmbeddingModel {
    pub model: String,
    pub dimensions: usize,
}

#[derive(Deserialize)]
struct Health {
    ready: bool,
    embedding: EmbeddingModel,
}

#[derive(Deserialize)]
pub(super) struct Embeddings {
    pub model: String,
    pub vectors: Vec<Vec<f32>>,
}

impl AiClient {
    pub fn configured(web: &Path, data: &Path) -> Result<Option<Self>, AiError> {
        let origin = env::var_os("ARCDOS_AI_WORKER_URL");
        let token = env::var_os("ARCDOS_AI_TOKEN_FILE");
        match (origin, token) {
            (None, None) => Ok(None),
            (Some(origin), Some(token)) => {
                let token = crate::auth::read_secret(web, data, Path::new(&token), 32, 256)
                    .map_err(|_| AiError::Configuration)?;
                Self::new(origin.to_str().ok_or(AiError::Configuration)?, &token).map(Some)
            }
            _ => Err(AiError::Configuration),
        }
    }

    pub fn new(origin: &str, token: &str) -> Result<Self, AiError> {
        let origin = Url::parse(origin).map_err(|_| AiError::Configuration)?;
        if origin.scheme() != "http"
            || !matches!(origin.host_str(), Some("127.0.0.1" | "[::1]" | "ai-worker"))
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
            || !(32..=256).contains(&token.len())
            || !token
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            return Err(AiError::Configuration);
        }
        let mut authorization = header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| AiError::Configuration)?;
        authorization.set_sensitive(true);
        let mut headers = header::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, authorization);
        let http = Client::builder()
            .default_headers(headers)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| AiError::Configuration)?;
        Ok(Self {
            http,
            origin,
            embed_slot: Arc::new(Semaphore::new(1)),
        })
    }

    pub async fn health(&self) -> Result<EmbeddingModel, AiError> {
        let response = self.http.get(self.endpoint("/healthz")).send().await;
        let health: Health = parse(response, 4096).await?;
        if !health.ready
            || !valid_model(&health.embedding.model)
            || !(1..=MAX_DIMENSIONS).contains(&health.embedding.dimensions)
        {
            return Err(AiError::InvalidResponse);
        }
        Ok(health.embedding)
    }

    pub async fn embed(&self, texts: &[String]) -> Result<Embeddings, AiError> {
        if !(1..=MAX_TEXTS).contains(&texts.len()) {
            return Err(AiError::InvalidInput);
        }
        let body = serde_json::to_vec(&serde_json::json!({ "texts": texts }))
            .map_err(|_| AiError::InvalidInput)?;
        if body.len() > MAX_REQUEST_BYTES {
            return Err(AiError::InvalidInput);
        }
        let _slot = self
            .embed_slot
            .try_acquire()
            .map_err(|_| AiError::Unavailable)?;
        let response = self
            .http
            .post(self.endpoint("/v1/embed"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        let output: Embeddings = parse(response, MAX_RESPONSE_BYTES).await?;
        if !valid_model(&output.model)
            || output.vectors.len() != texts.len()
            || output.vectors.iter().any(|vector| {
                !(1..=MAX_DIMENSIONS).contains(&vector.len())
                    || vector.len() != output.vectors[0].len()
                    || vector.iter().any(|value| !value.is_finite())
                    || vector.iter().all(|value| *value == 0.0)
            })
        {
            return Err(AiError::InvalidResponse);
        }
        Ok(output)
    }

    fn endpoint(&self, path: &str) -> Url {
        let mut url = self.origin.clone();
        url.set_path(path);
        url
    }
}

fn valid_model(model: &str) -> bool {
    (1..=128).contains(&model.len()) && model.bytes().all(|c| c.is_ascii_graphic())
}

async fn parse<T: DeserializeOwned>(
    response: Result<Response, reqwest::Error>,
    limit: usize,
) -> Result<T, AiError> {
    let mut response = response.map_err(|_| AiError::Unavailable)?;
    if !response.status().is_success() {
        return Err(AiError::Unavailable);
    }
    if response
        .content_length()
        .is_some_and(|len| len > limit as u64)
    {
        return Err(AiError::InvalidResponse);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AiError::Unavailable)? {
        if chunk.len() > limit - body.len() {
            return Err(AiError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| AiError::InvalidResponse)
}
