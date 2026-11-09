# Architecture decisions

## ADR 001 · Serve SvelteKit and Connect from Axum

**Status** Accepted

SvelteKit prerenders the status route with its static adapter. Axum serves the result alongside the Connect route. Production builds require one application process, and the browser uses same-origin requests. A Vite proxy forwards Connect calls to Axum during UI development.

This keeps the frontend independent of server-side JavaScript at runtime and sets up the same-origin Authentik session design. Use dedicated HTTP endpoints for streaming large files as that feature arrives.

## ADR 002 · Share a versioned Protobuf contract

**Status** Accepted

Buf validates the public schema and detects breaking changes. Rust uses the Connect implementation and its matching Buffa toolchain. The web uses Connect-ES with Protobuf-ES descriptors. Kotlin bindings come from the same schema for future Android use. [Connect Rust release](https://github.com/connectrpc/connect-rust/releases/tag/v0.9.1) · [Buffa release](https://github.com/anthropics/buffa/releases/tag/v0.9.2) · [Connect-ES documentation](https://connectrpc.com/docs/web/getting-started/) · [Connect Kotlin release](https://github.com/connectrpc/connect-kotlin/releases/tag/v0.10.0)

Generate bindings from source with pinned tools. Check in the Rust and TypeScript output so routine builds can use their package lockfiles without contacting code-generation services. CI regenerates those bindings and verifies that they match. Generate Android output on demand until the Android app exists. Keep wire conventions in [the API guide](../api/conventions.md) and add a namespace when it has a real contract.

## ADR 003 · Keep file data and AI inference separate

**Status** Accepted for subsequent milestones

Use a Linux filesystem as the source of truth and keep searchable metadata and change history in SQLite. The API will perform bulk transfers directly using bounded buffers. Local inference will run in a separate service with resource limits and a narrow job interface. File management and keyword search must continue through model reloads and AI worker outages.

## ADR 004 · Pin the compatible frontend build toolchain

**Status** Accepted

Vite 8.3.7 pulls in Rolldown 1.2.18. The Linux x86-64 binding for that release exits with an illegal instruction on the Sandy Bridge development host. The same host loads Rolldown 1.2.16 successfully. Pin Vite 8.3.6 with an explicit Rolldown 1.2.16 override so lockfile updates preserve the working pair. [Vite 8.3.7 release](https://github.com/vitejs/vite/releases/tag/v8.3.7)
