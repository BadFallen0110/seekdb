use minivectordb::error::{DbError, ErrorCategory};

#[test]
fn duplicate_key_has_stable_category() {
    let error = DbError::duplicate_key("docs");
    assert_eq!(error.category(), ErrorCategory::DuplicateKey);
    assert!(error.to_string().contains("docs"));
}
