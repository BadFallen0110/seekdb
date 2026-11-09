use minivectordb::db::Database;
use minivectordb::error::ErrorCategory;
use minivectordb::value::Value;

fn database() -> Database {
    let mut database = Database::memory();
    database
        .execute(
            "CREATE TABLE docs (\
               id BIGINT PRIMARY KEY,\
               category VARCHAR(16),\
               body VARCHAR(32),\
               embedding VECTOR(2)\
             )",
        )
        .unwrap();
    database
}

#[test]
fn all_comparison_operators_and_unicode_order_work() {
    let mut database = database();
    database
        .execute(
            "INSERT INTO docs (embedding,id,body,category) VALUES \
             ('[0,0]',1,'A','x'),\
             ('[0,0]',2,'中','x'),\
             ('[0,0]',3,'😀','x'),\
             ('[0,0]',4,'Z','x')",
        )
        .unwrap();
    let ids = |database: &mut Database, predicate: &str| {
        database
            .execute(&format!(
                "SELECT id FROM docs WHERE {predicate} ORDER BY body"
            ))
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&mut database, "id = 2"), vec![Value::BigInt(2)]);
    assert_eq!(ids(&mut database, "id < 2"), vec![Value::BigInt(1)]);
    assert_eq!(
        ids(&mut database, "id <= 2"),
        vec![Value::BigInt(1), Value::BigInt(2)]
    );
    assert_eq!(
        ids(&mut database, "id > 2"),
        vec![Value::BigInt(4), Value::BigInt(3)]
    );
    assert_eq!(
        ids(&mut database, "id >= 2"),
        vec![Value::BigInt(4), Value::BigInt(2), Value::BigInt(3)]
    );
    assert_eq!(
        ids(&mut database, "body > 'Z'"),
        vec![Value::BigInt(2), Value::BigInt(3)]
    );
}

#[test]
fn negative_keys_limit_zero_and_primary_key_update_are_explicit() {
    let mut database = database();
    database
        .execute("INSERT INTO docs VALUES (-2,'x','old','[0,0]')")
        .unwrap();
    assert!(
        database
            .execute("SELECT * FROM docs LIMIT 0")
            .unwrap()
            .rows
            .is_empty()
    );
    let error = database
        .execute("UPDATE docs SET id=1 WHERE id=-2")
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Unsupported);
    assert_eq!(
        database.execute("SELECT id FROM docs").unwrap().rows[0][0],
        Value::BigInt(-2)
    );
}

#[test]
fn vector_index_requires_a_vector_column() {
    let mut database = database();
    let error = database
        .execute("CREATE VECTOR INDEX bad ON docs(body)")
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::TypeMismatch);
    database
        .execute("CREATE VECTOR INDEX good ON docs(embedding)")
        .unwrap();
}

#[test]
fn distance_ties_use_primary_key_ascending() {
    let mut database = database();
    database
        .execute(
            "INSERT INTO docs VALUES \
             (9,'x','nine','[0,1]'),\
             (3,'x','three','[0,1]'),\
             (7,'x','seven','[0,1]')",
        )
        .unwrap();
    let rows = database
        .execute(
            "SELECT id, L2_DISTANCE(embedding,'[1,0]') AS d \
             FROM docs ORDER BY d LIMIT 2",
        )
        .unwrap()
        .rows;
    assert_eq!(rows[0][0], Value::BigInt(3));
    assert_eq!(rows[1][0], Value::BigInt(7));
}

#[test]
fn mysql_style_if_exists_setup_commands_are_idempotent() {
    let mut database = Database::memory();
    database.execute("DROP TABLE IF EXISTS docs").unwrap();
    database
        .execute("CREATE TABLE IF NOT EXISTS docs (id BIGINT PRIMARY KEY, body VARCHAR(8))")
        .unwrap();
    database
        .execute("CREATE TABLE IF NOT EXISTS docs (id BIGINT PRIMARY KEY, body VARCHAR(8))")
        .unwrap();
    database.execute("DROP TABLE IF EXISTS docs").unwrap();
    database.execute("DROP TABLE IF EXISTS docs").unwrap();
}

#[test]
fn common_not_null_column_markers_are_accepted() {
    let mut database = Database::memory();
    database
        .execute(
            "CREATE TABLE docs (\
               id BIGINT PRIMARY KEY NOT NULL,\
               body VARCHAR(8) NOT NULL\
             )",
        )
        .unwrap();
    database
        .execute("INSERT INTO docs VALUES (1,'ok')")
        .unwrap();
    assert_eq!(
        database.execute("SELECT body FROM docs").unwrap().rows,
        vec![vec![Value::Varchar("ok".to_owned())]]
    );
}

#[test]
fn common_mysql_table_options_are_accepted() {
    let mut database = Database::memory();
    database
        .execute(
            "CREATE TABLE docs (id BIGINT PRIMARY KEY, body VARCHAR(8)) \
             ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_bin",
        )
        .unwrap();
}

#[test]
fn truncate_table_preserves_schema_and_clears_rows() {
    let mut database = database();
    database
        .execute("INSERT INTO docs VALUES (1,'x','a','[0,0]')")
        .unwrap();
    database.execute("TRUNCATE TABLE docs").unwrap();
    assert!(
        database
            .execute("SELECT * FROM docs")
            .unwrap()
            .rows
            .is_empty()
    );
    database
        .execute("INSERT INTO docs VALUES (2,'x','b','[0,0]')")
        .unwrap();
}

