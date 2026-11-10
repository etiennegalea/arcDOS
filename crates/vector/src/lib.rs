//! Durable embedding records and replaceable cosine-search indexes.

mod index;
mod store;

#[cfg(feature = "usearch")]
pub use index::UsearchIndex;
pub use index::{ExactIndex, Neighbor, VectorIndex};
pub use store::{Chunk, EmbeddingStore, Match, Model, Snapshot, SourceChunk};

const MAX_DIMENSIONS: usize = 4096;
const MAX_NEIGHBORS: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid vector index input: {0}")]
    Invalid(&'static str),
    #[error("embedding database operation failed")]
    Database(#[from] rusqlite::Error),
    #[error("embedding database requires WAL-capable storage and a compatible schema")]
    IncompatibleDatabase,
    #[error("embedding records or index snapshot are inconsistent")]
    Inconsistent,
    #[cfg(feature = "usearch")]
    #[error("USearch index operation failed: {0}")]
    Usearch(String),
}

pub type Result<T> = std::result::Result<T, Error>;

fn check_dimensions(dimensions: usize) -> Result<()> {
    if !(1..=MAX_DIMENSIONS).contains(&dimensions) {
        return Err(Error::Invalid("invalid embedding dimensions"));
    }
    Ok(())
}

fn vector_norm(vector: &[f32], dimensions: usize) -> Result<f64> {
    if vector.len() != dimensions || vector.iter().any(|value| !value.is_finite()) {
        return Err(Error::Invalid("invalid embedding dimensions or values"));
    }
    let norm = vector
        .iter()
        .map(|&value| f64::from(value).powi(2))
        .sum::<f64>();
    if norm == 0.0 {
        return Err(Error::Invalid("cosine embeddings must have nonzero norm"));
    }
    Ok(norm.sqrt())
}
