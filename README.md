# MiniVectorDB

MiniVectorDB is a single-process, single-client vector database for the 2026
OceanBase competition playoff. The database speaks the official JSON Lines
protocol over standard input/output and persists committed transactions below
`/data`.

## Build and run

```bash
cargo build --release
printf '%s\n' \
  '{"id":1,"op":"execute","sql":"CREATE TABLE docs (id BIGINT PRIMARY KEY, body VARCHAR(32), embedding VECTOR(2));","search_mode":"exact"}' \
  '{"id":2,"op":"execute","sql":"INSERT INTO docs VALUES (1, '\''first'\'', '\''[1,0]'\'');","search_mode":"exact"}' \
  '{"id":3,"op":"execute","sql":"SELECT id, L2_DISTANCE(embedding, '\''[1,0]'\'') AS d FROM docs ORDER BY d LIMIT 1;","search_mode":"exact"}' \
  '{"id":4,"op":"shutdown"}' |
  target/release/minivectordb --data-dir ./data
```

Docker:

```bash
docker build -t minivectordb:local .
docker run --rm -i \
  -v minivectordb-data:/data \
  --cpus=2 \
  --memory=2g \
  minivectordb:local
```

The process does not listen on a network port. Use `-i` rather than `-d` or
`-t`.

## Startup options

| Option | Default | Meaning |
| --- | --- | --- |
| `--data-dir PATH` | `/data` | Durable data directory |
| `--protocol jsonl\|text` | `jsonl` | Official JSONL or legacy text adapter |
| `--mode exact\|ann` | `exact` | Vector query mode |
| `--output jsonl\|framed\|tsv\|mysql` | `jsonl` | Result framing format |
| `--ready TEXT` | `READY` | Legacy text-mode startup marker |
| `--cache-bytes N` | 64 MiB | Vector page/value cache budget |
| `--exact` / `--ann` | `--exact` | Shorthand for query mode |
| `--no-ready` | off | Suppress the legacy startup marker |

Equivalent environment variables are `MINIVECTORDB_DATA_DIR`,
`MINIVECTORDB_PROTOCOL`, `MINIVECTORDB_READY`,
`MINIVECTORDB_QUERY_MODE`, and `MINIVECTORDB_OUTPUT`. JSONL requests set mode
per request with `search_mode`. The legacy text adapter also accepts SQL comment
hints containing `mode=ann` or `mode=exact`. In this implementation, `ann`
deliberately uses exact scanning so its Recall@10 remains 1.0.

`MINIVECTORDB_CACHE_BYTES` controls the vector page/value cache budget. Its
default is 64 MiB. Vector payloads use 64 KiB pages and a bounded LRU cache;
rows retain only compact vector handles.

## SQL scope

Supported:

- `CREATE TABLE` and `DROP TABLE`
- `CREATE TABLE IF NOT EXISTS` and `DROP TABLE IF EXISTS`
- `BIGINT`, `VARCHAR(n)`, and `VECTOR(n)`
- one `BIGINT PRIMARY KEY` per table and at most one vector column
- multi-row `INSERT`
- constant queries such as `SELECT 1 AS value`
- `SELECT *`, ordinary projections, `L2_DISTANCE`, `WHERE`, `AND`,
  `ORDER BY`, and `LIMIT`
- ordering directly by `L2_DISTANCE(...)` even when the distance is not
  projected
- `UPDATE` and `DELETE` by predicate
- `BEGIN`, `COMMIT`, and `ROLLBACK`
- `CREATE VECTOR INDEX name ON table(vector_column)`
- `TRUNCATE TABLE` and `SHOW TABLES`
- common mysql-test setup such as `SET NAMES`, `SET SESSION sql_mode`,
  `SET autocommit=1`, `USE`, `NOT NULL` markers, and InnoDB/charset table
  options

Unsupported SQL is rejected explicitly. `NULL`, JOINs, aggregation, subqueries,
`ALTER TABLE`, foreign keys, nested transactions, and primary-key updates are
outside the supported subset.

Example:

```sql
CREATE TABLE docs (
    id BIGINT PRIMARY KEY,
    category VARCHAR(32),
    content VARCHAR(1024),
    embedding VECTOR(4)
);

INSERT INTO docs VALUES
    (1, 'database', '事务与恢复', '[1,0,0,0]'),
    (2, 'database', '向量索引', '[0,1,0,0]'),
    (3, 'travel', '旅行攻略', '[0,0,1,0]');

SELECT id, content,
       L2_DISTANCE(embedding, '[0.9,0.1,0,0]') AS distance
FROM docs
WHERE category = 'database'
ORDER BY distance
LIMIT 2;

BEGIN;
UPDATE docs SET embedding = '[0.8,0.2,0,0]' WHERE id = 1;
DELETE FROM docs WHERE id = 2;
ROLLBACK;
```

## Persistence and recovery

Durable state is stored in `/data/state.log`. `/data/vectors.bin` is a
rebuildable page store derived from committed WAL records.

- Each auto-commit statement or explicit `COMMIT` appends one checksummed
  mutation frame.
- The frame is synchronized before the command reports success.
- An incomplete final frame is ignored on recovery.
- Explicit transactions remain in memory until `COMMIT`; a forced process
  termination therefore discards them.
- Recovery completes before the JSONL `{"type":"ready"}` line is emitted.
- The vector file is rebuilt from committed WAL before readiness, so an
  interrupted vector-file update cannot expose partial data.
- JSONL `{"op":"shutdown"}` rolls back an open transaction, flushes committed
  state, sends one success response, and exits with status 0. The legacy text
  adapter also accepts `QUIT`, `EXIT`, `SHUTDOWN`, and `\q`.

## Protocol

The default protocol is one JSON object per physical line. Startup emits exactly:

```json
{"type":"ready"}
```

Requests preserve the original JSON `id` type. Queries return ordered `columns`
and a two-dimensional `rows` array. `INSERT`, `UPDATE`, and `DELETE` return
`affected_rows`, including zero. SQL failures return `ok:false` with a stable
semantic error code and do not stop the process. BIGINT values remain JSON
integers, so values outside JavaScript's safe-integer range retain full `i64`
precision.

Every response is flushed immediately, diagnostics go to stderr, and shutdown
emits no output after its response. The complete contract is in
[`STDIO_PROTOCOL.md`](STDIO_PROTOCOL.md). Legacy human-readable adapters remain
available with `--output framed`, `--output tsv`, or `--output mysql`.

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release --test benchmark -- --ignored --nocapture
```

`tests/public_protocol.rs` runs all 20 public cases and 95 SQL statements
through the JSONL process protocol, including restart and mode changes. The
repository also contains the official `protocol_check/` and `mysqltest/`
packages for Linux/Docker verification.

The benchmark uses 10,000 rows, 128 dimensions, 1,000 TopK queries, K=10, and
an independent brute-force oracle. It currently reports Recall@10 = 1.0.
See `docs/design.md` for durable layout, transaction, recovery, and resource
tradeoffs.

## Known limitations

- The official hidden cases and performance baseline are not published. Public
  protocol v4 conformance does not prove every hidden case will pass.
- Vector payloads use a bounded cache, but scalar values and primary-key
  metadata are still held in memory. Extremely large scalar-row sets can still
  exceed a tight memory budget.
- `ann` currently returns exact results. A real approximate index is deferred
  until admission correctness is stable.
