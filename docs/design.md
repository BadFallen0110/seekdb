# MiniVectorDB Implementation Design

## Runtime Shape

MiniVectorDB is one foreground Linux x86_64 process. It reads SQL from stdin,
writes responses to stdout, emits fatal diagnostics to stderr, and keeps no
network listener. The Docker command uses `--data-dir /data`; all persisted
and derived data remains below that directory.

## Modules

- `sql.rs` parses the supported SQL subset into a typed AST.
- `value.rs` and `vector.rs` validate `BIGINT`, Unicode `VARCHAR`, finite
  `float32` vectors, L2 distance, and bounded TopK.
- `table.rs` owns schema, primary-key rows, and a lightweight hash index for
  `VARCHAR` equality predicates.
- `db.rs` owns committed state, the single explicit transaction, DML
  preparation/application, query execution, and durability boundaries.
- `storage.rs` implements checksummed incremental WAL and state replay.
- `vector_store.rs` implements contiguous vector slots, 64 KiB page reads,
  LRU caches, and AVX2-friendly vector retrieval.
- `protocol.rs` owns stdin framing, output formats, readiness, errors, and
  lifecycle commands.

## Durable Files

`/data/state.log` is the source of truth. Each successful auto-commit
statement or explicit `COMMIT` appends one frame:

- `MVD1` magic
- payload length
- CRC32
- one or more ordered mutation records

The vector payload and UTF-8 string payload are part of the committed mutation.
`File::sync_data` completes before success is reported. Recovery ignores a
partial final frame and rejects checksum damage before a later frame. The WAL
is replayed one frame at a time, so recovery does not need to retain the whole
log in memory.

`/data/vectors.bin` is a derived page store. Records contain a `u32` dimension
and little-endian `float32` payload. A compact slot-to-file-offset vector maps
monotonic vector handles to records. The file is rebuilt from committed WAL
records before readiness. A crash during rebuild leaves the WAL untouched; an
unfinished temporary file is discarded on the next start.

## Transactions

The process permits at most one explicit transaction. `BEGIN` captures the
current table metadata and compact vector handles. Successful transaction DML
modifies the visible state and accumulates complete mutation payloads; reads
therefore see their own writes. `ROLLBACK` discards state and pending
mutations. `COMMIT` persists the accumulated mutations and synchronizes before
clearing the transaction. A forced process termination therefore leaves either
all committed changes or none.

Every DML statement validates all input values and target rows before changing
visible state. A failed statement cannot leave a partial row batch.

## Query Execution

Predicates are evaluated over the current transaction view. `VARCHAR`
equality predicates use a hash bucket containing only primary keys, followed by
an exact predicate check. Vector TopK filters first, uses AVX2 runtime dispatch
when available, maintains only K heap entries, and orders distance ties by
ascending primary key. `ann` currently uses the same exact path, preserving
Recall@10 while leaving the mode boundary explicit.

## Resource Behavior

Rows retain compact vector handles rather than vector payloads. The vector page
and parsed-vector caches share a configurable budget (`MINIVECTORDB_CACHE_BYTES`,
default 64 MiB). The benchmark working set fits in cache; larger vector sets
evict pages and continue through reads. Scalar rows and primary-key metadata
remain in memory, which is the primary remaining large-data limitation.

For tables whose vectors fit within half of the configured cache budget, the
query layer prepares a temporary contiguous vector matrix. This avoids per-row
hash/cache lookups during TopK scans; any table mutation invalidates the matrix
and the exact page-backed path remains available for larger tables. Dense scans
split into at most two scoped worker ranges and merge per-range TopK results
using the same distance/primary-key ordering.

## Recovery Sequence

1. Open/create the data directory.
2. Stream valid WAL frames in order.
3. Apply catalog and row mutations to compact in-memory metadata.
4. Append committed vector payloads into a temporary page file.
5. Atomically replace the previous derived vector file.
6. Open the final page file and establish slot offsets.
7. Emit readiness.

No ready marker is emitted before recovery completes.
