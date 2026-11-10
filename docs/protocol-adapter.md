# Protocol Adapter Notes

The official public protocol package is `minivectordb-public-v4`. JSON Lines is
the default and is the only protocol used by the competition driver.

## Official JSONL

Startup emits:

```json
{"type":"ready"}
```

Each subsequent stdout line is one response object. The response preserves
the request's original JSON `id` type.

- Query responses contain `ok`, `columns`, and a two-dimensional `rows` array.
- `INSERT`, `UPDATE`, and `DELETE` contain integer `affected_rows`, including
  zero.
- Errors contain `ok:false` and an `error` object with semantic `code` and
  human-readable `message`.
- `{"op":"shutdown"}` rolls back an unfinished transaction, persists committed
  state, sends one success response, flushes, and exits with status 0.
- BIGINT output uses JSON integers and does not pass through floating point.
- Every response is flushed immediately. Diagnostics use stderr only.

The full contract and local checker are in `STDIO_PROTOCOL.md` and
`protocol_check/`. `tests/public_protocol.rs` runs all public cases through the
real subprocess protocol.

## Legacy text adapter

The earlier local adapter remains available only when explicitly selected. It
is useful for manual inspection and existing recovery tests but is not used by
the official driver.

- `--output framed` uses `READY`, `BEGIN_RESULT`, `ROW`, `END_RESULT`, and
  `AFFECTED`.
- `--output tsv` emits a header row and tab-separated rows.
- `--output mysql` emits an ASCII table and `Query OK` statuses.
- SQL statements are separated by semicolons, or a complete single-line
  statement can omit the trailing semicolon.
- `QUIT`, `EXIT`, `SHUTDOWN`, and `\q` roll back an unfinished transaction and
  exit.
- SQL comments containing `mode=ann` or `mode=exact` change query mode before
  that statement executes.

Database execution, WAL framing, transaction semantics, and vector calculations
do not depend on stdout formatting.
