# Performance measurements

Run the release build on Linux with the pinned Rust toolchain and Python 3.12 or later. The benchmark starts a loopback-only server with a development token and a disposable synthetic store. Keep the temporary directory on the Linux filesystem. Use the same hardware, workload, client count, and build profile when comparing revisions.

```sh
cargo build --locked --release --bin arcdos-server
python3 -B scripts/benchmark-linux.py --server target/release/arcdos-server --files 10000
python3 -B scripts/benchmark-linux.py --server target/release/arcdos-server --files 100000
```

Each run seeds one flat directory, a 32-level directory tree, and a media collection with 20 PNGs and 20 PDFs. It measures the first flat-directory request after server start, deep navigation, and the wait for keyword indexing. The startup indexer may run before that first request and the filesystem cache reflects recent fixture creation. After indexing, it samples two 100-entry pages and indexed filename searches with one client and with four concurrent clients. The report also includes loopback download throughput for a 32 MiB synthetic file with SHA-256 verification and server resident memory from `/proc`.

Pass `--clients`, `--samples`, or `--transfer-mib` to vary concurrency, sample count, or the streamed file size. Time the same workload twice to check run-to-run variation. On a Linux host with profiling enabled, `perf record -g -- python3 -B scripts/benchmark-linux.py ...` can capture client and server CPU stacks together. Select the `arcdos-server` process in the profile when investigating server costs.

For an inference comparison, start an idle local embedding worker with the pinned model and rerun the same command with `--ai-url http://127.0.0.1:8081 --ai-token-file /path/to/worker-token`. The report records embedded chunk counts before and after the warm workload so you can check whether indexing progressed. Compare list latency and throughput with keyword-only mode and interpret search timing as end-to-end latency, including any query embedding. [Worker setup](deployment/local-ai.md)

## Keyword job selection

A 100,000-row catalog profile found the original job query starting with the live-entry index and sorting the joined rows for every 24-job batch. An in-memory SQLite 3.50.4 comparison using the repository schemas measured 205.574 ms per selection across ten repetitions. Starting with the ordered ready-job index measured 0.160 ms with the same selected rows. The server uses SQLite's `CROSS JOIN` ordering rule for this bounded queue query. A regression test checks the plan on the bundled engine and verifies job order, future-job exclusion, and trash filtering. [SQLite join-order control](https://www.sqlite.org/optoverview.html#manual_control_of_query_plans_using_cross_join)

Indexing also commits up to 24 extracted documents together. Extraction happens outside the file-store lock, and the server verifies each source revision before writing the batch. A failed batch rolls back its index rows and job removals together. Changed or unreadable sources follow the existing retry path. A regression test exercises rollback and a file edit between extraction and commit.

A separate `strace -f -c -e trace=fdatasync,fsync` run of the 1,000-file smoke workload counted 1,175 filesystem sync calls before batching and 112 afterward. The timed workload runs exclude the tracing overhead.

On the WSL2 host, a 10,000-file run with the queue-order fix took 117.189 seconds waiting for the keyword index. The subsequent batched run took 9.280 seconds. These are local end-to-end observations, including the host's storage variability.

## Release server results

The batched release build used the same i7-2600K host, Ubuntu 24.04.5, WSL2 kernel 6.18.58.1, and Rust 1.99.1. Each warm row below contains 60 requests over prewarmed loopback connections. Authorization used the synthetic development token.

| Flat files | Clients | First-page p95 | Filename search p95 |
| ---: | ---: | ---: | ---: |
| 10,000 | 1 | 26.029 ms | 5.071 ms |
| 10,000 | 4 | 73.985 ms | 14.990 ms |
| 100,000 | 1 | 117.689 ms | 32.369 ms |
| 100,000 | 4 | 313.007 ms | 230.528 ms |

The 100,000-file workload finished indexing after a 167.134-second wait. Its server peak RSS was 16,344 KiB and the verified 32 MiB download measured 178.50 MiB/s. Directory paging still scans the directory on each request. Listing latency exceeds the 50 ms target on this host, and the four-client 100,000-file search exceeds 100 ms.

