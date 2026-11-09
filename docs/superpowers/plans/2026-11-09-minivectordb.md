# MiniVectorDB Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans
> to implement this plan task-by-task. This session uses inline execution because
> subagents were not requested. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a crash-safe, stdin/stdout MiniVectorDB that passes a broad
local MySQL-style SQL suite and provides a measurable vector-query baseline.

**Architecture:** A Rust binary with a testable library core. The SQL parser,
vector kernel, in-memory transaction view, and WAL snapshot store communicate
through typed interfaces. The protocol adapter is replaceable when the official
driver is published.

**Tech Stack:** Rust 1.98, standard library only, Cargo tests, PowerShell local
runner, Linux x86_64 multi-stage Docker build.

---

## File Map

- `Cargo.toml`: package and test configuration.
- `src/lib.rs`: public module composition.
- `src/error.rs`: stable internal error categories and user-facing messages.
- `src/value.rs`: `BIGINT`, `VARCHAR`, and `VECTOR` values plus validation.
- `src/sql.rs`: lexer, AST, and parser for the competition SQL subset.
- `src/table.rs`: schema, rows, primary-key lookup, and mutation preparation.
- `src/vector.rs`: finite-float parsing, scalar/AVX2 L2, and TopK.
- `src/db.rs`: database state, statement execution, and transaction overlay.
- `src/storage.rs`: checksummed snapshot/WAL codec and recovery.
- `src/protocol.rs`: stdin framing, response formatting, and lifecycle commands.
- `src/main.rs`: command-line startup and run loop.
- `tests/sql_integration.rs`: end-to-end SQL semantics.
- `tests/recovery.rs`: subprocess restart and forced-termination recovery.
- `tests/benchmark.rs`: ignored 10k x 128 workload and Recall verification.
- `Dockerfile`: offline runtime image built from source.
- `README.md`: build, protocol, SQL scope, and recovery instructions.
- `agent/conversation.json`: bounded, truthful learning-evidence record.

### Task 1: Cargo Skeleton and Error Contract

**Files:**
- Create: `Cargo.toml`
- Create: `src/lib.rs`
- Create: `src/error.rs`
- Test: `src/error.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn duplicate_key_has_stable_category() {
    let e = DbError::duplicate_key("docs");
    assert_eq!(e.category(), ErrorCategory::DuplicateKey);
    assert!(e.to_string().contains("docs"));
}
```

- [ ] **Step 2: Run the test and observe failure**

Run: `cargo test duplicate_key_has_stable_category`
Expected: Cargo reports the package or symbols do not exist.

- [ ] **Step 3: Add the minimal package and enum**

Create a binary/library package named `minivectordb`, define `ErrorCategory`,
`DbError`, constructors, `Display`, and `std::error::Error`.

- [ ] **Step 4: Run the test**

Run: `cargo test duplicate_key_has_stable_category`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add Cargo.toml src/lib.rs src/error.rs
git commit -m "feat: add Rust crate and error contract"
```

### Task 2: Typed Values and Vector Validation

**Files:**
- Create: `src/value.rs`
- Create: `src/vector.rs`
- Modify: `src/lib.rs`
- Test: `src/value.rs`
- Test: `src/vector.rs`

- [ ] **Step 1: Write failing validation tests**

```rust
#[test]
fn varchar_counts_unicode_scalars() {
    assert!(Value::varchar("数据库", 3).is_ok());
    assert!(Value::varchar("数据库", 2).is_err());
}

