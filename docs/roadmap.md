# Roadmap

## M1 · Foundation and Protobuf

- Establish the Cargo workspace, SvelteKit app, and Buf module with pinned toolchains.
- Generate Rust, TypeScript, and Kotlin bindings for a unary Connect health check.
- Serve the prerendered site and binary RPC from Axum. Add integration tests and build a non-root container.

Exit when a clean build passes Rust and frontend checks, Buf regenerates all bindings, and a browser displays readiness from the packaged server.

## M2 · Filesystem engine

- Open the configured Linux storage root as a directory handle. Implement confined list, stat, create, rename, move, copy, trash, restore, and change history.
- Implement bounded HTTP streaming, byte ranges, and persistent resumable uploads with checksums, cancellation, retries, and atomic commits.
- Define revision preconditions and recovery behavior for concurrent updates, external edits, and cross-filesystem operations.

Exit when the essential operations pass backend-only tests for symlink races, Unicode, large directories, interrupted transfers, storage exhaustion, and concurrent changes.

## M3 · Svelte file manager

- Build accessible responsive browsing, breadcrumbs and history, sorting, filtering, multi-selection, keyboard actions, and drag-and-drop.
- Add upload progress, file previews, recent files, favorites, and trash management.
- Virtualize the list and use cursor pagination, cancellable requests, cached thumbnails, and subscriptions.

UI should be similar to the FilDOS implementation.
Exit when essential file operations work in desktop and mobile layouts and a 100,000-entry directory renders only the visible window and overscan.

## M4 · Authentik

- Implement OIDC authorization code and PKCE, backend-managed web sessions, secure cookies, CSRF protection, and logout and revocation.
- Authorize the shared filesystem by `drive-users` and `drive-admins` groups. Document Authentik MFA, TOTP, WebAuthn, and the separate Android public client.
- Protect files, transfers, previews, subscriptions, and all other data endpoints.

Exit when tests deny unauthenticated requests, revoked sessions, invalid tokens, missing groups, and CSRF attempts, including for direct file downloads.

## M5 · Metadata and search

- Add SQLite WAL migrations, FTS5, revision and event records, and persistent indexing jobs.
- Combine filesystem watching with periodic reconciliation and retry interrupted jobs.
- Benchmark USearch and keep vector metadata sufficient for index reconstruction.

Exit when keyword search works independently of AI and indexes recover after crashes and changes made outside ArcDOS.

## M6 · Local AI

- Review FilDOS implementation and licensing for extraction, retrieval, and inference components.
- Deploy an independently restartable, resource-limited worker using ONNX Runtime and llama.cpp. Start with document extraction, embeddings, hybrid search, and streaming chat with citations.
- Validate and audit proposed file changes after explicit user confirmation. Extend indexing to OCR and photos, with a separate face subsystem. Add file relationship visualization.

The first increment covers the FilDOS review, local ONNX embeddings for indexed UTF-8 documents, and hybrid search with keyword fallback. [FilDOS review](architecture/fildos-review.md) · [Worker setup](deployment/local-ai.md)

Exit when local document search and chat work without paid APIs and file operations remain available during inference and worker outages.

## M7 · Docker and Proxmox

- Package the server and worker with non-root users, restricted networks and mounts, limits, health checks, and controlled shutdown.
- Document Authentik and Traefik setup, HTTPS, secret management, storage placement, backups, and restore.

Exit when a clean VM can follow the deployment guide, serve authorized users, and restore a tested backup.

## M8 · Android and synchronization

- Build the Kotlin and Compose app with browser-based Authentik login, Keystore, biometric unlock, file operations, and previews.
- Use Room and WorkManager for offline state, persistent transfer retries, and automatic camera backup through MediaStore.
- Add sync cursors, revisions, checksums, tombstones, and preserved conflict copies before enabling bidirectional folders.

The first increment adds the native client, one resumable upload at a time, and a durable read-only change journal. [Android setup](android.md)

Exit when an Android device can browse, transfer, and back up photos across interrupted connections and conflicting changes are recoverable.

## M9 · Performance

- Build repeatable 10,000- and 100,000-file workloads, deep trees, media and PDF collections, and concurrent clients.
- Measure cold and warm latency, memory, throughput, browser frame rate, and impact of background inference.
- Target cached 100-entry lists below 50 ms p95, indexed filename search below 100 ms p95, 100,000-vector retrieval below 200 ms p95 excluding embedding time, and 60 FPS desktop scrolling.

[Reproduce the Linux, browser, and vector measurements](performance.md).

Exit when results are reproducible, regressions are checked in CI, and each optimization is supported by profiling and before-and-after measurements.