With the real MiniLM worker running on Python 3.12.3, ONNX Runtime 1.24.1, and two inference threads, the 10,000-short-document run recorded 47 embedded chunks before warm sampling and 55 afterward. Single-client list and hybrid-search p95 were 21.922 ms and 27.278 ms. At four clients they were 125.463 ms and 41.131 ms. The server peak RSS was 17,776 KiB. These samples include query embedding, worker contention, and the normal keyword fallback.

## Browser scroll

Build the web app, then run the benchmark server with `--serve` and the same file count. Once keyword indexing finishes, copy the printed fixture variables into the shell that runs Playwright. Use the installed Playwright Chromium or set `ARCDOS_E2E_BROWSER_CHANNEL=msedge` for a local Edge installation.

```sh
npm --prefix apps/web ci --no-audit --no-fund
npm --prefix apps/web run build
python3 -B scripts/benchmark-linux.py --server target/release/arcdos-server --files 10000 --serve
# In a second shell with the fixture variables set:
npm --prefix apps/web run bench:scroll
```

The scroll benchmark waits for the browser to reach the journal tail before timing the folder open. It loads ten real pages by default, then samples `requestAnimationFrame` callback intervals and peak mounted entry elements in the list and grid at 1440×900. Change `ARCDOS_BENCH_PAGES` to test more pages and `ARCDOS_BENCH_FRAME_MS` to change the sample duration. The report includes loaded entries, the declared source count, and paging timing. The existing Playwright regression also tests virtualization with all 100,000 entries loaded in an isolated component fixture.

A Windows headless Edge 157 run with pinned Node 24.22.2 used a fully indexed 100,000-file source directory and loaded ten pages, totaling 1,200 entries. Both layouts recorded 59.93 rAF callbacks/s and a 16.8 ms p95 interval. The list peaked at 22 mounted entries and the grid at 84. The browser reached the journal tail in 1.525 seconds. The first folder RPC took 77.48 ms and appended-page RPC p95 was 71.85 ms.

The large-library integration run exposed a journal catch-up delay. The browser had waited four seconds after every historical 500-change page, delaying live notifications by roughly 13 minutes for a 100,000-entry history. It now drains full pages sequentially and rechecks the active folder revision when reaching the tail. Normal tail and error polling retain their four-second interval. Regression tests cover exact-multiple histories, a concurrent folder change, and initial collection history.

## Vector retrieval

Run the two release comparisons using the same fixed query set. The output includes build time, first-query latency, warm p95, and USearch recall relative to the exact implementation. This benchmark excludes query embedding, SQLite reads, and revision validation. The file server's initial exact snapshot has a separate 32 MiB budget. [Vector index design](architecture/vector-index.md)

```sh
cargo run --locked --release -p arcdos-vector --features usearch --example compare -- 10000
cargo run --locked --release -p arcdos-vector --features usearch --example compare -- 100000
```

On an Intel Core i7-2600K under Ubuntu 24.04.5 and WSL2 with Rust 1.99.1, a paired exact-only release experiment isolated the order in which the index traverses its vectors. The benchmark ran 100 queries at 384 dimensions after ten warmups. It alternated the baseline and candidate binaries for three pairs at 100,000 vectors.

| Corpus | Hash-map scan warm p95 | Insertion-order scan warm p95 |
| ---: | ---: | ---: |
| 10,000, one pair | 8.54 ms | 4.11 ms |
| 100,000, three-pair median | 91.69 ms | 52.46 ms |

The candidate keeps insertion order for scanning and uses a separate key set for duplicate detection. All top-ten IDs and distance bits matched. Peak benchmark process RSS stayed between 311,764 and 312,020 KiB across the six 100,000-vector runs, including the generated input corpus. The timing experiment used wall-clock samples. A separate USearch run on the same host during compilation reported 2.48 ms warm p95 and 90.9% recall at 100,000 vectors.

CI validates pagination, filename results, downloaded checksums, vector recall, and the browser's 100,000-entry rendering bound. Timing numbers are collected for comparison on stable hardware.