#[test]
fn show_tables_returns_sorted_names() {
    let mut database = Database::memory();
    database
        .execute("CREATE TABLE z_docs (id BIGINT PRIMARY KEY)")
        .unwrap();
    database
        .execute("CREATE TABLE a_docs (id BIGINT PRIMARY KEY)")
        .unwrap();
    assert_eq!(
        database.execute("SHOW TABLES").unwrap().rows,
        vec![
            vec![Value::Varchar("a_docs".to_owned())],
            vec![Value::Varchar("z_docs".to_owned())]
        ]
    );
}

#[test]
fn common_mysqltest_session_setup_is_supported() {
    let mut database = Database::memory();
    database.execute("SET NAMES utf8mb4").unwrap();
    database.execute("SET @@session.sql_mode=''").unwrap();
    database.execute("SET SESSION time_zone='+00:00'").unwrap();
    database
        .execute("SET SESSION TRANSACTION ISOLATION LEVEL READ COMMITTED")
        .unwrap();
    database.execute("SET autocommit=1").unwrap();
    database.execute("USE test").unwrap();
    let error = database.execute("SET autocommit=0").unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Unsupported);
}

#[test]
fn commit_or_rollback_without_transaction_is_an_error() {
    let mut database = Database::memory();
    assert_eq!(
        database.execute("COMMIT").unwrap_err().category(),
        ErrorCategory::Transaction
    );
    assert_eq!(
        database.execute("ROLLBACK").unwrap_err().category(),
        ErrorCategory::Transaction
    );
    database.execute("START TRANSACTION").unwrap();
    database.execute("COMMIT WORK").unwrap();
}

#[test]
fn huge_limit_does_not_preallocate_from_the_requested_number() {
    let mut database = database();
    database
        .execute("INSERT INTO docs VALUES (1,'x','a','[0,0]'),(2,'x','b','[0,0]')")
        .unwrap();
    let result = database
        .execute(
            "SELECT id, L2_DISTANCE(embedding,'[0,0]') AS d \
             FROM docs ORDER BY d LIMIT 18446744073709551615",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 2);
}

#[test]
fn indexed_varchar_intersection_and_unfiltered_limit_are_exact() {
    let mut database = database();
    database
        .execute(
            "INSERT INTO docs VALUES \
             (1,'x','a','[0,0]'),\
             (2,'x','b','[0,0]'),\
             (3,'y','a','[0,0]')",
        )
        .unwrap();
    assert_eq!(
        database
            .execute("SELECT id FROM docs WHERE category='x' AND body='a'")
            .unwrap()
            .rows,
        vec![vec![Value::BigInt(1)]]
    );
    assert_eq!(
        database
            .execute("SELECT id FROM docs LIMIT 100")
            .unwrap()
            .rows,
        vec![
            vec![Value::BigInt(1)],
            vec![Value::BigInt(2)],
            vec![Value::BigInt(3)]
        ]
    );
}

#[test]
fn failed_vector_update_and_bad_query_dimension_leave_data_unchanged() {
    let mut database = database();
    database
        .execute("INSERT INTO docs VALUES (1,'x','a','[1,0]')")
        .unwrap();
    assert_eq!(
        database
            .execute("UPDATE docs SET embedding='[1]' WHERE id=1")
            .unwrap_err()
            .category(),
        ErrorCategory::TypeMismatch
    );
    assert_eq!(
        database
            .execute("SELECT id, L2_DISTANCE(embedding,'[1]') AS d FROM docs")
            .unwrap_err()
            .category(),
        ErrorCategory::TypeMismatch
    );
    assert_eq!(
        database
            .execute("SELECT id, L2_DISTANCE(embedding,'[1,0]') AS d FROM docs")
            .unwrap()
            .rows[0][0],
        Value::BigInt(1)
    );
}

#[test]
fn parallel_dense_topk_preserves_primary_key_tie_order() {
    let mut database = database();
    let values = (1..=5_000)
        .map(|id| format!("({id},'x','v{id}','[0,1]')"))
        .collect::<Vec<_>>()
        .join(",");
    database
        .execute(&format!("INSERT INTO docs VALUES {values}"))
        .unwrap();
    let rows = database
        .execute(
            "SELECT id, L2_DISTANCE(embedding,'[1,0]') AS d \
             FROM docs ORDER BY d LIMIT 10",
        )
        .unwrap()
        .rows;
    assert_eq!(
        rows.into_iter()
            .map(|row| row[0].clone())
            .collect::<Vec<_>>(),
        (1..=10).map(Value::BigInt).collect::<Vec<_>>()
    );
}

#[test]
fn bigint_boundaries_and_literal_overflow_are_handled() {
    let mut database = database();
    database
        .execute(
            "INSERT INTO docs VALUES \
             (-9223372036854775808,'x','min','[0,0]'),\
             (9223372036854775807,'x','max','[0,0]')",
        )
        .unwrap();
    assert_eq!(
        database
            .execute("SELECT id FROM docs ORDER BY id")
            .unwrap()
            .rows,
        vec![vec![Value::BigInt(i64::MIN)], vec![Value::BigInt(i64::MAX)]]
    );
    let error = database
        .execute("INSERT INTO docs VALUES (9223372036854775808,'x','overflow','[0,0]')")
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Syntax);
}
