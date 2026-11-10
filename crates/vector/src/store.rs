use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

use crate::{Error, Result, VectorIndex, check_dimensions, vector_norm};

const MAX_FILE_CHUNKS: usize = 4096;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Model {
    id: String,
    version: String,
    dimensions: usize,
}

impl Model {
    pub fn new(id: &str, version: &str, dimensions: usize) -> Result<Self> {
        check_identifier(id)?;
        check_identifier(version)?;
        check_dimensions(dimensions)?;
        Ok(Self {
            id: id.to_owned(),
            version: version.to_owned(),
            dimensions,
        })
    }
}

pub struct Chunk<'a> {
    pub position: u32,
    pub content_hash: [u8; 32],
    pub embedding: &'a [f32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceChunk {
    pub file_id: String,
    pub revision: String,
    pub position: u32,
    pub content_hash: [u8; 32],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Match {
    pub source: SourceChunk,
    pub distance: f32,
}

pub struct Snapshot<I> {
    index: I,
    sources: HashMap<u64, SourceChunk>,
}

impl<I: VectorIndex> Snapshot<I> {
    pub fn is_empty(&self) -> bool {
        self.index.is_empty()
    }

    pub fn search(&self, query: &[f32], limit: usize) -> Result<Vec<Match>> {
        self.index
            .search(query, limit)?
            .into_iter()
            .map(|neighbor| {
                let source = self
                    .sources
                    .get(&neighbor.key)
                    .ok_or(Error::Inconsistent)?
                    .clone();
                Ok(Match {
                    source,
                    distance: neighbor.distance,
                })
            })
            .collect()
    }
}

pub struct EmbeddingStore(Connection);

