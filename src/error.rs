use std::fmt;
use std::io;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCategory {
    Syntax,
    Unsupported,
    TableNotFound,
    TableExists,
    ColumnNotFound,
    TypeMismatch,
    Range,
    DuplicateKey,
    Transaction,
    Io,
    Corruption,
}

#[derive(Debug)]
pub struct DbError {
    category: ErrorCategory,
    message: String,
}

impl DbError {
    pub fn new(category: ErrorCategory, message: impl Into<String>) -> Self {
        Self {
            category,
            message: message.into(),
        }
    }

    pub fn duplicate_key(table: &str) -> Self {
        Self::new(
            ErrorCategory::DuplicateKey,
            format!("duplicate primary key in table `{table}`"),
        )
    }

    pub fn category(&self) -> ErrorCategory {
        self.category
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DbError {}

impl From<io::Error> for DbError {
    fn from(error: io::Error) -> Self {
        Self::new(ErrorCategory::Io, error.to_string())
    }
}
