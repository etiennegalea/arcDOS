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

## ADR 005 · Confine files and journal atomic renames

**Status** Accepted

Use Linux directory capabilities and `openat2` with beneath, no-symlink, no-magic-link, and no-mount-crossing resolution. Open regular singly linked files before streaming them. Use a dedicated Linux data mount containing both `files/` and private `state/` so staged data, trash, and live files can move atomically with `renameat2(RENAME_NOREPLACE)`. Cross-mount renames fail without copying or deleting the source. [Linux openat2](https://man7.org/linux/man-pages/man2/openat2.2.html)

Write a SQLite intent before each rename and synchronize the affected directories before committing the catalog and journal update. On startup, compare the saved identity with the source and destination to finalize a completed rename or clear an unexecuted intent. If neither location matches, fail closed so an operator can inspect the storage.

Serialize catalog mutations in one server process. Use file identity and metadata for revisions and detect changes observed on the next read. A local process with write access to the same mount can still change open file contents or move directory handles. Use a dedicated writable mount and restrict direct access during mutation and streaming.

## ADR 006 · Keep the file workspace static and page directory data

**Status** Accepted

Serve a prerendered `/files/` shell. Store folder identity and the active collection in the URL so browser history and direct links work without a dynamic server route. Load data through the existing Connect clients and use a virtual window for list and grid rendering. The server remains the source of truth for mutation revisions and cursor order.

Keep the development token in page memory. A scoped service worker relays audio and video range requests through the tab that owns the preview. The tab performs the authenticated fetch and transfers its response stream. The worker serves no other routes and stores neither credentials nor file bytes.

## ADR 007 · Keep Authentik tokens in the ArcDOS backend

**Status** Accepted

Use the confidential OAuth2/OIDC client in Axum for authorization code flow with S256 PKCE, single-use state, nonce, and RS256 ID token verification. Keep the provider access token in a private local SQLite session database and issue a Secure, HttpOnly, SameSite=Lax cookie containing an opaque session ID. Require exact same-origin and session CSRF verification on browser writes. The server enforces access on the entire file router, including direct streaming and change requests.

Request Authentik reauthentication on web sign-in so the provider runs its configured MFA flow. Read live Authentik UserInfo for current group membership and account status on every protected request. This couples file access to Authentik availability and gives token revocation and membership changes effect on the next request. Limit browser sessions to the provider token lifetime, eight hours total, and 30 minutes idle. Logout revokes the local session and requests provider token revocation. Use a distinct, optional public client for future Android access, with confidential token introspection and the same live group check. [Authentik setup](../deployment/authentik.md)
