use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minivectordb::db::Database;
use minivectordb::storage::{decode_frames, encode_frames};
use minivectordb::value::Value;

fn unique_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "minivectordb-{name}-{}-{nonce}",
        std::process::id()
    ))
}

#[test]
fn truncated_tail_is_ignored() {
    let mut bytes = encode_frames(&[b"one".to_vec(), b"two".to_vec()]);
    bytes.truncate(bytes.len() - 2);
    let frames = decode_frames(&bytes).unwrap();
    assert_eq!(frames, vec![b"one".to_vec()]);
}

#[test]
fn committed_database_state_survives_reopen() {
    let directory = unique_dir("reopen");
    {
        let mut database = Database::open(&directory).unwrap();
        database
            .execute("CREATE TABLE docs (id BIGINT PRIMARY KEY, body VARCHAR(16), embedding VECTOR(2))")
            .unwrap();
        database
            .execute("INSERT INTO docs VALUES (1, '持久化', '[1,0]')")
            .unwrap();
    }

    let mut reopened = Database::open(&directory).unwrap();
    let result = reopened.execute("SELECT * FROM docs").unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::BigInt(1));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn uncommitted_transaction_does_not_survive_reopen() {
    let directory = unique_dir("rollback");
    {
        let mut database = Database::open(&directory).unwrap();
        database
            .execute("CREATE TABLE docs (id BIGINT PRIMARY KEY)")
            .unwrap();
        database.execute("BEGIN").unwrap();
        database
            .execute("INSERT INTO docs VALUES (9)")
            .unwrap();
    }

    let mut reopened = Database::open(&directory).unwrap();
    assert!(reopened.execute("SELECT * FROM docs").unwrap().rows.is_empty());
    fs::remove_dir_all(directory).unwrap();
}
