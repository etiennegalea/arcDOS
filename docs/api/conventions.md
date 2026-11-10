# API conventions

## Transport and versioning

Use unary Connect RPC with binary Protobuf for ordinary structured calls. Define RPC and message types in `proto/` and generate client and server bindings. The first route is `POST /arcdos.account.v1.HealthService/Check`. It accepts an empty `CheckRequest` and returns `CheckResponse` with `ready` set when the process can answer the RPC.

Use `arcdos.<domain>.v1` packages. The planned domains are `files`, `search`, `ai`, `sync`, and `account`. Add fields compatibly, reserve removed names and numbers, and introduce a new package version for incompatible contracts. Use Buf's `FILE` compatibility checks to protect generated consumers as well as the wire format.

Use dedicated HTTP endpoints for bulk uploads and downloads. Use documented range and resume semantics. Reserve server streams for structured events or incremental answers and define cancellation, resumption, and the ordering scope per RPC.

## Authentication

The health RPC and static pages are public. File RPCs and HTTP file endpoints require authorization, including downloads, media ranges, previews, and the change journal.

- In Authentik mode, a browser starts sign-in at `GET /auth/login`. The OIDC callback establishes a server-side session with an HttpOnly cookie. Read `GET /auth/session` for the current identity and CSRF token. Browser requests using unsafe HTTP methods require the exact same-origin `Origin` and the `X-CSRF-Token` header. This includes all Connect `POST` calls and `POST /auth/logout`.
- When Android bearer support is configured, the native client sends `Authorization: Bearer <access-token>`. ArcDOS checks the token against the configured public client and checks the user's current group membership in Authentik.
- For loopback development with a development token, send `Authorization: Bearer <token>` with each protected request.

[Authentik configuration and session revocation](../deployment/authentik.md)

## Resource conventions

- Use server-issued opaque identifiers that remain stable across supported renames and moves. Treat file names as presentation data. Keep absolute paths internal.
- Accept a bounded `page_size` and opaque `page_token` on list requests. Return `next_page_token` only when more results are available. Bind a cursor to its parent, filters, and stable sort order.
- Use `google.protobuf.Timestamp` for times and `google.protobuf.Duration` for durations.
- Use an opaque revision precondition on writes that can conflict. Return `aborted` for a stale revision and require clients to reconcile before retrying.
- Use a monotonically increasing `uint64` change sequence within the shared filesystem journal. Use opaque resume cursors when replay may require additional state. Clients must preserve 64-bit values without rounding.
- Use standard Connect error codes. Return `invalid_argument` for malformed input, `unauthenticated` for missing or invalid identity, and `permission_denied` for authenticated identities without access. Keep error messages free of internal paths and secrets.
