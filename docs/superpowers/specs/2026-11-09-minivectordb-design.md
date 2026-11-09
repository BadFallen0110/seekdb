# MiniVectorDB Design

Status: approved for implementation

## 1. Goal

Build a single-process, single-client vector database for the 2026 OceanBase
competition playoff. The submission must pass every hidden MySQL-style
functional case before performance or Agent Prompt results count. The initial
implementation therefore prioritizes correctness, crash safety, deterministic
results, and a clean protocol boundary. Performance is optimized only after the
functional invariants are covered by tests.

The official protocol test package was not present when this design was
written. Raw SQL input over stdin and the observable stdout format are therefore
behind protocol-specific modules. Awaiting official details must not affect the
SQL, storage, query, or transaction engine.

## 2. Recommended Architecture

Use Rust and the standard library to produce one static-ish Linux executable
with no runtime database dependency. The components are:

1. `protocol`: reads commands from stdin, writes deterministic results to
   stdout, flushes each response, and isolates readiness, shutdown, mode-switch,
   and error formatting.
2. `sql`: lexer, parser, and typed AST for the supported SQL subset.
3. `catalog`: table schema metadata and durable versions of it.
4. `storage`: checksummed write-ahead log, page files, recovery, and a bounded
   read cache.
5. `transaction`: committed state plus one optional in-memory transaction
   overlay.
6. `executor`: statement atomicity, DML, filtering, ordering, projection, and
   TopK evaluation.
7. `vector`: float32 validation, L2 distance, AVX2 fast path, and ordered heap
   selection.
8. `tests`: unit, integration, model-based, crash, resource, and benchmark
   harnesses.

Alternatives considered:

- C++20 could reduce constant factors further but has materially higher
  correctness and memory-safety cost.
- Python with NumPy would reach a functional implementation sooner but has
  weaker startup, memory, packaging, and resource characteristics.

Rust is the best balance because the competition heavily rewards end-to-end
time and memory while requiring crash-safe storage.

## 3. SQL Semantics

Supported statements:

- `CREATE TABLE name (...)` with `BIGINT`, `VARCHAR(n)`, `VECTOR(n)`, and one
  inline or table-level `BIGINT PRIMARY KEY`.
- `DROP TABLE name`.
- Multi-row `INSERT INTO ... VALUES (...)`.
- `SELECT` with `*` or ordinary-column projection, `L2_DISTANCE(column,
  '<vector>') AS alias`, one table, `WHERE` comparisons joined by `AND`,
  `ORDER BY`, and `LIMIT`.
- `UPDATE ... SET ... WHERE ...`.
- `DELETE FROM ... WHERE ...`.
- `BEGIN`, `COMMIT`, and `ROLLBACK`.
- `CREATE VECTOR INDEX ...`; accepted and persisted, with exact scan retained
  as the initial implementation behind both `exact` and `ann` modes.

Unsupported SQL returns an explicit error and never silently changes meaning.
The lexer handles single-quoted strings with doubled quotes and common backslash
escapes, backtick identifiers, line comments, and block comments.

Value rules:

- Primary keys are unique signed 64-bit integers and cannot be updated.
- `VARCHAR(n)` measures Unicode scalar values. Comparisons use Rust string
  ordering, which matches Unicode code-point order for valid UTF-8.
- Vectors have a table-fixed dimension and are stored as finite `float32`
  values. NaN, infinity, overflow, malformed arrays, and wrong dimensions are
  rejected before mutation.
- Distances are computed from stored `float32` values.
- TopK is computed over the post-filter set. Ties use ascending primary key.
- Ordinary `ORDER BY` ties also use ascending primary key.
- A query without `ORDER BY` produces a deterministic set ordering for local
  testing; result comparison remains set-based when the driver specifies that.

Modes:

- `exact` is the default and always computes true TopK.
- `ann` initially uses the same exact computation. This guarantees valid IDs,
  filtering, and Recall@K while preserving room for a later index.
