use minivectordb::error::ErrorCategory;
use minivectordb::sql::{Statement, parse};

#[test]
fn parses_sample_vector_query() {
    let statement = parse(
        "SELECT id, L2_DISTANCE(embedding, '[1,0]') AS d \
         FROM docs WHERE category = 'db' ORDER BY d LIMIT 2",
    )
    .unwrap();
    assert!(matches!(statement, Statement::Select(_)));
}

#[test]
fn rejects_null_in_values() {
    let error = parse("INSERT INTO t VALUES (1, NULL)").unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Unsupported);
}

#[test]
fn rejects_null_projection_as_unsupported() {
    let error = parse("SELECT NULL").unwrap_err();
    assert_eq!(error.category(), ErrorCategory::Unsupported);
}

#[test]
fn parses_multiline_create_and_batch_insert() {
    let create = parse(
        "CREATE TABLE docs (\n\
           id BIGINT PRIMARY KEY,\n\
           category VARCHAR(32),\n\
           embedding VECTOR(4)\n\
         )",
    )
    .unwrap();
    assert!(matches!(create, Statement::CreateTable { .. }));

    let insert = parse(
        "INSERT INTO docs VALUES (1, 'db', '[1,0,0,0]'), \
         (2, 'travel', '[0,0,0,1]')",
    )
    .unwrap();
    assert!(matches!(insert, Statement::Insert { .. }));
}
