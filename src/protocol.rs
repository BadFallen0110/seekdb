use std::io::{BufRead, Write};

use serde_json::{Map, Number, Value as JsonValue, json};

use crate::db::{Database, QueryResult};
use crate::error::{DbError, ErrorCategory};
use crate::sql::{QueryMode, Statement, parse};
use crate::value::Value;

pub struct ProtocolConfig {
    pub ready_marker: String,
    pub output_format: OutputFormat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputFormat {
    Jsonl,
    Framed,
    Tsv,
    Mysql,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            ready_marker: "READY".to_owned(),
            output_format: OutputFormat::Tsv,
        }
    }
}

pub fn run_jsonl<R: BufRead, W: Write>(
    database: &mut Database,
    reader: R,
    mut writer: W,
) -> Result<(), DbError> {
    write_json_response(&mut writer, &json!({"type": "ready"}))?;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request = match serde_json::from_str::<JsonValue>(&line) {
            Ok(JsonValue::Object(request)) => request,
            Ok(_) => {
                write_json_response(
                    &mut writer,
                    &json_error(
                        JsonValue::Null,
                        "invalid_request",
                        "request must be a JSON object",
                    ),
                )?;
                continue;
            }
            Err(error) => {
                write_json_response(
                    &mut writer,
                    &json_error(JsonValue::Null, "invalid_json", error.to_string()),
                )?;
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let operation = request.get("op").and_then(JsonValue::as_str);
        match operation {
            Some("shutdown") => {
                database.close()?;
                write_json_response(&mut writer, &json_ok(id))?;
                return Ok(());
            }
            Some("execute") => {
                let Some(sql) = request.get("sql").and_then(JsonValue::as_str) else {
                    write_json_response(
                        &mut writer,
                        &json_error(id, "invalid_request", "execute requires a string sql field"),
                    )?;
                    continue;
                };
                let mode = match request
                    .get("search_mode")
                    .and_then(JsonValue::as_str)
                    .unwrap_or("exact")
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "exact" => QueryMode::Exact,
                    "ann" => QueryMode::Ann,
                    _ => {
                        write_json_response(
                            &mut writer,
                            &json_error(
                                id,
                                "invalid_search_mode",
                                "search_mode must be `exact` or `ann`",
                            ),
                        )?;
                        continue;
                    }
                };

                let statement = match parse(sql) {
                    Ok(statement) => statement,
                    Err(error) => {
                        write_json_response(&mut writer, &json_database_error(id, &error))?;
                        continue;
                    }
                };
                database.set_query_mode(mode);
                match database.execute(sql) {
                    Ok(result) => {
                        let response = json_query_result(id, result, is_mutation(&statement));
                        write_json_response(&mut writer, &response)?;
                    }
                    Err(error) => {
                        write_json_response(&mut writer, &json_database_error(id, &error))?;
                    }
                }
            }
            Some(operation) => {
                write_json_response(
                    &mut writer,
                    &json_error(
                        id,
                        "unsupported_operation",
                        format!("unsupported operation `{operation}`"),
                    ),
                )?;
            }
            None => {
                write_json_response(
                    &mut writer,
                    &json_error(id, "invalid_request", "request requires a string op field"),
                )?;
            }
        }
    }

    database.close()
}

fn json_ok(id: JsonValue) -> JsonValue {
    let mut response = Map::new();
    response.insert("id".to_owned(), id);
    response.insert("ok".to_owned(), JsonValue::Bool(true));
    JsonValue::Object(response)
}

fn json_error(id: JsonValue, code: impl AsRef<str>, message: impl Into<String>) -> JsonValue {
    let mut response = Map::new();
    response.insert("id".to_owned(), id);
    response.insert("ok".to_owned(), JsonValue::Bool(false));
    response.insert(
        "error".to_owned(),
        json!({"code": code.as_ref(), "message": message.into()}),
    );
    JsonValue::Object(response)
}

fn json_database_error(id: JsonValue, error: &DbError) -> JsonValue {
    json_error(id, semantic_error_code(error), error.to_string())
}