#[test]
fn vector_rejects_non_finite_and_overflow() {
    assert!(parse_vector("[1, NaN]", 2).is_err());
    assert!(parse_vector("[1e100, 0]", 2).is_err());
    assert_eq!(parse_vector("[0.1, 2]", 2).unwrap(), vec![0.1_f32, 2.0]);
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test value::tests vector::tests`
Expected: FAIL because the types do not exist.

- [ ] **Step 3: Implement validation**

Define `Value::{BigInt, Varchar, Vector}`, `ColumnType`, `Row`, `parse_vector`,
finite `f32` conversion, and L2 functions. The AVX2 path is gated by runtime
feature detection and must be bit-close to the scalar path.

- [ ] **Step 4: Run tests**

Run: `cargo test value::tests vector::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/value.rs src/vector.rs src/lib.rs
git commit -m "feat: validate SQL values and vector inputs"
```

### Task 3: SQL Lexer and Parser

**Files:**
- Create: `src/sql.rs`
- Modify: `src/lib.rs`
- Test: `src/sql.rs`

- [ ] **Step 1: Write failing parser tests**

```rust
#[test]
fn parses_sample_vector_query() {
    let stmt = parse("SELECT id, L2_DISTANCE(embedding, '[1,0]') AS d \
                      FROM docs WHERE category = 'db' ORDER BY d LIMIT 2")
        .unwrap();
    assert!(matches!(stmt, Statement::Select(_)));
}

#[test]
fn rejects_null_in_values() {
    let err = parse("INSERT INTO t VALUES (1, NULL)").unwrap_err();
    assert_eq!(err.category(), ErrorCategory::Unsupported);
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test sql::tests`
Expected: FAIL because `parse` and AST types do not exist.

- [ ] **Step 3: Implement the SQL subset**

Implement tokens, comments, quoted strings/identifiers, AST, and parsing for all
statements listed in the design. Parse unsupported clauses into explicit errors.

- [ ] **Step 4: Run tests**

Run: `cargo test sql::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/sql.rs src/lib.rs
git commit -m "feat: parse MiniVectorDB SQL subset"
```

### Task 4: Tables, Predicates, and Query Semantics

**Files:**
- Create: `src/table.rs`
- Create: `src/db.rs`
- Modify: `src/lib.rs`
- Test: `tests/sql_integration.rs`

- [ ] **Step 1: Write failing end-to-end tests**

```rust
#[test]
fn filtered_topk_filters_before_limit() {
    let mut db = Database::memory();
    db.execute("CREATE TABLE docs (id BIGINT PRIMARY KEY, category VARCHAR(16), \
                embedding VECTOR(2))").unwrap();
    db.execute("INSERT INTO docs VALUES \
                (1,'db','[1,0]'),(2,'travel','[0.9,0.1]'),(3,'db','[0,1]')").unwrap();
    let out = db.execute("SELECT id, L2_DISTANCE(embedding, '[1,0]') AS d \
                          FROM docs WHERE category='db' ORDER BY d LIMIT 1").unwrap();
    assert_eq!(out.rows[0][0], Value::BigInt(1));
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test --test sql_integration filtered_topk_filters_before_limit`
Expected: FAIL because `Database` does not exist.

- [ ] **Step 3: Implement tables and execution**

Implement schema checks, unique primary keys, projection, `AND` predicates,
ordering, `LIMIT`, scalar `L2_DISTANCE`, and DML using a prepared mutation list.

- [ ] **Step 4: Run the full suite**

Run: `cargo test --test sql_integration`
Expected: PASS for CRUD, filters, ordering, vector TopK, and invalid input.

- [ ] **Step 5: Commit**

```powershell
git add src/table.rs src/db.rs src/lib.rs tests/sql_integration.rs
git commit -m "feat: execute SQL and filtered vector top-k"
```

### Task 5: Transactions and Statement Atomicity

**Files:**
- Modify: `src/db.rs`
- Test: `tests/sql_integration.rs`

- [ ] **Step 1: Write failing transaction tests**

```rust
#[test]
fn rollback_restores_scalar_and_vector_queries() {
    let mut db = sample_db();
    db.execute("BEGIN").unwrap();
    db.execute("UPDATE docs SET embedding='[0,1]' WHERE id=1").unwrap();
    assert_eq!(db.scalar_i64("SELECT id FROM docs ORDER BY id LIMIT 1").unwrap(), 1);
    db.execute("ROLLBACK").unwrap();
    let before = db.execute("SELECT id, L2_DISTANCE(embedding,'[1,0]') AS d \
                             FROM docs ORDER BY d LIMIT 1").unwrap();
    assert_eq!(before.rows[0][0], Value::BigInt(1));
}
```

- [ ] **Step 2: Run and observe failure**

Run: `cargo test --test sql_integration rollback_restores_scalar_and_vector_queries`
Expected: FAIL because transaction state is not implemented.

- [ ] **Step 3: Implement overlay transactions**

Add committed state plus one overlay, read-own-writes merging, `BEGIN/COMMIT/
ROLLBACK`, nest rejection, and statement-level validation prior to application.

- [ ] **Step 4: Run tests**

Run: `cargo test --test sql_integration`
Expected: PASS including failed-statement atomicity.

- [ ] **Step 5: Commit**

```powershell
git add src/db.rs tests/sql_integration.rs
git commit -m "feat: add explicit transaction overlay"
```

### Task 6: Checksummed Snapshot/WAL and Recovery

**Files:**
- Create: `src/storage.rs`
- Modify: `src/db.rs`
- Modify: `src/lib.rs`
- Test: `src/storage.rs`
- Test: `tests/recovery.rs`

- [ ] **Step 1: Write failing codec and tail tests**

```rust
#[test]
fn truncated_tail_is_ignored() {
    let mut bytes = encode_frames(&[b"one".to_vec(), b"two".to_vec()]);
    bytes.truncate(bytes.len() - 2);
    let frames = decode_frames(&bytes).unwrap();
    assert_eq!(frames, vec![b"one".to_vec()]);
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test storage::tests`
Expected: FAIL because the frame codec does not exist.

- [ ] **Step 3: Implement durable commits**

Use length-prefixed checksummed frames. Each auto-commit or `COMMIT` serializes
the complete database state, appends one frame, and calls `sync_all` before
success. Recovery replays the last valid frame. A transaction that is open and
never committed is absent from storage.

- [ ] **Step 4: Run storage and recovery tests**

Run: `cargo test storage::tests`
Run: `cargo test --test recovery`
Expected: PASS for normal restart, truncated tail, and repeated recovery.

- [ ] **Step 5: Commit**

```powershell
git add src/storage.rs src/db.rs src/lib.rs tests/recovery.rs
git commit -m "feat: persist and recover checksummed transactions"
```

### Task 7: Protocol Adapter and Executable

**Files:**
- Create: `src/protocol.rs`
- Create: `src/main.rs`
- Test: `tests/sql_integration.rs`

- [ ] **Step 1: Write a failing subprocess protocol test**

```rust
#[test]
fn binary_accepts_multiple_semicolon_statements() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
        .args(["--data-dir", temp_dir.path().to_str().unwrap()])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(
        b"CREATE TABLE t (id BIGINT PRIMARY KEY);\nINSERT INTO t VALUES (1);\n\
          SELECT * FROM t;\nQUIT;\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("1"));
}
```

- [ ] **Step 2: Run and observe failure**

Run: `cargo test --test sql_integration binary_accepts_multiple_semicolon_statements`
Expected: FAIL because the binary entry point does not exist.

- [ ] **Step 3: Implement stdin/stdout lifecycle**

Read SQL incrementally, honor semicolons and EOF, flush every response, keep
diagnostics on stderr, accept `QUIT`/`EXIT`/`\\q`, and convert SIGTERM/SIGINT
into rollback plus checkpoint.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all current tests PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/protocol.rs src/main.rs tests/sql_integration.rs
git commit -m "feat: add stdin stdout database protocol"
```

### Task 8: Official-Protocol Adapter Hooks

**Files:**
- Modify: `src/protocol.rs`
- Create: `docs/protocol-adapter.md`
- Test: `tests/sql_integration.rs`

- [ ] **Step 1: Write failing mode and exit tests**

```rust
#[test]
fn ann_mode_is_visible_without_weakening_exact_results() {
    let mut db = Database::memory();
    db.set_query_mode(QueryMode::Ann);
    assert_eq!(db.query_mode(), QueryMode::Ann);
}
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test ann_mode_is_visible_without_weakening_exact_results`
Expected: FAIL because query mode is not exposed.

- [ ] **Step 3: Add adapters**

Support exact/ann through CLI flag, environment variable, and SQL comment hint
in priority order. Implement `CREATE VECTOR INDEX` metadata and exact fallback.
Document exactly where to alter readiness, result framing, error codes, and exit
handling when the official pack appears.

- [ ] **Step 4: Run tests**

Run: `cargo test`
Expected: all tests PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/protocol.rs tests/sql_integration.rs docs/protocol-adapter.md
git commit -m "feat: add protocol and query-mode adapter hooks"
```

### Task 9: Larger-than-Cache and Performance Baseline

**Files:**
- Modify: `src/storage.rs`
- Create: `tests/benchmark.rs`
- Create: `tools/generate_benchmark.rs`

- [ ] **Step 1: Write a failing benchmark smoke test**

```rust
#[test]
fn ten_thousand_vectors_return_exact_recall() {
    let db = seeded_benchmark_db(10_000, 128);
    let truths = brute_force_truths(&db, 20, 10);
    assert_eq!(db.topk_truth_recall(&truths), 1.0);
}
```

- [ ] **Step 2: Run and observe failure or timeout**

Run: `cargo test --test benchmark ten_thousand_vectors_return_exact_recall -- --ignored`
Expected: FAIL because the benchmark helpers do not exist.

- [ ] **Step 3: Implement benchmark and bounded cache**

Add deterministic generators, 10k writes in batches of 100, 1k queries, half
filtered, AVX2 distance, recall checks, and a page/cache abstraction large
enough for the benchmark but evictable for resource tests.

- [ ] **Step 4: Run and record**

Run: `cargo test --test benchmark -- --ignored --nocapture`
Expected: PASS, Recall@10 = 1.0, and timing output is printed.

- [ ] **Step 5: Commit**

```powershell
git add src/storage.rs tests/benchmark.rs tools/generate_benchmark.rs
git commit -m "perf: add exact-scan benchmark and bounded cache"
```

### Task 10: Delivery and Evidence

**Files:**
- Modify: `Dockerfile`
- Create: `README.md`
- Create: `agent/conversation.json`
- Create: `docs/verification.md`

- [ ] **Step 1: Write a failing artifact check**

```powershell
$size = (Get-Item .\agent\conversation.json).Length
if ($size -gt 131072) { throw "conversation.json exceeds 128 KiB" }
Get-Content -Raw .\agent\conversation.json | ConvertFrom-Json | Out-Null
```

- [ ] **Step 2: Run and verify failure**

Run: `cargo test; powershell -File tests/check_artifacts.ps1`
Expected: FAIL because the artifacts do not exist.

- [ ] **Step 3: Add deliverables**

Write a multi-stage Dockerfile that builds from source, a README with examples,
a verification report with actual command output summaries, and a truthful
conversation record containing the user's real prompts and completed
experiments.

- [ ] **Step 4: Run final verification**

Run: `cargo fmt --check`
Run: `cargo clippy --all-targets -- -D warnings`
Run: `cargo test`
Run: `powershell -File tests/check_artifacts.ps1`
Expected: all commands exit 0.

- [ ] **Step 5: Commit**

```powershell
git add Dockerfile README.md agent/conversation.json docs/verification.md tests/check_artifacts.ps1
git commit -m "docs: package submission and verification evidence"
```

## Self-Review

- Spec coverage: SQL, storage, transactions, recovery, vector queries, modes,
  performance, protocol, error handling, Docker, README, and evidence each map
  to a named task.
- Placeholder scan: mode adaptation points the spec leaves undecided are
  isolated and documented; no implementation step defers core functionality.
- Type consistency: `Database`, `Value`, `DbError`, `QueryMode`, `parse`, and
  storage frame functions use one spelling throughout.
