//! Persistent, bounded staging for resumable uploads on Linux.

#![cfg(target_os = "linux")]

mod state;

use std::{
    collections::HashMap,
    fs::{File, Metadata},
    io::{self, Read, SeekFrom},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use state::{Manifest, State};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    sync::{Mutex as AsyncMutex, OwnedMutexGuard},
};
use uuid::Uuid;

pub const MAX_CHUNK_BYTES: u64 = 16 * 1024 * 1024;
const BUFFER_BYTES: usize = 64 * 1024;
const CHUNK_TIMEOUT: Duration = Duration::from_secs(90);

type Result<T> = std::result::Result<T, TransferError>;

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("upload does not exist")]
    NotFound,
    #[error("another operation owns the upload")]
    Busy,
    #[error("upload offset differs from committed offset {expected}")]
    Offset { expected: u64 },
    #[error("upload length is invalid")]
    InvalidLength,
    #[error("upload destination is invalid")]
    InvalidTarget,
    #[error("upload is incomplete")]
    Incomplete,
    #[error("uploaded content does not match the expected SHA-256 digest")]
    Checksum,
    #[error("upload conflicts with its current publish state")]
    Conflict,
    #[error("persistent upload state is inconsistent")]
    Corrupt,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UploadTarget {
    pub parent_id: String,
    pub name: String,
    pub parent_revision: String,
}

impl UploadTarget {
    fn valid(&self) -> bool {
        [&self.parent_id, &self.name, &self.parent_revision]
            .into_iter()
            .all(|value| !value.is_empty() && value.len() <= 255)
            && !self.name.contains(['/', '\\', '\0'])
            && !matches!(self.name.as_str(), "." | "..")
    }
}

#[derive(Clone, Debug)]
pub struct UploadSpec {
    pub target: UploadTarget,
    pub length: u64,
    pub sha256: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct Upload {
    pub id: Uuid,
    pub target: UploadTarget,
    pub length: u64,
    pub offset: u64,
    pub published: bool,
}

/// Identity of the exact staged inode verified before an atomic rename.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StageIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime_secs: i64,
    pub mtime_nsecs: i64,
    ctime_secs: i64,
    ctime_nsecs: i64,
}

impl StageIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
            size: metadata.len(),
            mtime_secs: metadata.mtime(),
            mtime_nsecs: metadata.mtime_nsec(),
            ctime_secs: metadata.ctime(),
            ctime_nsecs: metadata.ctime_nsec(),
        }
    }

    /// Compare identity across a rename, which can update the inode change time.
    pub fn matches(&self, metadata: &Metadata) -> bool {
        let current = Self::from_metadata(metadata);
        self.dev == current.dev
            && self.ino == current.ino
            && self.size == current.size
            && self.mtime_secs == current.mtime_secs
            && self.mtime_nsecs == current.mtime_nsecs
    }
}

#[derive(Clone)]
pub struct Uploads(Arc<Inner>);

struct Inner {
    state: State,
    locks: Mutex<HashMap<Uuid, Weak<AsyncMutex<()>>>>,
}

impl Uploads {
    /// Open a private staging directory and claim it for one process.
    pub fn open(directory: &Path) -> Result<Self> {
        Ok(Self(Arc::new(Inner {
            state: State::open(directory)?,
            locks: Mutex::new(HashMap::new()),
        })))
    }

    pub async fn create(&self, spec: UploadSpec) -> Result<Upload> {
        if spec.length > i64::MAX as u64 {
            return Err(TransferError::InvalidLength);
        }
        if !spec.target.valid() {
            return Err(TransferError::InvalidTarget);
        }
        let store = self.clone();
        blocking(move || store.0.state.create(spec)).await
    }

    pub async fn status(&self, id: Uuid) -> Result<Upload> {
        let store = self.clone();
        blocking(move || Ok(store.0.state.load(id)?.snapshot())).await
    }

    /// Commit one exact-length chunk at the current offset.
    pub async fn append(
        &self,
        id: Uuid,
        offset: u64,
        length: u64,
        body: impl AsyncRead + Unpin + Send + 'static,
    ) -> Result<Upload> {
        if length == 0 || length > MAX_CHUNK_BYTES {
            return Err(TransferError::InvalidLength);
        }
        let guard = self.acquire(id)?;
        let store = self.clone();
        // Tokio file writes can still be in flight after the HTTP handler is
        // cancelled. The worker owns the guard through flush and rollback.
        tokio::spawn(async move {
            let _guard = guard;
            store.append_locked(id, offset, length, body).await
        })
        .await
        .map_err(worker_error)?
    }

    pub async fn cancel(&self, id: Uuid) -> Result<()> {
        let guard = self.acquire(id)?;
        let store = self.clone();
        blocking(move || {
            let _guard = guard;
            store.0.state.cancel(id)
        })
        .await
    }

    /// Verify the staged bytes and lock the upload until publication completes.
    pub async fn prepare_commit(&self, id: Uuid) -> Result<PreparedUpload> {
        let guard = self.acquire(id)?;
        let store = self.clone();
        blocking(move || {
            let mut manifest = store.0.state.load(id)?;
            if manifest.offset != manifest.length {
                return Err(TransferError::Incomplete);
            }
            let staging_name = State::staging_name(id);
            let staged = store.0.state.open_file(&staging_name, false)?;
            let (identity, is_staged) = match staged {
                Some(file) if manifest.published_at.is_none() => {
                    let identity = verify(file, &manifest)?;
                    if manifest.identity != Some(identity) {
                        manifest.identity = Some(identity);
                        store.0.state.save(&manifest, false)?;
                    }
                    (identity, true)
                }
                None => (manifest.identity.ok_or(TransferError::Corrupt)?, false),
                Some(_) => return Err(TransferError::Corrupt),
            };
            Ok(PreparedUpload {
                upload: manifest.snapshot(),
                manifest,
                staging_name,
                identity,
                is_staged,
                store,
                _guard: guard,
            })
        })
        .await
    }

