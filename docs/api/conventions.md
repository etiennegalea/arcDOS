# API conventions

## Transport and versioning

Use unary Connect RPC with binary Protobuf for ordinary structured calls. Define RPC and message types in `proto/` and generate client and server bindings. The first route is `POST /arcdos.account.v1.HealthService/Check`. It accepts an empty `CheckRequest` and returns `CheckResponse` with `ready` set when the process can answer the RPC.

Use `arcdos.<domain>.v1` packages. The planned domains are `files`, `search`, `ai`, `sync`, and `account`. Add fields compatibly, reserve removed names and numbers, and introduce a new package version for incompatible contracts. Use Buf's `FILE` compatibility checks to protect generated consumers as well as the wire format.

Use dedicated HTTP endpoints for bulk uploads and downloads. Use documented range and resume semantics. Reserve server streams for structured events or incremental answers and define cancellation, resumption, and the ordering scope per RPC.

## Resource conventions

- Use server-issued opaque identifiers that remain stable across supported renames and moves. Treat file names as presentation data. Keep absolute paths internal.
- Accept a bounded `page_size` and opaque `page_token` on list requests. Return `next_page_token` only when more results are available. Bind a cursor to its parent, filters, and stable sort order.
- Use `google.protobuf.Timestamp` for times and `google.protobuf.Duration` for durations.
- Use an opaque revision precondition on writes that can conflict. Return `aborted` for a stale revision and require clients to reconcile before retrying.
- Use a monotonically increasing `uint64` change sequence within the shared filesystem journal. Use opaque resume cursors when replay may require additional state. Clients must preserve 64-bit values without rounding.
- Use standard Connect error codes. Return `invalid_argument` for malformed input, `unauthenticated` for missing or invalid identity, and `permission_denied` for authenticated identities without access. Keep error messages free of internal paths and secrets.
