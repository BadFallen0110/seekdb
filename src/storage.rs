use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::error::{DbError, ErrorCategory};
use crate::sql::ColumnDefinition;
use crate::table::{Table, TableSchema};
use crate::value::{ColumnType, Value};
use crate::vector_store::VectorStore;

const MAGIC: &[u8; 4] = b"MVD1";
const HEADER_LENGTH: usize = 12;

#[derive(Clone, Debug)]
pub enum Mutation {
    CreateTable(TableSchema),
    DropTable(String),
    Upsert {
        table: String,
        rows: Vec<(i64, Vec<Value>)>,
    },
    Delete {
        table: String,
        keys: Vec<i64>,
    },
    CreateVectorIndex {
        table: String,
        name: String,
        column: String,
    },
}

pub fn encode_frames(payloads: &[Vec<u8>]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for payload in payloads {
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&crc32(payload).to_le_bytes());
        bytes.extend_from_slice(payload);
    }
    bytes
}

pub fn decode_frames(bytes: &[u8]) -> Result<Vec<Vec<u8>>, DbError> {
    let mut frames = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        if bytes.len() - position < HEADER_LENGTH {
            break;
        }
        if &bytes[position..position + 4] != MAGIC {
            return Err(corruption("invalid WAL magic before end of log"));
        }
        let length = u32::from_le_bytes(
            bytes[position + 4..position + 8]
                .try_into()
                .expect("four-byte length"),
        ) as usize;
        let checksum = u32::from_le_bytes(
            bytes[position + 8..position + 12]
                .try_into()
                .expect("four-byte checksum"),
        );
        let payload_start = position + HEADER_LENGTH;
        let Some(payload_end) = payload_start.checked_add(length) else {
            return Err(corruption("WAL frame length overflow"));
        };
        if payload_end > bytes.len() {
            break;
        }
        let payload = &bytes[payload_start..payload_end];
        if crc32(payload) != checksum {
            if payload_end < bytes.len() {
                return Err(corruption("checksum failure before end of WAL"));
            }
            break;
        }
        frames.push(payload.to_vec());
        position = payload_end;
    }
    Ok(frames)
}

pub fn read_last_frame(path: &Path) -> Result<Option<Vec<u8>>, DbError> {
    if !path.exists() {
        return Ok(None);
    }
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    Ok(decode_frames(&bytes)?.pop())
}

pub fn read_all_frames(path: &Path) -> Result<Vec<Vec<u8>>, DbError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    File::open(path)?.read_to_end(&mut bytes)?;
    decode_frames(&bytes)
}

pub fn append_frame(path: &Path, payload: &[u8]) -> Result<(), DbError> {
    let existed = path.exists();
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&encode_frames(&[payload.to_vec()]))?;
    file.sync_data()?;
    if !existed {
        sync_parent(path)?;
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), DbError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

pub fn encode_database(tables: &BTreeMap<String, Table>) -> Vec<u8> {
    let mut writer = Vec::new();
    writer.extend_from_slice(b"MvDB");
    writer.extend_from_slice(&1_u32.to_le_bytes());
    put_u32(&mut writer, tables.len() as u32);
    for table in tables.values() {
        encode_schema(&mut writer, &table.schema);
        put_u64(&mut writer, table.rows.len() as u64);
        for (key, row) in &table.rows {
            put_i64(&mut writer, *key);
            for value in row {
                encode_value(&mut writer, value);
            }
        }
    }
    writer
}

pub fn decode_database(bytes: &[u8]) -> Result<BTreeMap<String, Table>, DbError> {
    if bytes.len() < 8 || &bytes[..4] != b"MvDB" {
        return Err(corruption("invalid database snapshot header"));
    }
    let mut cursor = Cursor::new(&bytes[4..]);
    let version = cursor.u32()?;
    if version != 1 {
        return Err(corruption(format!(
            "unsupported database snapshot version {version}"
        )));
    }
    let table_count = cursor.u32()? as usize;
    let mut tables = BTreeMap::new();
    for _ in 0..table_count {
        let schema = decode_schema(&mut cursor)?;
        let name = schema.name.clone();
        let row_count = cursor.u64()? as usize;
        let mut rows = BTreeMap::new();
        for _ in 0..row_count {
            let key = cursor.i64()?;
            let mut row = Vec::with_capacity(schema.columns.len());
            for _ in 0..schema.columns.len() {
                row.push(cursor.value()?);
            }
            rows.insert(key, row);
        }
        tables.insert(name, Table::from_rows(schema, rows));
    }
    if cursor.position != cursor.bytes.len() {
        return Err(corruption("trailing bytes in database snapshot"));
    }
    Ok(tables)
}

