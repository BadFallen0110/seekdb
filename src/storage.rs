use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

use crate::error::{DbError, ErrorCategory};
use crate::sql::ColumnDefinition;
use crate::table::{Table, TableSchema};
use crate::value::{ColumnType, Value};

const MAGIC: &[u8; 4] = b"MVD1";
const HEADER_LENGTH: usize = 12;

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
            if frames.is_empty() {
                return Err(corruption("invalid WAL magic"));
            }
            break;
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

pub fn append_frame(path: &Path, payload: &[u8]) -> Result<(), DbError> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&encode_frames(&[payload.to_vec()]))?;
    file.sync_all()?;
    sync_parent(path)?;
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
        put_string(&mut writer, &table.schema.name);
        put_u32(&mut writer, table.schema.columns.len() as u32);
        for column in &table.schema.columns {
            put_string(&mut writer, &column.name);
            match &column.column_type {
                ColumnType::BigInt => writer.push(0),
                ColumnType::Varchar(size) => {
                    writer.push(1);
                    put_u32(&mut writer, *size as u32);
                }
                ColumnType::Vector(dimensions) => {
                    writer.push(2);
                    put_u32(&mut writer, *dimensions as u32);
                }
            }
            writer.push(u8::from(column.primary_key));
        }
        put_u32(&mut writer, table.schema.primary_key_index as u32);
        match table.schema.vector_index {
            Some(index) => {
                writer.push(1);
                put_u32(&mut writer, index as u32);
            }
            None => writer.push(0),
        }
        match &table.schema.vector_index_name {
            Some(name) => {
                writer.push(1);
                put_string(&mut writer, name);
            }
            None => writer.push(0),
        }
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
        let name = cursor.string()?;
        let column_count = cursor.u32()? as usize;
        let mut columns = Vec::with_capacity(column_count);
        for _ in 0..column_count {
            let column_name = cursor.string()?;
            let column_type = match cursor.u8()? {
                0 => ColumnType::BigInt,
                1 => ColumnType::Varchar(cursor.u32()? as usize),
                2 => ColumnType::Vector(cursor.u32()? as usize),
                tag => {
                    return Err(corruption(format!(
                        "invalid column type tag {tag}"
                    )));
                }
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
            tag => {
                return Err(corruption(format!(
                    "invalid vector index tag {tag}"
                )));
            }
        };
        let vector_index_name = match cursor.u8()? {
            0 => None,
            1 => Some(cursor.string()?),
            tag => {
                return Err(corruption(format!(
                    "invalid vector index name tag {tag}"
                )));
            }
        };
        if primary_key_index >= columns.len() {
            return Err(corruption("primary key index is out of bounds"));
        }
        let schema = TableSchema {
            name: name.clone(),
            columns,
            primary_key_index,
            vector_index,
            vector_index_name,
        };
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
        tables.insert(
            name,
            Table {
                schema,
                rows,
            },
        );
    }
    if cursor.position != cursor.bytes.len() {
        return Err(corruption("trailing bytes in database snapshot"));
    }
    Ok(tables)
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
