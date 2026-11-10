use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
    time::Duration,
};

use arcdos_vector::{Chunk, EmbeddingStore, ExactIndex, Model, Snapshot};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sha2::{Digest, Sha256};
use tokio::{
    sync::Semaphore,
    task::{JoinError, JoinHandle, spawn_blocking},
    time::{Instant, sleep},
};

use super::ai_client::{AiClient, AiError, EmbeddingModel, Embeddings};

const PAGE: usize = 64;
const CHUNK_CHARS: usize = 1024;
const MAX_CHUNKS: usize = 8;
const MODEL_VERSION: &str = "text-prefix-8x1024-v1";
const SNAPSHOT_BUDGET: usize = 32 * 1024 * 1024;
const CHUNK_METADATA_BYTES: usize = 512;
const MODEL_POLL: Duration = Duration::from_secs(15);
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
pub(super) enum SemanticError {
    #[error("semantic index unavailable")]
    Unavailable,
    #[error("semantic search is busy")]
    Busy,
    #[error("embedding model changed")]
    ModelChanged,
    #[error(transparent)]
    Worker(#[from] AiError),
    #[error(transparent)]
    Catalog(#[from] rusqlite::Error),
    #[error(transparent)]
    Embeddings(#[from] arcdos_vector::Error),
    #[error(transparent)]
    Task(#[from] JoinError),
}

type Result<T> = std::result::Result<T, SemanticError>;
type Current = Arc<RwLock<Option<Arc<Published>>>>;

struct Published {
    model: String,
    dimensions: usize,
    index: Snapshot<ExactIndex>,
}

pub(super) struct Candidate {
    pub id: String,
    pub revision: String,
}

pub(super) struct SemanticIndex {
    worker: AiClient,
    current: Current,
    search_slot: Arc<Semaphore>,
    task: JoinHandle<()>,
}

impl SemanticIndex {
    pub fn start(data_root: &Path, worker: AiClient) -> Self {
        let current = Current::default();
        let task = tokio::spawn(run(
            data_root.join("state"),
            worker.clone(),
            current.clone(),
        ));
        Self {
            worker,
            current,
            search_slot: Arc::new(Semaphore::new(1)),
            task,
        }
    }

    pub async fn candidates(&self, query: String, limit: usize) -> Result<Vec<Candidate>> {
        let permit = self
            .search_slot
            .clone()
            .try_acquire_owned()
            .map_err(|_| SemanticError::Busy)?;
        let published = self
            .current
            .read()
            .map_err(|_| SemanticError::Unavailable)?
            .clone()
            .ok_or(SemanticError::Unavailable)?;
        let embeddings = self.worker.embed(&[query]).await?;
        if !same_model(&published.model, published.dimensions, &embeddings, 1) {
            return Err(SemanticError::ModelChanged);
        }
        spawn_blocking(move || {
            let _permit = permit;
            let mut seen = HashSet::new();
            Ok(published
                .index
                .search(&embeddings.vectors[0], limit.clamp(1, 100))?
                .into_iter()
                .take_while(|hit| hit.distance < 1.0)
                .filter_map(|hit| {
                    let source = hit.source;
                    seen.insert(source.file_id.clone()).then_some(Candidate {
                        id: source.file_id,
                        revision: source.revision,
                    })
                })
                .collect())
        })
        .await?
    }
}

impl Drop for SemanticIndex {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(state: PathBuf, worker: AiClient, current: Current) {
    loop {
        if let Err(error) = maintain(&state, &worker, &current).await {
            tracing::debug!(%error, "semantic indexing will retry");
        }
        sleep(RETRY).await;
    }
}

async fn maintain(state: &Path, worker: &AiClient, current: &Current) -> Result<()> {
    let mut model = worker.health().await?;
    let mut db = open(state.to_path_buf(), &model).await?;
    publish(&db, current).await?;
    let mut health_at = Instant::now() + MODEL_POLL;
    let mut publish_at = Instant::now() + SNAPSHOT_INTERVAL;
    let mut dirty = false;
    loop {
        if Instant::now() >= health_at {
            let reported = worker.health().await?;
            if (reported.model.as_str(), reported.dimensions)
                != (model.model.as_str(), model.dimensions)
            {
                db = open(state.to_path_buf(), &reported).await?;
                model = reported;
                publish(&db, current).await?;
                dirty = false;
            }
            health_at = Instant::now() + MODEL_POLL;
        }
        let (job, pruned, idle) = with_db(&db, IndexDb::next).await?;
        dirty |= pruned;
        let mut wait = if idle {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(150)
        };
        if let Some(job) = job {
            match worker.embed(&job.texts).await {
                Ok(embeddings) => {
                    if !same_model(&model.model, model.dimensions, &embeddings, job.texts.len()) {
                        return Err(SemanticError::ModelChanged);
                    }
                    dirty |= with_db(&db, move |db| db.save(job, embeddings.vectors)).await?;
                }
                Err(error) => {
                    tracing::debug!(%error, "document embedding will retry on the next scan");
                    wait = RETRY;
                }
            }
        }
        if dirty && Instant::now() >= publish_at {
            publish(&db, current).await?;
            publish_at = Instant::now() + SNAPSHOT_INTERVAL;
            dirty = false;
        }
        sleep(wait).await;
    }
}

fn same_model(model: &str, dimensions: usize, embeddings: &Embeddings, count: usize) -> bool {
    embeddings.model == model
        && embeddings.vectors.len() == count
        && embeddings
            .vectors
            .iter()
            .all(|vector| vector.len() == dimensions)
}

async fn open(state: PathBuf, model: &EmbeddingModel) -> Result<Arc<Mutex<IndexDb>>> {
    let dimensions = model.dimensions;
    let model = model.model.clone();
    spawn_blocking(move || {
        Ok(Arc::new(Mutex::new(IndexDb::open(
            &state, model, dimensions,
        )?)))
    })
    .await?
}

async fn with_db<T: Send + 'static>(
    db: &Arc<Mutex<IndexDb>>,
    operation: impl FnOnce(&mut IndexDb) -> Result<T> + Send + 'static,
) -> Result<T> {
    let db = db.clone();
    spawn_blocking(move || {
        let mut db = db.lock().map_err(|_| SemanticError::Unavailable)?;
        operation(&mut db)
    })
    .await?
}

async fn publish(db: &Arc<Mutex<IndexDb>>, current: &Current) -> Result<()> {
    let current = current.clone();
    with_db(db, move |db| {
        let snapshot = db.snapshot()?.map(Arc::new);
        *current.write().map_err(|_| SemanticError::Unavailable)? = snapshot;
        Ok(())
    })
    .await
}

struct Job {
    id: String,
    revision: String,
    texts: Vec<String>,
}

struct IndexDb {
    catalog: Connection,
    embeddings: EmbeddingStore,
    model_key: String,
    model: Model,
    dimensions: usize,
    after: String,
    prune_after: String,
}

impl IndexDb {
    fn open(state: &Path, model_key: String, dimensions: usize) -> Result<Self> {
        let model = Model::new(&model_key, MODEL_VERSION, dimensions)?;
        let catalog = Connection::open_with_flags(
            state.join("metadata.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        catalog.busy_timeout(Duration::from_millis(250))?;
        Ok(Self {
            catalog,
            embeddings: EmbeddingStore::open(&state.join("semantic.db"))?,
            model_key,
            model,
            dimensions,
            after: String::new(),
            prune_after: String::new(),
        })
    }

    fn next(&mut self) -> Result<(Option<Job>, bool, bool)> {
        let pruned = self.prune()?;
        let mut page = self.catalog.prepare_cached(
            "SELECT s.entry_id, s.revision,
                    (e.area=0 AND e.path=s.indexed_path AND s.indexed_revision=s.revision
                     AND s.content_sha256 IS NOT NULL) IS TRUE
             FROM search_state s JOIN entries e ON e.id=s.entry_id
             WHERE s.entry_id>?1 ORDER BY s.entry_id LIMIT ?2",
        )?;
        let mut rows = page.query(params![self.after, PAGE as i64])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            count += 1;
            self.after = row.get(0)?;
            if !row.get::<_, bool>(2)? {
                continue;
            }
            let revision: String = row.get(1)?;
            if self
                .embeddings
                .file_revision(&self.model, &self.after)?
                .as_deref()
                == Some(revision.as_str())
            {
                continue;
            }
            let text: Option<String> = self
                .catalog
                .prepare_cached(
                    "SELECT substr(f.content, 1, ?3)
                 FROM search_state s JOIN file_search f ON f.rowid=s.document_id
                 WHERE s.entry_id=?1 AND s.indexed_revision=?2",
                )?
                .query_row(
                    params![self.after, revision, (CHUNK_CHARS * MAX_CHUNKS) as i64],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(text) = text else { continue };
            let texts = chunks(&text);
            if !texts.is_empty() {
                return Ok((
                    Some(Job {
                        id: self.after.clone(),
                        revision,
                        texts,
                    }),
                    pruned,
                    false,
                ));
            }
        }
        if count < PAGE {
            self.after.clear();
        }
        Ok((None, pruned, count < PAGE))
    }

    fn save(&mut self, job: Job, vectors: Vec<Vec<f32>>) -> Result<bool> {
        let current: bool = self
            .catalog
            .prepare_cached(
                "SELECT EXISTS (
                SELECT 1 FROM search_state s JOIN entries e ON e.id=s.entry_id
                WHERE s.entry_id=?1 AND e.area=0 AND e.path=s.indexed_path
                  AND s.revision=?2 AND s.indexed_revision=?2 AND s.content_sha256 IS NOT NULL
            )",
            )?
            .query_row(params![job.id, job.revision], |row| row.get(0))?;
        if !current {
            return Ok(false);
        }
        let chunks: Vec<_> = job
            .texts
            .iter()
            .zip(&vectors)
            .enumerate()
            .map(|(position, (text, vector))| Chunk {
                position: position as u32,
                content_hash: Sha256::digest(text.as_bytes()).into(),
                embedding: vector,
            })
            .collect();
        self.embeddings
            .replace_file(&self.model, &job.id, &job.revision, &chunks)?;
        Ok(true)
    }

    fn prune(&mut self) -> Result<bool> {
        let ids = self.embeddings.file_ids_after(&self.prune_after, PAGE)?;
        let mut query = self.catalog.prepare_cached(
            "SELECT EXISTS (
                SELECT 1 FROM entries e LEFT JOIN search_state s ON s.entry_id=e.id
                WHERE e.id=?1 AND e.area=0
                  AND NOT (s.indexed_revision=s.revision AND s.content_sha256 IS NULL) IS TRUE
             )",
        )?;
        let mut changed = false;
        for id in &ids {
            if !query.query_row([id], |row| row.get::<_, bool>(0))? {
                self.embeddings.remove_file(id)?;
                changed = true;
            }
        }
        self.prune_after = if ids.len() == PAGE {
            ids.into_iter().last().unwrap_or_default()
        } else {
            String::new()
        };
        Ok(changed)
    }

    fn snapshot(&mut self) -> Result<Option<Published>> {
        let limit = SNAPSHOT_BUDGET / (self.dimensions * size_of::<f32>() + CHUNK_METADATA_BYTES);
        let index = self.embeddings.rebuild_bounded(
            &self.model,
            ExactIndex::new(self.dimensions)?,
            limit,
        )?;
        Ok(index
            .filter(|index| !index.is_empty())
            .map(|index| Published {
                model: self.model_key.clone(),
                dimensions: self.dimensions,
                index,
            }))
    }
}

fn chunks(mut text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    for _ in 0..MAX_CHUNKS {
        if text.is_empty() {
            break;
        }
        let end = text
            .char_indices()
            .nth(CHUNK_CHARS)
            .map_or(text.len(), |(offset, _)| offset);
        let (head, tail) = text.split_at(end);
        let head = head.trim();
        if !head.is_empty() {
            chunks.push(head.to_owned());
        }
        text = tail;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::files::{index::Document, store::Store};

    #[test]
    fn resumes_indexing_and_discards_stale_work() {
        let data = tempfile::tempdir().unwrap();
        let store = Store::open(data.path()).unwrap();
        let file = data.path().join("files/notes.txt");
        let state = data.path().join("state");
        fs::write(&file, "orchard").unwrap();
        let index_document = || {
            store.observe_path("notes.txt").unwrap();
            let job = store.index_jobs(1).unwrap().pop().unwrap();
            let text = Document::read(&store.files_root(), &job).unwrap().unwrap();
            store.write_index(&job, text).unwrap();
        };
        index_document();

        let mut db = IndexDb::open(&state, "test-model".to_owned(), 2).unwrap();
        let first = db.next().unwrap().0.unwrap();
        assert_eq!(first.texts, ["orchard"]);
        let id = first.id.clone();
        let first_revision = first.revision.clone();
        assert!(db.save(first, vec![vec![1.0, 0.0]]).unwrap());
        drop(db);

        let mut db = IndexDb::open(&state, "test-model".to_owned(), 2).unwrap();
        assert!(db.next().unwrap().0.is_none());
        assert!(
            db.embeddings
                .rebuild_bounded(&db.model, ExactIndex::new(2).unwrap(), 0)
                .unwrap()
                .is_none()
        );
        let snapshot = db.snapshot().unwrap().unwrap();
        let hit = snapshot
            .index
            .search(&[1.0, 0.0], 1)
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(hit.source.file_id, id);
        assert_eq!(hit.source.revision, first_revision);
        let hits = store
            .semantic_hits(vec![Candidate {
                id: id.clone(),
                revision: first_revision.clone(),
            }])
            .unwrap();
        assert_eq!(hits[0].path, "notes.txt");

        fs::write(&file, "changed during embedding").unwrap();
        index_document();
        let job = db.next().unwrap().0.unwrap();
        fs::write(&file, "changed again during embedding").unwrap();
        index_document();
        assert!(!db.save(job, vec![vec![1.0, 0.0]]).unwrap());
        assert!(
            store
                .semantic_hits(vec![Candidate {
                    id: id.clone(),
                    revision: first_revision
                }])
                .unwrap()
                .is_empty()
        );
        db.after.clear();
        let replacement = db.next().unwrap().0.unwrap();
        assert!(db.save(replacement, vec![vec![0.0, 1.0]]).unwrap());

        fs::remove_file(file).unwrap();
        store.observe_path("notes.txt").unwrap();
        assert!(db.next().unwrap().1);
        assert!(db.embeddings.file_ids_after("", 10).unwrap().is_empty());
    }

    #[test]
    fn chunks_bound_unicode_text_at_scalar_boundaries() {
        let text = "🦀".repeat(CHUNK_CHARS * MAX_CHUNKS + 1);
        let chunks = chunks(&text);
        assert_eq!(chunks.len(), MAX_CHUNKS);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() == CHUNK_CHARS)
        );
        assert!(super::chunks(" \n\t ").is_empty());
    }
}
