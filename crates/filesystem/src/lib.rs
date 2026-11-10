//! Linux filesystem operations relative to trusted storage directory handles.

#![cfg(target_os = "linux")]

use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    os::{
        fd::{AsFd, AsRawFd},
        unix::fs::MetadataExt,
    },
    path::Path,
    time::SystemTime,
};

use rustix::{
    fs::{self, AtFlags, Dir, FileType, Mode, OFlags, RenameFlags, ResolveFlags},
    io::Errno,
};

/// The largest page accepted by the filesystem layer, including a lookahead entry.
pub const MAX_LIST_LIMIT: usize = 1024;

const CONFINED: ResolveFlags = ResolveFlags::BENEATH
    .union(ResolveFlags::NO_SYMLINKS)
    .union(ResolveFlags::NO_MAGICLINKS)
    .union(ResolveFlags::NO_XDEV);

/// An opened storage directory. Paths are relative and use slash-separated UTF-8 names.
#[derive(Debug)]
pub struct Root {
    directory: File,
    proc_fds: File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    File,
    Directory,
}

/// Metadata for a supported entry, read from an open handle to that entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub size: u64,
    pub dev: u64,
    pub ino: u64,
    pub mtime_sec: i64,
    pub mtime_nsec: i64,
    pub ctime_sec: i64,
    pub ctime_nsec: i64,
    pub birth_time: Option<SystemTime>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamedEntry {
    pub name: String,
    pub entry: Entry,
}

impl Entry {
    pub fn from_file(file: &File) -> io::Result<Self> {
        let metadata = file.metadata()?;
        let kind = if metadata.is_dir() {
            Kind::Directory
        } else if metadata.is_file() && metadata.nlink() == 1 {
            Kind::File
        } else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "unsupported storage entry",
            ));
        };

        Ok(Self {
            kind,
            size: metadata.len(),
            dev: metadata.dev(),
            ino: metadata.ino(),
            mtime_sec: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime_sec: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            birth_time: metadata.created().ok(),
        })
    }
}

