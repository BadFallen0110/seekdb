use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{DbError, ErrorCategory};
use crate::sql::{
    Comparison, Expression, Literal, OrderExpressionKind, QueryMode, SelectExpression,
    SelectStatement, Statement, parse,
};
use crate::storage;
use crate::table::{Table, TableSchema};
use crate::value::{ColumnType, Row, Value};
use crate::vector::parse_vector;
use crate::vector_store::VectorStore;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Row>,
    pub affected: usize,
}

pub struct Database {
    pub(crate) tables: BTreeMap<String, Table>,
    query_mode: QueryMode,
    transaction_backup: Option<BTreeMap<String, Table>>,
    data_directory: Option<PathBuf>,
    pending_mutations: Vec<storage::Mutation>,
    vectors: VectorStore,
    dense_vectors: Option<DenseVectorCache>,
    dense_cache_budget: usize,
}

impl Database {
    pub fn memory() -> Self {
        Self {
            tables: BTreeMap::new(),
            query_mode: QueryMode::Exact,
            transaction_backup: None,
            data_directory: None,
            pending_mutations: Vec::new(),
            vectors: VectorStore::memory(),
            dense_vectors: None,
            dense_cache_budget: 64 * 1024 * 1024,
        }
    }

    pub fn open(data_directory: impl AsRef<Path>) -> Result<Self, DbError> {
        let cache_limit = std::env::var("MINIVECTORDB_CACHE_BYTES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(64 * 1024 * 1024);
        Self::open_with_cache_limit(data_directory, cache_limit)
    }

    pub fn open_with_cache_limit(
        data_directory: impl AsRef<Path>,
        cache_limit: usize,
    ) -> Result<Self, DbError> {
        let data_directory = data_directory.as_ref().to_path_buf();
        storage::ensure_data_directory(&data_directory)?;
        let wal_path = data_directory.join("state.log");
        let vector_path = data_directory.join("vectors.bin");
        let rebuild_path = data_directory.join("vectors.bin.tmp");
        if rebuild_path.exists() {
            std::fs::remove_file(&rebuild_path)?;
        }
        let mut rebuilding_vectors = VectorStore::open(&rebuild_path, cache_limit)?;
        let tables = storage::load_database(&wal_path, &mut rebuilding_vectors)?;
        drop(rebuilding_vectors);
        if vector_path.exists() {
            std::fs::remove_file(&vector_path)?;
        }
        std::fs::rename(&rebuild_path, &vector_path)?;
        let vectors = VectorStore::open(&vector_path, cache_limit)?;
        Ok(Self {
            tables,
            query_mode: QueryMode::Exact,
            transaction_backup: None,
            data_directory: Some(data_directory),
            pending_mutations: Vec::new(),
            vectors,
            dense_vectors: None,
            dense_cache_budget: cache_limit / 2,
        })
    }

    pub fn query_mode(&self) -> QueryMode {
        self.query_mode
    }

    pub fn set_query_mode(&mut self, mode: QueryMode) {
        self.query_mode = mode;
    }

    pub fn vector_cache_bytes(&self) -> usize {
        self.vectors.cache_bytes()
            + self
                .dense_vectors
                .as_ref()
                .map(|cache| std::mem::size_of_val(cache.data.as_slice()))
                .unwrap_or(0)
    }

    pub fn execute(&mut self, source: &str) -> Result<QueryResult, DbError> {
        let statement = parse(source)?;
        let persist_after = self.should_persist_after(&statement);
        let commit = matches!(&statement, Statement::Commit);
        let result = self.execute_statement(statement)?;
        if persist_after {
            self.persist_pending()?;
            if commit {
                self.transaction_backup = None;
            }
        }
        Ok(result)
    }

    fn execute_statement(&mut self, statement: Statement) -> Result<QueryResult, DbError> {
        match statement {
            Statement::CreateTable {
                name,
                columns,
                if_not_exists,
            } => {
                self.reject_ddl_in_transaction("CREATE TABLE")?;
                if self.table(&name).is_ok() {
                    if if_not_exists {
                        return Ok(QueryResult::default());
                    }
                    return Err(DbError::new(
                        ErrorCategory::TableExists,
                        format!("table `{name}` already exists"),
                    ));
                }
                let schema = TableSchema::from_definitions(name.clone(), columns)?;
                self.tables.insert(name, Table::new(schema.clone()));
                self.dense_vectors = None;
                self.pending_mutations
                    .push(storage::Mutation::CreateTable(schema));
                Ok(QueryResult::default())
            }
            Statement::DropTable { name, if_exists } => {
                self.reject_ddl_in_transaction("DROP TABLE")?;
                let Some(_) = self.tables.remove(&name) else {
                    if if_exists {
                        return Ok(QueryResult::default());
                    }
                    return Err(DbError::new(
                        ErrorCategory::TableNotFound,
                        format!("table `{name}` does not exist"),
                    ));
                };
                self.dense_vectors = None;
                self.pending_mutations
                    .push(storage::Mutation::DropTable(name));
                Ok(QueryResult::default())
            }
            Statement::TruncateTable { name } => {
                self.reject_ddl_in_transaction("TRUNCATE TABLE")?;
                let keys = self.table(&name)?.rows.keys().copied().collect::<Vec<_>>();
                let table = self.table_mut(&name)?;
                for key in &keys {
                    table.remove_row(*key);
                }
                if !keys.is_empty() {
                    self.pending_mutations
                        .push(storage::Mutation::Delete { table: name, keys });
                }
                Ok(QueryResult::default())
            }
            Statement::ShowTables => Ok(QueryResult {
                columns: vec!["Tables".to_owned()],
                rows: self
                    .tables
                    .keys()
                    .map(|name| vec![Value::Varchar(name.clone())])
                    .collect(),
                affected: 0,
            }),
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
                table.schema.vector_index_name = Some(name.clone());
                let mutation = storage::Mutation::CreateVectorIndex {
                    table: table.schema.name.clone(),
                    name,
                    column,
                };
                self.pending_mutations.push(mutation);
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
                self.pending_mutations.clear();
                Ok(QueryResult::default())
            }
            Statement::SetMode(mode) => {
                self.query_mode = mode;
                Ok(QueryResult::default())
            }
            Statement::Noop => Ok(QueryResult::default()),
        }
    }