- The protocol layer owns the official mechanism for selecting modes. Until it
  is published, tests may use a comment directive or `SET` command behind that
  adapter.

## 4. Storage Layout and Durability

All database files live below the configured data directory, normally `/data`.

### 4.1 Catalog

Each table has a durable schema record containing its identifier, name, ordered
columns, primary-key position, optional vector position and dimension, and index
metadata. Catalog changes are serialized, checksummed, written to a temporary
file, synchronized, and atomically renamed before success is reported.

### 4.2 Write-ahead Log

`wal.log` is append-only. Every committed transaction is one framed record:

- magic and format version
- complete frame length
- transaction and commit sequence numbers
- operation payload
- CRC32 checksum

The complete frame is written and synchronized before `COMMIT` or an
auto-committed DML statement reports success. Recovery scans from the durable
checkpoint sequence:

- a complete, checksummed record is replayed;
- a truncated final frame is ignored;
- an invalid checksum at the tail is treated as an interrupted final frame;
- corruption followed by valid-looking data is a fatal recovery error rather
  than a silent repair.

Each transaction payload contains all row mutations required by that
transaction. Therefore an interrupted transaction is all-or-nothing during
replay.

### 4.3 Page Store and Cache

Committed scalar rows and vectors are stored in append-oriented 64 KiB page
files per table. Row metadata maps primary key to the latest committed scalar
location and vector slot. Page writes may lag the WAL; recovery reconstructs
missing or interrupted page content from committed WAL frames.

A bounded LRU page cache serves scalar rows and vectors. The benchmark-sized
working set fits in the cache, but larger-than-memory cases continue by reading
pages on demand. Cache capacity is configurable and defaults cautiously below
the likely container limit. Full row payloads, vectors, and strings are not
loaded into one allocation.

Checkpointing persists completed page files and records the highest safely
replayed commit. It never advances the checkpoint before relevant pages and the
checkpoint marker have been synchronized. Stale WAL segments may be compacted
or removed only after the new checkpoint is durable.

### 4.4 Recovery Guarantees

- A transaction whose success response was emitted survives restart.
- A transaction with no commit request is rolled back.
- A transaction whose commit request was received but whose response was not
  emitted may be entirely present or entirely absent.
- No transaction is ever partially applied.
- A normal shutdown rolls back an open transaction and checkpoints committed
  state.
- Recovery completes before the protocol emits its ready signal.
- Recovery is repeatable if the process is killed and restarted again.

## 5. Transaction Execution

Committed state is immutable with respect to an open transaction. `BEGIN`
creates an overlay containing inserted rows, updated rows, and deleted keys.
Reads and TopK merge the overlay over committed state so the transaction sees
its own writes.

For each DML statement:

1. Resolve the target set against the current transaction view.
2. Validate every new value and uniqueness constraint.
3. Build the complete mutation list without mutating state.
4. Either apply it to the overlay or serialize it as one auto-commit frame.

Auto-commit serializes one statement as a single durable transaction. In an
explicit transaction, each successful statement remains in the overlay until a
single `COMMIT` frame is synchronized. A failed statement changes nothing but
does not discard earlier successful statements. `ROLLBACK` drops the overlay.

DDL is auto-committed and is rejected inside an explicit transaction in the
initial implementation. This explicit boundary avoids undocumented MySQL
implicit-commit behavior.

## 6. Vector Query and Performance

Exact queries scan the visible, filtered row set. The stored vector layout is
column-oriented and addressable by slot so scalar predicates never force
readers to fetch unrelated vectors.

The L2 kernel uses an AVX2 implementation selected at runtime on x86_64 and a
portable scalar fallback. TopK uses a bounded max-heap of `(distance, primary
key)` and never sorts the complete candidate set.

Planned optimizations, applied only after correctness tests pass:

