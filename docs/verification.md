# Verification Record

Date: 2026-11-09

## Current checks

```text
cargo test
```

Result: pass. The suite covers parser acceptance and rejection, Unicode
`VARCHAR`, finite `float32` vectors, CRUD, filtered TopK, transaction rollback,
incremental WAL recovery, forced-kill recovery, and TopK tie ordering.
It also includes a deterministic 240-step transaction model with periodic
restarts, scalar-index maintenance checks, common mysql-test setup statements,
direct `ORDER BY L2_DISTANCE(...)`, and protocol output-mode tests.

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

- Official hidden MySQL-case pack and exact stdout protocol
- Platform measurements for wall time, physical I/O, and cgroup peak memory
- Bounded storage for extremely large scalar-row sets; vector payloads are
  already paged

## Container verification

GitHub Actions run
[42572788687](https://github.com/BadFallen0110/seekdb/actions/runs/42572788687)
successfully built the Dockerfile, ran the full Rust test suite inside the
Linux build stage, started the resulting container with stdin only, and matched
the expected SQL smoke output.

The same Linux builder ran the persistent benchmark:

```text
rows=10000 dimensions=128 queries=1000 recall=1.000000 write=785.733802ms query=1.329366971s cache_bytes=5120000
```

This is a shared hosted-runner measurement, not the official cgroup baseline.
