use std::{path::Path, time::SystemTime};

use arcdos_filesystem::{Entry, Kind};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Error, Result, index, timestamp_parts};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum Area {
    Files = 0,
    Trash = 1,
    Stage = 2,
    Uploads = 3,
}

impl Area {
    fn from_db(value: i64) -> rusqlite::Result<Self> {
        match value {
            0 => Ok(Self::Files),
            1 => Ok(Self::Trash),
            _ => Err(rusqlite::Error::IntegralValueOutOfRange(1, value)),
        }
    }

    fn indexed(self) -> bool {
        matches!(self, Self::Files | Self::Trash)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Location {
    pub area: Area,
    pub path: String,
}

impl Location {
    pub fn new(area: Area, path: impl Into<String>) -> Self {
        Self {
            area,
            path: path.into(),
        }
    }

    pub fn parent(&self) -> Self {
        Self::new(
            self.area,
            self.path.rsplit_once('/').map_or("", |(parent, _)| parent),
        )
    }

    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or_default()
    }

    pub fn child(&self, name: &str) -> Self {
        let path = if self.path.is_empty() {
            name.to_owned()
        } else {
            format!("{}/{name}", self.path)
        };
        Self::new(self.area, path)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Identity {
    dev: u64,
    ino: u64,
    birth: Option<(i64, i32)>,
    ctime: (i64, i64),
}

impl Identity {
    pub fn new(entry: &Entry) -> Self {
        Self {
            dev: entry.dev,
            ino: entry.ino,
            birth: entry.birth_time.map(timestamp_parts),
            ctime: (entry.ctime_sec, entry.ctime_nsec),
        }
    }

    pub fn matches(&self, entry: &Entry) -> bool {
        let actual = Self::new(entry);
        self.dev == actual.dev
            && self.ino == actual.ino
            && match (self.birth, actual.birth) {
                (Some(expected), Some(actual)) => expected == actual,
                (None, None) => self.ctime == actual.ctime,
                _ => false,
            }
    }

    pub fn matches_after_rename(&self, entry: &Entry) -> bool {
        let actual = Self::new(entry);
        self.dev == actual.dev && self.ino == actual.ino && self.birth == actual.birth
    }

    pub fn unchanged(&self, entry: &Entry) -> bool {
        self.matches(entry) && self.ctime == (entry.ctime_sec, entry.ctime_nsec)
    }
}

#[derive(Debug)]
pub(super) struct Record {
    pub id: String,
    pub location: Location,
    pub identity: Identity,
    pub origin: Option<Origin>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Origin {
    pub parent_id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) enum Action {
    Created = 1,
    Moved = 2,
    Copied = 3,
    Trashed = 4,
    Restored = 5,
    Uploaded = 6,
    Modified = 7,
    Removed = 8,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Intent {
    pub id: String,
    pub from: Location,
    pub to: Location,
    pub identity: Identity,
    pub action: Action,
    pub origin: Option<Origin>,
    pub upload_id: Option<String>,
}

pub(super) struct Change {
    pub sequence: u64,
    pub entry_id: String,
    pub kind: i32,
    pub seconds: i64,
    pub nanos: i32,
}

pub(super) struct Catalog(Connection);

const SCHEMA_V1: &str = r"
BEGIN;
CREATE TABLE entries (
    id TEXT PRIMARY KEY,
    area INTEGER NOT NULL CHECK (area IN (0, 1)),
    path TEXT NOT NULL,
    identity TEXT NOT NULL,
    origin TEXT,
    UNIQUE (area, path)
);
CREATE TABLE changes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id TEXT NOT NULL,
    kind INTEGER NOT NULL,
    seconds INTEGER NOT NULL,
    nanos INTEGER NOT NULL
);
CREATE TABLE pending (
    slot INTEGER PRIMARY KEY CHECK (slot = 1),
    intent TEXT NOT NULL
);
CREATE TABLE published_uploads (
    upload_id TEXT PRIMARY KEY,
    entry_id TEXT NOT NULL
);
PRAGMA user_version=1;
COMMIT;
";

impl Catalog {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let version: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 2 {
            return Err(Error::Inconsistent);
        }
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;",
        )?;
        if version == 0 {
            conn.execute_batch(SCHEMA_V1)?;
        }
        if version < 2 {
            conn.execute_batch(index::SCHEMA_V2)?;
        }
        Ok(Self(conn))
    }

    pub fn connection(&self) -> &Connection {
        &self.0
    }

    pub fn paths_after(&self, path: &str, limit: usize) -> Result<Vec<String>> {
        let mut statement = self.0.prepare_cached(
            "SELECT path FROM entries WHERE area=0 AND path>?1 ORDER BY path LIMIT ?2",
        )?;
        Ok(statement
            .query_map(params![path, limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn remove_missing(&self, location: &Location) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        if let Some(record) = by_path(&tx, location)? {
            record_change(&tx, &record.id, Action::Removed)?;
        }
        delete_subtree(&tx, location)?;
        tx.commit()?;
        Ok(())
    }

    pub fn by_id(&self, id: &str) -> Result<Option<Record>> {
        let row = self
            .0
            .query_row(
                "SELECT id, area, path, identity, origin FROM entries WHERE id=?1",
                [id],
                decode_record,
            )
            .optional()?;
        row.transpose()
    }

    pub fn register(&self, location: &Location, entry: &Entry) -> Result<Record> {
        let tx = self.0.unchecked_transaction()?;
        let record = register(&tx, location, entry)?;
        tx.commit()?;
        Ok(record)
    }

    pub fn register_page(&self, entries: &[(Location, Entry)]) -> Result<Vec<Record>> {
        let tx = self.0.unchecked_transaction()?;
        let records = entries
            .iter()
            .map(|(location, entry)| register(&tx, location, entry))
            .collect::<Result<Vec<_>>>()?;
        tx.commit()?;
        Ok(records)
    }

    pub fn pending(&self) -> Result<Option<Intent>> {
        let value: Option<String> = self
            .0
            .query_row("SELECT intent FROM pending WHERE slot=1", [], |row| {
                row.get(0)
            })
            .optional()?;
        value
            .map(|text| serde_json::from_str(&text).map_err(Error::from))
            .transpose()
    }

    pub fn begin(&self, intent: &Intent) -> Result<()> {
        self.0.execute(
            "INSERT INTO pending VALUES (1, ?1)",
            [serde_json::to_string(intent)?],
        )?;
        Ok(())
    }

    pub fn abort(&self) -> Result<()> {
        self.0.execute("DELETE FROM pending WHERE slot=1", [])?;
        Ok(())
    }

    pub fn finish(&self, intent: &Intent, entry: &Entry) -> Result<()> {
        let tx = self.0.unchecked_transaction()?;
        delete_subtree(&tx, &intent.to)?;
        if intent.from.area.indexed() {
            tx.execute(
                "UPDATE entries SET area=?1, path=?2 || substr(path, length(?4)+1)
                 WHERE area=?3 AND (path=?4 OR (path>=?4||'/' AND path<?4||'0'))",
                params![
                    intent.to.area as i64,
                    intent.to.path,
                    intent.from.area as i64,
                    intent.from.path
                ],
            )?;
        }
        tx.execute(
            "INSERT INTO entries (id, area, path, identity, origin) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET area=excluded.area, path=excluded.path,
               identity=excluded.identity, origin=excluded.origin",
            params![
                intent.id,
                intent.to.area as i64,
                intent.to.path,
                serde_json::to_string(&Identity::new(entry))?,
                intent
                    .origin
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?
            ],
        )?;
        index::observe(&tx, &intent.id, &intent.to, entry)?;
        record_change(&tx, &intent.id, intent.action)?;
        if let Some(upload) = &intent.upload_id {
            tx.execute(
                "INSERT INTO published_uploads VALUES (?1, ?2)",
                params![upload, intent.id],
            )?;
        }
        tx.execute("DELETE FROM pending WHERE slot=1", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn published(&self, upload_id: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .query_row(
                "SELECT entry_id FROM published_uploads WHERE upload_id=?1",
                [upload_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn changes(&self, after: u64, limit: usize) -> Result<Vec<Change>> {
        let after = i64::try_from(after).map_err(|_| Error::Invalid("invalid change sequence"))?;
        let mut query = self.0.prepare(
            "SELECT sequence, entry_id, kind, seconds, nanos FROM changes
             WHERE sequence>?1 ORDER BY sequence LIMIT ?2",
        )?;
        let rows = query.query_map(params![after, limit as i64], |row| {
            let sequence: i64 = row.get(0)?;
            Ok(Change {
                sequence: u64::try_from(sequence)
                    .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(0, sequence))?,
                entry_id: row.get(1)?,
                kind: row.get(2)?,
                seconds: row.get(3)?,
                nanos: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

fn by_path(conn: &Connection, location: &Location) -> Result<Option<Record>> {
    conn.query_row(
        "SELECT id, area, path, identity, origin FROM entries WHERE area=?1 AND path=?2",
        params![location.area as i64, location.path],
        decode_record,
    )
    .optional()?
    .transpose()
}

pub(super) fn record_change(conn: &Connection, id: &str, action: Action) -> Result<()> {
    let (seconds, nanos) = timestamp_parts(SystemTime::now());
    conn.execute(
        "INSERT INTO changes (entry_id, kind, seconds, nanos) VALUES (?1, ?2, ?3, ?4)",
        params![id, action as i64, seconds, nanos],
    )?;
    Ok(())
}

fn register(conn: &Connection, location: &Location, entry: &Entry) -> Result<Record> {
    if let Some(record) = by_path(conn, location)? {
        if record.identity.matches(entry) {
            if index::observe(conn, &record.id, location, entry)? && entry.kind == Kind::File {
                record_change(conn, &record.id, Action::Modified)?;
            }
            return Ok(record);
        }
        if location.area == Area::Trash {
            return Err(Error::Inconsistent);
        }
        if !location.path.is_empty() {
            record_change(conn, &record.id, Action::Removed)?;
        }
        conn.execute(
            "DELETE FROM entries WHERE area=?1 AND path=?2",
            params![location.area as i64, location.path],
        )?;
    } else if location.area == Area::Trash && !location.path.contains('/') {
        return Err(Error::Inconsistent);
    }
    let record = Record {
        id: Uuid::new_v4().to_string(),
        location: location.clone(),
        identity: Identity::new(entry),
        origin: None,
    };
    conn.execute(
        "INSERT INTO entries (id, area, path, identity) VALUES (?1, ?2, ?3, ?4)",
        params![
            record.id,
            location.area as i64,
            location.path,
            serde_json::to_string(&record.identity)?
        ],
    )?;
    if location.area == Area::Files && !location.path.is_empty() {
        index::observe(conn, &record.id, location, entry)?;
        record_change(conn, &record.id, Action::Created)?;
    }
    Ok(record)
}

fn delete_subtree(conn: &Connection, location: &Location) -> Result<()> {
    if location.path.is_empty() {
        conn.execute("DELETE FROM entries WHERE area=?1", [location.area as i64])?;
    } else {
        conn.execute(
            "DELETE FROM entries WHERE area=?1 AND (path=?2 OR (path>=?2||'/' AND path<?2||'0'))",
            params![location.area as i64, location.path],
        )?;
    }
    Ok(())
}

fn decode_record(row: &Row<'_>) -> rusqlite::Result<Result<Record>> {
    let id: String = row.get(0)?;
    let area = Area::from_db(row.get(1)?)?;
    let path: String = row.get(2)?;
    let identity: String = row.get(3)?;
    let origin: Option<String> = row.get(4)?;
    Ok((|| {
        Ok(Record {
            id,
            location: Location { area, path },
            identity: serde_json::from_str(&identity)?,
            origin: origin.map(|json| serde_json::from_str(&json)).transpose()?,
        })
    })())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arcdos_filesystem::Root;

    #[test]
    fn migration_preserves_entry_ids_and_journal_then_enqueues_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("note.txt"), "fixture").unwrap();
        let entry = Root::open(directory.path())
            .unwrap()
            .metadata("note.txt")
            .unwrap();
        let db = directory.path().join("metadata.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(SCHEMA_V1).unwrap();
        conn.execute(
            "INSERT INTO entries (id, area, path, identity) VALUES ('legacy', 0, 'note.txt', ?1)",
            [serde_json::to_string(&Identity::new(&entry)).unwrap()],
        )
        .unwrap();
        record_change(&conn, "legacy", Action::Uploaded).unwrap();
        drop(conn);

        let catalog = Catalog::open(&db).unwrap();
        let location = Location::new(Area::Files, "note.txt");
        assert_eq!(catalog.register(&location, &entry).unwrap().id, "legacy");
        let jobs = index::jobs(catalog.connection(), 2).unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "legacy");
        let changes = catalog.changes(0, 10).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].kind, Action::Uploaded as i32);
    }
}
