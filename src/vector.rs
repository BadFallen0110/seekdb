use crate::error::{DbError, ErrorCategory};
use crate::value::Value;

pub fn parse_vector(source: &str, dimensions: usize) -> Result<Vec<f32>, DbError> {
    let trimmed = source.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') {
        return Err(DbError::new(
            ErrorCategory::Syntax,
            "vector literal must be enclosed in brackets",
        ));
    }
    let body = &trimmed[1..trimmed.len() - 1];
    let values = if body.trim().is_empty() {
        Vec::new()
    } else {
        body.split(',')
            .map(|part| {
                let part = part.trim();
                let value = part.parse::<f32>().map_err(|_| {
                    DbError::new(
                        ErrorCategory::TypeMismatch,
                        format!("invalid VECTOR number `{part}`"),
                    )
                })?;
                if !value.is_finite() {
                    return Err(DbError::new(
                        ErrorCategory::Range,
                        "VECTOR values must be finite float32 numbers",
                    ));
                }
                Ok(value)
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    match Value::vector(values, dimensions)? {
        Value::Vector(values) => Ok(values),
        _ => unreachable!(),
    }
}

pub fn l2_squared(left: &[f32], right: &[f32]) -> Result<f64, DbError> {
    if left.len() != right.len() {
        return Err(DbError::new(
            ErrorCategory::TypeMismatch,
            format!(
                "VECTOR dimension mismatch: {} and {}",
                left.len(),
                right.len()
            ),
        ));
    }

    let sum = left
        .iter()
        .zip(right)
        .map(|(left, right)| {
            let difference = f64::from(*left) - f64::from(*right);
            difference * difference
        })
        .sum::<f64>();
    Ok(sum)
}

pub fn l2_distance(left: &[f32], right: &[f32]) -> Result<f64, DbError> {
    Ok(l2_squared(left, right)?.sqrt())
}
