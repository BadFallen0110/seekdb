use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use minivectordb::db::Database;
use minivectordb::error::ErrorCategory;
use minivectordb::value::Value;

fn unique_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("minivectordb-delta-{}-{nonce}", std::process::id()))
}

fn zero_vector() -> String {
    format!("[{}]", vec!["0"; 128].join(","))
}

#[test]
fn many_small_commits_do_not_rewrite_the_whole_database() {
    let directory = unique_dir();
    let mut database = Database::open(&directory).unwrap();
    database
        .execute("CREATE TABLE bench (id BIGINT PRIMARY KEY, embedding VECTOR(128))")
        .unwrap();
    let vector = zero_vector();
    for batch in 0..50 {
        let rows = (0..20)
            .map(|offset| {
                let id = batch * 20 + offset;
                format!("({id},'{vector}')")
            })
            .collect::<Vec<_>>()
            .join(",");
        database
            .execute(&format!("INSERT INTO bench VALUES {rows}"))
            .unwrap();
    }
    database.close().unwrap();

    let log_size = std::fs::metadata(directory.join("state.log"))
        .unwrap()
        .len();
    eprintln!("incremental_wal_bytes={log_size}");
    assert!(
        log_size < 2 * 1024 * 1024,
        "incremental WAL grew to {log_size} bytes"
    );

    let mut reopened = Database::open(&directory).unwrap();
    let result = reopened
        .execute("SELECT id FROM bench ORDER BY id")
        .unwrap();
    assert_eq!(result.rows.len(), 1_000);
    assert_eq!(result.rows[999][0], Value::BigInt(999));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn recovery_replays_ddl_upserts_and_deletes_in_order() {
    let directory = unique_dir();
    {
        let mut database = Database::open(&directory).unwrap();
        database
            .execute("CREATE TABLE docs (id BIGINT PRIMARY KEY, body VARCHAR(16))")
            .unwrap();
        database
            .execute("INSERT INTO docs VALUES (1,'old'),(2,'deleted')")
            .unwrap();
        database
            .execute("UPDATE docs SET body='new' WHERE id=1")
            .unwrap();
        database.execute("DELETE FROM docs WHERE id=2").unwrap();
        database.close().unwrap();
    }

    let mut reopened = Database::open(&directory).unwrap();
    let result = reopened.execute("SELECT * FROM docs").unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::BigInt(1), Value::Varchar("new".to_owned())]]
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn vector_rows_use_the_bounded_page_store() {
    let directory = unique_dir();
    let mut database = Database::open_with_cache_limit(&directory, 64 * 1024).unwrap();
    database
        .execute("CREATE TABLE docs (id BIGINT PRIMARY KEY, embedding VECTOR(128))")
        .unwrap();
    let vector = zero_vector();
    for batch in 0..10 {
        let rows = (0..100)
            .map(|offset| {
                let id = batch * 100 + offset;
                format!("({id},'{vector}')")
            })
            .collect::<Vec<_>>()
            .join(",");
        database
            .execute(&format!("INSERT INTO docs VALUES {rows}"))
            .unwrap();
    }
    let query = format!(
        "SELECT id, L2_DISTANCE(embedding,'{vector}') AS d \
         FROM docs ORDER BY d LIMIT 10"
    );
    let result = database.execute(&query).unwrap();
    assert_eq!(result.rows.len(), 10);
    assert!(database.vector_cache_bytes() <= 64 * 1024);
    assert!(directory.join("vectors.bin").metadata().unwrap().len() > 512_000);
    let first_axis = format!("[1,{}]", vec!["0"; 127].join(","));
    database
        .execute(&format!(
            "UPDATE docs SET embedding='{first_axis}' WHERE id=999"
        ))
        .unwrap();
    database.execute("DELETE FROM docs WHERE id=998").unwrap();
    assert_eq!(
        database
            .execute(&format!(
                "SELECT id, L2_DISTANCE(embedding,'{first_axis}') AS d \
                 FROM docs ORDER BY d LIMIT 1"
            ))
            .unwrap()
            .rows[0][0],
        Value::BigInt(999)
    );
    let nearest = database
        .execute(&format!(
            "SELECT id FROM docs WHERE embedding = '{first_axis}'"
        ))
        .unwrap_err();
    assert_eq!(nearest.category(), ErrorCategory::Unsupported);

    let mut reopened = Database::open_with_cache_limit(&directory, 64 * 1024).unwrap();
    assert_eq!(
        reopened
            .execute("SELECT id FROM docs ORDER BY id DESC LIMIT 1")
            .unwrap()
            .rows[0][0],
        Value::BigInt(999)
    );
    assert!(
        reopened
            .execute("SELECT id FROM docs WHERE id=998")
            .unwrap()
            .rows
            .is_empty()
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn corrupt_vector_file_is_rebuilt_from_committed_wal() {
    let directory = unique_dir();
    {
        let mut database = Database::open_with_cache_limit(&directory, 64 * 1024).unwrap();
        database
            .execute("CREATE TABLE docs (id BIGINT PRIMARY KEY, embedding VECTOR(2))")
            .unwrap();
        database
            .execute("INSERT INTO docs VALUES (1,'[1,0]'),(2,'[0,1]')")
            .unwrap();
        database.close().unwrap();
    }
    std::fs::write(directory.join("vectors.bin"), b"corrupt").unwrap();
    let mut reopened = Database::open_with_cache_limit(&directory, 64 * 1024).unwrap();
    let result = reopened
        .execute(
            "SELECT id, L2_DISTANCE(embedding,'[1,0]') AS d \
             FROM docs ORDER BY d LIMIT 1",
        )
        .unwrap();
    assert_eq!(result.rows[0][0], Value::BigInt(1));
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn truncate_is_durable_across_reopen() {
    let directory = unique_dir();
    {
        let mut database = Database::open(&directory).unwrap();
        database
            .execute("CREATE TABLE docs (id BIGINT PRIMARY KEY)")
            .unwrap();
        database
            .execute("INSERT INTO docs VALUES (1),(2),(3)")
            .unwrap();
        database.execute("TRUNCATE TABLE docs").unwrap();
        database.close().unwrap();
    }
    let mut reopened = Database::open(&directory).unwrap();
    assert!(
        reopened
            .execute("SELECT * FROM docs")
            .unwrap()
            .rows
            .is_empty()
    );
    std::fs::remove_dir_all(directory).unwrap();
}