pub fn encode_mutations(mutations: &[Mutation]) -> Vec<u8> {
    let mut writer = Vec::new();
    writer.extend_from_slice(b"MvOP");
    writer.extend_from_slice(&1_u32.to_le_bytes());
    put_u32(&mut writer, mutations.len() as u32);
    for mutation in mutations {
        match mutation {
            Mutation::CreateTable(schema) => {
                writer.push(0);
                encode_schema(&mut writer, schema);
            }
            Mutation::DropTable(table) => {
                writer.push(1);
                put_string(&mut writer, table);
            }
            Mutation::Upsert { table, rows } => {
                writer.push(2);
                put_string(&mut writer, table);
                put_u64(&mut writer, rows.len() as u64);
                for (key, row) in rows {
                    put_i64(&mut writer, *key);
                    put_u32(&mut writer, row.len() as u32);
                    for value in row {
                        encode_value(&mut writer, value);
                    }
                }
            }
            Mutation::Delete { table, keys } => {
                writer.push(3);
                put_string(&mut writer, table);
                put_u64(&mut writer, keys.len() as u64);
                for key in keys {
                    put_i64(&mut writer, *key);
                }
            }
            Mutation::CreateVectorIndex {
                table,
                name,
                column,
            } => {
                writer.push(4);
                put_string(&mut writer, table);
                put_string(&mut writer, name);
                put_string(&mut writer, column);
            }
        }
    }
    writer
}

pub fn decode_mutations(bytes: &[u8]) -> Result<Vec<Mutation>, DbError> {
    if bytes.len() < 8 || &bytes[..4] != b"MvOP" {
        return Err(corruption("invalid mutation batch header"));
    }
    let mut cursor = Cursor::new(&bytes[4..]);
    let version = cursor.u32()?;
    if version != 1 {
        return Err(corruption(format!(
            "unsupported mutation batch version {version}"
        )));
    }
    let count = cursor.u32()? as usize;
    let mut mutations = Vec::with_capacity(count);
    for _ in 0..count {
        mutations.push(match cursor.u8()? {
            0 => Mutation::CreateTable(decode_schema(&mut cursor)?),
            1 => Mutation::DropTable(cursor.string()?),
            2 => {
                let table = cursor.string()?;
                let row_count = cursor.u64()? as usize;
                let mut rows = Vec::with_capacity(row_count);
                for _ in 0..row_count {
                    let key = cursor.i64()?;
                    let value_count = cursor.u32()? as usize;
                    let mut row = Vec::with_capacity(value_count);
                    for _ in 0..value_count {
                        row.push(cursor.value()?);
                    }
                    rows.push((key, row));
                }
                Mutation::Upsert { table, rows }
            }
            3 => {
                let table = cursor.string()?;
                let key_count = cursor.u64()? as usize;
                let mut keys = Vec::with_capacity(key_count);
                for _ in 0..key_count {
                    keys.push(cursor.i64()?);
                }
                Mutation::Delete { table, keys }
            }
            4 => Mutation::CreateVectorIndex {
                table: cursor.string()?,
                name: cursor.string()?,
                column: cursor.string()?,
            },
            tag => return Err(corruption(format!("invalid mutation tag {tag}"))),
        });
    }
    if cursor.position != cursor.bytes.len() {
        return Err(corruption("trailing bytes in mutation batch"));
    }
    Ok(mutations)
}

pub fn apply_mutations(
    tables: &mut BTreeMap<String, Table>,
    mutations: &[Mutation],
    vectors: &mut VectorStore,
) -> Result<(), DbError> {
    for mutation in mutations {
        match mutation {
            Mutation::CreateTable(schema) => {
                tables.insert(schema.name.clone(), Table::new(schema.clone()));
            }
            Mutation::DropTable(name) => {
                tables.remove(name);
            }
            Mutation::Upsert { table, rows } => {
                let table = tables.get_mut(table).ok_or_else(|| {
                    DbError::new(
                        ErrorCategory::TableNotFound,
                        format!("WAL references missing table `{table}`"),
                    )
                })?;
                for (key, row) in rows {
                    table.replace_row(*key, materialize_row(row, vectors)?);
                }
            }
            Mutation::Delete { table, keys } => {
                let table = tables.get_mut(table).ok_or_else(|| {
                    DbError::new(
                        ErrorCategory::TableNotFound,
                        format!("WAL references missing table `{table}`"),
                    )
                })?;
                for key in keys {
                    table.remove_row(*key);
                }
            }
            Mutation::CreateVectorIndex {
                table,
                name,
                column,
            } => {
                let table = tables.get_mut(table).ok_or_else(|| {
                    DbError::new(
                        ErrorCategory::TableNotFound,
                        format!("WAL references missing table `{table}`"),
                    )
                })?;
                let _ = table.schema.column_index(column)?;
                table.schema.vector_index_name = Some(name.clone());
            }
        }
    }
    Ok(())
}

