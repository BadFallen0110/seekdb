use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{DbError, ErrorCategory};
use crate::sql::{
    Comparison, Expression, Literal, QueryMode, SelectExpression, SelectStatement, Statement,
    parse,
};
use crate::table::{Table, TableSchema};
use crate::storage;
use crate::value::{ColumnType, Row, Value};
use crate::vector::parse_vector;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub affected: usize,
}

#[derive(Clone, Debug)]
pub struct Database {
    pub(crate) tables: BTreeMap<String, Table>,
    query_mode: QueryMode,
    transaction_backup: Option<BTreeMap<String, Table>>,
    data_directory: Option<PathBuf>,
}

impl Database {
    pub fn memory() -> Self {
        Self {
            tables: BTreeMap::new(),
            query_mode: QueryMode::Exact,
            transaction_backup: None,
            data_directory: None,
        }
    }

    pub fn open(data_directory: impl AsRef<Path>) -> Result<Self, DbError> {
        let data_directory = data_directory.as_ref().to_path_buf();
        storage::ensure_data_directory(&data_directory)?;
        let wal_path = data_directory.join("state.log");
        let tables = match storage::read_last_frame(&wal_path)? {
            Some(payload) => storage::decode_database(&payload)?,
            None => BTreeMap::new(),
        };
        Ok(Self {
            tables,
            query_mode: QueryMode::Exact,
            transaction_backup: None,
            data_directory: Some(data_directory),
        })
    }

    pub fn query_mode(&self) -> QueryMode {
        self.query_mode
    }

    pub fn set_query_mode(&mut self, mode: QueryMode) {
        self.query_mode = mode;
    }

    pub fn execute(&mut self, source: &str) -> Result<QueryResult, DbError> {
        let statement = parse(source)?;
        let persist_after = self.should_persist_after(&statement);
        let commit = matches!(&statement, Statement::Commit);
        let result = self.execute_statement(statement)?;
        if persist_after {
            self.checkpoint()?;
            if commit {
                self.transaction_backup = None;
            }
        }
        Ok(result)
    }

    fn execute_statement(&mut self, statement: Statement) -> Result<QueryResult, DbError> {
        match statement {
            Statement::CreateTable { name, columns } => {
                self.reject_ddl_in_transaction("CREATE TABLE")?;
                if self.table(&name).is_ok() {
                    return Err(DbError::new(
                        ErrorCategory::TableExists,
                        format!("table `{name}` already exists"),
                    ));
                }
                let schema = TableSchema::from_definitions(name.clone(), columns)?;
                self.tables.insert(name, Table::new(schema));
                Ok(QueryResult::default())
            }
            Statement::DropTable { name } => {
                self.reject_ddl_in_transaction("DROP TABLE")?;
                self.tables.remove(&name).ok_or_else(|| {
                    DbError::new(
                        ErrorCategory::TableNotFound,
                        format!("table `{name}` does not exist"),
                    )
                })?;
                Ok(QueryResult::default())
            }
            Statement::Insert {
                table,
                columns,
                rows,
            } => self.execute_insert(&table, columns, rows),
            Statement::Select(select) => self.execute_select(select),
            Statement::Update {
                table,
                assignments,
                filter,
            } => self.execute_update(&table, assignments, filter),
            Statement::Delete { table, filter } => self.execute_delete(&table, filter),
            Statement::CreateVectorIndex {
                name,
                table,
                column,
            } => {
                self.reject_ddl_in_transaction("CREATE VECTOR INDEX")?;
                let table = self.table_mut(&table)?;
                let column_index = table.schema.column_index(&column)?;
                if !matches!(
                    table.schema.columns[column_index].column_type,
                    ColumnType::Vector(_)
                ) {
                    return Err(DbError::new(
                        ErrorCategory::TypeMismatch,
                        format!("column `{column}` is not a VECTOR"),
                    ));
                }
                table.schema.vector_index_name = Some(name);
                Ok(QueryResult::default())
            }
            Statement::Begin => {
                if self.transaction_backup.is_some() {
                    return Err(DbError::new(
                        ErrorCategory::Transaction,
                        "nested transactions are not supported",
                    ));
                }
                self.transaction_backup = Some(self.tables.clone());
                Ok(QueryResult::default())
            }
            Statement::Commit => {
                self.transaction_backup.as_ref().ok_or_else(|| {
                    DbError::new(ErrorCategory::Transaction, "no transaction is active")
                })?;
                Ok(QueryResult::default())
            }
            Statement::Rollback => {
                let backup = self.transaction_backup.take().ok_or_else(|| {
                    DbError::new(ErrorCategory::Transaction, "no transaction is active")
                })?;
                self.tables = backup;
                Ok(QueryResult::default())
            }
            Statement::SetMode(mode) => {
                self.query_mode = mode;
                Ok(QueryResult::default())
            }
        }
    }

