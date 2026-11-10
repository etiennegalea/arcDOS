# Vector index evaluation

`arcdos-vector` owns durable embeddings and cosine retrieval. The `VectorIndex` interface supports an exhaustive Rust implementation and an optional USearch implementation. The M6 file server uses the exact index when local embeddings are enabled. A background task builds snapshots from the private embedding database. [Local AI setup](../deployment/local-ai.md) · [USearch Rust API](https://docs.rs/usearch/latest/usearch/)

`EmbeddingStore` keeps model IDs, model versions, dimensions, opaque file IDs, source revisions, chunk positions, SHA-256 content hashes, and little-endian `f32` embeddings in a versioned SQLite database. Place it in the private state directory on local storage that supports WAL. A transaction replaces all chunks for one file and model version together. A rebuild reads one consistent database snapshot into a fresh index and binds its numeric keys to the stored chunk identities. File IDs and revisions let a consumer validate a result against the live catalog before exposing it. [SQLite WAL](https://www.sqlite.org/wal.html)

The database can rebuild either index after process restart. If the embedding database is lost or a model changes, regenerate the records from source files with the configured extractor, chunker, and model. Use a new model version when changing the embedding model or chunking policy. Rebuilding an in-memory graph holds a SQLite read transaction until it completes, so concurrent updates can grow the WAL during a rebuild.

## Reproduce the benchmark

Run the comparison on a Linux host with the pinned Rust toolchain and a C++ compiler.

```sh
cargo run --locked --release -p arcdos-vector --features usearch --example compare -- 10000
cargo run --locked --release -p arcdos-vector --features usearch --example compare -- 100000
```

The benchmark uses a fixed seed to generate 384-dimensional vectors around 64 synthetic clusters and 100 distinct queries. Both implementations use `f32` storage and cosine distance. USearch 2.26.6 runs with its default graph parameters and without optional SIMD or OpenMP features. Each implementation processes the same insertions and warms up with 10 queries. The report measures insertion time, p95 single-query latency for 10 results, and USearch's mean recall against the exact top 10. Timings exclude vector generation, initial capacity reservation, SQLite I/O, and embedding inference. Real embeddings and concurrent workloads can give different latency and recall.

Single-caller measurements from the pinned Rust 1.99.1 Bookworm container on an Intel Core i7-2600K host.

| Vectors | Exact build | Exact p95 | USearch build | USearch p95 | USearch recall@10 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 10,000 | 32.99 ms | 8.66 ms | 3.76 s | 235.22 µs | 99.9% |
| 100,000 | 345.06 ms | 79.74 ms | 65.17 s | 936.68 µs | 91.3% |

Keep USearch optional while M6 evaluates recall and graph rebuild cost on real embeddings. The exhaustive implementation provides a reference for those measurements.