pub fn load_database(
    path: &Path,
    vectors: &mut VectorStore,
) -> Result<BTreeMap<String, Table>, DbError> {
    let mut tables = BTreeMap::new();
    visit_frames(path, |frame| {
        if frame.starts_with(b"MvDB") {
            let decoded = decode_database(&frame)?;
            tables.clear();
            for (name, table) in decoded {
                let rows = table
                    .rows
                    .into_iter()
                    .map(|(key, row)| Ok((key, materialize_row(&row, vectors)?)))
                    .collect::<Result<_, DbError>>()?;
                tables.insert(name, Table::from_rows(table.schema, rows));
            }
        } else if frame.starts_with(b"MvOP") {
            let mutations = decode_mutations(&frame)?;
            apply_mutations(&mut tables, &mutations, vectors)?;
        } else {
            return Err(corruption("unknown WAL payload"));
        }
        Ok(())
    })?;
    Ok(tables)
}

fn visit_frames(
    path: &Path,
    mut visit: impl FnMut(Vec<u8>) -> Result<(), DbError>,
) -> Result<(), DbError> {
    if !path.exists() {
        return Ok(());
    }
    let mut file = File::open(path)?;
    let file_length = file.metadata()?.len();
    let mut position = 0_u64;
    while position < file_length {
        let mut header = [0_u8; HEADER_LENGTH];
        let remaining = file_length - position;
        if remaining < HEADER_LENGTH as u64 {
            break;
        }
        file.read_exact(&mut header)?;
        position += HEADER_LENGTH as u64;
        if &header[..4] != MAGIC {
            return Err(corruption("invalid WAL magic before end of log"));
        }
        let length =
            u32::from_le_bytes(header[4..8].try_into().expect("four-byte length")) as usize;
        let checksum = u32::from_le_bytes(header[8..12].try_into().expect("four-byte checksum"));
        let payload_end = position
            .checked_add(length as u64)
            .ok_or_else(|| corruption("WAL frame length overflow"))?;
        if payload_end > file_length {
            break;
        }
        let mut payload = vec![0_u8; length];
        file.read_exact(&mut payload)?;
        position = payload_end;
        if crc32(&payload) != checksum {
            if position < file_length {
                return Err(corruption("checksum failure before end of WAL"));
            }
            break;
        }
        visit(payload)?;
    }
    Ok(())
}

fn materialize_row(row: &[Value], vectors: &mut VectorStore) -> Result<Vec<Value>, DbError> {
    row.iter()
        .map(|value| match value {
            Value::Vector(values) => Ok(Value::VectorRef(vectors.append(values)?)),
            Value::VectorRef(_) => Err(corruption(
                "WAL contains a VECTOR reference instead of committed VECTOR data",
            )),
            other => Ok(other.clone()),
        })
        .collect()
}

fn encode_schema(writer: &mut Vec<u8>, schema: &TableSchema) {
    put_string(writer, &schema.name);
    put_u32(writer, schema.columns.len() as u32);
    for column in &schema.columns {
        put_string(writer, &column.name);
        match &column.column_type {
            ColumnType::BigInt => writer.push(0),
            ColumnType::Varchar(size) => {
                writer.push(1);
                put_u32(writer, *size as u32);
            }
            ColumnType::Vector(dimensions) => {
                writer.push(2);
                put_u32(writer, *dimensions as u32);
            }
        }
        writer.push(u8::from(column.primary_key));
    }
    put_u32(writer, schema.primary_key_index as u32);
    match schema.vector_index {
        Some(index) => {
            writer.push(1);
            put_u32(writer, index as u32);
        }
        None => writer.push(0),
    }
    match &schema.vector_index_name {
        Some(name) => {
            writer.push(1);
            put_string(writer, name);
        }
        None => writer.push(0),
    }
}

