use std::time::{Duration, Instant};

use arcdos_vector::{ExactIndex, Neighbor, UsearchIndex, VectorIndex};

const DIMENSIONS: usize = 384;
const CLUSTERS: usize = 64;
const QUERIES: usize = 100;
const TOP: usize = 10;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = std::env::args()
        .nth(1)
        .map(|arg| arg.parse())
        .transpose()?
        .unwrap_or(10_000);
    if !(TOP..=100_000).contains(&count) {
        return Err("use between 10 and 100000 synthetic vectors".into());
    }
    let mut rng = Generator(0xf157_41cb_93fa_7c19);
    let centers: Vec<Vec<f32>> = (0..CLUSTERS)
        .map(|_| (0..DIMENSIONS).map(|_| rng.value()).collect())
        .collect();
    let vectors: Vec<_> = (0..count)
        .map(|i| rng.near(&centers[i % CLUSTERS]))
        .collect();
    let queries: Vec<_> = (0..QUERIES)
        .map(|i| rng.near(&centers[i % CLUSTERS]))
        .collect();

    let exact = measure(ExactIndex::new(DIMENSIONS)?, &vectors, &queries)?;
    let approximate = measure(UsearchIndex::new(DIMENSIONS, count)?, &vectors, &queries)?;
    let recall: usize = exact
        .results
        .iter()
        .zip(&approximate.results)
        .map(|(expected, actual)| {
            expected
                .iter()
                .filter(|target| actual.iter().any(|hit| hit.key == target.key))
                .count()
        })
        .sum();

    println!("{count} vectors · {DIMENSIONS} dimensions · {QUERIES} queries · top {TOP}");
    println!(
        "Exact   build {:>9.2?} · p95 {:>9.2?}",
        exact.build, exact.p95
    );
    println!(
        "USearch build {:>9.2?} · p95 {:>9.2?} · recall {:.1}%",
        approximate.build,
        approximate.p95,
        recall as f64 * 100.0 / (QUERIES * TOP) as f64
    );
    Ok(())
}

struct Sample {
    build: Duration,
    p95: Duration,
    results: Vec<Vec<Neighbor>>,
}

fn measure(
    mut index: impl VectorIndex,
    vectors: &[Vec<f32>],
    queries: &[Vec<f32>],
) -> arcdos_vector::Result<Sample> {
    let started = Instant::now();
    for (i, vector) in vectors.iter().enumerate() {
        index.add(i as u64, vector)?;
    }
    let build = started.elapsed();
    for query in &queries[..10] {
        index.search(query, TOP)?;
    }
    let mut timings = Vec::with_capacity(queries.len());
    let mut results = Vec::with_capacity(queries.len());
    for query in queries {
        let started = Instant::now();
        let hits = index.search(query, TOP)?;
        timings.push(started.elapsed());
        results.push(hits);
    }
    timings.sort_unstable();
    let p95 = timings[(95 * timings.len()).div_ceil(100) - 1];
    Ok(Sample {
        build,
        p95,
        results,
    })
}

struct Generator(u64);

impl Generator {
    fn value(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32) * (1.0 / 8_388_608.0) - 1.0
    }

    fn near(&mut self, center: &[f32]) -> Vec<f32> {
        center
            .iter()
            .map(|value| value + 0.35 * self.value())
            .collect()
    }
}
