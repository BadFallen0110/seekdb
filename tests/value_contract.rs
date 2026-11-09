use minivectordb::value::Value;
use minivectordb::vector::parse_vector;

#[test]
fn varchar_counts_unicode_scalars() {
    assert!(Value::varchar("数据库", 3).is_ok());
    assert!(Value::varchar("数据库", 2).is_err());
}

#[test]
fn vector_rejects_non_finite_and_overflow() {
    assert!(parse_vector("[1, NaN]", 2).is_err());
    assert!(parse_vector("[1e100, 0]", 2).is_err());
    assert_eq!(parse_vector("[0.1, 2]", 2).unwrap(), vec![0.1_f32, 2.0]);
}

#[test]
fn vector_l2_uses_float32_values() {
    let a = parse_vector("[0.1, 2]", 2).unwrap();
    let b = parse_vector("[0, 0]", 2).unwrap();
    let distance = minivectordb::vector::l2_squared(&a, &b).unwrap();
    let rebuilt = f64::from(0.1_f32).powi(2) + 4.0;
    assert!((distance - rebuilt).abs() < 1e-12);
}