fn decode_schema(cursor: &mut Cursor<'_>) -> Result<TableSchema, DbError> {
    let name = cursor.string()?;
    let column_count = cursor.u32()? as usize;
    let mut columns = Vec::with_capacity(column_count);
    for _ in 0..column_count {
        let column_name = cursor.string()?;
        let column_type = match cursor.u8()? {
            0 => ColumnType::BigInt,
            1 => ColumnType::Varchar(cursor.u32()? as usize),
            2 => ColumnType::Vector(cursor.u32()? as usize),
            tag => return Err(corruption(format!("invalid column type tag {tag}"))),
        };
        let primary_key = cursor.u8()? != 0;
        columns.push(ColumnDefinition {
            name: column_name,
            column_type,
            primary_key,
        });
    }
    let primary_key_index = cursor.u32()? as usize;
    let vector_index = match cursor.u8()? {
        0 => None,
        1 => Some(cursor.u32()? as usize),
        tag => return Err(corruption(format!("invalid vector index tag {tag}"))),
    };
    let vector_index_name = match cursor.u8()? {
        0 => None,
        1 => Some(cursor.string()?),
        tag => {
            return Err(corruption(format!("invalid vector index name tag {tag}")));
        }
    };
    if primary_key_index >= columns.len() {
        return Err(corruption("primary key index is out of bounds"));
    }
    Ok(TableSchema {
        name,
        columns,
        primary_key_index,
        vector_index,
        vector_index_name,
    })
}

pub fn ensure_data_directory(path: &Path) -> Result<(), DbError> {
    fs::create_dir_all(path)?;
    Ok(())
}

fn encode_value(writer: &mut Vec<u8>, value: &Value) {
    match value {
        Value::BigInt(value) => {
            writer.push(0);
            put_i64(writer, *value);
        }
        Value::Float(value) => {
            writer.push(1);
            writer.extend_from_slice(&value.to_le_bytes());
        }
        Value::Varchar(value) => {
            writer.push(2);
            put_string(writer, value);
        }
        Value::Vector(value) => {
            writer.push(3);
            put_u32(writer, value.len() as u32);
            for component in value {
                writer.extend_from_slice(&component.to_le_bytes());
            }
        }
        Value::VectorRef(handle) => {
            writer.push(4);
            put_u64(writer, *handle);
        }
    }
}

fn put_u32(writer: &mut Vec<u8>, value: u32) {
    writer.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(writer: &mut Vec<u8>, value: u64) {
    writer.extend_from_slice(&value.to_le_bytes());
}

fn put_i64(writer: &mut Vec<u8>, value: i64) {
    writer.extend_from_slice(&value.to_le_bytes());
}

fn put_string(writer: &mut Vec<u8>, value: &str) {
    put_u32(writer, value.len() as u32);
    writer.extend_from_slice(value.as_bytes());
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DbError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| corruption("snapshot offset overflow"))?;
        if end > self.bytes.len() {
            return Err(corruption("truncated database snapshot"));
        }
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DbError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, DbError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four-byte integer"),
        ))
    }

    fn u64(&mut self) -> Result<u64, DbError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight-byte integer"),
        ))
    }

    fn i64(&mut self) -> Result<i64, DbError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("eight-byte integer"),
        ))
    }

    fn f64(&mut self) -> Result<f64, DbError> {
        Ok(f64::from_le_bytes(
            self.take(8)?.try_into().expect("eight-byte float"),
        ))
    }

    fn f32(&mut self) -> Result<f32, DbError> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("four-byte float"),
        ))
    }

    fn string(&mut self) -> Result<String, DbError> {
        let length = self.u32()? as usize;
        String::from_utf8(self.take(length)?.to_vec())
            .map_err(|_| corruption("snapshot string is not UTF-8"))
    }

    fn value(&mut self) -> Result<Value, DbError> {
        match self.u8()? {
            0 => Ok(Value::BigInt(self.i64()?)),
            1 => Ok(Value::Float(self.f64()?)),
            2 => Ok(Value::Varchar(self.string()?)),
            3 => {
                let length = self.u32()? as usize;
                let mut values = Vec::with_capacity(length);
                for _ in 0..length {
                    values.push(self.f32()?);
                }
                Ok(Value::Vector(values))
            }
            4 => Ok(Value::VectorRef(self.u64()?)),
            tag => Err(corruption(format!("invalid value tag {tag}"))),
        }
    }
}

fn corruption(message: impl Into<String>) -> DbError {
    DbError::new(ErrorCategory::Corruption, message)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
