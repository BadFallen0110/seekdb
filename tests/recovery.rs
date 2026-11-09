use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct Harness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Harness {
    fn start(data_dir: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_minivectordb"))
            .args([
                "--data-dir",
                data_dir.to_str().unwrap(),
                "--ready",
                "READY",
                "--output",
                "framed",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let mut harness = Self {
            child,
            stdin,
            stdout,
        };
        assert_eq!(harness.next_line(), "READY");
        harness
    }

    fn send(&mut self, sql: &str) {
        writeln!(self.stdin, "{sql}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn next_line(&mut self) -> String {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        line.trim_end().to_owned()
    }

    fn wait_for_prefix(&mut self, prefix: &str) {
        for _ in 0..64 {
            let line = self.next_line();
            if line.starts_with(prefix) {
                return;
            }
        }
        panic!("did not observe response prefix `{prefix}`");
    }

    fn kill(mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
    }
}

fn unique_dir() -> std::path::PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "minivectordb-recovery-{}-{nonce}",
        std::process::id()
    ))
}

#[test]
fn forced_kill_keeps_committed_and_drops_uncommitted_rows() {
    let data_dir = unique_dir();
    {
        let mut database = Harness::start(&data_dir);
        database.send("CREATE TABLE docs (id BIGINT PRIMARY KEY);");
        database.wait_for_prefix("OK");
        database.send("INSERT INTO docs VALUES (1);");
        database.wait_for_prefix("AFFECTED\t1");
        database.send("BEGIN;");
        database.wait_for_prefix("OK");
        database.send("INSERT INTO docs VALUES (2);");
        database.wait_for_prefix("AFFECTED\t1");
        database.kill();
    }

    let mut database = Harness::start(&data_dir);
    database.send("SELECT * FROM docs ORDER BY id;");
    database.wait_for_prefix("BEGIN_RESULT");
    assert_eq!(database.next_line(), "COLUMNS\tid");
    assert_eq!(database.next_line(), "ROW\t1");
    assert_eq!(database.next_line(), "END_RESULT");
    database.send("QUIT;");
    let status = database.child.wait().unwrap();
    assert!(status.success());
    std::fs::remove_dir_all(data_dir).unwrap();
}

#[test]
fn explicit_commit_is_durable_before_its_response() {
    let data_dir = unique_dir();
    {
        let mut database = Harness::start(&data_dir);
        database.send("CREATE TABLE docs (id BIGINT PRIMARY KEY);");
        database.wait_for_prefix("OK");
        database.send("BEGIN;");
        database.wait_for_prefix("OK");
        database.send("INSERT INTO docs VALUES (7);");
        database.wait_for_prefix("AFFECTED\t1");
        database.send("COMMIT;");
        database.wait_for_prefix("OK");
        database.kill();
    }

    let mut database = Harness::start(&data_dir);
    database.send("SELECT * FROM docs;");
    database.wait_for_prefix("BEGIN_RESULT");
    assert_eq!(database.next_line(), "COLUMNS\tid");
    assert_eq!(database.next_line(), "ROW\t7");
    assert_eq!(database.next_line(), "END_RESULT");
    database.send("QUIT;");
    assert!(database.child.wait().unwrap().success());
    std::fs::remove_dir_all(data_dir).unwrap();
}