impl Root {
    /// Open an administrator-configured storage root. Linux 5.6 and procfs are required.
    pub fn open(path: &Path) -> io::Result<Self> {
        let directory = fs::openat2(
            fs::CWD,
            path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )?
        .into();
        let proc_fds = fs::open(
            "/proc/self/fd",
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?
        .into();
        if fs::fstatfs(&proc_fds)?.f_type != fs::PROC_SUPER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "procfs is required for storage access",
            ));
        }
        Ok(Self {
            directory,
            proc_fds,
        })
    }

    /// Read metadata. An empty path refers to this root.
    pub fn metadata(&self, path: &str) -> io::Result<Entry> {
        let file = open_under(
            &self.directory,
            relative_path(path)?,
            OFlags::PATH,
            Mode::empty(),
        )?;
        Entry::from_file(&file)
    }

    /// Read one name-ordered page strictly after the optional name. Memory is bounded by
    /// the page size. Each call scans the directory and omits unsupported entries.
    pub fn list(
        &self,
        path: &str,
        after: Option<&str>,
        limit: usize,
    ) -> io::Result<Vec<NamedEntry>> {
        if !(1..=MAX_LIST_LIMIT).contains(&limit) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid directory page size",
            ));
        }
        if let Some(after) = after {
            validate_name(after)?;
        }
        let mut dir = Dir::new(self.open_dir(path, OFlags::RDONLY)?)?;
        let mut page = BTreeMap::<String, Entry>::new();
        while let Some(item) = dir.read() {
            let item = item?;
            let Ok(name) = item.file_name().to_str() else {
                continue;
            };
            if !valid_name(name) || after.is_some_and(|after| name <= after) {
                continue;
            }
            if page.len() == limit
                && page
                    .last_key_value()
                    .is_some_and(|(last, _)| name >= last.as_str())
            {
                continue;
            }
            if !matches!(
                item.file_type(),
                FileType::RegularFile | FileType::Directory | FileType::Unknown
            ) {
                continue;
            }
            if let Some(entry) = read_entry(&mut dir, name)? {
                page.insert(name.to_owned(), entry);
                if page.len() > limit {
                    page.pop_last();
                }
            }
        }
        Ok(page
            .into_iter()
            .map(|(name, entry)| NamedEntry { name, entry })
            .collect())
    }

    /// Visit supported children in directory order without retaining the listing.
    pub fn scan(
        &self,
        path: &str,
        mut visit: impl FnMut(NamedEntry) -> io::Result<bool>,
    ) -> io::Result<()> {
        let mut dir = Dir::new(self.open_dir(path, OFlags::RDONLY)?)?;
        while let Some(item) = dir.read() {
            let item = item?;
            let Ok(name) = item.file_name().to_str() else {
                continue;
            };
            if !valid_name(name)
                || !matches!(
                    item.file_type(),
                    FileType::RegularFile | FileType::Directory | FileType::Unknown
                )
            {
                continue;
            }
            if let Some(entry) = read_entry(&mut dir, name)?
                && !visit(NamedEntry {
                    name: name.to_owned(),
                    entry,
                })?
            {
                break;
            }
        }
        Ok(())
    }

    /// Open a singly linked regular file for reading.
    pub fn open_file(&self, path: &str) -> io::Result<File> {
        self.open_regular(path, OFlags::RDONLY)
    }

    /// Open an existing staging file without truncating it.
    pub fn open_writable_file(&self, path: &str) -> io::Result<File> {
        self.open_regular(path, OFlags::RDWR)
    }

    /// Exclusively create a new staging file with private permissions.
    pub fn create_file(&self, path: &str) -> io::Result<File> {
        let (dir, name) = self.open_parent(path)?;
        open_under(
            &dir,
            name,
            OFlags::CREATE | OFlags::EXCL | OFlags::RDWR,
            Mode::RUSR | Mode::WUSR,
        )
    }

    /// Exclusively create a directory.
    pub fn create_dir(&self, path: &str) -> io::Result<()> {
        let (dir, name) = self.open_parent(path)?;
        fs::mkdirat(&dir, name, Mode::RWXU)?;
        Ok(())
    }

    /// Atomically move a file or directory without replacing an existing destination.
    /// Cross-mount moves fail without changing either path.
    pub fn rename(&self, from: &str, destination: &Self, to: &str) -> io::Result<()> {
        let (source_dir, source_name) = self.open_parent(from)?;
        let (target_dir, target_name) = destination.open_parent(to)?;
        let source = open_under(&source_dir, source_name, OFlags::PATH, Mode::empty())?;
        Entry::from_file(&source)?;
        fs::renameat_with(
            &source_dir,
            source_name,
            &target_dir,
            target_name,
            RenameFlags::NOREPLACE,
        )?;
        Ok(())
    }

    /// Copy the original file length into an exclusive staging file and sync its contents.
    /// If the open source changes during the copy, discard the staging file.
    pub fn copy_file(&self, from: &str, staging: &Self, staging_name: &str) -> io::Result<Entry> {
        let mut source = self.open_file(from)?;
        let before = Entry::from_file(&source)?;
        let mut target = staging.create_file(staging_name)?;
        let result = (|| {
            let copied = io::copy(&mut (&mut source).take(before.size), &mut target)?;
            if copied != before.size || Entry::from_file(&source)? != before {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "source changed while copying",
                ));
            }
            target.sync_all()
        })();
        if result.is_err() {
            let _ = staging.remove_file(staging_name);
        }
        result.map(|()| before)
    }

    /// Remove a staging file. The final name is never followed.
    pub fn remove_file(&self, path: &str) -> io::Result<()> {
        let (dir, name) = self.open_parent(path)?;
        fs::unlinkat(&dir, name, AtFlags::empty())?;
        Ok(())
    }

    /// Remove an empty staging directory. The final name is never followed.
    pub fn remove_dir(&self, path: &str) -> io::Result<()> {
        let (dir, name) = self.open_parent(path)?;
        fs::unlinkat(&dir, name, AtFlags::REMOVEDIR)?;
        Ok(())
    }

    /// Sync a directory after a journaled change to its entries.
    pub fn sync_dir(&self, path: &str) -> io::Result<()> {
        self.open_dir(path, OFlags::RDONLY)?.sync_all()
    }

    fn open_dir(&self, path: &str, access: OFlags) -> io::Result<File> {
        open_under(
            &self.directory,
            relative_path(path)?,
            access | OFlags::DIRECTORY,
            Mode::empty(),
        )
    }

    fn open_parent<'a>(&self, path: &'a str) -> io::Result<(File, &'a str)> {
        if path.is_empty() {
            return Err(invalid_path());
        }
        let (parent, name) = relative_path(path)?.rsplit_once('/').unwrap_or((".", path));
        let dir = open_under(
            &self.directory,
            parent,
            OFlags::PATH | OFlags::DIRECTORY,
            Mode::empty(),
        )?;
        Ok((dir, name))
    }

    fn open_regular(&self, path: &str, access: OFlags) -> io::Result<File> {
        let handle = open_under(
            &self.directory,
            relative_path(path)?,
            OFlags::PATH,
            Mode::empty(),
        )?;
        let entry = Entry::from_file(&handle)?;
        if entry.kind != Kind::File {
            return Err(io::Error::new(
                io::ErrorKind::IsADirectory,
                "expected a file",
            ));
        }

        // The kernel's procfs link names this live descriptor. A pathname replacement
        // cannot substitute a FIFO or device between the type check and opening for I/O.
        let file = File::from(fs::openat(
            &self.proc_fds,
            handle.as_raw_fd().to_string(),
            access | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::empty(),
        )?);
        let reopened = Entry::from_file(&file)?;
        if (reopened.dev, reopened.ino) != (entry.dev, entry.ino) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "storage handle changed",
            ));
        }
        Ok(file)
    }
}

/// Validate a single name before combining it with a trusted parent path.
pub fn validate_name(name: &str) -> io::Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(invalid_path())
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
}

fn relative_path(path: &str) -> io::Result<&str> {
    if path.is_empty() {
        return Ok(".");
    }
    if path.len() >= 4096 || !path.split('/').all(valid_name) {
        return Err(invalid_path());
    }
    Ok(path)
}

fn invalid_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "invalid storage path")
}

fn open_under(dir: &impl AsFd, path: &str, flags: OFlags, mode: Mode) -> io::Result<File> {
    Ok(fs::openat2(dir, path, flags | OFlags::CLOEXEC, mode, CONFINED)?.into())
}

fn read_entry(dir: &mut Dir, name: &str) -> io::Result<Option<Entry>> {
    match open_under(&dir.fd()?, name, OFlags::PATH, Mode::empty())
        .and_then(|file| Entry::from_file(&file))
    {
        Ok(entry) => Ok(Some(entry)),
        Err(error) if omitted_entry(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether a lookup failed because the entry is missing or outside the supported boundary.
pub fn omitted_entry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied | io::ErrorKind::NotADirectory
    ) || [Errno::LOOP, Errno::XDEV]
        .iter()
        .any(|errno| error.raw_os_error() == Some(errno.raw_os_error()))
}
