use std::io::{BufRead, Write};

use crate::db::{Database, QueryResult};
use crate::error::{DbError, ErrorCategory};
use crate::sql::QueryMode;
use crate::value::Value;

pub struct ProtocolConfig {
    pub ready_marker: String,
}

impl Default for ProtocolConfig {
    fn default() -> Self {
        Self {
            ready_marker: "READY".to_owned(),
        }
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

        let control = pending.trim();
        if !control.contains(';') && is_exit_command(control) {
            pending.clear();
            database.close()?;
            return Ok(());
        }
        if !control.contains(';') && is_mode_hint(control) {
            apply_mode_hint(database, control)?;
            pending.clear();
            writeln!(writer, "OK")?;
            writer.flush()?;
            continue;
        }

        let (statements, consumed) = extract_statements(&pending);
        if consumed > 0 {
            pending.drain(..consumed);
        }
        for statement in statements {
            let statement = statement.trim();
            if statement.is_empty() {
                continue;
            }
            if is_exit_command(statement) {
                database.close()?;
                return Ok(());
            }
            apply_comment_mode_hint(database, statement);
            match database.execute(statement) {
                Ok(result) => write_result(&mut writer, &result)?,
                Err(error) => write_error(&mut writer, &error)?,
            }
            writer.flush()?;
        }
    }

    if !pending.trim().is_empty() {
        let statement = pending.trim();
        if is_exit_command(statement) || is_mode_hint(statement) {
            database.close()?;
            return Ok(());
        }
        apply_comment_mode_hint(database, statement);
        match database.execute(statement) {
            Ok(result) => write_result(&mut writer, &result)?,
            Err(error) => write_error(&mut writer, &error)?,
        }
        writer.flush()?;
    }

    database.close()
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

fn write_result(writer: &mut impl Write, result: &QueryResult) -> Result<(), DbError> {
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

fn write_error(writer: &mut impl Write, error: &DbError) -> Result<(), DbError> {
    writeln!(
        writer,
        "ERROR\t{}\t{}\t{}",
        error_code(error.category()),
        category_name(error.category()),
        escape_field(&error.to_string())
    )?;
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
    if lower.contains("mode=ann") || lower.contains("mode: ann") {
        database.set_query_mode(QueryMode::Ann);
    } else if lower.contains("mode=exact") || lower.contains("mode: exact") {
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
    use super::extract_statements;

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
}
