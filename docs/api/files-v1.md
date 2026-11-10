# File API

The Linux file API requires `ARCDOS_DATA_DIR` and an access configuration. Authentication covers file metadata, changes, transfers, and direct download or preview requests, including ranges and `HEAD`. [Authentication conventions](conventions.md#authentication) · [Authentik setup](../deployment/authentik.md)

Use the generated clients for `arcdos.files.v1.FileService` and `arcdos.files.v1.TransferService`. Both services accept unary binary Protobuf or ProtoJSON. With ProtoJSON, send `Content-Type: application/json` and `Connect-Protocol-Version: 1`.

## Entries and changes

- Call `GetEntry` with an empty request to get the root ID and revision. Use `ListEntries` with an empty parent ID to list the root.
- Use the returned IDs to fetch metadata or address a mutation. Use the returned revision as the mutation precondition. To rename an entry, use `MoveEntry` with its current parent ID and a new name.
- Use `next_page_token` to continue the same listing. The default page size is 100 and the maximum is 500. Restart the listing if a directory change invalidates its token.
- `CopyEntry` copies a regular file. Directory moves and trash operations preserve descendant IDs. Use `ListTrash` to discover trashed entries and `RestoreEntry` to return them to their original parent and name.
- A stale revision returns Connect `aborted`. An occupied destination returns `already_exists` and leaves both objects intact. Refresh metadata before retrying.
- Persist the `next_sequence` from `ListChanges` and send it as `after_sequence` on the next call. This journal records ArcDOS mutations and observed external changes. Listings and metadata reads consult the live filesystem. [Keyword search and indexing](search-v1.md)

## Download

`GET /api/files/v1/{entry_id}/content` streams the open file. `HEAD` returns size and the revision ETag. Requests can select a single byte range using `Range: bytes=START-END`, `bytes=START-`, or `bytes=-LENGTH`. A satisfiable range returns `206` and `Content-Range`. An unsatisfiable range returns `416` and `Content-Range: bytes */SIZE`. A matching strong `If-Range` ETag lets a client resume the same revision. Other `If-Range` values receive the full representation.

## Upload

1. Call `BeginUpload` with the destination parent, name, parent revision, total byte length, and raw SHA-256 digest. In ProtoJSON, encode the digest as base64 and send the length as a decimal string.
2. Send each chunk as the body of `PATCH /api/files/v1/uploads/{upload_id}`. Include `Upload-Offset` and `Content-Length`. A successful chunk returns `204` and the new `Upload-Offset`. Each chunk can carry up to 16 MiB.
3. Call `GetUpload` after interruption to recover the committed offset. If a chunk uses a stale offset, `409` includes the authoritative `Upload-Offset` header. Resume from that offset.
4. Call `CommitUpload` to verify the whole-file SHA-256 and publish the staged inode. If the parent revision changed, check the destination and retry with the current revision in `expected_parent_revision`. An occupied name remains a conflict. Retrying a successful commit returns the published entry without writing a duplicate.

`CancelUpload` discards unpublished staging data. Uploads create new entries and preserve existing files.
