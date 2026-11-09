# Verification Record

Date: 2026-11-09

## Current checks

```text
cargo test
```

Result: pass. The suite covers parser acceptance and rejection, Unicode
`VARCHAR`, finite `float32` vectors, CRUD, filtered TopK, transaction rollback,
incremental WAL recovery, forced-kill recovery, and TopK tie ordering.

```text
cargo test --release --test benchmark -- --ignored --nocapture
```

Result:

```text
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=222.7979ms query=1.376657s
```

The benchmark uses an independent brute-force oracle for every query. The
number is a local regression signal, not an official cgroup-based score.

## Write-amplification check

The same 1,000-row, 50-batch workload produced:

```text
full-snapshot WAL: 14155264 bytes
incremental WAL:     540175 bytes
```

The incremental representation reduces this workload's WAL by about 96%.

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

- Official hidden MySQL-case pack and exact stdout protocol
- Docker build and Linux x86_64 execution
- Platform measurements for wall time, physical I/O, and cgroup peak memory
- A true bounded page cache for data larger than the memory limit
