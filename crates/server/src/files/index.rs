use std::{
    borrow::Cow,
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use arcdos_filesystem::{Entry, Kind, Root};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};
use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use super::{
    Error, Result,
    catalog::{Area, Location},
    store::revision,
};

const TEXT_LIMIT: u64 = 256 * 1024;
const MAX_QUERY_CHARS: usize = 256;
const MAX_QUERY_TERMS: usize = 12;
const MAX_TAGS: usize = 24;
const MAX_TAG_CHARS: usize = 48;

pub(super) const SCHEMA_V2: &str = r"
BEGIN IMMEDIATE;
CREATE TABLE search_state (
    document_id INTEGER PRIMARY KEY,
    entry_id TEXT NOT NULL UNIQUE REFERENCES entries(id) ON DELETE CASCADE,
    revision TEXT NOT NULL,
    indexed_revision TEXT,
    indexed_path TEXT,
    content_sha256 BLOB
);
CREATE TABLE index_jobs (
    entry_id TEXT PRIMARY KEY REFERENCES entries(id) ON DELETE CASCADE,
    attempts INTEGER NOT NULL DEFAULT 0,
    ready_at INTEGER NOT NULL DEFAULT 0
) WITHOUT ROWID;
CREATE INDEX index_jobs_ready ON index_jobs(ready_at, entry_id);
CREATE TABLE file_tags (
    entry_id TEXT NOT NULL REFERENCES entries(id) ON DELETE CASCADE,
    tag TEXT NOT NULL,
    PRIMARY KEY (entry_id, tag)
) WITHOUT ROWID;
CREATE VIRTUAL TABLE file_search USING fts5(
    name, path, tags, content, metadata,
    tokenize = 'unicode61 remove_diacritics 2', prefix = '2 3'
);
CREATE TRIGGER search_drop_document AFTER DELETE ON search_state BEGIN
    DELETE FROM file_search WHERE rowid = old.document_id;
END;
CREATE TRIGGER search_move_document AFTER UPDATE OF area, path ON entries
WHEN old.area <> new.area OR old.path <> new.path
BEGIN
    DELETE FROM file_search
    WHERE new.area <> 0 AND rowid = (
        SELECT document_id FROM search_state WHERE entry_id = new.id
    );
    DELETE FROM index_jobs WHERE entry_id = new.id AND new.area <> 0;
    INSERT INTO index_jobs (entry_id)
    SELECT new.id WHERE new.area = 0 AND new.path <> ''
    ON CONFLICT(entry_id) DO UPDATE SET ready_at = 0, attempts = 0;
END;
PRAGMA user_version=2;
COMMIT;
";

pub(super) struct Job {
    pub id: String,
    pub path: String,
    pub revision: String,
}

pub(super) struct Document {
    name: String,
    content: String,
    metadata: String,
    content_hash: Option<[u8; 32]>,
}

