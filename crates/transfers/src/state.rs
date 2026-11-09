use std::{
    fs::{DirBuilder, File, TryLockError},
    io::{self, Read},
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use rustix::fs::{self, AtFlags, Dir, Mode, OFlags, RenameFlags};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Result, StageIdentity, TransferError, Upload, UploadSpec, UploadTarget};

const MANIFEST_VERSION: u8 = 1;
const MANIFEST_BYTES: u64 = 8192;
const RECEIPT_RETENTION_SECONDS: u64 = 24 * 60 * 60;
const PRIVATE_FILE: Mode = Mode::from_bits_truncate(0o600);

pub(crate) struct State {
    directory: File,
    _owner_lock: File,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct Manifest {
    version: u8,
    id: Uuid,
    target: UploadTarget,
    pub length: u64,
    pub offset: u64,
    pub sha256: [u8; 32],
    pub identity: Option<StageIdentity>,
    pub published_at: Option<u64>,
}

impl Manifest {
    pub fn snapshot(&self) -> Upload {
        Upload {
            id: self.id,
            target: self.target.clone(),
            length: self.length,
            offset: self.offset,
            published: self.published_at.is_some(),
        }
    }

    fn valid(&self, id: Uuid) -> bool {
        self.version == MANIFEST_VERSION
            && self.id == id
            && self.length <= i64::MAX as u64
            && self.offset <= self.length
            && self.target.valid()
            && self
                .identity
                .is_none_or(|identity| self.offset == self.length && self.length == identity.size)
            && (self.published_at.is_none() || self.identity.is_some())
    }
}

impl State {
    pub fn open(path: &Path) -> Result<Self> {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let directory = File::from(
            fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
        if directory.metadata()?.mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the upload directory must be private",
            )
            .into());
        }
        let owner_lock = open_regular(
            &directory,
            ".lock",
            OFlags::CREATE | OFlags::RDWR,
            PRIVATE_FILE,
        )?;
        match owner_lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(TransferError::Busy),
            Err(TryLockError::Error(error)) => return Err(error.into()),
        }
        let state = Self {
            directory,
            _owner_lock: owner_lock,
        };
        state.recover()?;
        Ok(state)
    }

    pub fn create(&self, spec: UploadSpec) -> Result<Upload> {
        let manifest = Manifest {
            version: MANIFEST_VERSION,
            id: Uuid::new_v4(),
            target: spec.target,
            length: spec.length,
            offset: 0,
            sha256: spec.sha256,
            identity: None,
            published_at: None,
        };
        let staging_name = Self::staging_name(manifest.id);
        open_regular(
            &self.directory,
            &staging_name,
            OFlags::CREATE | OFlags::EXCL | OFlags::RDWR,
            PRIVATE_FILE,
        )?
        .sync_all()?;
        if let Err(error) = self.save(&manifest, true) {
            // If the manifest rename succeeded but directory sync failed, both
            // entries might still be committed. Only remove an orphan.
            if matches!(self.load(manifest.id), Err(TransferError::NotFound)) {
                self.remove(&staging_name)?;
            }
            return Err(error);
        }
        Ok(manifest.snapshot())
    }

    pub fn load(&self, id: Uuid) -> Result<Manifest> {
        let file = self
            .open_file(&manifest_name(id), false)?
            .ok_or(TransferError::NotFound)?;
        let mut bytes = Vec::new();
        file.take(MANIFEST_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MANIFEST_BYTES {
            return Err(TransferError::Corrupt);
        }
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|_| TransferError::Corrupt)?;
        if !manifest.valid(id) {
            return Err(TransferError::Corrupt);
        }
        Ok(manifest)
    }

    pub fn save(&self, manifest: &Manifest, new: bool) -> Result<()> {
        let temporary = format!("{}.tmp", manifest.id);
        self.remove(&temporary)?;
        let file = open_regular(
            &self.directory,
            &temporary,
            OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY,
            PRIVATE_FILE,
        )?;
        let written = serde_json::to_writer(&file, manifest)
            .map_err(io::Error::from)
            .and_then(|()| file.sync_all());
        if let Err(error) = written {
            self.remove(&temporary)?;
            return Err(error.into());
        }
        let flags = if new {
            RenameFlags::NOREPLACE
        } else {
            RenameFlags::empty()
        };
        if let Err(error) = fs::renameat_with(
            &self.directory,
            temporary.as_str(),
            &self.directory,
            manifest_name(manifest.id).as_str(),
            flags,
        ) {
            self.remove(&temporary)?;
            return Err(io::Error::from(error).into());
        }
        self.directory.sync_all()?;
        Ok(())
    }

    pub fn cancel(&self, id: Uuid) -> Result<()> {
        let manifest = self.load(id)?;
        let staged = Self::staging_name(id);
        if manifest.identity.is_some() && self.open_file(&staged, false)?.is_none() {
            return Err(TransferError::Conflict);
        }
        self.remove(&manifest_name(id))?;
        // Make the cancellation durable before unlinking the content, so a
        // crash can leave only an orphan rather than an unusable upload.
        self.directory.sync_all()?;
        self.remove(&staged)?;
        self.directory.sync_all()?;
        Ok(())
    }

    pub fn staging_name(id: Uuid) -> String {
        format!("{id}.upload")
    }

    pub fn open_file(&self, name: &str, writable: bool) -> io::Result<Option<File>> {
        let access = if writable {
            OFlags::RDWR
        } else {
            OFlags::RDONLY
        };
        match open_regular(&self.directory, name, access, Mode::empty()) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn recover(&self) -> Result<()> {
        let now = unix_seconds();
        for entry in Dir::read_from(&self.directory).map_err(io::Error::from)? {
            let entry = entry.map_err(io::Error::from)?;
            let Ok(name) = entry.file_name().to_str() else {
                continue;
            };
            let Some((id, suffix)) = name.split_once('.') else {
                continue;
            };
            let Ok(upload_id) = Uuid::parse_str(id) else {
                continue;
            };
            if upload_id.to_string() != id {
                continue;
            }
            let obsolete = match suffix {
                "tmp" => true,
                "upload" => self.open_file(&manifest_name(upload_id), false)?.is_none(),
                "json" => self
                    .load(upload_id)
                    .ok()
                    .and_then(|manifest| manifest.published_at)
                    .is_some_and(|published| {
                        now.saturating_sub(published) >= RECEIPT_RETENTION_SECONDS
                    }),
                _ => false,
            };
            if obsolete {
                self.remove(name)?;
            }
        }
        self.directory.sync_all()?;
        Ok(())
    }

    fn remove(&self, name: &str) -> io::Result<()> {
        match fs::unlinkat(&self.directory, name, AtFlags::empty()) {
            Ok(()) | Err(rustix::io::Errno::NOENT) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn open_regular(directory: &File, name: &str, flags: OFlags, mode: Mode) -> io::Result<File> {
    let file = File::from(fs::openat(
        directory,
        name,
        flags | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        mode,
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "upload state must use private regular files",
        ));
    }
    Ok(file)
}

fn manifest_name(id: Uuid) -> String {
    format!("{id}.json")
}

pub(crate) fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
