use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashSet},
};

use crate::{Error, MAX_NEIGHBORS, Result, check_dimensions, vector_norm};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Neighbor {
    pub key: u64,
    /// Cosine distance. Lower values are closer.
    pub distance: f32,
}

pub trait VectorIndex {
    fn dimensions(&self) -> usize;
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn add(&mut self, key: u64, vector: &[f32]) -> Result<()>;
    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<Neighbor>>;
}

pub struct ExactIndex {
    dimensions: usize,
    // Search scans in insertion order. The key set is only used to detect duplicates.
    vectors: Vec<(u64, Vec<f32>)>,
    keys: HashSet<u64>,
}

impl ExactIndex {
    pub fn new(dimensions: usize) -> Result<Self> {
        check_dimensions(dimensions)?;
        Ok(Self {
            dimensions,
            vectors: Vec::new(),
            keys: HashSet::new(),
        })
    }
}

impl VectorIndex for ExactIndex {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn len(&self) -> usize {
        self.vectors.len()
    }

    fn add(&mut self, key: u64, vector: &[f32]) -> Result<()> {
        let vector = normalized(vector, self.dimensions)?;
        if !self.keys.insert(key) {
            return Err(Error::Invalid("duplicate vector key"));
        }
        self.vectors.push((key, vector));
        Ok(())
    }

    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<Neighbor>> {
        check_limit(limit)?;
        let query = normalized(query, self.dimensions)?;
        let mut best = BinaryHeap::with_capacity(limit);
        for (key, vector) in &self.vectors {
            let similarity = vector.iter().zip(&query).map(|(a, b)| a * b).sum::<f32>();
            let candidate = Ranked(Neighbor {
                key: *key,
                distance: 1.0 - similarity.clamp(-1.0, 1.0),
            });
            if best.len() < limit {
                best.push(candidate);
            } else if let Some(mut worst) = best.peek_mut()
                && candidate < *worst
            {
                *worst = candidate;
            }
        }
        Ok(best
            .into_sorted_vec()
            .into_iter()
            .map(|ranked| ranked.0)
            .collect())
    }
}

fn normalized(vector: &[f32], dimensions: usize) -> Result<Vec<f32>> {
    let norm = vector_norm(vector, dimensions)?;
    Ok(vector
        .iter()
        .map(|&value| (f64::from(value) / norm) as f32)
        .collect())
}

fn check_limit(limit: usize) -> Result<()> {
    if !(1..=MAX_NEIGHBORS).contains(&limit) {
        return Err(Error::Invalid("search limit must be between 1 and 100"));
    }
    Ok(())
}

struct Ranked(Neighbor);

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Ranked {}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0
            .distance
            .total_cmp(&other.0.distance)
            .then_with(|| self.0.key.cmp(&other.0.key))
    }
}

#[cfg(feature = "usearch")]
pub struct UsearchIndex(usearch::Index);

#[cfg(feature = "usearch")]
impl UsearchIndex {
    pub fn new(dimensions: usize, capacity: usize) -> Result<Self> {
        check_dimensions(dimensions)?;
        let index = usearch::new_index(&usearch::IndexOptions {
            dimensions,
            metric: usearch::MetricKind::Cos,
            quantization: usearch::ScalarKind::F32,
            ..Default::default()
        })
        .map_err(|error| Error::Usearch(error.to_string()))?;
        index
            .reserve(capacity)
            .map_err(|error| Error::Usearch(error.to_string()))?;
        Ok(Self(index))
    }
}

#[cfg(feature = "usearch")]
impl VectorIndex for UsearchIndex {
    fn dimensions(&self) -> usize {
        self.0.dimensions()
    }

    fn len(&self) -> usize {
        self.0.size()
    }

    fn add(&mut self, key: u64, vector: &[f32]) -> Result<()> {
        let vector = normalized(vector, self.dimensions())?;
        if self.0.contains(key) {
            return Err(Error::Invalid("duplicate vector key"));
        }
        let capacity = self.0.capacity();
        if self.len() == capacity {
            self.0
                .reserve(capacity.saturating_mul(2).max(1))
                .map_err(|error| Error::Usearch(error.to_string()))?;
        }
        self.0
            .add(key, &vector)
            .map_err(|error| Error::Usearch(error.to_string()))
    }

    fn search(&self, query: &[f32], limit: usize) -> Result<Vec<Neighbor>> {
        check_limit(limit)?;
        let query = normalized(query, self.dimensions())?;
        if self.is_empty() {
            return Ok(Vec::new());
        }
        let neighbors = self
            .0
            .search(&query, limit)
            .map_err(|error| Error::Usearch(error.to_string()))?;
        Ok(neighbors
            .keys
            .into_iter()
            .zip(neighbors.distances)
            .map(|(key, distance)| Neighbor {
                key,
                distance: distance.clamp(0.0, 2.0),
            })
            .collect())
    }
}
