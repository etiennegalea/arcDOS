mod catalog;
mod http;
mod rpc;
mod store;

use std::{
    io,
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use arcdos_transfers::{TransferError, Uploads};
use axum::{Router, http::StatusCode};
use store::Store;
use tokio::sync::Mutex;

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

pub(super) fn router(web_dir: &Path, data: &Path) -> Result<Router> {
    let data_root = data.canonicalize()?;
    if let Ok(web) = web_dir.canonicalize()
        && (web.starts_with(&data_root) || data_root.starts_with(&web))
    {
        return Err(Error::Invalid(
            "web assets and data directories must be separate",
        ));
    }
    let store = Store::open(data)?;
    let uploads = Uploads::open(&data.join("state/transfers"))?;
    let api = Arc::new(FileApi {
        store: Arc::new(Mutex::new(store)),
        uploads,
    });
    Ok(rpc::router(api.clone()).merge(http::router(api)))
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
