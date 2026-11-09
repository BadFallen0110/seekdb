# MiniVectorDB

MiniVectorDB is a single-process, single-client vector database for the 2026
OceanBase competition playoff. The database reads SQL from standard input,
writes deterministic framed results to standard output, and persists committed
transactions below `/data`.

## Build and run

```bash
cargo build --release
printf 'CREATE TABLE docs (id BIGINT PRIMARY KEY, body VARCHAR(32), embedding VECTOR(2));\nINSERT INTO docs VALUES (1, '\''first'\'', '\''[1,0]'\'');\nSELECT id, L2_DISTANCE(embedding, '\''[1,0]'\'') AS d FROM docs ORDER BY d LIMIT 1;\nQUIT;\n' | target/release/minivectordb --data-dir ./data
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
| `--ready TEXT` | `READY` | Startup marker written to stdout |
| `--mode exact\|ann` | `exact` | Vector query mode |
| `--output framed\|tsv\|mysql` | `framed` | Result framing format |
| `--cache-bytes N` | 64 MiB | Vector page/value cache budget |

Equivalent environment variables are `MINIVECTORDB_DATA_DIR`,
`MINIVECTORDB_READY`, and `MINIVECTORDB_QUERY_MODE`. A SQL comment containing
`mode=ann` or `mode=exact` overrides the query mode for subsequent statements.
In the first implementation, `ann` deliberately uses exact scanning so its
Recall@10 remains 1.0.

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
- `UPDATE` and `DELETE` by predicate
- `BEGIN`, `COMMIT`, and `ROLLBACK`
- `CREATE VECTOR INDEX name ON table(vector_column)`

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
- Recovery completes before `READY` is emitted.
- The vector file is rebuilt from committed WAL before readiness, so an
  interrupted vector-file update cannot expose partial data.
- `QUIT`, `EXIT`, `SHUTDOWN`, and `\q` roll back an open transaction, flush
  committed state, and exit.

## Protocol

The default framed adapter emits:

```text
READY
BEGIN_RESULT
COLUMNS	id	d
ROW	1	0
END_RESULT
AFFECTED	1
OK
ERROR	1201	TYPE_MISMATCH	mismatched type
```

`--output tsv` emits a header row and tab-separated rows suitable for
mysql-batch-style parsing. `--output mysql` emits an ASCII table and
`Query OK` statuses. Input accepts semicolon boundaries plus single-line SQL
without a trailing semicolon.

Fields use backslash escapes for tab, carriage return, newline, and backslash.
Every response is flushed immediately. Diagnostics go to stderr. The official
protocol is not published, so all readiness, framing, error-code, mode, and exit
decisions are isolated in `src/protocol.rs`; see
`docs/protocol-adapter.md`.

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --release --test benchmark -- --ignored --nocapture
```

The benchmark uses 10,000 rows, 128 dimensions, 1,000 TopK queries, K=10, and
an independent brute-force oracle. It currently reports Recall@10 = 1.0.

## Known limitations

- The official hidden MySQL-case pack and baseline were not available during
  implementation; the adapter is intentionally isolated for later alignment.
- Vector payloads use a bounded cache, but scalar values and primary-key
  metadata are still held in memory. Extremely large scalar-row sets can still
  exceed a tight memory budget.
- `ann` currently returns exact results. A real approximate index is deferred
  until admission correctness is stable.