    fn execute_insert(
        &mut self,
        table_name: &str,
        columns: Option<Vec<String>>,
        rows: Vec<Vec<Literal>>,
    ) -> Result<QueryResult, DbError> {
        let table = self.table_mut(table_name)?;
        let prepared = if let Some(columns) = columns {
            let indexes = columns
                .iter()
                .map(|column| table.schema.column_index(column))
                .collect::<Result<Vec<_>, _>>()?;
            if indexes.len() != table.schema.columns.len() {
                return Err(DbError::new(
                    ErrorCategory::TypeMismatch,
                    "explicit INSERT column lists must include every column",
                ));
            }
            rows.into_iter()
                .map(|row| {
                    if row.len() != indexes.len() {
                        return Err(DbError::new(
                            ErrorCategory::TypeMismatch,
                            "INSERT row length does not match column list",
                        ));
                    }
                    let mut decoded = Vec::with_capacity(row.len());
                    for (value, index) in row.into_iter().zip(&indexes) {
                        decoded.push(table.schema.decode_literal(*index, value)?);
                    }
                    let mut ordered = vec![Value::BigInt(0); table.schema.columns.len()];
                    for (value, index) in decoded.into_iter().zip(indexes.iter().copied()) {
                        ordered[index] = value;
                    }
                    Ok(ordered)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            rows.into_iter()
                .map(|row| table.schema.decode_row(row))
                .collect::<Result<Vec<_>, _>>()?
        };
        let affected = table.insert_prepared(prepared)?;
        Ok(QueryResult {
            affected,
            ..QueryResult::default()
        })
    }

    fn execute_update(
        &mut self,
        table_name: &str,
        assignments: Vec<(String, Literal)>,
        filter: Option<Expression>,
    ) -> Result<QueryResult, DbError> {
        let table = self.table_mut(table_name)?;
        let mut changes = Vec::new();
        for (column, literal) in assignments {
            let index = table.schema.column_index(&column)?;
            if index == table.schema.primary_key_index {
                return Err(DbError::new(
                    ErrorCategory::Unsupported,
                    "primary key updates are not supported",
                ));
            }
            changes.push((index, table.schema.decode_literal(index, literal)?));
        }

        let mut keys = Vec::new();
        for (key, row) in &table.rows {
            if matches_filter(&table.schema, row, filter.as_ref())? {
                keys.push(*key);
            }
        }

        let mut replacements = Vec::with_capacity(keys.len());
        for key in &keys {
            let mut row = table.rows[key].clone();
            for (index, value) in &changes {
                row[*index] = value.clone();
            }
            replacements.push((*key, row));
        }
        for (key, row) in &replacements {
            table.rows.insert(*key, row.clone());
        }
        Ok(QueryResult {
            affected: replacements.len(),
            ..QueryResult::default()
        })
    }

    fn execute_delete(
        &mut self,
        table_name: &str,
        filter: Option<Expression>,
    ) -> Result<QueryResult, DbError> {
        let table = self.table_mut(table_name)?;
        let mut keys = Vec::new();
        for (key, row) in &table.rows {
            if matches_filter(&table.schema, row, filter.as_ref())? {
                keys.push(*key);
            }
        }
        for key in &keys {
            table.rows.remove(key);
        }
        Ok(QueryResult {
            affected: keys.len(),
            ..QueryResult::default()
        })
    }

    fn execute_select(&self, select: SelectStatement) -> Result<QueryResult, DbError> {
        let table = self.table(&select.table)?;
        let mut columns = Vec::new();
        let mut projections = Vec::new();
        for expression in &select.projections {
            match expression {
                SelectExpression::Wildcard => {
                    for (index, column) in table.schema.columns.iter().enumerate() {
                        columns.push(column.name.clone());
                        projections.push(Projection::Column(index));
                    }
                }
                SelectExpression::Column { name, alias } => {
                    let index = table.schema.column_index(name)?;
                    columns.push(alias.clone().unwrap_or_else(|| name.clone()));
                    projections.push(Projection::Column(index));
                }
                SelectExpression::L2Distance {
                    column,
                    vector_literal,
                    alias,
                } => {
                    let index = table.schema.column_index(column)?;
                    let dimensions = match table.schema.columns[index].column_type {
                        ColumnType::Vector(dimensions) => dimensions,
                        _ => {
                            return Err(DbError::new(
                                ErrorCategory::TypeMismatch,
                                format!("column `{column}` is not a VECTOR"),
                            ));
                        }
                    };
                    let query = parse_vector(vector_literal, dimensions)?;
                    columns.push(alias.clone().unwrap_or_else(|| format!("L2_DISTANCE({column})")));
                    projections.push(Projection::L2Distance { index, query });
                }
            }
        }

        let order_indices = select
            .order_by
            .iter()
            .map(|order| {
                columns
                    .iter()
                    .position(|column| column.eq_ignore_ascii_case(&order.column_or_alias))
                    .map(OrderTarget::Projection)
                    .or_else(|| {
                        table
                            .schema
                            .column_index(&order.column_or_alias)
                            .ok()
                            .map(OrderTarget::TableColumn)
                    })
                    .ok_or_else(|| {
                        DbError::new(
                            ErrorCategory::ColumnNotFound,
                            format!("ORDER BY column `{}` does not exist", order.column_or_alias),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let mut candidates = Vec::new();
        for (primary_key, row) in &table.rows {
            if !matches_filter(&table.schema, row, select.filter.as_ref())? {
                continue;
            }
            let mut output = Vec::with_capacity(projections.len());
            for projection in &projections {
                output.push(match projection {
                    Projection::Column(index) => row[*index].clone(),
                    Projection::L2Distance { index, query } => Value::Float(
                        crate::vector::l2_distance(row[*index].as_vector().unwrap(), query)?,
                    ),
                });
            }
            let mut sort_values = Vec::with_capacity(order_indices.len());
            for target in &order_indices {
                sort_values.push(match target {
                    OrderTarget::Projection(index) => output[*index].clone(),
                    OrderTarget::TableColumn(index) => row[*index].clone(),
                });
            }
            candidates.push(Candidate {
                primary_key: *primary_key,
                output,
                sort_values,
            });
        }

        if !select.order_by.is_empty() {
            candidates.sort_by(|left, right| {
                for ((left_value, right_value), order) in left
                    .sort_values
                    .iter()
                    .zip(&right.sort_values)
                    .zip(&select.order_by)
                {
                    let ordering = compare_values(left_value, right_value);
                    if ordering != Ordering::Equal {
                        return if order.ascending {
                            ordering
                        } else {
                            ordering.reverse()
                        };
                    }
                }
                left.primary_key.cmp(&right.primary_key)
            });
        }
        if let Some(limit) = select.limit {
            candidates.truncate(limit);
        }
        Ok(QueryResult {
            columns,
            rows: candidates
                .into_iter()
                .map(|candidate| candidate.output)
                .collect(),
            affected: 0,
        })
    }

    pub fn scalar_i64(&mut self, sql: &str) -> Result<i64, DbError> {
        let result = self.execute(sql)?;
        result
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(Value::as_bigint)
            .ok_or_else(|| {
                DbError::new(
                    ErrorCategory::TypeMismatch,
                    "query did not return an integer scalar",
                )
            })
    }

    fn table(&self, name: &str) -> Result<&Table, DbError> {
        self.tables.get(name).ok_or_else(|| {
            DbError::new(
                ErrorCategory::TableNotFound,
                format!("table `{name}` does not exist"),
            )
        })
    }

    fn table_mut(&mut self, name: &str) -> Result<&mut Table, DbError> {
        self.tables.get_mut(name).ok_or_else(|| {
            DbError::new(
                ErrorCategory::TableNotFound,
                format!("table `{name}` does not exist"),
            )
        })
    }

    fn reject_ddl_in_transaction(&self, operation: &str) -> Result<(), DbError> {
        if self.transaction_backup.is_some() {
            return Err(DbError::new(
                ErrorCategory::Transaction,
                format!("{operation} is not allowed inside an explicit transaction"),
            ));
        }
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<(), DbError> {
        let Some(directory) = &self.data_directory else {
            return Ok(());
        };
        let payload = storage::encode_database(&self.tables);
        storage::append_frame(&directory.join("state.log"), &payload)
    }

    pub fn close(&mut self) -> Result<(), DbError> {
        if let Some(backup) = self.transaction_backup.take() {
            self.tables = backup;
        }
        self.checkpoint()
    }

    fn should_persist_after(&self, statement: &Statement) -> bool {
        let in_transaction = self.transaction_backup.is_some();
        match statement {
            Statement::Insert { .. }
            | Statement::Update { .. }
            | Statement::Delete { .. }
            | Statement::CreateTable { .. }
            | Statement::DropTable { .. }
            | Statement::CreateVectorIndex { .. } => !in_transaction,
            Statement::Commit => in_transaction,
            Statement::Begin
            | Statement::Rollback
            | Statement::Select(_)
            | Statement::SetMode(_) => false,
        }
    }
}

enum Projection {
    Column(usize),
    L2Distance { index: usize, query: Vec<f32> },
}

enum OrderTarget {
    Projection(usize),
    TableColumn(usize),
}

struct Candidate {
    primary_key: i64,
    output: Row,
    sort_values: Row,
}

fn matches_filter(
    schema: &TableSchema,
    row: &Row,
    filter: Option<&Expression>,
) -> Result<bool, DbError> {
    let Some(filter) = filter else {
        return Ok(true);
    };
    match filter {
        Expression::And(left, right) => {
            Ok(matches_filter(schema, row, Some(left))?
                && matches_filter(schema, row, Some(right))?)
        }
        Expression::Compare {
            left,
            operator,
            right,
        } => {
            let index = schema.column_index(left)?;
            let expected = schema.decode_literal(index, right.clone())?;
            match (&expected, &row[index]) {
                (Value::BigInt(right), Value::BigInt(left)) => {
                    Ok(compare_ordering(left.cmp(right), *operator))
                }
                (Value::Varchar(right), Value::Varchar(left)) => {
                    Ok(compare_ordering(left.cmp(right), *operator))
                }
                (Value::Vector(_), Value::Vector(_)) => Err(DbError::new(
                    ErrorCategory::Unsupported,
                    "VECTOR comparison predicates are not supported",
                )),
                _ => Err(DbError::new(
                    ErrorCategory::TypeMismatch,
                    format!("predicate type mismatch for column `{left}`"),
                )),
            }
        }
        Expression::Column(_) | Expression::Literal(_) => Err(DbError::new(
            ErrorCategory::Unsupported,
            "predicate must be a comparison",
        )),
    }
}

fn compare_ordering(ordering: Ordering, operator: Comparison) -> bool {
    match operator {
        Comparison::Equal => ordering == Ordering::Equal,
        Comparison::Less => ordering == Ordering::Less,
        Comparison::LessOrEqual => ordering != Ordering::Greater,
        Comparison::Greater => ordering == Ordering::Greater,
        Comparison::GreaterOrEqual => ordering != Ordering::Less,
    }
}

fn compare_values(left: &Value, right: &Value) -> Ordering {
    match (left, right) {
        (Value::BigInt(left), Value::BigInt(right)) => left.cmp(right),
        (Value::Float(left), Value::Float(right)) => {
            left.partial_cmp(right).unwrap_or(Ordering::Equal)
        }
        (Value::Varchar(left), Value::Varchar(right)) => left.cmp(right),
        (Value::Vector(left), Value::Vector(right)) => left
            .first()
            .partial_cmp(&right.first())
            .unwrap_or(Ordering::Equal),
        _ => Ordering::Equal,
    }
}