impl Document {
    pub fn read(root: &Root, job: &Job) -> Result<Option<Self>> {
        let before = root.metadata(&job.path)?;
        if revision(&job.id, &before) != job.revision {
            return Ok(None);
        }
        let location = Location::new(Area::Files, &job.path);
        let name = location.name().to_owned();
        let extension = Path::new(&name)
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("");
        let mut content = String::new();
        let mut content_hash = None;
        let kind = match before.kind {
            Kind::Directory => "folder directory",
            Kind::File => {
                if before.size <= TEXT_LIMIT && text_extension(extension) {
                    let file = root.open_file(&job.path);
                    match file {
                        Ok(mut file) => {
                            if Entry::from_file(&file)? != before {
                                return Ok(None);
                            }
                            let mut bytes = Vec::new();
                            (&mut file).take(TEXT_LIMIT + 1).read_to_end(&mut bytes)?;
                            if Entry::from_file(&file)? != before
                                || bytes.len() as u64 != before.size
                            {
                                return Ok(None);
                            }
                            if !bytes.contains(&0)
                                && let Ok(text) = String::from_utf8(bytes)
                            {
                                content_hash = Some(Sha256::digest(text.as_bytes()).into());
                                content = if text.is_ascii() {
                                    text
                                } else {
                                    text.nfc().collect()
                                };
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                "file"
            }
        };
        let metadata = format!("{kind} {extension}");
        Ok(Some(Self {
            name,
            content,
            metadata,
            content_hash,
        }))
    }
}

fn text_extension(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "txt"
            | "md"
            | "markdown"
            | "rst"
            | "log"
            | "csv"
            | "tsv"
            | "json"
            | "toml"
            | "yaml"
            | "yml"
            | "xml"
            | "html"
            | "htm"
            | "css"
            | "js"
            | "ts"
            | "rs"
            | "go"
            | "py"
            | "sh"
            | "sql"
            | "ini"
    )
}

pub(super) fn observe(
    conn: &Connection,
    id: &str,
    location: &Location,
    entry: &Entry,
) -> Result<bool> {
    if location.area != Area::Files || location.path.is_empty() {
        return Ok(false);
    }
    let current = revision(id, entry);
    let previous: Option<String> = conn
        .query_row(
            "SELECT revision FROM search_state WHERE entry_id=?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    if previous.as_deref() == Some(&current) {
        return Ok(false);
    }
    conn.execute(
        "INSERT INTO search_state (entry_id, revision) VALUES (?1, ?2)
         ON CONFLICT(entry_id) DO UPDATE SET revision=excluded.revision",
        params![id, current],
    )?;
    conn.execute(
        "INSERT INTO index_jobs (entry_id) VALUES (?1)
         ON CONFLICT(entry_id) DO UPDATE SET ready_at=0, attempts=0",
        [id],
    )?;
    Ok(previous.is_some())
}

pub(super) fn jobs(conn: &Connection, limit: usize) -> Result<Vec<Job>> {
    let mut query = conn.prepare_cached(
        "SELECT j.entry_id, e.path, s.revision
         FROM index_jobs j JOIN entries e ON e.id=j.entry_id
         JOIN search_state s ON s.entry_id=j.entry_id
         WHERE j.ready_at <= ?1 AND e.area=0
         ORDER BY j.ready_at, j.entry_id LIMIT ?2",
    )?;
    Ok(query
        .query_map(params![now(), limit as i64], |row| {
            Ok(Job {
                id: row.get(0)?,
                path: row.get(1)?,
                revision: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn retry(conn: &Connection, job: &Job) -> Result<()> {
    conn.execute(
        "UPDATE index_jobs
         SET ready_at=?2 + min(60, 1 << attempts), attempts=min(6, attempts + 1)
         WHERE entry_id=?1 AND EXISTS (
           SELECT 1 FROM search_state s JOIN entries e ON e.id=s.entry_id
           WHERE s.entry_id=?1 AND e.area=0 AND e.path=?3 AND s.revision=?4
         )",
        params![job.id, now(), job.path, job.revision],
    )?;
    Ok(())
}

pub(super) fn write(conn: &Connection, job: &Job, document: Document) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let rowid: Option<i64> = tx
        .query_row(
            "SELECT s.document_id FROM search_state s JOIN entries e ON e.id=s.entry_id
         WHERE s.entry_id=?1 AND e.area=0 AND e.path=?2 AND s.revision=?3",
            params![job.id, job.path, job.revision],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(rowid) = rowid {
        let tags = tags(&tx, &job.id)?.join(" ");
        tx.execute(
            "INSERT OR REPLACE INTO file_search (rowid, name, path, tags, content, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rowid,
                nfc(&document.name).as_ref(),
                nfc(&job.path).as_ref(),
                nfc(&tags).as_ref(),
                document.content,
                nfc(&document.metadata).as_ref()
            ],
        )?;
        tx.execute(
            "UPDATE search_state SET indexed_revision=?2, indexed_path=?3, content_sha256=?4
             WHERE document_id=?1",
            params![
                rowid,
                job.revision,
                job.path,
                document.content_hash.as_ref().map(|hash| hash.as_slice())
            ],
        )?;
        tx.execute("DELETE FROM index_jobs WHERE entry_id=?1", [&job.id])?;
    }
    tx.commit()?;
    Ok(())
}

pub(super) fn search_expression(query: &str) -> Result<String> {
    if query.chars().take(MAX_QUERY_CHARS + 1).count() > MAX_QUERY_CHARS {
        return Err(Error::Invalid("search query is too long"));
    }
    let query = nfc(query);
    let mut terms = query
        .split(|ch: char| !ch.is_alphanumeric() && !is_combining_mark(ch))
        .filter(|term| term.chars().any(char::is_alphanumeric));
    let expression = terms
        .by_ref()
        .take(MAX_QUERY_TERMS)
        .map(|term| {
            let suffix = if term.chars().nth(1).is_some() {
                "*"
            } else {
                ""
            };
            format!("\"{term}\"{suffix}")
        })
        .collect::<Vec<_>>()
        .join(" AND ");
    if terms.next().is_some() {
        return Err(Error::Invalid("search query has too many terms"));
    }
    Ok(expression)
}

pub(super) fn candidates(conn: &Connection, expression: &str, count: usize) -> Result<Vec<Job>> {
    if expression.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare_cached(
        "SELECT e.id, e.path, s.revision
         FROM file_search JOIN search_state s ON s.document_id=file_search.rowid
         JOIN entries e ON e.id=s.entry_id
         WHERE file_search MATCH ?1 AND e.area=0 AND e.path=s.indexed_path
           AND s.indexed_revision=s.revision
           AND rank MATCH 'bm25(9.0, 4.0, 6.0, 1.0, 1.0)'
         ORDER BY rank
         LIMIT ?2",
    )?;
    Ok(statement
        .query_map(params![expression, count as i64], |row| {
            Ok(Job {
                id: row.get(0)?,
                path: row.get(1)?,
                revision: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn tags(conn: &Connection, id: &str) -> Result<Vec<String>> {
    let mut statement =
        conn.prepare_cached("SELECT tag FROM file_tags WHERE entry_id=?1 ORDER BY tag")?;
    Ok(statement
        .query_map([id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

pub(super) fn normalized_tags(input: Vec<String>) -> Result<Vec<String>> {
    if input.len() > MAX_TAGS {
        return Err(Error::Invalid("too many tags"));
    }
    let mut tags = input
        .into_iter()
        .map(|tag| {
            let tag = tag.trim();
            if tag.is_empty()
                || tag.chars().take(MAX_TAG_CHARS + 1).count() > MAX_TAG_CHARS
                || tag.chars().any(char::is_control)
            {
                return Err(Error::Invalid("invalid tag"));
            }
            Ok(tag.to_owned())
        })
        .collect::<Result<Vec<_>>>()?;
    tags.sort_unstable();
    tags.dedup();
    Ok(tags)
}

pub(super) fn replace_tags(conn: &Connection, id: &str, tags: &[String]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    if self::tags(&tx, id)? == tags {
        return Ok(());
    }
    tx.execute("DELETE FROM file_tags WHERE entry_id=?1", [id])?;
    for tag in tags {
        tx.execute("INSERT INTO file_tags VALUES (?1, ?2)", params![id, tag])?;
    }
    tx.execute(
        "UPDATE file_search SET tags=?2
         WHERE rowid=(SELECT document_id FROM search_state WHERE entry_id=?1)",
        params![id, nfc(&tags.join(" ")).as_ref()],
    )?;
    super::catalog::record_change(&tx, id, super::catalog::Action::Modified)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn rebuild(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "BEGIN IMMEDIATE;
         DELETE FROM file_search;
         UPDATE search_state SET indexed_revision=NULL, indexed_path=NULL, content_sha256=NULL;
         INSERT INTO index_jobs (entry_id) SELECT id FROM entries WHERE area=0 AND path<>''
         ON CONFLICT(entry_id) DO UPDATE SET ready_at=0, attempts=0;
         COMMIT;",
    )?;
    Ok(())
}

fn nfc(text: &str) -> Cow<'_, str> {
    if text.is_ascii() {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.nfc().collect())
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}
