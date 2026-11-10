use std::{
    fs::{self, DirBuilder, File},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    sync::Arc,
};

use arcdos_filesystem::{Entry, Kind, Root, omitted_entry, validate_name};
use arcdos_transfers::{StageIdentity, UploadTarget};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    Error, Result,
    catalog::{Action, Area, Catalog, Change, Identity, Intent, Location, Origin, Record},
    index::{self, Document, Job},
    semantic::Candidate,
};

const DEFAULT_PAGE_SIZE: usize = 100;
const MAX_PAGE_SIZE: usize = 500;
const MAX_TOKEN_BYTES: usize = 2048;

pub(super) struct Store {
    files: Arc<Root>,
    trash: Root,
    stage: Arc<Root>,
    uploads: Root,
    catalog: Catalog,
    _lock: File,
}

pub(super) struct Resource {
    pub id: String,
    pub parent_id: String,
    pub name: String,
    pub entry: Entry,
    pub revision: String,
    location: Location,
    origin: Option<Origin>,
}

impl Resource {
    pub fn trashed(&self) -> bool {
        self.location.area == Area::Trash
    }
}

pub(super) struct Page {
    pub entries: Vec<Resource>,
    pub next: String,
}

pub(super) struct SearchHit {
    pub entry: Resource,
    pub path: String,
    pub tags: Vec<String>,
}

pub(super) struct CopyJob {
    files: Arc<Root>,
    source: Resource,
    parent_id: String,
    parent_revision: String,
    name: String,
    stage: StagedCopy,
}

struct StagedCopy {
    root: Arc<Root>,
    name: String,
    cleanup: bool,
}

impl Drop for StagedCopy {
    fn drop(&mut self) {
        if self.cleanup {
            let _ = self.root.remove_file(&self.name);
        }
    }
}

impl CopyJob {
    pub fn copy(mut self) -> Result<Self> {
        let opened = self.files.copy_file(
            &self.source.location.path,
            &self.stage.root,
            &self.stage.name,
        )?;
        self.stage.cleanup = true;
        check_revision(&self.source.revision, &revision(&self.source.id, &opened))?;
        Ok(self)
    }
}

#[derive(Serialize, Deserialize)]
struct Cursor {
    scope: String,
    revision: String,
    after: String,
}

impl Store {
    pub fn open(data: &Path) -> Result<Self> {
        require_directory(data)?;
        let files = data.join("files");
        make_directory(&files, 0o750)?;
        let state = data.join("state");
        private_directory(&state)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(state.join("filesystem.lock"))?;
        lock.try_lock()
            .map_err(|_| Error::Invalid("storage is already in use"))?;

        let stage = state.join("staging");
        let trash = state.join("trash");
        let uploads = state.join("transfers");
        for path in [&stage, &trash, &uploads] {
            private_directory(path)?;
        }
        let boundary = Root::open(data)?;
        for path in ["files", "state/staging", "state/trash", "state/transfers"] {
            boundary.metadata(path)?;
        }
        let store = Self {
            files: Arc::new(Root::open(&files)?),
            trash: Root::open(&trash)?,
            stage: Arc::new(Root::open(&stage)?),
            uploads: Root::open(&uploads)?,
            catalog: Catalog::open(&state.join("metadata.db"))?,
            _lock: lock,
        };
        boundary.sync_dir("state")?;
        boundary.sync_dir("")?;
        store.recover()?;
        store.clean_stage()?;
        Ok(store)
    }

    pub fn get(&self, id: &str) -> Result<Resource> {
        if id.is_empty() {
            let location = Location::new(Area::Files, "");
            let entry = self.metadata(&location)?;
            return Ok(resource(
                self.catalog.register(&location, &entry)?,
                entry,
                String::new(),
            ));
        }
        let id = parse_id(id)?;
        let record = self.catalog.by_id(&id)?.ok_or(Error::NotFound)?;
        let entry = self.metadata(&record.location)?;
        if !record.identity.matches(&entry) {
            return Err(Error::Stale);
        }
        let parent_id = if record.location.path.is_empty() || record.origin.is_some() {
            String::new()
        } else {
            let parent = record.location.parent();
            self.catalog.register(&parent, &self.metadata(&parent)?)?.id
        };
        Ok(resource(record, entry, parent_id))
    }