fn json_query_result(id: JsonValue, result: QueryResult, mutation: bool) -> JsonValue {
    if mutation {
        let mut response = json_ok(id);
        response
            .as_object_mut()
            .expect("constructed object")
            .insert(
                "affected_rows".to_owned(),
                JsonValue::Number(Number::from(result.affected)),
            );
        return response;
    }
    if result.columns.is_empty() {
        return json_ok(id);
    }

    let mut response = json_ok(id);
    let object = response.as_object_mut().expect("constructed object");
    object.insert(
        "columns".to_owned(),
        JsonValue::Array(result.columns.into_iter().map(JsonValue::String).collect()),
    );
    object.insert(
        "rows".to_owned(),
        JsonValue::Array(
            result
                .rows
                .into_iter()
                .map(|row| JsonValue::Array(row.iter().map(json_value).collect()))
                .collect(),
        ),
    );
    response
}

fn json_value(value: &Value) -> JsonValue {
    match value {
        Value::BigInt(value) => JsonValue::Number(Number::from(*value)),
        Value::Float(value) => Number::from_f64(*value)
            .map(JsonValue::Number)
            .unwrap_or(JsonValue::Null),
        Value::Varchar(value) => JsonValue::String(value.clone()),
        Value::Vector(values) => JsonValue::Array(
            values
                .iter()
                .map(|value| {
                    Number::from_f64(f64::from(*value))
                        .map(JsonValue::Number)
                        .unwrap_or(JsonValue::Null)
                })
                .collect(),
        ),
        Value::VectorRef(handle) => JsonValue::String(format!("<vector:{handle}>")),
    }
}

fn write_json_response(writer: &mut impl Write, response: &JsonValue) -> Result<(), DbError> {
    serde_json::to_writer(&mut *writer, response)
        .map_err(|error| DbError::new(ErrorCategory::Io, error.to_string()))?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn is_mutation(statement: &Statement) -> bool {
    matches!(
        statement,
        Statement::Insert { .. } | Statement::Update { .. } | Statement::Delete { .. }
    )
}

fn semantic_error_code(error: &DbError) -> &'static str {
    match error.category() {
        ErrorCategory::Syntax => "syntax_error",
        ErrorCategory::Unsupported => "unsupported_sql",
        ErrorCategory::TableNotFound => "table_not_found",
        ErrorCategory::TableExists => "table_exists",
        ErrorCategory::ColumnNotFound => "column_not_found",
        ErrorCategory::TypeMismatch
            if error.to_string().to_ascii_lowercase().contains("vector")
                && error.to_string().to_ascii_lowercase().contains("dimension") =>
        {
            "dimension_mismatch"
        }
        ErrorCategory::TypeMismatch => "type_mismatch",
        ErrorCategory::Range => "range_error",
        ErrorCategory::DuplicateKey => "duplicate_key",
        ErrorCategory::Transaction => "transaction_error",
        ErrorCategory::Io => "io_error",
        ErrorCategory::Corruption => "corruption",
    }
}

pub fn run<R: BufRead, W: Write>(
    database: &mut Database,
    reader: R,
    mut writer: W,
    config: &ProtocolConfig,
) -> Result<(), DbError> {
    if !config.ready_marker.is_empty() {
        writeln!(writer, "{}", config.ready_marker)?;
        writer.flush()?;
    }

    let mut pending = String::new();
    for line in reader.lines() {
        let line = line?;
        pending.push_str(&line);
        pending.push('\n');

        let control = pending.trim().to_owned();
        if !control.contains(';') && is_exit_command(&control) {
            pending.clear();
            database.close()?;
            return Ok(());
        }
        if !control.contains(';') && is_mode_hint(&control) {
            apply_mode_hint(database, &control)?;
            pending.clear();
            writeln!(writer, "OK")?;
            writer.flush()?;
            continue;
        }

        let (statements, consumed) = extract_statements(&pending);
        if consumed > 0 {
            pending.drain(..consumed);
        }
        if statements.is_empty() && consumed == 0 && !control.is_empty() {
            let should_execute = match parse(&control) {
                Ok(_) => true,
                Err(error) => error.category() != ErrorCategory::Syntax,
            };
            if should_execute {
                if execute_text(database, &mut writer, &control, config)? {
                    return Ok(());
                }
                pending.clear();
                continue;
            }
        }
        for statement in statements {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            if execute_text(database, &mut writer, statement, config)? {
                return Ok(());
            }
        }
    }

    if !pending.trim().is_empty() {
        let statement = pending.trim();
        if execute_text(database, &mut writer, statement, config)? {
            return Ok(());
        }
    }

    database.close()
}

