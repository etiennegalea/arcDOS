# Keyword search and indexing

Use the generated `arcdos.search.v1.SearchService` client. Search and tag requests use the same server authorization and browser CSRF checks as the file API. [Authentication conventions](conventions.md#authentication)

- `SearchFiles` accepts up to 256 characters and 12 terms. Search matches Unicode-normalized terms across filenames, root-relative paths, tags, file type, extension, and extracted text. Terms with two or more characters match prefixes. Punctuation separates terms, and every term must match. The default result limit is 40 and the maximum is 100. `has_more` means additional current matches were found.
- Each hit contains the normal file `Entry` with its opaque ID and revision, its display path, and its current tags. Search covers live files and folders. Trashed entries and obsolete revisions are omitted.
- `SetTags` replaces all tags for a live entry. Pass the current file revision to guard against editing a changed file, and send an empty list to clear the tags. ArcDOS trims, sorts, and deduplicates up to 24 labels of 48 characters each. When two requests replace tags on the same file revision, the last successful replacement wins.

Search indexes metadata for all supported entries. It also indexes validated UTF-8 text and source files of up to 256 KiB and stores a SHA-256 hash of the extracted bytes. Larger text files and binary formats remain searchable by metadata. Document extraction, OCR, and semantic search are later capabilities.

## Reconciliation and recovery

The catalog and index share `/data/state/metadata.db` in SQLite WAL mode. Catalog writes and their search jobs commit together. On startup, the worker reconciles the tree and resumes persisted jobs. Linux notifications accelerate updates. A bounded notification queue requests a full scan on overflow, and the worker also reconciles every two minutes. An out-of-band rename can appear as removal and discovery with a new opaque ID. Use ArcDOS moves to preserve the existing ID and its tags.

File revisions are verified before an index write and again before search returns a hit. After a large external change, ranked results may temporarily omit matches until reconciliation catches up. `ListChanges` reports observed additions, modifications, and removals alongside ArcDOS file operations.

To rebuild the keyword index, stop the server using the data directory, run `arcdos-server rebuild-search` with the same `ARCDOS_DATA_DIR`, then restart normally. The command acquires the normal storage lock, clears derived search rows, and schedules jobs without clearing file IDs, tags, or the journal. For a containerized Linux host with the image built as `arcdos:local`, run the following with the server stopped.

```sh
docker run --rm --read-only --cap-drop=ALL --security-opt=no-new-privileges \
  --mount type=bind,source=/srv/arcdos/data,target=/data \
  -e ARCDOS_DATA_DIR=/data arcdos:local rebuild-search
```

Treat the SQLite database and its backups as private file data because they contain filenames, tags, and extracted text. [Vector index evaluation](../architecture/vector-index.md)
