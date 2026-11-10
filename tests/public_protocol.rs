use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Map, Number, Value};

mod support;
use support::unique_temp_path;

struct Harness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Harness {
    fn start(data_directory: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
            .args(["--data-dir", data_directory.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut harness = Self {
            child,
            stdin,
            stdout,
        };
        assert_eq!(harness.read(), serde_json::json!({"type": "ready"}));
        harness
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "database exited before sending a response"
        );
        serde_json::from_str(line.trim_end()).unwrap()
    }

    fn request(&mut self, id: u64, sql: &str, mode: &str) -> Value {
        writeln!(
            self.stdin,
            "{}",
            serde_json::json!({
                "id": id,
                "op": "execute",
                "sql": sql,
                "search_mode": mode
            })
        )
        .unwrap();
        self.stdin.flush().unwrap();
        let response = self.read();
        assert_eq!(response.get("id"), Some(&Value::Number(Number::from(id))));
        response
    }

    fn shutdown(&mut self, id: u64) {
        writeln!(
            self.stdin,
            "{}",
            serde_json::json!({"id": id, "op": "shutdown"})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        assert_eq!(self.read(), serde_json::json!({"id": id, "ok": true}));
        assert!(self.child.wait().unwrap().success());
        let mut extra = String::new();
        self.stdout.read_to_string(&mut extra).unwrap();
        assert!(extra.is_empty(), "extra stdout after shutdown: {extra:?}");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unique_directory(case_id: &str) -> PathBuf {
    unique_temp_path(&format!("minivectordb-public-{case_id}"))
}

fn assert_expected(response: &Value, expected: &Value) {
    if expected.get("error").is_some() {
        assert_eq!(response.get("ok"), Some(&Value::Bool(false)), "{response}");
        let code = response
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str);
        assert!(code.is_some_and(|code| !code.is_empty()), "{response}");
        assert_eq!(
            code,
            expected.get("error").and_then(Value::as_str),
            "semantic error code: {response}"
        );
        return;
    }
    assert_eq!(response.get("ok"), Some(&Value::Bool(true)), "{response}");
    for key in ["affected_rows", "columns"] {
        if let Some(wanted) = expected.get(key) {
            assert_same(response.get(key).unwrap_or(&Value::Null), wanted, key);
        }
    }
    let Some(wanted_rows) = expected.get("rows") else {
        return;
    };
    let actual_rows = response
        .get("rows")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing rows: {response}"));
    if expected
        .get("unordered")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mut remaining = actual_rows.clone();
        for wanted in wanted_rows.as_array().unwrap() {
            let index = remaining
                .iter()
                .position(|candidate| same(candidate, wanted))
                .unwrap_or_else(|| panic!("missing row: {wanted}"));
            remaining.remove(index);
        }
        assert!(remaining.is_empty(), "unexpected rows: {remaining:?}");
    } else {
        assert_same(&Value::Array(actual_rows.clone()), wanted_rows, "rows");
    }
}

fn assert_same(actual: &Value, expected: &Value, context: &str) {
    assert!(same(actual, expected), "{context}: {actual} != {expected}");
}

fn same(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Number(actual), Value::Number(expected)) if expected.is_f64() => {
            let actual = actual.as_f64().unwrap();
            let expected = expected.as_f64().unwrap();
            let difference = (actual - expected).abs();
            actual.is_finite() && (difference <= 1e-5 || difference <= 1e-5 * expected.abs())
        }
        (Value::Number(actual), Value::Number(expected)) => {
            (expected.is_i64() && actual.as_i64() == expected.as_i64())
                || (expected.is_u64() && actual.as_u64() == expected.as_u64())
        }
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual
                    .iter()
                    .zip(expected)
                    .all(|(actual, expected)| same(actual, expected))
        }
        (Value::Object(actual), Value::Object(expected)) => same_object(actual, expected),
        _ => actual == expected,
    }
}

fn same_object(actual: &Map<String, Value>, expected: &Map<String, Value>) -> bool {
    actual.len() == expected.len()
        && expected
            .iter()
            .all(|(key, expected)| actual.get(key).is_some_and(|actual| same(actual, expected)))
}

#[test]
fn all_official_public_cases_pass() {
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("mysqltest");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(package.join("manifest.json")).unwrap())
            .unwrap();
    let cases = manifest["cases"].as_array().unwrap();

    for case in cases {
        let case_id = case["id"].as_str().unwrap();
        let test_source =
            std::fs::read_to_string(package.join(case["test"].as_str().unwrap())).unwrap();
        let expected: Value = serde_json::from_str(
            &std::fs::read_to_string(package.join(case["result"].as_str().unwrap())).unwrap(),
        )
        .unwrap();
        let expected = expected.as_array().unwrap();
        let directory = unique_directory(case_id);
        let mut harness = Harness::start(&directory);
        let mut mode = "exact";
        let mut expected_index = 0_usize;
        let mut sequence = 0_u64;

        for (line_number, source_line) in test_source.lines().enumerate() {
            let sql = source_line.trim();
            if sql.is_empty() || sql.starts_with('#') {
                continue;
            }
            if sql == "--restart" {
                harness.shutdown(sequence);
                harness = Harness::start(&directory);
                continue;
            }
            if sql == "--mode exact" || sql == "--mode ann" {
                mode = sql.split_whitespace().nth(1).unwrap();
                continue;
            }
            assert!(!sql.starts_with("--"), "unsupported directive in {case_id}");
            assert!(
                expected_index < expected.len(),
                "too many SQL statements in {case_id} near line {}",
                line_number + 1
            );
            sequence += 1;
            let response = harness.request(sequence, sql, mode);
            assert_expected(&response, &expected[expected_index]);
            expected_index += 1;
        }
        assert_eq!(
            expected_index,
            expected.len(),
            "SQL/result count mismatch in {case_id}"
        );
        sequence += 1;
        harness.shutdown(sequence);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
