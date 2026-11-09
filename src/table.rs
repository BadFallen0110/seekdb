use std::collections::BTreeMap;

use crate::error::{DbError, ErrorCategory};
use crate::sql::{ColumnDefinition, Literal};
use crate::value::{ColumnType, Row, Value};
use crate::vector::parse_vector;

#[derive(Clone, Debug)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnDefinition>,
    pub primary_key_index: usize,
    pub vector_index: Option<usize>,
    pub vector_index_name: Option<String>,
}

impl TableSchema {
    pub fn from_definitions(name: String, columns: Vec<ColumnDefinition>) -> Result<Self, DbError> {
        if columns.is_empty() {
            return Err(DbError::new(
                ErrorCategory::Syntax,
                "CREATE TABLE requires at least one column",
            ));
        }
        let mut primary_key = None;
        let mut vector_index = None;
        for (index, column) in columns.iter().enumerate() {
            if columns[..index]
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&column.name))
            {
                return Err(DbError::new(
                    ErrorCategory::Syntax,
                    format!("duplicate column `{}`", column.name),
                ));
            }
            if column.primary_key {
                if primary_key.replace(index).is_some() {
                    return Err(DbError::new(
                        ErrorCategory::Syntax,
                        "CREATE TABLE supports exactly one primary key",
                    ));
                }
                if column.column_type != ColumnType::BigInt {
                    return Err(DbError::new(
                        ErrorCategory::TypeMismatch,
                        "primary key must be BIGINT",
                    ));
                }
            }
            if matches!(column.column_type, ColumnType::Vector(_))
                && vector_index.replace(index).is_some()
            {
                return Err(DbError::new(
                    ErrorCategory::Unsupported,
                    "a table supports at most one VECTOR column",
                ));
            }
        }
        let primary_key_index = primary_key.ok_or_else(|| {
            DbError::new(
                ErrorCategory::Syntax,
                "CREATE TABLE requires one BIGINT PRIMARY KEY",
            )
        })?;
        Ok(Self {
            name,
            columns,
            primary_key_index,
            vector_index,
            vector_index_name: None,
        })
    }

    pub fn column_index(&self, name: &str) -> Result<usize, DbError> {
        self.columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                DbError::new(
                    ErrorCategory::ColumnNotFound,
                    format!("column `{name}` does not exist in table `{}`", self.name),
                )
            })
    }

    pub fn decode_row(&self, values: Vec<Literal>) -> Result<Row, DbError> {
        if values.len() != self.columns.len() {
            return Err(DbError::new(
                ErrorCategory::TypeMismatch,
                format!(
                    "table `{}` expects {} values, received {}",
                    self.name,
                    self.columns.len(),
                    values.len()
                ),
            ));
        }
        values
            .into_iter()
            .zip(&self.columns)
            .map(|(value, column)| decode_literal(value, &column.column_type, &self.name))
            .collect()
    }

    pub fn decode_literal(&self, column_index: usize, value: Literal) -> Result<Value, DbError> {
        decode_literal(value, &self.columns[column_index].column_type, &self.name)
    }
}

fn decode_literal(value: Literal, column_type: &ColumnType, table: &str) -> Result<Value, DbError> {
    match (value, column_type) {
        (Literal::BigInt(value), ColumnType::BigInt) => Ok(Value::BigInt(value)),
        (Literal::Varchar(value), ColumnType::Varchar(max_chars)) => {
            Value::varchar(value, *max_chars)
        }
        (Literal::Varchar(value), ColumnType::Vector(dimensions)) => {
            Value::vector(parse_vector(&value, *dimensions)?, *dimensions)
        }
        (_, expected) => Err(DbError::new(
            ErrorCategory::TypeMismatch,
            format!("value does not match column type {expected:?} in table `{table}`"),
        )),
    }
}

#[derive(Clone, Debug)]
pub struct Table {
    pub schema: TableSchema,
    pub rows: BTreeMap<i64, Row>,
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
        Self {
            schema,
            rows: BTreeMap::new(),
        }
    }

    pub fn primary_key(&self, row: &Row) -> i64 {
        row[self.schema.primary_key_index]
            .as_bigint()
            .expect("validated primary key")
    }

    pub fn insert_prepared(&mut self, rows: Vec<Row>) -> Result<usize, DbError> {
        let mut pending = BTreeMap::new();
        for row in rows {
            let key = self.primary_key(&row);
            if self.rows.contains_key(&key) || pending.contains_key(&key) {
                return Err(DbError::duplicate_key(&self.schema.name));
            }
            pending.insert(key, row);
        }
        let affected = pending.len();
        self.rows.extend(pending);
        Ok(affected)
    }
}
