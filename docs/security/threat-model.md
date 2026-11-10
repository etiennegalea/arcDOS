# Threat model

## Default exposure

The Axum process serves the bundled status page, file workspace shell, and public health RPC by default. It binds to loopback by default for native development. The Docker image runs as a numeric non-root user, and the documented port mapping binds to loopback on the Docker host.

The browser uses the same origin as the API. Build outputs are public to clients that can reach the application. Store application state and secrets outside the static asset directory.

## Opt-in Linux file API

Set a private data directory and configure Authentik to enable file operations on Linux. The Axum router authorizes metadata, mutations, the change journal, uploads, and downloads. Development token mode is available for loopback testing with synthetic data and is mutually exclusive with Authentik. [Authentik setup](../deployment/authentik.md)

Authentik protects login with its authentication flow. ArcDOS verifies the OIDC callback with single-use state, PKCE, nonce, and a signed ID token. The browser authenticates with an opaque session cookie. Same-origin checks and a session-bound CSRF token protect cookie-authenticated writes. Live UserInfo must confirm current account and group access. Provider failure denies file access. Revocation takes effect on subsequent requests. A request already authorized can finish streaming.

Keep the session database and provider credentials private to the server. Authentik access tokens are sensitive even in backups. The secret file belongs outside both the web and data directories.

The file workspace keeps the development token in page memory. The media worker handles only preview URLs and requests ranges through the tab that owns the preview. Preview reads are bounded when using in-memory buffers. Text and code render as escaped text, and the PDF viewer receives a validated PDF blob.

The filesystem engine validates names, resolves paths relative to opened roots, and excludes symlinks, hardlinked regular files, special files, and nested mounts. The catalog and transfer directories are private to the server. All writable storage belongs on a single dedicated mount. Host processes with direct write access to that mount are trusted. In particular, a local writer can change a file during streaming or move a parent directory after the server has opened it.

## Before exposing file operations

| Trust boundary | Required controls |
| --- | --- |
| Browser or Android client to API | Verify identity and authorization at every data endpoint. Protect cookie-authenticated writes against CSRF. Cover revocation and role changes in tests. |
| API to shared storage | Use directory-relative handles and Linux confinement primitives. Test symlink replacement, traversal, concurrent writes, and external changes. |
| Client to upload and download endpoints | Bound resource use, check revision preconditions, verify transfer integrity, and commit uploads atomically. |
| API to workers and indexes | Treat extracted text and model output as untrusted. Keep inference isolated. Validate proposals and audit user-approved changes. |
| Backups and operations | Protect user data, metadata, identity records, and secrets. Test restoration against synthetic data. |
