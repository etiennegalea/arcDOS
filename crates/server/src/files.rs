mod catalog;
mod http;
mod rpc;
mod store;

use std::{
    fs,
    io::{self, Read},
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use arcdos_transfers::{TransferError, Uploads};
use axum::{
    Router,
    extract::{Request, State},
    http::{
        HeaderValue, StatusCode,
        header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, WWW_AUTHENTICATE},
    },
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use store::Store;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

const MAX_TOKEN_LENGTH: usize = 256;

pub(super) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("entry does not exist")]
    NotFound,
    #[error("destination already exists")]
    Exists,
    #[error("revision or page token is stale")]
    Stale,
    #[error("{0}")]
    Conflict(&'static str),
    #[error("storage access denied")]
    Forbidden,
    #[error("storage is full")]
    Exhausted,
    #[error("persistent state is inconsistent")]
    Inconsistent,
    #[error(transparent)]
    Storage(io::Error),
    #[error(transparent)]
    Catalog(#[from] rusqlite::Error),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

impl Error {
    fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) => StatusCode::BAD_REQUEST,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Exists | Self::Stale | Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Exhausted => StatusCode::INSUFFICIENT_STORAGE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn message(&self) -> &'static str {
        match self {
            Self::Invalid(message) | Self::Conflict(message) => message,
            Self::NotFound => "entry does not exist",
            Self::Exists => "destination already exists",
            Self::Stale => "revision or page token is stale",
            Self::Forbidden => "storage access denied",
            Self::Exhausted => "storage is full",
            _ => "storage operation failed",
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        use io::ErrorKind::*;
        match error.kind() {
            InvalidInput => Self::Invalid("invalid filesystem input"),
            NotFound | NotADirectory => Self::NotFound,
            AlreadyExists => Self::Exists,
            PermissionDenied => Self::Forbidden,
            WouldBlock => Self::Stale,
            CrossesDevices => Self::Conflict("cross-filesystem moves are unavailable"),
            StorageFull | QuotaExceeded | FileTooLarge => Self::Exhausted,
            _ => Self::Storage(error),
        }
    }
}

impl From<TransferError> for Error {
    fn from(error: TransferError) -> Self {
        match error {
            TransferError::NotFound => Self::NotFound,
            TransferError::Busy => Self::Conflict("upload is busy"),
            TransferError::Offset { .. } => Self::Conflict("upload offset is stale"),
            TransferError::InvalidLength => Self::Invalid("invalid upload length"),
            TransferError::InvalidTarget => Self::Invalid("invalid upload destination"),
            TransferError::Incomplete => Self::Conflict("upload is incomplete"),
            TransferError::Checksum => Self::Invalid("upload checksum mismatch"),
            TransferError::Conflict => Self::Conflict("upload state conflicts with this operation"),
            TransferError::Corrupt => Self::Inconsistent,
            TransferError::Io(error) => error.into(),
        }
    }
}

pub(super) struct FileApi {
    store: Arc<Mutex<Store>>,
    uploads: Uploads,
}

impl FileApi {
    async fn run<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&Store) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            store.recover()?;
            operation(&store)
        })
        .await
        .map_err(|_| Error::Inconsistent)?
    }
}

#[derive(Clone)]
struct AccessKey([u8; 32]);

pub(super) fn router(web_dir: &Path, data: &Path, token_file: &Path) -> Result<Router> {
    let data_root = data.canonicalize()?;
    if let Ok(web) = web_dir.canonicalize()
        && (web.starts_with(&data_root) || data_root.starts_with(&web))
    {
        return Err(Error::Invalid(
            "web assets and data directories must be separate",
        ));
    }
    if let Ok(token_path) = token_file.canonicalize()
        && (token_path.starts_with(&data_root)
            || web_dir
                .canonicalize()
                .is_ok_and(|web| token_path.starts_with(web)))
    {
        return Err(Error::Invalid(
            "token file must be outside the web and data directories",
        ));
    }
    if !fs::metadata(token_file)?.is_file() {
        return Err(Error::Invalid("token file must be a regular file"));
    }
    let mut token = Vec::new();
    fs::File::open(token_file)?
        .take((MAX_TOKEN_LENGTH + 3) as u64)
        .read_to_end(&mut token)?;
    let secret = token.trim_ascii_end();
    if !(32..=MAX_TOKEN_LENGTH).contains(&secret.len())
        || token.len() > MAX_TOKEN_LENGTH + 2
        || !secret.iter().all(u8::is_ascii_graphic)
    {
        return Err(Error::Invalid(
            "development token must be 32 to 256 printable ASCII bytes",
        ));
    }
    let key = AccessKey(Sha256::digest(secret).into());
    let store = Store::open(data)?;
    let uploads = Uploads::open(&data.join("state/transfers"))?;
    let api = Arc::new(FileApi {
        store: Arc::new(Mutex::new(store)),
        uploads,
    });
    Ok(rpc::router(api.clone())
        .merge(http::router(api))
        .route_layer(middleware::from_fn_with_state(key, authorize)))
}

async fn authorize(State(key): State<AccessKey>, request: Request, next: Next) -> Response {
    let mut values = request.headers().get_all(AUTHORIZATION).iter();
    let allowed = values
        .next()
        .filter(|_| values.next().is_none())
        .and_then(|value| value.as_bytes().split_at_checked(7))
        .filter(|(scheme, token)| {
            scheme.eq_ignore_ascii_case(b"Bearer ")
                && (32..=MAX_TOKEN_LENGTH).contains(&token.len())
        })
        .is_some_and(|(_, token)| bool::from(key.0.ct_eq(Sha256::digest(token).as_slice())));
    let mut response = if allowed {
        next.run(request).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [
                (CONTENT_TYPE, "application/json"),
                (WWW_AUTHENTICATE, "Bearer"),
            ],
            r#"{"code":"unauthenticated","message":"development access token required"}"#,
        )
            .into_response()
    };
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn timestamp_parts(time: SystemTime) -> (i64, i32) {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => (duration.as_secs() as i64, duration.subsec_nanos() as i32),
        Err(error) => {
            let duration = error.duration();
            let nanos = duration.subsec_nanos() as i32;
            let seconds = -(duration.as_secs() as i64);
            if nanos == 0 {
                (seconds, 0)
            } else {
                (seconds - 1, 1_000_000_000 - nanos)
            }
        }
    }
}
