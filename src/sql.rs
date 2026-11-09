use crate::error::{DbError, ErrorCategory};
use crate::value::ColumnType;

#[derive(Clone, Debug, PartialEq)]
pub enum Statement {
    CreateTable {
        name: String,
        columns: Vec<ColumnDefinition>,
        if_not_exists: bool,
    },
    DropTable {
        name: String,
        if_exists: bool,
    },
    Insert {
        table: String,
        columns: Option<Vec<String>>,
        rows: Vec<Vec<Literal>>,
    },
    Select(SelectStatement),
    Update {
        table: String,
        assignments: Vec<(String, Literal)>,
        filter: Option<Expression>,
    },
    Delete {
        table: String,
        filter: Option<Expression>,
    },
    CreateVectorIndex {
        name: String,
        table: String,
        column: String,
    },
    Begin,
    Commit,
    Rollback,
    SetMode(QueryMode),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnDefinition {
    pub name: String,
    pub column_type: ColumnType,
    pub primary_key: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SelectStatement {
    pub projections: Vec<SelectExpression>,
    pub table: Option<String>,
    pub filter: Option<Expression>,
    pub order_by: Vec<OrderExpression>,
    pub limit: Option<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SelectExpression {
    Wildcard,
    Column {
        name: String,
        alias: Option<String>,
    },
    Literal {
        value: Literal,
        alias: Option<String>,
    },
    L2Distance {
        column: String,
        vector_literal: String,
        alias: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Expression {
    Column(String),
    Literal(Literal),
    Compare {
        left: String,
        operator: Comparison,
        right: Literal,
    },
    And(Box<Expression>, Box<Expression>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Comparison {
    Equal,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderExpression {
    pub column_or_alias: String,
    pub ascending: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Literal {
    BigInt(i64),
    Varchar(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QueryMode {
    Exact,
    Ann,
}

#[derive(Clone, Debug, PartialEq)]
enum TokenKind {
    Identifier(String),
    Number(String),
    String(String),
    Symbol(char),
    End,
}

#[derive(Clone, Debug, PartialEq)]
struct Token {
    kind: TokenKind,
}

pub fn parse(source: &str) -> Result<Statement, DbError> {
    let tokens = lex(source)?;
    let mut parser = Parser {
        tokens,
        position: 0,
    };
    let statement = parser.parse_statement()?;
    parser.consume_symbol(';');
    if !matches!(parser.peek().kind, TokenKind::End) {
        return Err(parser.syntax("unexpected tokens after statement"));
    }
    Ok(statement)
}

fn lex(source: &str) -> Result<Vec<Token>, DbError> {
    let chars: Vec<char> = source.chars().collect();
    let mut position = 0;
    let mut tokens = Vec::new();

    while position < chars.len() {
        let current = chars[position];
        if current.is_whitespace() {
            position += 1;
            continue;
        }
        if current == '-' && chars.get(position + 1) == Some(&'-') {
            position += 2;
            while position < chars.len() && chars[position] != '\n' {
                position += 1;
            }
            continue;
        }
        if current == '#' {
            while position < chars.len() && chars[position] != '\n' {
                position += 1;
            }
            continue;
        }
        if current == '/' && chars.get(position + 1) == Some(&'*') {
            position += 2;
            let mut closed = false;
            while position + 1 < chars.len() {
                if chars[position] == '*' && chars[position + 1] == '/' {
                    position += 2;
                    closed = true;
                    break;
                }
                position += 1;
            }
            if !closed {
                return Err(DbError::new(
                    ErrorCategory::Syntax,
                    "unterminated block comment",
                ));
            }
            continue;
        }
        if current == '\'' {
            position += 1;
            let mut value = String::new();
            let mut closed = false;
            while position < chars.len() {
                let character = chars[position];
                position += 1;
                if character == '\'' {
                    if chars.get(position) == Some(&'\'') {
                        value.push('\'');
                        position += 1;
                        continue;
                    }
                    closed = true;
                    break;
                }
                if character == '\\' {
                    let escaped = chars.get(position).copied().ok_or_else(|| {
                        DbError::new(ErrorCategory::Syntax, "unterminated string escape")
                    })?;
                    position += 1;
                    value.push(match escaped {
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        '0' => '\0',
                        other => other,
                    });
                } else {
                    value.push(character);
                }
            }
            if !closed {
                return Err(DbError::new(
                    ErrorCategory::Syntax,
                    "unterminated string literal",
                ));
            }
            tokens.push(Token {
                kind: TokenKind::String(value),
            });
            continue;
        }
        if current == '`' {
            position += 1;
            let mut value = String::new();
            let mut closed = false;
            while position < chars.len() {
                let character = chars[position];
                position += 1;
                if character == '`' {
                    if chars.get(position) == Some(&'`') {
                        value.push('`');
                        position += 1;
                        continue;
                    }
                    closed = true;
                    break;
                }
                value.push(character);
            }
            if !closed {
                return Err(DbError::new(
                    ErrorCategory::Syntax,
                    "unterminated quoted identifier",
                ));
            }
            tokens.push(Token {
                kind: TokenKind::Identifier(value),
            });
            continue;
        }
        if current.is_ascii_digit() {
            let start = position;
            while position < chars.len() && chars[position].is_ascii_digit() {
                position += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number(chars[start..position].iter().collect()),
            });
            continue;
        }
        if current.is_alphabetic() || current == '_' {
            let start = position;
            position += 1;
            while position < chars.len()
                && (chars[position].is_alphanumeric() || chars[position] == '_')
            {
                position += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Identifier(chars[start..position].iter().collect()),
            });
            continue;
        }
        if matches!(
            current,
            '(' | ')' | ',' | ';' | '*' | '=' | '<' | '>' | '-' | '.'
        ) {
            let mut symbol = current;
            if current == '<' && chars.get(position + 1) == Some(&'=') {
                symbol = '≤';
                position += 1;
            } else if current == '>' && chars.get(position + 1) == Some(&'=') {
                symbol = '≥';
                position += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Symbol(symbol),
            });
            position += 1;
            continue;
        }
        return Err(DbError::new(
            ErrorCategory::Syntax,
            format!("unexpected character `{current}`"),
        ));
    }

    tokens.push(Token {
        kind: TokenKind::End,
    });
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    position: usize,
}

impl Parser {
    fn parse_statement(&mut self) -> Result<Statement, DbError> {
        if self.consume_keyword("CREATE") {
            if self.consume_keyword("TABLE") {
                return self.parse_create_table();
            }
            if self.consume_keyword("VECTOR") {
                self.expect_keyword("INDEX")?;
                return self.parse_create_vector_index();
            }
            return Err(self.unsupported("only CREATE TABLE and CREATE VECTOR INDEX are supported"));
        }
        if self.consume_keyword("DROP") {
            self.expect_keyword("TABLE")?;
            let if_exists = if self.consume_keyword("IF") {
                self.expect_keyword("EXISTS")?;
                true
            } else {
                false
            };
            return Ok(Statement::DropTable {
                name: self.expect_identifier()?,
                if_exists,
            });
        }
        if self.consume_keyword("INSERT") {
            return self.parse_insert();
        }
        if self.consume_keyword("SELECT") {
            return self.parse_select();
        }
        if self.consume_keyword("UPDATE") {
            return self.parse_update();
        }
        if self.consume_keyword("DELETE") {
            return self.parse_delete();
        }
        if self.consume_keyword("BEGIN") || self.consume_keyword("START") {
            if self.peek_is_keyword("TRANSACTION") {
                self.next();
            }
            return Ok(Statement::Begin);
        }
        if self.consume_keyword("COMMIT") {
            return Ok(Statement::Commit);
        }
        if self.consume_keyword("ROLLBACK") {
            return Ok(Statement::Rollback);
        }
        if self.consume_keyword("SET") {
            return self.parse_set_mode();
        }
        if self.peek_is_keyword("ALTER")
            || self.peek_is_keyword("JOIN")
            || self.peek_is_keyword("NULL")
        {
            return Err(self.unsupported("statement or value is outside MiniVectorDB's SQL subset"));
        }
        Err(self.syntax("expected a supported SQL statement"))
    }

    fn parse_create_table(&mut self) -> Result<Statement, DbError> {
        let if_not_exists = if self.consume_keyword("IF") {
            self.expect_keyword("NOT")?;
            self.expect_keyword("EXISTS")?;
            true
        } else {
            false
        };
        let name = self.expect_identifier()?;
        self.expect_symbol('(')?;
        let mut columns = Vec::new();
        loop {
            if self.consume_keyword("PRIMARY") {
                self.expect_keyword("KEY")?;
                self.expect_symbol('(')?;
                let primary = self.expect_identifier()?;
                self.expect_symbol(')')?;
                let column = columns
                    .iter_mut()
                    .find(|column: &&mut ColumnDefinition| column.name == primary)
                    .ok_or_else(|| {
                        DbError::new(
                            ErrorCategory::ColumnNotFound,
                            format!("primary key column `{primary}` does not exist"),
                        )
                    })?;
                column.primary_key = true;
            } else {
                let column_name = self.expect_identifier()?;
                let type_name = self.expect_identifier()?;
                let column_type = match type_name.to_ascii_uppercase().as_str() {
                    "BIGINT" => ColumnType::BigInt,
                    "VARCHAR" => {
                        let size = self.parse_parenthesized_size()?;
                        ColumnType::Varchar(size)
                    }
                    "VECTOR" => {
                        let size = self.parse_parenthesized_size()?;
                        ColumnType::Vector(size)
                    }
                    _ => {
                        return Err(
                            self.unsupported(format!("column type `{type_name}` is not supported"))
                        );
                    }
                };
                let primary_key = if self.consume_keyword("PRIMARY") {
                    self.expect_keyword("KEY")?;
                    true
                } else {
                    false
                };
                columns.push(ColumnDefinition {
                    name: column_name,
                    column_type,
                    primary_key,
                });
            }
            if self.consume_symbol(',') {
                continue;
            }
            self.expect_symbol(')')?;
            break;
        }
        Ok(Statement::CreateTable {
            name,
            columns,
            if_not_exists,
        })
    }

    fn parse_parenthesized_size(&mut self) -> Result<usize, DbError> {
        self.expect_symbol('(')?;
        let size = self.expect_number()?;
        if size == 0 {
            return Err(self.syntax("type size must be greater than zero"));
        }
        self.expect_symbol(')')?;
        Ok(size as usize)
    }

    fn parse_create_vector_index(&mut self) -> Result<Statement, DbError> {
        let name = self.expect_identifier()?;
        self.expect_keyword("ON")?;
        let table = self.expect_identifier()?;
        self.expect_symbol('(')?;
        let column = self.expect_identifier()?;
        self.expect_symbol(')')?;
        Ok(Statement::CreateVectorIndex {
            name,
            table,
            column,
        })
    }

    fn parse_insert(&mut self) -> Result<Statement, DbError> {
        self.expect_keyword("INTO")?;
        let table = self.expect_identifier()?;
        let columns = if self.consume_symbol('(') {
            let names = self.parse_identifier_list()?;
            self.expect_symbol(')')?;
            Some(names)
        } else {
            None
        };
        self.expect_keyword("VALUES")?;
        let mut rows = Vec::new();
        loop {
            self.expect_symbol('(')?;
            let mut row = Vec::new();
            if !self.peek_symbol(')') {
                loop {
                    row.push(self.parse_literal()?);
                    if !self.consume_symbol(',') {
                        break;
                    }
                }
            }
            self.expect_symbol(')')?;
            rows.push(row);
            if !self.consume_symbol(',') {
                break;
            }
        }
        Ok(Statement::Insert {
            table,
            columns,
            rows,
        })
    }

    fn parse_select(&mut self) -> Result<Statement, DbError> {
        let mut projections = Vec::new();
        loop {
            if self.consume_symbol('*') {
                projections.push(SelectExpression::Wildcard);
            } else if matches!(
                self.peek().kind,
                TokenKind::Number(_) | TokenKind::String(_)
            ) {
                let value = self.parse_literal()?;
                let alias = self.parse_optional_alias()?;
                projections.push(SelectExpression::Literal { value, alias });
            } else {
                let name = self.expect_identifier()?;
                if name.eq_ignore_ascii_case("L2_DISTANCE") && self.consume_symbol('(') {
                    let column = self.expect_identifier()?;
                    self.expect_symbol(',')?;
                    let vector_literal = self.expect_string()?;
                    self.expect_symbol(')')?;
                    let alias = self.parse_optional_alias()?;
                    projections.push(SelectExpression::L2Distance {
                        column,
                        vector_literal,
                        alias,
                    });
                } else {
                    let alias = self.parse_optional_alias()?;
                    projections.push(SelectExpression::Column { name, alias });
                }
            }
            if !self.consume_symbol(',') {
                break;
            }
        }

        let table = if self.consume_keyword("FROM") {
            let table = self.expect_identifier()?;
            if self.peek_is_keyword("JOIN")
                || self.peek_is_keyword("INNER")
                || self.peek_is_keyword("LEFT")
                || self.peek_is_keyword("RIGHT")
            {
                return Err(self.unsupported("JOIN is not supported"));
            }
            Some(table)
        } else {
            None
        };
        let filter = if self.consume_keyword("WHERE") {
            Some(self.parse_expression()?)
        } else {
            None
        };
        let order_by = if self.consume_keyword("ORDER") {
            self.expect_keyword("BY")?;
            let mut items = Vec::new();
            loop {
                let column_or_alias = self.expect_identifier()?;
                let ascending = if self.consume_keyword("ASC") {
                    true
                } else {
                    !self.consume_keyword("DESC")
                };
                items.push(OrderExpression {
                    column_or_alias,
                    ascending,
                });
                if !self.consume_symbol(',') {
                    break;
                }
            }
            items
        } else {
            Vec::new()
        };
        if self.peek_is_keyword("GROUP") || self.peek_is_keyword("HAVING") {
            return Err(self.unsupported("aggregation and grouping are not supported"));
        }
        let limit = if self.consume_keyword("LIMIT") {
            Some(self.expect_number()? as usize)
        } else {
            None
        };
        Ok(Statement::Select(SelectStatement {
            projections,
            table,
            filter,
            order_by,
            limit,
        }))
    }

    fn parse_update(&mut self) -> Result<Statement, DbError> {
        let table = self.expect_identifier()?;
        self.expect_keyword("SET")?;
        let mut assignments = Vec::new();
        loop {
            let column = self.expect_identifier()?;
            self.expect_symbol('=')?;
            assignments.push((column, self.parse_literal()?));
            if !self.consume_symbol(',') {
                break;
            }
        }
        let filter = if self.consume_keyword("WHERE") {
            Some(self.parse_expression()?)
        } else {
            None
        };
        Ok(Statement::Update {
            table,
            assignments,
            filter,
        })
    }

    fn parse_delete(&mut self) -> Result<Statement, DbError> {
        self.expect_keyword("FROM")?;
        let table = self.expect_identifier()?;
        let filter = if self.consume_keyword("WHERE") {
            Some(self.parse_expression()?)
        } else {
            None
        };
        Ok(Statement::Delete { table, filter })
    }

    fn parse_set_mode(&mut self) -> Result<Statement, DbError> {
        let mut name = self.expect_identifier()?;
        if self.consume_symbol('.') {
            name = self.expect_identifier()?;
        }
        self.expect_symbol('=')?;
        let value = self.expect_string()?;
        if !name.eq_ignore_ascii_case("MODE")
            && !name.eq_ignore_ascii_case("QUERY_MODE")
            && !name.eq_ignore_ascii_case("MINIVECTORDDB_QUERY_MODE")
        {
            return Err(self.unsupported(format!("SET variable `{name}` is not supported")));
        }
        match value.to_ascii_lowercase().as_str() {
            "exact" => Ok(Statement::SetMode(QueryMode::Exact)),
            "ann" => Ok(Statement::SetMode(QueryMode::Ann)),
            _ => Err(self.unsupported("query mode must be `exact` or `ann`")),
        }
    }

    fn parse_expression(&mut self) -> Result<Expression, DbError> {
        let mut left = self.parse_comparison()?;
        while self.consume_keyword("AND") {
            let right = self.parse_comparison()?;
            left = Expression::And(Box::new(left), Box::new(right));
        }
        if self.consume_keyword("OR") {
            return Err(self.unsupported("OR is not supported"));
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expression, DbError> {
        if self.consume_symbol('(') {
            let expression = self.parse_expression()?;
            self.expect_symbol(')')?;
            return Ok(expression);
        }
        let left = self.expect_identifier()?;
        let operator = if self.consume_symbol('=') {
            Comparison::Equal
        } else if self.consume_symbol('<') {
            Comparison::Less
        } else if self.consume_symbol('≤') {
            Comparison::LessOrEqual
        } else if self.consume_symbol('>') {
            Comparison::Greater
        } else if self.consume_symbol('≥') {
            Comparison::GreaterOrEqual
        } else {
            return Err(self.syntax("expected comparison operator"));
        };
        Ok(Expression::Compare {
            left,
            operator,
            right: self.parse_literal()?,
        })
    }

    fn parse_literal(&mut self) -> Result<Literal, DbError> {
        if self.peek_is_keyword("NULL") {
            return Err(self.unsupported("NULL is not supported"));
        }
        if self.consume_symbol('-') {
            let value = match self.next().kind {
                TokenKind::Number(value) => value,
                _ => return Err(self.syntax("expected unsigned integer after `-`")),
            };
            return format!("-{value}")
                .parse::<i64>()
                .map(Literal::BigInt)
                .map_err(|_| self.syntax("BIGINT literal is out of range"));
        }
        match self.next().kind {
            TokenKind::Number(value) => value
                .parse::<i64>()
                .map(Literal::BigInt)
                .map_err(|_| self.syntax("BIGINT literal is out of range")),
            TokenKind::String(value) => Ok(Literal::Varchar(value)),
            _ => Err(self.syntax("expected BIGINT or string literal")),
        }
    }

    fn parse_optional_alias(&mut self) -> Result<Option<String>, DbError> {
        if self.consume_keyword("AS") {
            return self.expect_identifier().map(Some);
        }
        if matches!(self.peek().kind, TokenKind::Identifier(_))
            && !self.peek_is_keyword("FROM")
            && !self.peek_is_keyword("WHERE")
            && !self.peek_is_keyword("ORDER")
            && !self.peek_is_keyword("LIMIT")
        {
            return self.expect_identifier().map(Some);
        }
        Ok(None)
    }

    fn parse_identifier_list(&mut self) -> Result<Vec<String>, DbError> {
        let mut names = Vec::new();
        loop {
            names.push(self.expect_identifier()?);
            if !self.consume_symbol(',') {
                break;
            }
        }
        Ok(names)
    }

    fn expect_identifier(&mut self) -> Result<String, DbError> {
        match self.next().kind {
            TokenKind::Identifier(value) => Ok(value),
            _ => Err(self.syntax("expected identifier")),
        }
    }

    fn expect_string(&mut self) -> Result<String, DbError> {
        match self.next().kind {
            TokenKind::String(value) => Ok(value),
            _ => Err(self.syntax("expected string literal")),
        }
    }

    fn expect_number(&mut self) -> Result<u64, DbError> {
        match self.next().kind {
            TokenKind::Number(value) => value
                .parse::<u64>()
                .map_err(|_| self.syntax("unsigned integer is out of range")),
            _ => Err(self.syntax("expected unsigned integer")),
        }
    }

    fn expect_keyword(&mut self, keyword: &str) -> Result<(), DbError> {
        if self.consume_keyword(keyword) {
            Ok(())
        } else {
            Err(self.syntax(format!("expected keyword {keyword}")))
        }
    }

    fn expect_symbol(&mut self, symbol: char) -> Result<(), DbError> {
        if self.consume_symbol(symbol) {
            Ok(())
        } else {
            Err(self.syntax(format!("expected `{symbol}`")))
        }
    }

    fn consume_keyword(&mut self, keyword: &str) -> bool {
        if self.peek_is_keyword(keyword) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn consume_symbol(&mut self, symbol: char) -> bool {
        if self.peek_symbol(symbol) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn peek_is_keyword(&self, keyword: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Identifier(value) if value.eq_ignore_ascii_case(keyword))
    }

    fn peek_symbol(&self, symbol: char) -> bool {
        matches!(self.peek().kind, TokenKind::Symbol(value) if value == symbol)
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.position]
    }

    fn next(&mut self) -> Token {
        let token = self.tokens[self.position].clone();
        if !matches!(token.kind, TokenKind::End) {
            self.position += 1;
        }
        token
    }

    fn syntax(&self, message: impl Into<String>) -> DbError {
        DbError::new(ErrorCategory::Syntax, message)
    }

    fn unsupported(&self, message: impl Into<String>) -> DbError {
        DbError::new(ErrorCategory::Unsupported, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_comparison_operators() {
        for operator in ["=", "<", "<=", ">", ">="] {
            parse(&format!("SELECT id FROM t WHERE id {operator} 1")).unwrap();
        }
    }
}