impl EmbeddingStore {
    /// Use a file in the server's private state directory on local storage.
    pub fn open(path: &Path) -> Result<Self> {
        let mut db = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        db.busy_timeout(Duration::from_secs(2))?;
        let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::IncompatibleDatabase);
        }
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL;")?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match tx.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))? {
            0 => tx.execute_batch(
                "CREATE TABLE models (
                    id TEXT NOT NULL, version TEXT NOT NULL,
                    dimensions INTEGER NOT NULL CHECK (dimensions BETWEEN 1 AND 4096),
                    PRIMARY KEY (id, version)
                 ) STRICT, WITHOUT ROWID;
                 CREATE TABLE embeddings (
                    key INTEGER PRIMARY KEY,
                    model_id TEXT NOT NULL, model_version TEXT NOT NULL,
                    file_id TEXT NOT NULL, revision TEXT NOT NULL,
                    position INTEGER NOT NULL CHECK (position BETWEEN 0 AND 4294967295),
                    content_hash BLOB NOT NULL CHECK (length(content_hash) = 32),
                    embedding BLOB NOT NULL,
                    FOREIGN KEY (model_id, model_version) REFERENCES models (id, version),
                    UNIQUE (model_id, model_version, file_id, position)
                 ) STRICT;
                 CREATE INDEX embeddings_file_id ON embeddings (file_id);
                 PRAGMA user_version = 1;",
            )?,
            1 => (),
            _ => return Err(Error::IncompatibleDatabase),
        }
        tx.commit()?;
        Ok(Self(db))
    }

    /// Replace all of a file's chunks for this model in one transaction.
    pub fn replace_file(
        &mut self,
        model: &Model,
        file_id: &str,
        revision: &str,
        chunks: &[Chunk<'_>],
    ) -> Result<()> {
        check_identifier(file_id)?;
        check_identifier(revision)?;
        if chunks.len() > MAX_FILE_CHUNKS {
            return Err(Error::Invalid("too many chunks for one file revision"));
        }
        let mut positions = HashSet::with_capacity(chunks.len());
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO models (id, version, dimensions) VALUES (?1, ?2, ?3)
                    ON CONFLICT (id, version) DO NOTHING",
            params![model.id, model.version, model.dimensions as i64],
        )?;
        check_model(&tx, model)?;
        tx.execute(
            "DELETE FROM embeddings WHERE model_id = ?1 AND model_version = ?2 AND file_id = ?3",
            params![model.id, model.version, file_id],
        )?;
        {
            let mut insert = tx.prepare(
                "INSERT INTO embeddings
                (model_id, model_version, file_id, revision, position, content_hash, embedding)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for chunk in chunks {
                if !positions.insert(chunk.position) {
                    return Err(Error::Invalid("duplicate chunk position"));
                }
                vector_norm(chunk.embedding, model.dimensions)?;
                let bytes: Vec<_> = chunk
                    .embedding
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                insert.execute(params![
                    model.id,
                    model.version,
                    file_id,
                    revision,
                    chunk.position,
                    &chunk.content_hash[..],
                    bytes
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a file across all stored model versions.
    pub fn remove_file(&mut self, file_id: &str) -> Result<()> {
        check_identifier(file_id)?;
        self.0
            .execute("DELETE FROM embeddings WHERE file_id = ?1", [file_id])?;
        Ok(())
    }

    /// Return the source revision of a previously embedded file for this model.
    pub fn file_revision(&self, model: &Model, file_id: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .prepare_cached(
                "SELECT revision FROM embeddings
                 WHERE model_id=?1 AND model_version=?2 AND file_id=?3 LIMIT 1",
            )?
            .query_row(params![model.id, model.version, file_id], |row| row.get(0))
            .optional()?)
    }

    /// Page the distinct source IDs for reconciliation with the file catalog.
    pub fn file_ids_after(&self, after: &str, limit: usize) -> Result<Vec<String>> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid("file page limit must be between 1 and 1000"));
        }
        let mut query = self.0.prepare_cached(
            "SELECT DISTINCT file_id FROM embeddings WHERE file_id>?1 ORDER BY file_id LIMIT ?2",
        )?;
        Ok(query
            .query_map(params![after, limit as i64], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Build a new index and its source mapping from one consistent database snapshot.
    /// A consumer checks each result's source revision against the live file catalog.
    pub fn rebuild<I: VectorIndex>(&mut self, model: &Model, index: I) -> Result<Snapshot<I>> {
        self.rebuild_bounded(model, index, i64::MAX as usize)?
            .ok_or(Error::Inconsistent)
    }

    /// Return no snapshot if the model has more records than the supplied limit.
    pub fn rebuild_bounded<I: VectorIndex>(
        &mut self,
        model: &Model,
        mut index: I,
        limit: usize,
    ) -> Result<Option<Snapshot<I>>> {
        if !index.is_empty() || index.dimensions() != model.dimensions {
            return Err(Error::Invalid(
                "rebuild requires an empty index with matching dimensions",
            ));
        }
        let tx = self.0.transaction()?;
        check_model(&tx, model)?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        if limit < i64::MAX {
            let exceeds_limit = tx
                .query_row(
                    "SELECT 1 FROM embeddings WHERE model_id=?1 AND model_version=?2 LIMIT 1 OFFSET ?3",
                    params![model.id, model.version, limit],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if exceeds_limit {
                return Ok(None);
            }
        }
        let mut sources = HashMap::new();
        {
            let mut select = tx.prepare(
                "SELECT key, file_id, revision, position, content_hash, embedding
                FROM embeddings WHERE model_id = ?1 AND model_version = ?2 ORDER BY key",
            )?;
            let mut rows = select.query(params![model.id, model.version])?;
            while let Some(row) = rows.next()? {
                let key = u64::try_from(row.get::<_, i64>(0)?).map_err(|_| Error::Inconsistent)?;
                let hash: Vec<u8> = row.get(4)?;
                let bytes: Vec<u8> = row.get(5)?;
                if bytes.len() != model.dimensions * 4 {
                    return Err(Error::Inconsistent);
                }
                let vector: Vec<_> = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .copied()
                    .map(f32::from_le_bytes)
                    .collect();
                index.add(key, &vector)?;
                sources.insert(
                    key,
                    SourceChunk {
                        file_id: row.get(1)?,
                        revision: row.get(2)?,
                        position: row.get(3)?,
                        content_hash: hash.try_into().map_err(|_| Error::Inconsistent)?,
                    },
                );
            }
        }
        tx.commit()?;
        Ok(Some(Snapshot { index, sources }))
    }
}

fn check_model(db: &Connection, model: &Model) -> Result<()> {
    let dimensions: Option<i64> = db
        .query_row(
            "SELECT dimensions FROM models WHERE id = ?1 AND version = ?2",
            params![model.id, model.version],
            |row| row.get(0),
        )
        .optional()?;
    if dimensions.is_some_and(|dimensions| dimensions != model.dimensions as i64) {
        return Err(Error::Invalid(
            "model version already uses different dimensions",
        ));
    }
    Ok(())
}

fn check_identifier(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 255 || !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(Error::Invalid(
            "identifiers must contain 1 to 255 printable ASCII bytes",
        ));
    }
    Ok(())
}
