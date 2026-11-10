# Verification Record

Date: 2026-11-10

## Current checks

```text
cargo test
```

Result: pass, 70 tests plus one explicitly ignored benchmark. The suite covers
parser acceptance and rejection, Unicode
`VARCHAR`, finite `float32` vectors, CRUD, filtered TopK, transaction rollback,
incremental WAL recovery, forced-kill recovery, and TopK tie ordering.
It also includes a deterministic 240-step transaction model with periodic
restarts, scalar-index maintenance checks, common mysql-test setup statements,
direct `ORDER BY L2_DISTANCE(...)`, and protocol output-mode tests.

```text
cargo test --test public_protocol
```

Result: pass. The official public v4 manifest contains 20 cases and 95 SQL
statements. The test drives the real binary through JSON Lines, checks startup
readiness before input, preserves typed request IDs, verifies BIGINT precision,
applies exact/ANN mode per request, reuses the data directory across restarts,
checks semantic error codes, and validates all published result rows.

```text
cargo test --test jsonl_protocol
```

Result: pass. Focused protocol tests cover JSON string and integer IDs,
9007199254740993-scale BIGINT values, zero-row mutations,
`duplicate_key` followed by continued execution, shutdown rollback, and reopening
the same data directory. They also force-kill an open transaction through the
JSONL process interface, verify committed recovery, and check dimension,
NaN, and float32-overflow vector errors without terminating the protocol.

```text
cargo test --test sql_integration deleting_then_reinserting_same_primary_key_does_not_duplicate_index_matches
```

Result: pass after a regression fix. Before the fix, deleting primary key `1`
and inserting it again produced duplicate equality-index candidates
`[[1], [1]]`; `DELETE` now uses the index-maintaining row removal path and the
query returns `[[1]]`.

`SELECT NULL` and `INSERT ... NULL` are both rejected as unsupported rather
than being interpreted as an ordinary column name or value.

Vector payload appends are batched at the row-batch and WAL-replay levels.
The persistent benchmark after this change produced:

```text
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=445.7862ms query=1.3043352s cache_bytes=6151232
```

The prior non-batched local run on the same machine was approximately
`write=484.4624ms query=1.2972755s`. The write improvement is a local regression
signal; official cgroup measurements remain authoritative.

```text
cargo test --release --test benchmark -- --ignored --nocapture
```

Result:

```text
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=476.0788ms query=1.3466101s cache_bytes=5120000
```

The benchmark uses an independent brute-force oracle for every query. The
number is a local regression signal, not an official cgroup-based score.
This run uses the persistent WAL/page-store path, batches each vector record
into one file write, and prepares a bounded contiguous vector scan matrix for
the benchmark table. The numbers remain local regression signals and are not
directly comparable with the Linux cgroup evaluation environment.

## Write-amplification check

The same 1,000-row, 50-batch workload produced:

```text
full-snapshot WAL: 14155264 bytes
incremental WAL:     540175 bytes
```

The incremental representation reduces this workload's WAL by about 96%.

## Bounded vector cache

The vector-store regression writes 1,000 vectors of 128 dimensions, traverses
the whole file, updates and deletes rows, reopens with a forced 64 KiB cache,
and verifies the restored data. The observed cache remains at or below 64 KiB
while vector records exceed 512 KiB.

## Forced termination

The recovery test kills the process after an auto-committed row and after an
uncommitted row inside an open transaction. The restarted process sees only the
auto-committed row:

```text
forced_kill_keeps_committed_and_drops_uncommitted_rows ... ok
```

## Competition example

The exact SQL example from the competition statement was sent through the real
stdin/stdout binary. It produced affected-row counts of 3 and 1, returned the
two `database` rows in distance order, and restored all three rows after
`ROLLBACK`.

## Pending verification

- Official hidden cases
- Platform measurements for wall time, physical I/O, and cgroup peak memory
- Bounded storage for extremely large scalar-row sets; vector payloads are
  already paged

## Container verification

GitHub Actions run
[42573405545](https://github.com/BadFallen0110/seekdb/actions/runs/42573405545)
successfully built the Dockerfile, ran the full Rust test suite inside the
Linux build stage, started the resulting container with stdin only, and matched
the expected SQL smoke output.

Two successive Linux builder runs produced:

```text
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=785.733802ms query=1.329366971s cache_bytes=5120000
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=447.515141ms query=2.148253476s cache_bytes=5120000
```

The spread shows shared-runner variance. These are not official cgroup baseline
measurements; the competition's three-round median remains authoritative.