fn execute_text(
    database: &mut Database,
    writer: &mut impl Write,
    statement: &str,
    config: &ProtocolConfig,
) -> Result<bool, DbError> {
    if is_exit_command(statement) {
        database.close()?;
        return Ok(true);
    }
    if is_mode_hint(statement) {
        match apply_mode_hint(database, statement) {
            Ok(()) => writeln!(writer, "OK")?,
            Err(error) => write_error(writer, &error, config.output_format)?,
        }
        writer.flush()?;
        return Ok(false);
    }
    apply_comment_mode_hint(database, statement);
    match database.execute(statement) {
        Ok(result) => write_result(writer, &result, config.output_format)?,
        Err(error) => write_error(writer, &error, config.output_format)?,
    }
    writer.flush()?;
    Ok(false)
}

pub fn extract_statements(buffer: &str) -> (Vec<String>, usize) {
    let chars: Vec<char> = buffer.chars().collect();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut position = 0;
    let mut state = ScanState::Normal;
    while position < chars.len() {
        match state {
            ScanState::Normal => {
                if chars[position] == '\'' {
                    state = ScanState::SingleQuote;
                    position += 1;
                } else if chars[position] == '`' {
                    state = ScanState::Backtick;
                    position += 1;
                } else if chars[position] == '-' && chars.get(position + 1) == Some(&'-') {
                    state = ScanState::LineComment;
                    position += 2;
                } else if chars[position] == '#'
                    || (chars[position] == '/' && chars.get(position + 1) == Some(&'/'))
                {
                    state = ScanState::LineComment;
                    position += if chars[position] == '#' { 1 } else { 2 };
                } else if chars[position] == '/' && chars.get(position + 1) == Some(&'*') {
                    state = ScanState::BlockComment;
                    position += 2;
                } else if chars[position] == ';' {
                    statements.push(chars[start..position].iter().collect());
                    position += 1;
                    start = position;
                } else {
                    position += 1;
                }
            }
            ScanState::SingleQuote => {
                if chars[position] == '\\' {
                    position = (position + 2).min(chars.len());
                } else if chars[position] == '\'' {
                    if chars.get(position + 1) == Some(&'\'') {
                        position += 2;
                    } else {
                        state = ScanState::Normal;
                        position += 1;
                    }
                } else {
                    position += 1;
                }
            }
            ScanState::Backtick => {
                if chars[position] == '`' {
                    if chars.get(position + 1) == Some(&'`') {
                        position += 2;
                    } else {
                        state = ScanState::Normal;
                        position += 1;
                    }
                } else {
                    position += 1;
                }
            }
            ScanState::LineComment => {
                if chars[position] == '\n' {
                    state = ScanState::Normal;
                }
                position += 1;
            }
            ScanState::BlockComment => {
                if chars[position] == '*' && chars.get(position + 1) == Some(&'/') {
                    state = ScanState::Normal;
                    position += 2;
                } else {
                    position += 1;
                }
            }
        }
    }
    let consumed_chars = if start == 0 { 0 } else { start };
    let consumed_bytes = buffer
        .char_indices()
        .nth(consumed_chars)
        .map(|(index, _)| index)
        .unwrap_or(buffer.len());
    (statements, consumed_bytes)
}

fn write_result(
    writer: &mut impl Write,
    result: &QueryResult,
    format: OutputFormat,
) -> Result<(), DbError> {
    match format {
        OutputFormat::Jsonl => Err(DbError::new(
            ErrorCategory::Io,
            "JSON Lines output requires the JSONL protocol loop",
        )),
        OutputFormat::Framed => write_framed_result(writer, result),
        OutputFormat::Tsv => write_tsv_result(writer, result),
        OutputFormat::Mysql => write_mysql_result(writer, result),
    }
}