    pub fn list(&self, id: &str, size: u32, token: &str) -> Result<Page> {
        let parent = self.directory(id)?;
        self.list_at(&parent.location, &parent.id, &parent.revision, size, token)
    }

    pub fn list_trash(&self, size: u32, token: &str) -> Result<Page> {
        let location = Location::new(Area::Trash, "");
        let revision = revision("", &self.metadata(&location)?);
        self.list_at(&location, "", &revision, size, token)
    }

    fn list_at(
        &self,
        location: &Location,
        parent_id: &str,
        dir_revision: &str,
        size: u32,
        token: &str,
    ) -> Result<Page> {
        let limit = page_limit(size)?;
        let cursor = decode_cursor(token, parent_id, dir_revision)?;
        let mut entries = self.root(location.area).list(
            &location.path,
            cursor.as_ref().map(|cursor| cursor.after.as_str()),
            limit + 1,
        )?;
        if revision(parent_id, &self.metadata(location)?) != dir_revision {
            return Err(Error::Stale);
        }
        let next = if entries.len() > limit {
            entries.truncate(limit);
            let cursor = Cursor {
                scope: parent_id.to_owned(),
                revision: dir_revision.to_owned(),
                after: entries[limit - 1].name.clone(),
            };
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&cursor)?)
        } else {
            String::new()
        };
        let page: Vec<_> = entries
            .into_iter()
            .map(|named| (location.child(&named.name), named.entry))
            .collect();
        let entries = self
            .catalog
            .register_page(&page)?
            .into_iter()
            .zip(page)
            .map(|(record, (_, entry))| resource(record, entry, parent_id.to_owned()))
            .collect();
        Ok(Page { entries, next })
    }

    pub fn changes(&self, after: u64, size: u32) -> Result<Vec<Change>> {
        self.catalog.changes(after, page_limit(size)?)
    }

    pub fn search(&self, expression: &str, limit: usize) -> Result<(Vec<SearchHit>, bool)> {
        let candidates = index::candidates(self.catalog.connection(), expression, 4 * (limit + 1))?;
        let mut hits = Vec::new();
        for candidate in candidates {
            let Some(entry) = self.current_search_entry(&candidate.id, &candidate.revision)? else {
                continue;
            };
            if entry.location.path != candidate.path {
                continue;
            }
            if hits.len() == limit {
                return Ok((hits, true));
            }
            hits.push(self.search_hit(entry)?);
        }
        Ok((hits, false))
    }

    pub fn semantic_hits(&self, candidates: Vec<Candidate>) -> Result<Vec<SearchHit>> {
        let mut hits = Vec::new();
        for candidate in candidates {
            if let Some(entry) = self.current_search_entry(&candidate.id, &candidate.revision)? {
                hits.push(self.search_hit(entry)?);
            }
        }
        Ok(hits)
    }

    fn current_search_entry(&self, id: &str, revision: &str) -> Result<Option<Resource>> {
        match self.get(id) {
            Ok(entry) if !entry.trashed() && entry.revision == revision => Ok(Some(entry)),
            Ok(_) | Err(Error::NotFound | Error::Stale | Error::Forbidden) => Ok(None),
            Err(Error::Storage(error)) if omitted_entry(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn search_hit(&self, entry: Resource) -> Result<SearchHit> {
        Ok(SearchHit {
            tags: index::tags(self.catalog.connection(), &entry.id)?,
            path: entry.location.path.clone(),
            entry,
        })
    }

    pub fn set_tags(&self, id: &str, expected: &str, tags: Vec<String>) -> Result<Vec<String>> {
        let entry = self.live_entry(id, expected)?;
        let tags = index::normalized_tags(tags)?;
        index::replace_tags(self.catalog.connection(), &entry.id, &tags)?;
        Ok(tags)
    }

    pub fn files_root(&self) -> Arc<Root> {
        self.files.clone()
    }

    pub fn observe_path(&self, path: &str) -> Result<Option<Kind>> {
        let location = Location::new(Area::Files, path);
        match self.files.metadata(path) {
            Ok(entry) => {
                self.catalog.register(&location, &entry)?;
                Ok(Some(entry.kind))
            }
            Err(error) if absent_from_catalog(&error) && !path.is_empty() => {
                self.catalog.remove_missing(&location)?;
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn observe_batch(&self, paths: &[String]) -> Result<Vec<String>> {
        let mut batch = Vec::with_capacity(paths.len());
        let mut directories = Vec::new();
        for path in paths {
            match self.files.metadata(path) {
                Ok(entry) => {
                    if entry.kind == Kind::Directory {
                        directories.push(path.clone());
                    }
                    batch.push((Location::new(Area::Files, path), entry));
                }
                Err(error)
                    if omitted_entry(&error) || error.kind() == io::ErrorKind::InvalidInput => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.catalog.register_page(&batch)?;
        Ok(directories)
    }

    pub fn prune_index(&self, after: &str, limit: usize) -> Result<Option<String>> {
        let paths = self.catalog.paths_after(after, limit)?;
        for path in &paths {
            if let Err(error) = self.files.metadata(path) {
                if absent_from_catalog(&error) || error.kind() == io::ErrorKind::InvalidInput {
                    self.catalog
                        .remove_missing(&Location::new(Area::Files, path))?;
                } else if error.kind() != io::ErrorKind::PermissionDenied {
                    return Err(error.into());
                }
            }
        }
        Ok(paths.into_iter().last())
    }

    pub fn index_jobs(&self, limit: usize) -> Result<Vec<Job>> {
        index::jobs(self.catalog.connection(), limit)
    }

    pub fn write_index(&self, job: &Job, document: Document) -> Result<()> {
        if self
            .files
            .metadata(&job.path)
            .is_ok_and(|entry| revision(&job.id, &entry) == job.revision)
        {
            return index::write(self.catalog.connection(), job, document);
        }
        self.retry_index(job)
    }

    pub fn retry_index(&self, job: &Job) -> Result<()> {
        if let Err(error) = self.observe_path(&job.path) {
            tracing::debug!(%error, "could not reobserve file for search");
        }
        index::retry(self.catalog.connection(), job)
    }

    pub fn rebuild_search(&self) -> Result<()> {
        index::rebuild(self.catalog.connection())
    }

    pub fn create_directory(
        &self,
        parent_id: &str,
        name: &str,
        expected_parent: &str,
    ) -> Result<Resource> {
        let parent = self.live_parent(parent_id, expected_parent)?;
        validate_name(name)?;
        let id = new_id();
        let stage = Location::new(Area::Stage, &id);
        self.stage.create_dir(&stage.path)?;
        self.publish_staged(id, stage, parent.location.child(name), Action::Created)
    }

    pub fn move_entry(
        &self,
        id: &str,
        expected: &str,
        parent_id: &str,
        name: &str,
        expected_parent: &str,
    ) -> Result<Resource> {
        let source = self.live_entry(id, expected)?;
        let parent = self.live_parent(parent_id, expected_parent)?;
        validate_name(name)?;
        let target = parent.location.child(name);
        if source.location.path == target.path {
            return Ok(source);
        }
        if parent
            .location
            .path
            .strip_prefix(&source.location.path)
            .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        {
            return Err(Error::Invalid("cannot move a directory into itself"));
        }
        self.mutate(Intent {
            id: source.id,
            from: source.location,
            to: target,
            identity: Identity::new(&source.entry),
            action: Action::Moved,
            origin: None,
            upload_id: None,
        })
    }

    pub fn plan_copy(
        &self,
        id: &str,
        expected: &str,
        parent_id: &str,
        name: &str,
        expected_parent: &str,
    ) -> Result<CopyJob> {
        let source = self.live_entry(id, expected)?;
        let parent = self.live_parent(parent_id, expected_parent)?;
        validate_name(name)?;
        if source.entry.kind != Kind::File {
            return Err(Error::Invalid("copy requires a regular file"));
        }
        if self
            .optional_metadata(&parent.location.child(name))?
            .is_some()
        {
            return Err(Error::Exists);
        }
        Ok(CopyJob {
            files: self.files.clone(),
            source,
            parent_id: parent.id,
            parent_revision: expected_parent.to_owned(),
            name: name.to_owned(),
            stage: StagedCopy {
                root: self.stage.clone(),
                name: new_id(),
                cleanup: false,
            },
        })
    }

    pub fn finish_copy(&self, mut job: CopyJob) -> Result<Resource> {
        self.live_entry(&job.source.id, &job.source.revision)?;
        let parent = self.live_parent(&job.parent_id, &job.parent_revision)?;
        let id = job.stage.name.clone();
        let stage = Location::new(Area::Stage, &id);
        // Once an intent can refer to this stage, recovery owns its lifecycle.
        job.stage.cleanup = false;
        self.publish_staged(id, stage, parent.location.child(&job.name), Action::Copied)
    }

    pub fn trash(&self, id: &str, expected: &str) -> Result<Resource> {
        let source = self.live_entry(id, expected)?;
        let slot = Location::new(Area::Trash, &source.id);
        self.mutate(Intent {
            id: source.id,
            from: source.location,
            to: slot,
            identity: Identity::new(&source.entry),
            action: Action::Trashed,
            origin: Some(Origin {
                parent_id: source.parent_id,
                name: source.name,
            }),
            upload_id: None,
        })
    }

    pub fn restore(&self, id: &str, expected: &str) -> Result<Resource> {
        let source = self.get(id)?;
        check_revision(expected, &source.revision)?;
        let origin = source
            .origin
            .ok_or(Error::Invalid("entry is not a trash item"))?;
        let parent = self.directory(&origin.parent_id)?;
        if parent.trashed() {
            return Err(Error::Conflict("restore the original parent first"));
        }
        self.mutate(Intent {
            id: source.id,
            from: source.location,
            to: parent.location.child(&origin.name),
            identity: Identity::new(&source.entry),
            action: Action::Restored,
            origin: None,
            upload_id: None,
        })
    }

    pub fn upload_target(
        &self,
        parent_id: &str,
        name: &str,
        expected: &str,
    ) -> Result<UploadTarget> {
        let parent = self.live_parent(parent_id, expected)?;
        validate_name(name)?;
        if self
            .optional_metadata(&parent.location.child(name))?
            .is_some()
        {
            return Err(Error::Exists);
        }
        Ok(UploadTarget {
            parent_id: parent.id,
            name: name.to_owned(),
            parent_revision: expected.to_owned(),
        })
    }

    pub fn committed_upload(&self, id: &str) -> Result<Option<Resource>> {
        self.catalog
            .published(id)?
            .map(|entry_id| self.get(&entry_id))
            .transpose()
    }

    pub fn publish_upload(
        &self,
        id: &str,
        target: &UploadTarget,
        expected_parent: &str,
        staging_name: &str,
        verified: &StageIdentity,
    ) -> Result<Resource> {
        if let Some(entry) = self.committed_upload(id)? {
            return Ok(entry);
        }
        let parent = self.live_parent(&target.parent_id, expected_parent)?;
        validate_name(&target.name)?;
        validate_name(staging_name)?;
        let from = Location::new(Area::Uploads, staging_name);
        let entry = self.metadata(&from)?;
        if entry.dev != verified.dev
            || entry.ino != verified.ino
            || entry.size != verified.size
            || entry.mtime_sec != verified.mtime_secs
            || entry.mtime_nsec != verified.mtime_nsecs
        {
            return Err(Error::Stale);
        }
        self.mutate(Intent {
            id: new_id(),
            from,
            to: parent.location.child(&target.name),
            identity: Identity::new(&entry),
            action: Action::Uploaded,
            origin: None,
            upload_id: Some(id.to_owned()),
        })
    }

    pub fn download(&self, id: &str) -> Result<(File, Resource)> {
        let entry = self.get(id)?;
        if entry.trashed() || entry.entry.kind != Kind::File {
            return Err(Error::NotFound);
        }
        let file = self.files.open_file(&entry.location.path)?;
        let opened = Entry::from_file(&file)?;
        check_revision(&entry.revision, &revision(&entry.id, &opened))?;
        Ok((file, entry))
    }

    fn directory(&self, id: &str) -> Result<Resource> {
        let entry = self.get(id)?;
        if entry.entry.kind != Kind::Directory {
            return Err(Error::Invalid("parent must be a directory"));
        }
        Ok(entry)
    }

    fn live_parent(&self, id: &str, expected: &str) -> Result<Resource> {
        let parent = self.directory(id)?;
        if parent.trashed() {
            return Err(Error::Conflict("parent is in the trash"));
        }
        check_revision(expected, &parent.revision)?;
        Ok(parent)
    }

    fn live_entry(&self, id: &str, expected: &str) -> Result<Resource> {
        let entry = self.get(id)?;
        if entry.trashed() || entry.location.path.is_empty() {
            return Err(Error::Conflict("entry cannot be changed"));
        }
        check_revision(expected, &entry.revision)?;
        Ok(entry)
    }

    fn publish_staged(
        &self,
        id: String,
        stage: Location,
        to: Location,
        action: Action,
    ) -> Result<Resource> {
        let entry = self.metadata(&stage)?;
        self.stage.sync_dir("")?;
        let intent = Intent {
            id,
            from: stage,
            to,
            identity: Identity::new(&entry),
            action,
            origin: None,
            upload_id: None,
        };
        self.mutate(intent)
    }

    fn mutate(&self, intent: Intent) -> Result<Resource> {
        if !intent.identity.unchanged(&self.metadata(&intent.from)?) {
            return Err(Error::Stale);
        }
        self.catalog.begin(&intent)?;
        let source = self.root(intent.from.area);
        let destination = self.root(intent.to.area);
        if let Err(error) = source.rename(&intent.from.path, destination, &intent.to.path) {
            self.recover()?;
            return Err(error.into());
        }
        self.recover()?;
        self.get(&intent.id)
    }

    pub fn recover(&self) -> Result<()> {
        let Some(intent) = self.catalog.pending()? else {
            return Ok(());
        };
        if let Some(entry) = self.optional_metadata(&intent.from)? {
            if !intent.identity.matches(&entry) {
                return Err(Error::Inconsistent);
            }
            self.catalog.abort()?;
            if intent.from.area == Area::Stage {
                self.remove_staged(&intent.from.path, entry.kind)?;
            }
            return Ok(());
        }
        let entry = self
            .optional_metadata(&intent.to)?
            .filter(|entry| intent.identity.matches_after_rename(entry))
            .ok_or(Error::Inconsistent)?;
        self.sync_parent(&intent.from)?;
        self.sync_parent(&intent.to)?;
        self.catalog.finish(&intent, &entry)?;
        Ok(())
    }

    fn clean_stage(&self) -> Result<()> {
        loop {
            let entries = self.stage.list("", None, MAX_PAGE_SIZE)?;
            if entries.is_empty() {
                break;
            }
            for named in entries {
                self.remove_staged(&named.name, named.entry.kind)?;
            }
        }
        Ok(())
    }

    fn remove_staged(&self, path: &str, kind: Kind) -> Result<()> {
        match kind {
            Kind::Directory => self.stage.remove_dir(path)?,
            Kind::File => self.stage.remove_file(path)?,
        }
        Ok(())
    }

    fn root(&self, area: Area) -> &Root {
        match area {
            Area::Files => &self.files,
            Area::Trash => &self.trash,
            Area::Stage => &self.stage,
            Area::Uploads => &self.uploads,
        }
    }

    fn metadata(&self, location: &Location) -> Result<Entry> {
        Ok(self.root(location.area).metadata(&location.path)?)
    }

    fn optional_metadata(&self, location: &Location) -> Result<Option<Entry>> {
        match self.metadata(location) {
            Ok(entry) => Ok(Some(entry)),
            Err(Error::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn sync_parent(&self, location: &Location) -> Result<()> {
        self.root(location.area).sync_dir(&location.parent().path)?;
        Ok(())
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn absent_from_catalog(error: &io::Error) -> bool {
    omitted_entry(error)
        && (error.kind() != io::ErrorKind::PermissionDenied || error.raw_os_error().is_none())
}

pub(super) fn parse_id(id: &str) -> Result<String> {
    Uuid::try_parse(id)
        .map(|id| id.to_string())
        .map_err(|_| Error::Invalid("invalid resource ID"))
}

fn resource(record: Record, entry: Entry, parent_id: String) -> Resource {
    let name = record.origin.as_ref().map_or_else(
        || record.location.name().to_owned(),
        |origin| origin.name.clone(),
    );
    Resource {
        revision: revision(&record.id, &entry),
        id: record.id,
        parent_id,
        name,
        entry,
        location: record.location,
        origin: record.origin,
    }
}

pub(super) fn revision(id: &str, entry: &Entry) -> String {
    let mut hash = Sha256::new();
    hash.update(id.as_bytes());
    for value in [entry.dev, entry.ino, entry.size] {
        hash.update(value.to_le_bytes());
    }
    for value in [
        entry.mtime_sec,
        entry.mtime_nsec,
        entry.ctime_sec,
        entry.ctime_nsec,
    ] {
        hash.update(value.to_le_bytes());
    }
    URL_SAFE_NO_PAD.encode(hash.finalize())
}

fn check_revision(expected: &str, current: &str) -> Result<()> {
    if expected.is_empty() {
        return Err(Error::Invalid("a revision precondition is required"));
    }
    if expected != current {
        return Err(Error::Stale);
    }
    Ok(())
}

fn page_limit(size: u32) -> Result<usize> {
    match size as usize {
        0 => Ok(DEFAULT_PAGE_SIZE),
        size if size <= MAX_PAGE_SIZE => Ok(size),
        _ => Err(Error::Invalid("page size exceeds 500")),
    }
}

fn decode_cursor(token: &str, scope: &str, revision: &str) -> Result<Option<Cursor>> {
    if token.is_empty() {
        return Ok(None);
    }
    let invalid = || Error::Invalid("invalid page token");
    if token.len() > MAX_TOKEN_BYTES {
        return Err(invalid());
    }
    let decoded = URL_SAFE_NO_PAD.decode(token).map_err(|_| invalid())?;
    let cursor: Cursor = serde_json::from_slice(&decoded).map_err(|_| invalid())?;
    if cursor.scope != scope {
        return Err(invalid());
    }
    validate_name(&cursor.after).map_err(|_| invalid())?;
    if cursor.revision != revision {
        return Err(Error::Stale);
    }
    Ok(Some(cursor))
}

fn require_directory(path: &Path) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(Error::Invalid("storage directory must be a real directory"));
    }
    Ok(metadata)
}

fn make_directory(path: &Path, mode: u32) -> Result<fs::Metadata> {
    match DirBuilder::new().mode(mode).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    require_directory(path)
}

fn private_directory(path: &Path) -> Result<()> {
    let metadata = make_directory(path, 0o700)?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(Error::Invalid("storage state directories must be private"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_a_rename_that_reached_disk_before_the_catalog() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        fs::write(data.path().join("files/before"), b"example").unwrap();
        let root = store.get("").unwrap();
        let before = store.list(&root.id, 1, "").unwrap().entries.remove(0);
        let intent = Intent {
            id: before.id.clone(),
            from: before.location,
            to: Location::new(Area::Files, "after"),
            identity: Identity::new(&before.entry),
            action: Action::Moved,
            origin: None,
            upload_id: None,
        };
        store.catalog.begin(&intent).unwrap();
        store.files.rename("before", &store.files, "after").unwrap();
        drop(store);

        let store = Store::open(data.path()).unwrap();
        assert_eq!(store.get(&before.id).unwrap().name, "after");
        let events = store.changes(0, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, Action::Created as i32);
        assert_eq!(events[1].kind, Action::Moved as i32);
        assert_eq!(events[1].entry_id, before.id);
        store.recover().unwrap();
        assert_eq!(store.changes(0, 10).unwrap().len(), 2);
    }
}
