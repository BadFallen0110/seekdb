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