    fn execute_insert(
        &mut self,
        table_name: &str,
        columns: Option<Vec<String>>,
        rows: Vec<Vec<Literal>>,
    ) -> Result<QueryResult, DbError> {
        let schema = self.table(table_name)?.schema.clone();
        let prepared = if let Some(columns) = columns {
            let unique_columns = columns
                .iter()
                .map(|column| column.to_ascii_lowercase())
                .collect::<std::collections::BTreeSet<_>>();
            if unique_columns.len() != columns.len() {
                return Err(DbError::new(
                    ErrorCategory::Syntax,
                    "INSERT column list contains duplicate columns",
                ));
            }
            let indexes = columns
                .iter()
                .map(|column| schema.column_index(column))
                .collect::<Result<Vec<_>, _>>()?;
            if indexes.len() != schema.columns.len() {
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
                        decoded.push(schema.decode_literal(*index, value)?);
                    }
                    let mut ordered = vec![Value::BigInt(0); schema.columns.len()];
                    for (value, index) in decoded.into_iter().zip(indexes.iter().copied()) {
                        ordered[index] = value;
                    }
                    Ok(ordered)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            rows.into_iter()
                .map(|row| schema.decode_row(row))
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut prepared = prepared;
        let mutation_rows = prepared
            .iter()
            .map(|row| {
                (
                    row[schema.primary_key_index]
                        .as_bigint()
                        .expect("validated primary key"),
                    row.clone(),
                )
            })
            .collect::<Vec<_>>();
        for row in &mut prepared {
            self.materialize_row_for_storage(row)?;
        }
        let table = self.table_mut(table_name)?;
        let affected = table.insert_prepared(prepared)?;
        self.pending_mutations.push(storage::Mutation::Upsert {
            table: table_name.to_owned(),
            rows: mutation_rows,
        });
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
        let schema = self.table(table_name)?.schema.clone();
        let mut changes = Vec::new();
        for (column, literal) in assignments {
            let index = schema.column_index(&column)?;
            if index == schema.primary_key_index {
                return Err(DbError::new(
                    ErrorCategory::Unsupported,
                    "primary key updates are not supported",
                ));
            }
            changes.push((index, schema.decode_literal(index, literal)?));
        }

        let keys = matching_keys(self.table(table_name)?, filter.as_ref())?;

        let mut replacements = Vec::with_capacity(keys.len());
        let mut persisted_replacements = Vec::with_capacity(keys.len());
        for key in &keys {
            let mut row = self.table(table_name)?.rows[key].clone();
            for (index, value) in &changes {
                row[*index] = value.clone();
            }
            persisted_replacements.push((*key, self.hydrate_row(&row)?));
            self.materialize_row_for_storage(&mut row)?;
            replacements.push((*key, row));
        }
        {
            let table = self.table_mut(table_name)?;
            for (key, row) in &replacements {
                table.replace_row(*key, row.clone());
            }
        }
        if !replacements.is_empty() {
            self.pending_mutations.push(storage::Mutation::Upsert {
                table: table_name.to_owned(),
                rows: persisted_replacements,
            });
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
        let keys = matching_keys(self.table(table_name)?, filter.as_ref())?;
        let table = self.table_mut(table_name)?;
        for key in &keys {
            table.remove_row(*key);
        }
        if !keys.is_empty() {
            self.pending_mutations.push(storage::Mutation::Delete {
                table: table_name.to_owned(),
                keys: keys.clone(),
            });
        }
        Ok(QueryResult {
            affected: keys.len(),
            ..QueryResult::default()
        })
    }

    fn execute_select(&mut self, select: SelectStatement) -> Result<QueryResult, DbError> {
        if select.table.is_none() {
            return execute_constant_select(select);
        }
        let table_name = select.table.as_deref().expect("checked table source");
        let table = self.table(table_name)?;
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
                SelectExpression::Literal { value, alias } => {
                    let value = literal_value(value);
                    columns.push(alias.clone().unwrap_or_else(|| value_column_name(&value)));
                    projections.push(Projection::Literal(value));
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
                    columns.push(
                        alias
                            .clone()
                            .unwrap_or_else(|| format!("L2_DISTANCE({column})")),
                    );
                    projections.push(Projection::L2Distance { index, query });
                }
            }
        }

        let order_indices = select
            .order_by
            .iter()
            .map(|order| match &order.expression {
                OrderExpressionKind::Named(name) => columns
                    .iter()
                    .position(|column| column.eq_ignore_ascii_case(name))
                    .map(OrderTarget::Projection)
                    .or_else(|| {
                        table
                            .schema
                            .column_index(name)
                            .ok()
                            .map(OrderTarget::TableColumn)
                    })
                    .ok_or_else(|| {
                        DbError::new(
                            ErrorCategory::ColumnNotFound,
                            format!("ORDER BY column `{name}` does not exist"),
                        )
                    }),
                OrderExpressionKind::L2Distance {
                    column,
                    vector_literal,
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
                    Ok(OrderTarget::L2Distance {
                        index,
                        query: parse_vector(vector_literal, dimensions)?,
                    })
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        let specialized_topk =
            if let ([order], Some(limit)) = (select.order_by.as_slice(), select.limit) {
                if order.ascending {
                    match &order_indices[0] {
                        OrderTarget::Projection(index) => match &projections[*index] {
                            Projection::L2Distance {
                                index: vector_index,
                                query,
                            } => Some((limit, *vector_index, query.clone())),
                            Projection::Column(_) | Projection::Literal(_) => None,
                        },
                        OrderTarget::TableColumn(_) => None,
                        OrderTarget::L2Distance { index, query } => {
                            Some((limit, *index, query.clone()))
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            };

        let dense_ready = if let Some((_, vector_index, _)) = &specialized_topk {
            self.ensure_dense_vectors(table_name, *vector_index)?
        } else {
            false
        };
        let (tables, vectors, dense_vectors) =
            (&self.tables, &mut self.vectors, self.dense_vectors.as_ref());
        let table = tables.get(table_name).expect("table checked above");

        if let Some((limit, vector_index, query)) = specialized_topk {
            let mut topk = crate::vector::TopKAccumulator::new(limit);
            if dense_ready {
                let dense = dense_vectors.expect("dense cache was just built");
                for (distance, primary_key) in
                    dense_topk(dense, table, select.filter.as_ref(), &query, limit)?
                {
                    topk.push(distance, primary_key);
                }
            } else {
                for_each_matching_row(table, select.filter.as_ref(), |primary_key, row| {
                    let distance = match &row[vector_index] {
                        Value::Vector(values) => crate::vector::l2_distance(values, &query)?,
                        Value::VectorRef(handle) => vectors.l2_distance(*handle, &query)?,
                        _ => {
                            return Err(DbError::new(
                                ErrorCategory::TypeMismatch,
                                "expected VECTOR value",
                            ));
                        }
                    };
                    topk.push(distance, primary_key);
                    Ok(())
                })?;
            }
            let mut rows = Vec::with_capacity(limit.min(table.rows.len()));
            for (distance, primary_key) in topk.finish() {
                let row = &table.rows[&primary_key];
                rows.push(project_row_with_ranked_distance(
                    row,
                    &projections,
                    vectors,
                    Some((vector_index, &query, distance)),
                )?);
            }
            return Ok(QueryResult {
                columns,
                rows,
                affected: 0,
            });
        }

        let mut candidates = Vec::new();
        for_each_matching_row(table, select.filter.as_ref(), |primary_key, row| {
            let output = project_row(row, &projections, vectors)?;
            let mut sort_values = Vec::with_capacity(order_indices.len());
            for target in &order_indices {
                sort_values.push(match target {
                    OrderTarget::Projection(index) => output[*index].clone(),
                    OrderTarget::TableColumn(index) => row[*index].clone(),
                    OrderTarget::L2Distance { index, query } => {
                        Value::Float(distance_for_value(vectors, &row[*index], query)?)
                    }
                });
            }
            candidates.push(Candidate {
                primary_key,
                output,
                sort_values,
            });
            Ok(())
        })?;

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
        } else {
            candidates.sort_by_key(|candidate| candidate.primary_key);
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

    pub fn close(&mut self) -> Result<(), DbError> {
        if let Some(backup) = self.transaction_backup.take() {
            self.tables = backup;
            self.pending_mutations.clear();
        }
        self.persist_pending()
    }

    fn ensure_dense_vectors(&mut self, table_name: &str, column: usize) -> Result<bool, DbError> {
        let (generation, dimensions, row_count) = {
            let table = self.table(table_name)?;
            let dimensions = match table.schema.columns[column].column_type {
                ColumnType::Vector(dimensions) => dimensions,
                _ => {
                    return Err(DbError::new(
                        ErrorCategory::TypeMismatch,
                        "dense vector cache requires a VECTOR column",
                    ));
                }
            };
            (table.generation(), dimensions, table.rows.len())
        };
        if self.dense_vectors.as_ref().is_some_and(|cache| {
            cache.table == table_name && cache.column == column && cache.generation == generation
        }) {
            return Ok(true);
        }
        let bytes = row_count
            .checked_mul(dimensions)
            .and_then(|values| values.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| DbError::new(ErrorCategory::Range, "dense VECTOR cache overflow"))?;
        if bytes > self.dense_cache_budget {
            self.dense_vectors = None;
            return Ok(false);
        }

        let (tables, vectors) = (&self.tables, &mut self.vectors);
        let table = tables.get(table_name).ok_or_else(|| {
            DbError::new(
                ErrorCategory::TableNotFound,
                format!("table `{table_name}` does not exist"),
            )
        })?;
        let mut ids = Vec::with_capacity(row_count);
        let mut data = Vec::with_capacity(row_count * dimensions);
        let mut scratch = Vec::with_capacity(dimensions);
        for (id, row) in &table.rows {
            ids.push(*id);
            match &row[column] {
                Value::Vector(values) => data.extend_from_slice(values),
                Value::VectorRef(handle) => {
                    vectors.read_into(*handle, &mut scratch)?;
                    data.extend_from_slice(&scratch);
                }
                _ => {
                    return Err(DbError::new(
                        ErrorCategory::TypeMismatch,
                        "expected VECTOR value",
                    ));
                }
            }
        }
        self.vectors.clear_value_cache();
        self.dense_vectors = Some(DenseVectorCache {
            table: table_name.to_owned(),
            column,
            generation,
            dimensions,
            ids,
            data,
        });
        Ok(true)
    }

    fn should_persist_after(&self, statement: &Statement) -> bool {
        let in_transaction = self.transaction_backup.is_some();
        match statement {
            Statement::Insert { .. }
            | Statement::Update { .. }
            | Statement::Delete { .. }
            | Statement::CreateTable { .. }
            | Statement::DropTable { .. }
            | Statement::TruncateTable { .. }
            | Statement::CreateVectorIndex { .. } => !in_transaction,
            Statement::Commit => in_transaction,
            Statement::Begin
            | Statement::Rollback
            | Statement::Select(_)
            | Statement::ShowTables
            | Statement::SetMode(_)
            | Statement::Noop => false,
        }
    }

    fn persist_pending(&mut self) -> Result<(), DbError> {
        if self.pending_mutations.is_empty() {
            return Ok(());
        }
        let Some(directory) = &self.data_directory else {
            self.pending_mutations.clear();
            return Ok(());
        };
        let payload = storage::encode_mutations(&self.pending_mutations);
        storage::append_frame(&directory.join("state.log"), &payload)?;
        self.pending_mutations.clear();
        Ok(())
    }

    fn materialize_row_for_storage(&mut self, row: &mut Row) -> Result<(), DbError> {
        for value in row {
            if let Value::Vector(values) = value {
                *value = Value::VectorRef(self.vectors.append(values)?);
            }
        }
        Ok(())
    }

    fn hydrate_row(&mut self, row: &Row) -> Result<Row, DbError> {
        row.iter()
            .map(|value| match value {
                Value::VectorRef(handle) => Ok(Value::Vector(self.vectors.read(*handle)?)),
                other => Ok(other.clone()),
            })
            .collect()
    }
}

struct DenseVectorCache {
    table: String,
    column: usize,
    generation: u64,
    dimensions: usize,
    ids: Vec<i64>,
    data: Vec<f32>,
}

enum Projection {
    Column(usize),
    Literal(Value),
    L2Distance { index: usize, query: Vec<f32> },
}

enum OrderTarget {
    Projection(usize),
    TableColumn(usize),
    L2Distance { index: usize, query: Vec<f32> },
}

struct Candidate {
    primary_key: i64,
    output: Row,
    sort_values: Row,
}

fn matching_keys(table: &Table, filter: Option<&Expression>) -> Result<Vec<i64>, DbError> {
    let mut keys = Vec::new();
    if let Some(candidates) = table.equality_candidates(filter)? {
        for key in candidates {
            if let Some(row) = table.rows.get(&key)
                && matches_filter(&table.schema, row, filter)?
            {
                keys.push(key);
            }
        }
    } else {
        for (key, row) in &table.rows {
            if matches_filter(&table.schema, row, filter)? {
                keys.push(*key);
            }
        }
    }
    Ok(keys)
}

fn for_each_matching_row(
    table: &Table,
    filter: Option<&Expression>,
    mut callback: impl FnMut(i64, &Row) -> Result<(), DbError>,
) -> Result<(), DbError> {
    if let Some(candidates) = table.equality_candidates(filter)? {
        for key in candidates {
            let Some(row) = table.rows.get(&key) else {
                continue;
            };
            if matches_filter(&table.schema, row, filter)? {
                callback(key, row)?;
            }
        }
    } else {
        for (key, row) in &table.rows {
            if matches_filter(&table.schema, row, filter)? {
                callback(*key, row)?;
            }
        }
    }
    Ok(())
}

fn dense_topk(
    dense: &DenseVectorCache,
    table: &Table,
    filter: Option<&Expression>,
    query: &[f32],
    limit: usize,
) -> Result<Vec<(f64, i64)>, DbError> {
    if limit == 0 || dense.ids.is_empty() {
        return Ok(Vec::new());
    }
    let allowed = table
        .equality_candidates(filter)?
        .map(|keys| keys.into_iter().collect::<HashSet<_>>());
    let parallelism = dense_parallelism();
    let thread_count = (dense.ids.len() / 2_048).min(parallelism).max(1);
    if thread_count == 1 {
        return scan_dense_range(
            dense,
            table,
            filter,
            allowed.as_ref(),
            query,
            limit,
            0,
            dense.ids.len(),
        );
    }
    let chunk_size = dense.ids.len().div_ceil(thread_count);
    let partials = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(thread_count);
        for thread_index in 0..thread_count {
            let start = thread_index * chunk_size;
            let end = (start + chunk_size).min(dense.ids.len());
            if start >= end {
                break;
            }
            let allowed = allowed.as_ref();
            handles.push(scope.spawn(move || {
                scan_dense_range(dense, table, filter, allowed, query, limit, start, end)
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle.join().map_err(|_| {
                    DbError::new(ErrorCategory::Corruption, "vector query worker panicked")
                })?
            })
            .collect::<Result<Vec<_>, DbError>>()
    })?;

    let mut merged = crate::vector::TopKAccumulator::new(limit);
    for partial in partials {
        for (distance, primary_key) in partial {
            merged.push(distance, primary_key);
        }
    }
    Ok(merged.finish())
}

#[allow(clippy::too_many_arguments)]
fn scan_dense_range(
    dense: &DenseVectorCache,
    table: &Table,
    filter: Option<&Expression>,
    allowed: Option<&HashSet<i64>>,
    query: &[f32],
    limit: usize,
    start: usize,
    end: usize,
) -> Result<Vec<(f64, i64)>, DbError> {
    let mut topk = crate::vector::TopKAccumulator::new(limit);
    for (row_index, primary_key) in dense.ids.iter().enumerate().take(end).skip(start) {
        if allowed.is_some_and(|allowed| !allowed.contains(primary_key)) {
            continue;
        }
        if filter.is_some() {
            let row = &table.rows[primary_key];
            if !matches_filter(&table.schema, row, filter)? {
                continue;
            }
        }
        let vector_start = row_index * dense.dimensions;
        let distance = crate::vector::l2_distance(
            &dense.data[vector_start..vector_start + dense.dimensions],
            query,
        )?;
        topk.push(distance, *primary_key);
    }
    Ok(topk.finish())
}

fn dense_parallelism() -> usize {
    static PARALLELISM: OnceLock<usize> = OnceLock::new();
    *PARALLELISM.get_or_init(|| {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
            .min(2)
    })
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
                (Value::Vector(_), Value::Vector(_) | Value::VectorRef(_))
                | (Value::VectorRef(_), Value::Vector(_) | Value::VectorRef(_)) => {
                    Err(DbError::new(
                        ErrorCategory::Unsupported,
                        "VECTOR comparison predicates are not supported",
                    ))
                }
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
        (Value::VectorRef(left), Value::VectorRef(right)) => left.cmp(right),
        _ => Ordering::Equal,
    }
}

fn project_row(
    row: &Row,
    projections: &[Projection],
    vectors: &mut VectorStore,
) -> Result<Row, DbError> {
    project_row_with_ranked_distance(row, projections, vectors, None)
}

fn project_row_with_ranked_distance(
    row: &Row,
    projections: &[Projection],
    vectors: &mut VectorStore,
    ranked_distance: Option<(usize, &[f32], f64)>,
) -> Result<Row, DbError> {
    let mut output = Vec::with_capacity(projections.len());
    for projection in projections {
        output.push(match projection {
            Projection::Column(index) => match &row[*index] {
                Value::VectorRef(handle) => Value::Vector(vectors.read(*handle)?),
                other => other.clone(),
            },
            Projection::Literal(value) => value.clone(),
            Projection::L2Distance { index, query } => {
                if let Some((ranked_index, ranked_query, distance)) = ranked_distance
                    && ranked_index == *index
                    && ranked_query == query.as_slice()
                {
                    Value::Float(distance)
                } else {
                    Value::Float(crate::vector::l2_distance(
                        &read_vector_value(vectors, &row[*index])?,
                        query,
                    )?)
                }
            }
        });
    }
    Ok(output)
}

fn execute_constant_select(select: SelectStatement) -> Result<QueryResult, DbError> {
    if let Some(filter) = &select.filter {
        let _ = filter;
        return Err(DbError::new(
            ErrorCategory::Unsupported,
            "WHERE requires a FROM table",
        ));
    }
    let mut columns = Vec::new();
    let mut row = Vec::new();
    for projection in select.projections {
        match projection {
            SelectExpression::Wildcard => {
                return Err(DbError::new(
                    ErrorCategory::Unsupported,
                    "SELECT * requires a FROM table",
                ));
            }
            SelectExpression::Column { name, .. } => {
                return Err(DbError::new(
                    ErrorCategory::ColumnNotFound,
                    format!("column `{name}` requires a FROM table"),
                ));
            }
            SelectExpression::Literal { value, alias } => {
                let value = literal_value(&value);
                columns.push(alias.unwrap_or_else(|| value_column_name(&value)));
                row.push(value);
            }
            SelectExpression::L2Distance { .. } => {
                return Err(DbError::new(
                    ErrorCategory::Unsupported,
                    "L2_DISTANCE requires a FROM table",
                ));
            }
        }
    }
    if !select.order_by.is_empty() {
        return Err(DbError::new(
            ErrorCategory::Unsupported,
            "ORDER BY on constants is not supported",
        ));
    }
    let rows = if select.limit == Some(0) {
        Vec::new()
    } else {
        vec![row]
    };
    Ok(QueryResult {
        columns,
        rows,
        affected: 0,
    })
}

fn literal_value(literal: &Literal) -> Value {
    match literal {
        Literal::BigInt(value) => Value::BigInt(*value),
        Literal::Varchar(value) => Value::Varchar(value.clone()),
    }
}

fn value_column_name(value: &Value) -> String {
    match value {
        Value::BigInt(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Varchar(value) => value.clone(),
        Value::Vector(_) => "VECTOR".to_owned(),
        Value::VectorRef(_) => "VECTOR".to_owned(),
    }
}

fn read_vector_value(vectors: &mut VectorStore, value: &Value) -> Result<Vec<f32>, DbError> {
    match value {
        Value::Vector(values) => Ok(values.clone()),
        Value::VectorRef(handle) => vectors.read(*handle),
        _ => Err(DbError::new(
            ErrorCategory::TypeMismatch,
            "expected VECTOR value",
        )),
    }
}

fn distance_for_value(
    vectors: &mut VectorStore,
    value: &Value,
    query: &[f32],
) -> Result<f64, DbError> {
    match value {
        Value::Vector(values) => crate::vector::l2_distance(values, query),
        Value::VectorRef(handle) => vectors.l2_distance(*handle, query),
        _ => Err(DbError::new(
            ErrorCategory::TypeMismatch,
            "expected VECTOR value",
        )),
    }
}
