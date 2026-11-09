use minivectordb::db::Database;
use minivectordb::error::ErrorCategory;
use minivectordb::value::Value;

fn sample_db() -> Database {
    let mut db = Database::memory();
    db.execute(
        "CREATE TABLE docs (\
           id BIGINT PRIMARY KEY,\
           category VARCHAR(16),\
           content VARCHAR(64),\
           embedding VECTOR(2)\
         )",
    )
    .unwrap();
    db.execute(
        "INSERT INTO docs VALUES \
         (1,'db','事务','[1,0]'),\
         (2,'travel','旅行','[0.9,0.1]'),\
         (3,'db','索引','[0,1]')",
    )
    .unwrap();
    db
}

#[test]
fn filtered_topk_filters_before_limit() {
    let mut db = sample_db();
    let result = db
        .execute(
            "SELECT id, L2_DISTANCE(embedding, '[1,0]') AS d \
             FROM docs WHERE category='db' ORDER BY d LIMIT 1",
        )
        .unwrap();
    assert_eq!(
        result.rows,
        vec![vec![Value::BigInt(1), Value::Float(0.0)]]
    );
}

#[test]
fn select_star_and_comparisons_are_deterministic() {
    let mut db = sample_db();
    let result = db
        .execute(
            "SELECT * FROM docs WHERE id >= 2 AND category = 'db' ORDER BY id",
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    assert_eq!(result.rows[0][0], Value::BigInt(3));
}

#[test]
fn update_and_delete_affect_existing_rows() {
    let mut db = sample_db();
    let updated = db
        .execute("UPDATE docs SET content='新内容' WHERE category='db'")
        .unwrap();
    assert_eq!(updated.affected, 2);
    let selected = db
        .execute("SELECT content FROM docs WHERE id=1")
        .unwrap();
    assert_eq!(
        selected.rows,
        vec![vec![Value::Varchar("新内容".to_owned())]]
    );

    let deleted = db.execute("DELETE FROM docs WHERE id=2").unwrap();
    assert_eq!(deleted.affected, 1);
    assert_eq!(db.execute("SELECT * FROM docs").unwrap().rows.len(), 2);
}

#[test]
fn invalid_vector_rejects_the_entire_batch() {
    let mut db = sample_db();
    let error = db
        .execute("INSERT INTO docs VALUES (4,'db','ok','[1,0]'),(5,'db','bad','[1]')")
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::TypeMismatch);
    assert_eq!(db.execute("SELECT * FROM docs").unwrap().rows.len(), 3);
}

#[test]
fn duplicate_primary_key_is_rejected() {
    let mut db = sample_db();
    let error = db
        .execute("INSERT INTO docs VALUES (1,'db','重复','[0,1]')")
        .unwrap_err();
    assert_eq!(error.category(), ErrorCategory::DuplicateKey);
}