    fn acquire(&self, id: Uuid) -> Result<OwnedMutexGuard<()>> {
        let mut locks = self.0.locks.lock().map_err(worker_error)?;
        locks.retain(|_, lock| lock.strong_count() != 0);
        let slot = locks.entry(id).or_default();
        let lock = slot.upgrade().unwrap_or_else(|| {
            let lock = Arc::new(AsyncMutex::new(()));
            *slot = Arc::downgrade(&lock);
            lock
        });
        lock.try_lock_owned().map_err(|_| TransferError::Busy)
    }

    async fn append_locked(
        &self,
        id: Uuid,
        offset: u64,
        length: u64,
        mut body: impl AsyncRead + Unpin,
    ) -> Result<Upload> {
        let store = self.clone();
        let (mut manifest, file) = blocking(move || {
            let manifest = store.0.state.load(id)?;
            if manifest.identity.is_some() {
                return Err(TransferError::Conflict);
            }
            if offset != manifest.offset {
                return Err(TransferError::Offset {
                    expected: manifest.offset,
                });
            }
            if length > manifest.length - offset {
                return Err(TransferError::InvalidLength);
            }
            let file = store
                .0
                .state
                .open_file(&State::staging_name(id), true)?
                .ok_or(TransferError::Corrupt)?;
            let physical_length = file.metadata()?.len();
            if physical_length < offset {
                return Err(TransferError::Corrupt);
            }
            if physical_length > offset {
                // A crash can leave bytes past the last durable manifest.
                file.set_len(offset)?;
            }
            Ok((manifest, file))
        })
        .await?;
        let mut file = tokio::fs::File::from_std(file);
        file.seek(SeekFrom::Start(offset)).await?;
        let write_result =
            tokio::time::timeout(CHUNK_TIMEOUT, write_chunk(&mut body, &mut file, length))
                .await
                .unwrap_or_else(|_| {
                    Err(io::Error::new(io::ErrorKind::TimedOut, "upload chunk timed out").into())
                });
        if let Err(error) = write_result {
            // Even when rollback encounters a storage error, the prior
            // manifest remains authoritative for the next retry.
            file.flush().await?;
            file.set_len(offset).await?;
            file.sync_data().await?;
            return Err(error);
        }
        file.flush().await?;
        file.sync_data().await?;
        manifest.offset += length;
        let store = self.clone();
        blocking(move || {
            store.0.state.save(&manifest, false)?;
            Ok(manifest.snapshot())
        })
        .await
    }
}

/// Holds the upload lock while the caller publishes or reconciles a prior rename.
pub struct PreparedUpload {
    upload: Upload,
    manifest: Manifest,
    staging_name: String,
    identity: StageIdentity,
    is_staged: bool,
    store: Uploads,
    _guard: OwnedMutexGuard<()>,
}

impl PreparedUpload {
    pub fn upload(&self) -> &Upload {
        &self.upload
    }

    pub fn staging_name(&self) -> &str {
        &self.staging_name
    }

    pub fn identity(&self) -> StageIdentity {
        self.identity
    }

    pub fn is_staged(&self) -> bool {
        self.is_staged
    }

    /// Confirm publication only after the caller verifies the destination.
    pub async fn finish(self) -> Result<()> {
        blocking(move || {
            let mut prepared = self;
            if prepared
                .store
                .0
                .state
                .open_file(&prepared.staging_name, false)?
                .is_some()
            {
                return Err(TransferError::Conflict);
            }
            if prepared.manifest.published_at.is_none() {
                prepared.manifest.published_at = Some(state::unix_seconds());
                prepared.store.0.state.save(&prepared.manifest, false)?;
            }
            Ok(())
        })
        .await
    }
}

async fn write_chunk(
    body: &mut (impl AsyncRead + Unpin),
    file: &mut tokio::fs::File,
    mut remaining: u64,
) -> Result<()> {
    let mut buffer = vec![0; BUFFER_BYTES];
    while remaining != 0 {
        let count = buffer.len().min(remaining as usize);
        let read = body.read(&mut buffer[..count]).await?;
        if read == 0 {
            return Err(TransferError::InvalidLength);
        }
        file.write_all(&buffer[..read]).await?;
        remaining -= read as u64;
    }
    if body.read(&mut buffer[..1]).await? != 0 {
        return Err(TransferError::InvalidLength);
    }
    Ok(())
}

fn verify(mut file: File, manifest: &Manifest) -> Result<StageIdentity> {
    let identity = StageIdentity::from_metadata(&file.metadata()?);
    if identity.size != manifest.length {
        return Err(TransferError::Corrupt);
    }
    if manifest.identity == Some(identity) {
        return Ok(identity);
    }

    let mut digest = Sha256::new();
    let mut buffer = vec![0; BUFFER_BYTES];
    let mut remaining = manifest.length;
    while remaining != 0 {
        let count = (remaining.min(buffer.len() as u64)) as usize;
        file.read_exact(&mut buffer[..count])?;
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    let sha256: [u8; 32] = digest.finalize().into();
    if StageIdentity::from_metadata(&file.metadata()?) != identity {
        return Err(TransferError::Conflict);
    }
    if sha256 != manifest.sha256 {
        return Err(TransferError::Checksum);
    }
    Ok(identity)
}

async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(worker_error)?
}

fn worker_error(_: impl std::fmt::Display) -> TransferError {
    io::Error::other("upload worker failed").into()
}