fn write_framed_result(writer: &mut impl Write, result: &QueryResult) -> Result<(), DbError> {
    if !result.columns.is_empty() {
        writeln!(writer, "BEGIN_RESULT")?;
        writeln!(
            writer,
            "COLUMNS\t{}",
            result
                .columns
                .iter()
                .map(|column| escape_field(column))
                .collect::<Vec<_>>()
                .join("\t")
        )?;
        for row in &result.rows {
            writeln!(
                writer,
                "ROW\t{}",
                row.iter().map(format_value).collect::<Vec<_>>().join("\t")
            )?;
        }
        writeln!(writer, "END_RESULT")?;
    } else if result.affected > 0 {
        writeln!(writer, "AFFECTED\t{}", result.affected)?;
    } else {
        writeln!(writer, "OK")?;
    }
    Ok(())
}

fn write_tsv_result(writer: &mut impl Write, result: &QueryResult) -> Result<(), DbError> {
    if !result.columns.is_empty() {
        writeln!(
            writer,
            "{}",
            result
                .columns
                .iter()
                .map(|column| escape_field(column))
                .collect::<Vec<_>>()
                .join("\t")
        )?;
        for row in &result.rows {
            writeln!(
                writer,
                "{}",
                row.iter().map(format_value).collect::<Vec<_>>().join("\t")
            )?;
        }
    } else if result.affected > 0 {
        writeln!(writer, "AFFECTED\t{}", result.affected)?;
    } else {
        writeln!(writer, "OK")?;
    }
    Ok(())
}

fn write_mysql_result(writer: &mut impl Write, result: &QueryResult) -> Result<(), DbError> {
    if result.columns.is_empty() {
        if result.affected > 0 {
            writeln!(writer, "Query OK, {} rows affected", result.affected)?;
        } else {
            writeln!(writer, "Query OK")?;
        }
        return Ok(());
    }
    let mut widths = result
        .columns
        .iter()
        .map(|column| column.chars().count())
        .collect::<Vec<_>>();
    let values = result
        .rows
        .iter()
        .map(|row| row.iter().map(format_value).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    for row in &values {
        for (index, value) in row.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }
    let border = format!(
        "+{}+",
        widths
            .iter()
            .map(|width| "-".repeat(width + 2))
            .collect::<Vec<_>>()
            .join("+")
    );
    writeln!(writer, "{border}")?;
    writeln!(
        writer,
        "| {} |",
        result
            .columns
            .iter()
            .zip(&widths)
            .map(|(column, width)| format!("{column:<width$}"))
            .collect::<Vec<_>>()
            .join(" | ")
    )?;
    writeln!(writer, "{border}")?;
    for row in values {
        writeln!(
            writer,
            "| {} |",
            row.iter()
                .zip(&widths)
                .map(|(value, width)| format!("{value:<width$}"))
                .collect::<Vec<_>>()
                .join(" | ")
        )?;
    }
    writeln!(writer, "{border}")?;
    Ok(())
}

fn write_error(
    writer: &mut impl Write,
    error: &DbError,
    format: OutputFormat,
) -> Result<(), DbError> {
    match format {
        OutputFormat::Jsonl => {
            serde_json::to_writer(
                &mut *writer,
                &json_error(
                    JsonValue::Null,
                    semantic_error_code(error),
                    error.to_string(),
                ),
            )
            .map_err(|error| DbError::new(ErrorCategory::Io, error.to_string()))?;
            writeln!(writer)?;
        }
        OutputFormat::Framed | OutputFormat::Tsv => {
            writeln!(
                writer,
                "ERROR\t{}\t{}\t{}",
                error_code(error.category()),
                category_name(error.category()),
                escape_field(&error.to_string())
            )?;
        }
        OutputFormat::Mysql => {
            writeln!(
                writer,
                "ERROR {} (HY000): {}",
                error_code(error.category()),
                error
            )?;
        }
    }
    Ok(())
}

fn format_value(value: &Value) -> String {
    match value {
        Value::BigInt(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Varchar(value) => escape_field(value),
        Value::Vector(values) => {
            let values = values
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",");
            escape_field(&format!("[{values}]"))
        }
        Value::VectorRef(handle) => format!("<vector:{handle}>"),
    }
}

fn escape_field(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
}

fn category_name(category: ErrorCategory) -> &'static str {
    match category {
        ErrorCategory::Syntax => "SYNTAX",
        ErrorCategory::Unsupported => "UNSUPPORTED",
        ErrorCategory::TableNotFound => "TABLE_NOT_FOUND",
        ErrorCategory::TableExists => "TABLE_EXISTS",
        ErrorCategory::ColumnNotFound => "COLUMN_NOT_FOUND",
        ErrorCategory::TypeMismatch => "TYPE_MISMATCH",
        ErrorCategory::Range => "RANGE",
        ErrorCategory::DuplicateKey => "DUPLICATE_KEY",
        ErrorCategory::Transaction => "TRANSACTION",
        ErrorCategory::Io => "IO",
        ErrorCategory::Corruption => "CORRUPTION",
    }
}

