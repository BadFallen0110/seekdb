use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};

use crate::error::{DbError, ErrorCategory};
use crate::sql::{ColumnDefinition, Comparison, Expression, Literal};
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
    equality_index: Vec<HashMap<u64, Vec<i64>>>,
}

impl Table {
    pub fn new(schema: TableSchema) -> Self {
        Self {
            equality_index: (0..schema.columns.len()).map(|_| HashMap::new()).collect(),
            schema,
            rows: BTreeMap::new(),
        }
    }

    pub(crate) fn from_rows(schema: TableSchema, rows: BTreeMap<i64, Row>) -> Self {
        let mut table = Self {
            equality_index: (0..schema.columns.len()).map(|_| HashMap::new()).collect(),
            schema,
            rows: BTreeMap::new(),
        };
        for (key, row) in rows {
            table.index_row(key, &row);
            table.rows.insert(key, row);
        }
        table
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
        for (key, row) in &pending {
            self.index_row(*key, row);
        }
        self.rows.extend(pending);
        Ok(affected)
    }

    pub fn replace_row(&mut self, key: i64, row: Row) {
        if let Some(previous) = self.rows.remove(&key) {
            self.unindex_row(key, &previous);
        }
        self.index_row(key, &row);
        self.rows.insert(key, row);
    }

    pub fn remove_row(&mut self, key: i64) -> Option<Row> {
        let row = self.rows.remove(&key)?;
        self.unindex_row(key, &row);
        Some(row)
    }

    pub fn equality_candidates(
        &self,
        filter: Option<&Expression>,
    ) -> Result<Option<Vec<i64>>, DbError> {
        let Some(filter) = filter else {
            return Ok(None);
        };
        match filter {
            Expression::Compare {
                left,
                operator: Comparison::Equal,
                right,
            } => {
                let index = self.schema.column_index(left)?;
                if index == self.schema.primary_key_index {
                    let key = match self.schema.decode_literal(index, right.clone())? {
                        Value::BigInt(key) => key,
                        _ => unreachable!("primary key is BIGINT"),
                    };
                    return Ok(Some(
                        self.rows
                            .contains_key(&key)
                            .then_some(key)
                            .into_iter()
                            .collect(),
                    ));
                }
                if !matches!(
                    self.schema.columns[index].column_type,
                    ColumnType::Varchar(_)
                ) {
                    return Ok(None);
                }
                let hash = match self.schema.decode_literal(index, right.clone())? {
                    Value::Varchar(value) => hash_text(&value),
                    _ => return Ok(None),
                };
                Ok(Some(
                    self.equality_index[index]
                        .get(&hash)
                        .cloned()
                        .unwrap_or_default(),
                ))
            }
            Expression::And(left, right) => {
                let left = self.equality_candidates(Some(left))?;
                let right = self.equality_candidates(Some(right))?;
                match (left, right) {
                    (Some(left), Some(right)) => {
                        let right = right.into_iter().collect::<std::collections::HashSet<_>>();
                        Ok(Some(
                            left.into_iter().filter(|key| right.contains(key)).collect(),
                        ))
                    }
                    (Some(keys), None) | (None, Some(keys)) => Ok(Some(keys)),
                    (None, None) => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }

    fn index_row(&mut self, key: i64, row: &Row) {
        for (index, column) in self.schema.columns.iter().enumerate() {
            if matches!(column.column_type, ColumnType::Varchar(_))
                && let Some(text) = row[index].as_varchar()
            {
                self.equality_index[index]
                    .entry(hash_text(text))
                    .or_default()
                    .push(key);
            }
        }
    }

    fn unindex_row(&mut self, key: i64, row: &Row) {
        for (index, column) in self.schema.columns.iter().enumerate() {
            if matches!(column.column_type, ColumnType::Varchar(_))
                && let Some(text) = row[index].as_varchar()
            {
                let hash = hash_text(text);
                let mut remove_bucket = false;
                if let Some(keys) = self.equality_index[index].get_mut(&hash) {
                    if let Some(position) = keys.iter().position(|candidate| *candidate == key) {
                        keys.swap_remove(position);
                    }
                    remove_bucket = keys.is_empty();
                }
                if remove_bucket {
                    self.equality_index[index].remove(&hash);
                }
            }
        }
    }
}

fn hash_text(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
