use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

struct Harness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Harness {
    fn start() -> (Self, PathBuf) {
        static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let data_directory = std::env::temp_dir().join(format!(
            "minivectordb-jsonl-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        let harness = Self::start_at(&data_directory);
        (harness, data_directory)
    }

    fn start_at(data_directory: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
            .args(["--data-dir", data_directory.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(line.trim_end()).unwrap()
    }

    fn send(&mut self, request: Value) {
        writeln!(self.stdin, "{request}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn execute(&mut self, id: Value, sql: &str) -> Value {
        self.send(serde_json::json!({
            "id": id,
            "op": "execute",
            "sql": sql,
            "search_mode": "exact"
        }));
        self.read()
    }

    fn shutdown(&mut self, id: Value) {
        self.send(serde_json::json!({"id": id, "op": "shutdown"}));
        assert_eq!(self.read(), serde_json::json!({"id": id, "ok": true}));
        assert!(self.child.wait().unwrap().success());
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn default_protocol_emits_ready_then_executes_ascii_sql() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));

    let response = harness.execute(serde_json::json!("request-1"), "SELECT 1 AS value;");
    assert_eq!(
        response,
        serde_json::json!({
            "id": "request-1",
            "ok": true,
            "columns": ["value"],
            "rows": [[1]]
        })
    );

    harness.shutdown(serde_json::json!(2));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn request_ids_and_bigints_keep_full_json_precision() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
    harness.execute(
        1.into(),
        "CREATE TABLE docs (id BIGINT PRIMARY KEY, value BIGINT, negative BIGINT);",
    );
    harness.execute(
        2.into(),
        "INSERT INTO docs VALUES (1, 9007199254740993, -9007199254740993);",
    );
    let id = serde_json::json!(9007199254740993_i64);
    assert_eq!(
        harness.execute(id.clone(), "SELECT value, negative FROM docs;"),
        serde_json::json!({
            "id": id,
            "ok": true,
            "columns": ["value", "negative"],
            "rows": [[9007199254740993_i64, -9007199254740993_i64]]
        })
    );
    harness.shutdown(serde_json::json!("shutdown-id"));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn zero_row_mutations_return_integer_affected_rows() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
    assert_eq!(
        harness.execute(1.into(), "CREATE TABLE docs (id BIGINT PRIMARY KEY);"),
        serde_json::json!({"id": 1, "ok": true})
    );
    assert_eq!(
        harness.execute(2.into(), "DELETE FROM docs WHERE id=1;"),
        serde_json::json!({"id": 2, "ok": true, "affected_rows": 0})
    );
    harness.shutdown(3.into());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn sql_errors_return_semantic_code_and_process_continues() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
    harness.execute(
        1.into(),
        "CREATE TABLE docs (id BIGINT PRIMARY KEY, content VARCHAR(8));",
    );
    harness.execute(2.into(), "INSERT INTO docs VALUES (1,'first');");
    let duplicate = harness.execute(3.into(), "INSERT INTO docs VALUES (1,'second');");
    assert_eq!(duplicate["id"], serde_json::json!(3));
    assert_eq!(duplicate["ok"], serde_json::json!(false));
    assert_eq!(
        duplicate["error"]["code"],
        serde_json::json!("duplicate_key")
    );
    assert_eq!(
        harness.execute(4.into(), "SELECT content FROM docs;"),
        serde_json::json!({
            "id": 4,
            "ok": true,
            "columns": ["content"],
            "rows": [["first"]]
        })
    );
    harness.shutdown(5.into());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn shutdown_rolls_back_uncommitted_changes_and_reopens_same_data() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
    harness.execute(1.into(), "CREATE TABLE docs (id BIGINT PRIMARY KEY);");
    harness.execute(2.into(), "INSERT INTO docs VALUES (1);");
    harness.execute(3.into(), "BEGIN;");
    harness.execute(4.into(), "INSERT INTO docs VALUES (2);");
    harness.shutdown(5.into());

    let mut reopened = Harness::start_at(&directory);
    assert_eq!(reopened.read(), serde_json::json!({"type": "ready"}));
    assert_eq!(
        reopened.execute(6.into(), "SELECT id FROM docs ORDER BY id;"),
        serde_json::json!({
            "id": 6,
            "ok": true,
            "columns": ["id"],
            "rows": [[1]]
        })
    );
    reopened.shutdown(7.into());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn forced_kill_recovers_committed_jsonl_writes_and_drops_the_open_transaction() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
    harness.execute(1.into(), "CREATE TABLE docs (id BIGINT PRIMARY KEY);");
    harness.execute(2.into(), "INSERT INTO docs VALUES (1);");
    harness.execute(3.into(), "BEGIN;");
    harness.execute(4.into(), "INSERT INTO docs VALUES (2);");
    harness.child.kill().unwrap();
    harness.child.wait().unwrap();
    drop(harness);

    let mut recovered = Harness::start_at(&directory);
    assert_eq!(recovered.read(), serde_json::json!({"type": "ready"}));
    assert_eq!(
        recovered.execute(5.into(), "SELECT id FROM docs ORDER BY id;"),
        serde_json::json!({
            "id": 5,
            "ok": true,
            "columns": ["id"],
            "rows": [[1]]
        })
    );
    recovered.shutdown(6.into());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn invalid_vectors_fail_atomically_and_do_not_stop_the_protocol() {
    let (mut harness, directory) = Harness::start();
    assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
    harness.execute(
        1.into(),
        "CREATE TABLE docs (id BIGINT PRIMARY KEY, embedding VECTOR(2));",
    );
    for (id, vector, expected_code) in [
        (2_u64, "[1]", "dimension_mismatch"),
        (3, "[NaN,0]", "invalid_vector"),
        (4, "[1e100,0]", "invalid_vector"),
    ] {
        let response = harness.execute(
            serde_json::json!(id),
            &format!("INSERT INTO docs VALUES ({id},'{vector}');"),
        );
        assert_eq!(response["ok"], serde_json::json!(false));
        assert_eq!(response["error"]["code"], serde_json::json!(expected_code));
    }
    assert_eq!(
        harness.execute(5.into(), "INSERT INTO docs VALUES (5,'[1,0]');"),
        serde_json::json!({"id": 5, "ok": true, "affected_rows": 1})
    );
    assert_eq!(
        harness.execute(6.into(), "SELECT id FROM docs;"),
        serde_json::json!({
            "id": 6,
            "ok": true,
            "columns": ["id"],
            "rows": [[5]]
        })
    );
    harness.shutdown(7.into());
    std::fs::remove_dir_all(directory).unwrap();
}