fn error_code(category: ErrorCategory) -> u16 {
    match category {
        ErrorCategory::Syntax => 1001,
        ErrorCategory::Unsupported => 1002,
        ErrorCategory::TableNotFound => 1101,
        ErrorCategory::TableExists => 1102,
        ErrorCategory::ColumnNotFound => 1103,
        ErrorCategory::TypeMismatch => 1201,
        ErrorCategory::Range => 1202,
        ErrorCategory::DuplicateKey => 1301,
        ErrorCategory::Transaction => 1401,
        ErrorCategory::Io => 1501,
        ErrorCategory::Corruption => 1502,
    }
}

fn is_exit_command(command: &str) -> bool {
    matches!(
        command
            .trim()
            .trim_end_matches(';')
            .to_ascii_uppercase()
            .as_str(),
        "QUIT" | "EXIT" | "SHUTDOWN" | "\\Q"
    )
}

fn is_mode_hint(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    lower.starts_with("mode=") || lower.starts_with("mode:") || lower.starts_with("query_mode=")
}

fn apply_mode_hint(database: &mut Database, command: &str) -> Result<(), DbError> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("ann") {
        database.set_query_mode(QueryMode::Ann);
    } else if lower.contains("exact") {
        database.set_query_mode(QueryMode::Exact);
    } else {
        return Err(DbError::new(
            ErrorCategory::Unsupported,
            "query mode hint must contain exact or ann",
        ));
    }
    Ok(())
}

fn apply_comment_mode_hint(database: &mut Database, statement: &str) {
    let lower = statement.to_ascii_lowercase();
    if lower.contains("mode=ann")
        || lower.contains("mode: ann")
        || lower.contains("/*ann*/")
        || lower.contains("/* ann */")
    {
        database.set_query_mode(QueryMode::Ann);
    } else if lower.contains("mode=exact")
        || lower.contains("mode: exact")
        || lower.contains("/*exact*/")
        || lower.contains("/* exact */")
    {
        database.set_query_mode(QueryMode::Exact);
    }
}

enum ScanState {
    Normal,
    SingleQuote,
    Backtick,
    LineComment,
    BlockComment,
}

#[cfg(test)]
mod tests {
    use super::{OutputFormat, extract_statements, write_result};
    use crate::db::QueryResult;
    use crate::value::Value;

    #[test]
    fn statement_split_ignores_semicolons_in_strings_and_comments() {
        let input =
            "INSERT INTO t VALUES ('a;b'); -- ignored ; comment\n/* ignored ; block */ SELECT 1;";
        let (statements, consumed) = extract_statements(input);
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("'a;b'"));
        assert!(statements[1].contains("SELECT 1"));
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn mysql_result_format_contains_borders_and_rows() {
        let result = QueryResult {
            columns: vec!["id".to_owned(), "body".to_owned()],
            rows: vec![vec![Value::BigInt(1), Value::Varchar("hello".to_owned())]],
            affected: 0,
        };
        let mut output = Vec::new();
        write_result(&mut output, &result, OutputFormat::Mysql).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.starts_with('+'));
        assert!(output.contains("| id"));
        assert!(output.contains("| 1"));
    }
}