- fixed-dimension contiguous vector slots
- runtime AVX2 dispatch over several independent accumulators
- prepared filter constants and cheap early rejection
- page cache sized for the official 10,000 x 128 workload
- direct buffered stdout and minimal formatting allocations

An approximate index is deferred. Until it is implemented and tested, `ann`
must not return unverified approximate results merely to claim speed.

## 7. Protocol Boundary

The first adapter consumes raw SQL from stdin, handles statement boundaries in
the lexer, and emits one flushed response per statement. Diagnostics go only to
stderr. Ready and shutdown markers, error-code text, result columns, null
formatting, and mode-switch commands are isolated constants or functions in
this module.

When the official driver appears, protocol fixtures are added before changing
the adapter. No engine module may depend on stdout formatting.

## 8. Error and Output Rules

Errors have stable internal categories: syntax, unsupported feature, table
lookup, column lookup, type and range validation, duplicate key, transaction
state, storage I/O, and corruption.

The protocol adapter maps categories to the official numeric/error strings once
published. Before then, local output uses a deterministic textual format and
tests assert the internal category rather than hard-coding provisional protocol
codes.

Successful DML returns an affected-row count. DDL returns success. Selects emit
typed columns in projection order and deterministic formatting for integers,
strings, and float32-derived distances.

## 9. Testing Strategy

Implementation follows test-driven development.

Unit tests:

- lexer and parser acceptance/rejection
- BIGINT and Unicode VARCHAR validation
- finite float32 and dimension validation
- L2 ordering, ties, distance output, and TopK filtering
- table and row binary codecs
- WAL partial-frame and checksum handling

Integration tests:

- end-to-end SQL scripts through subprocess stdin/stdout
- multi-row insert, projection, all comparisons, AND, ordering, and limits
- update/delete by key and predicate
- exact and ANN-mode Recall@K checks
- begin/commit/rollback with reads of own writes
- normal restart and forceful termination at selected durability points
- repeated recovery after a second forced termination
- cache eviction and data larger than the configured cache

Model-based tests:

- random SQL operation chains compared with a simple in-memory oracle
- restart checkpoints compared byte-independent with the oracle state

Benchmark:

- seed-defined 10,000 rows x 128 dimensions
- 1,000 TopK queries, K=10, half filtered
- exact brute-force truth for Recall@10
- report wall time, bytes read/written where available, and peak resident
  memory for local regression tracking

The official performance baseline and cgroup measurements remain authoritative;
local numbers are regression signals only.

## 10. Agent Prompt Material

`agent/conversation.json` is maintained under 128 KiB UTF-8. It contains only
real user prompts, Agent responses or summaries, actual commands, and actual
results. Plans are labeled as plans. The material records independent user
decisions, predictions, counterexamples, result interpretations, corrections,
retests, and limitations so that any scored claim can be located in the JSON.

No test result, user statement, or understanding is fabricated to increase the
score. Synthetic harness data must be marked as synthetic. Sensitive tokens and
credentials are excluded.

## 11. Deliverables

- Rust source and locked build metadata
- multi-stage `Dockerfile` that builds from source and runs a minimal Linux
  x86_64 image
- `README.md` covering SQL scope, startup, data directory, examples, errors, and
  known limitations
- local protocol and crash-test utilities
- design and verification records
- valid `agent/conversation.json`
- final full commit SHA and repository instructions after the user supplies the
  registered fork

## 12. Acceptance Criteria

The first milestone is complete when:

- the binary builds and starts with an empty data directory
- all supported statements execute through stdin and flush to stdout
- the sample SQL produces the specified filtering and ordering
- transaction commit, rollback, and read-own-writes tests pass
- forced-kill recovery tests show no partial transaction
- a second restart reproduces committed state
- exact and ANN-mode benchmark queries satisfy Recall@10 >= 0.95
- page-cache eviction works with a data set larger than the configured cache

Performance tuning starts only after these criteria pass. Official protocol
compatibility and official hidden-case passage remain required before claiming
competition readiness.
