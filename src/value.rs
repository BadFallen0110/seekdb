use crate::error::{DbError, ErrorCategory};

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    BigInt(i64),
    Float(f64),
    Varchar(String),
    Vector(Vec<f32>),
    VectorRef(u64),
}

impl Value {
    pub fn varchar(value: impl Into<String>, max_chars: usize) -> Result<Self, DbError> {
        let value = value.into();
        let length = value.chars().count();
        if length > max_chars {
            return Err(DbError::new(
                ErrorCategory::Range,
                format!("VARCHAR value has {length} characters, exceeding limit {max_chars}"),
            ));
        }
        Ok(Self::Varchar(value))
    }

    pub fn vector(values: Vec<f32>, dimensions: usize) -> Result<Self, DbError> {
        if values.len() != dimensions {
            return Err(DbError::new(
                ErrorCategory::TypeMismatch,
                format!(
                    "VECTOR expects {dimensions} dimensions, received {}",
                    values.len()
                ),
            ));
        }
        if values.iter().any(|value| !value.is_finite()) {
            return Err(DbError::new(
                ErrorCategory::Range,
                "VECTOR values must be finite float32 numbers",
            ));
        }
        Ok(Self::Vector(values))
    }

    pub fn as_bigint(&self) -> Option<i64> {
        match self {
            Self::BigInt(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_varchar(&self) -> Option<&str> {
        match self {
            Self::Varchar(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_vector(&self) -> Option<&[f32]> {
        match self {
            Self::Vector(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_vector_ref(&self) -> Option<u64> {
        match self {
            Self::VectorRef(handle) => Some(*handle),
            _ => None,
        }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::Float(value) => Some(*value),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ColumnType {
    BigInt,
    Varchar(usize),
    Vector(usize),
}

pub type Row = Vec<Value>;
