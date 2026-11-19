use super::{Result, refused};

#[derive(Clone, Debug)]
pub(super) enum TokenKind {
    Word(String),
    QuotedIdentifier(String),
    Literal(String),
    Body(String),
    Symbol(char),
}

#[derive(Clone, Debug)]
pub(super) struct Token {
    pub(super) kind: TokenKind,
}

impl Token {
    pub(super) fn word(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Word(word) => Some(word),
            _ => None,
        }
    }

    pub(super) fn name(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Word(name) | TokenKind::QuotedIdentifier(name) => Some(name),
            _ => None,
        }
    }

    pub(super) fn literal(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Literal(value) | TokenKind::Body(value) => Some(value),
            _ => None,
        }
    }

    pub(super) fn symbol(&self, expected: char) -> bool {
        matches!(&self.kind, TokenKind::Symbol(value) if *value == expected)
    }
}

pub(super) fn tokenize(sql: &str) -> Result<Vec<Token>> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'-' && bytes.get(index + 1) == Some(&b'-') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            let mut depth = 1usize;
            while index < bytes.len() && depth > 0 {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    depth += 1;
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            if depth != 0 {
                return Err(refused("migration SQL has an unterminated block comment"));
            }
            continue;
        }
        if byte == b'\'' {
            let escaped = index >= 1
                && matches!(bytes[index - 1], b'e' | b'E')
                && (index < 2 || !is_identifier_byte(bytes[index - 2]));
            let (value, next) = quoted_value(bytes, index, b'\'', escaped)?;
            tokens.push(Token {
                kind: TokenKind::Literal(value),
            });
            index = next;
            continue;
        }
        if byte == b'"' {
            let (value, next) = quoted_value(bytes, index, b'"', false)?;
            tokens.push(Token {
                kind: TokenKind::QuotedIdentifier(value),
            });
            index = next;
            continue;
        }
        if byte == b'$' {
            if let Some(tag_end) = dollar_tag_end(bytes, index) {
                let tag = &sql[index..tag_end];
                let body_start = tag_end;
                let body_end = sql[body_start..]
                    .find(tag)
                    .map(|offset| body_start + offset)
                    .ok_or_else(|| refused("migration SQL has an unterminated dollar body"))?;
                tokens.push(Token {
                    kind: TokenKind::Body(sql[body_start..body_end].to_owned()),
                });
                index = body_end + tag.len();
                continue;
            }
        }
        if is_identifier_start(byte) {
            let start = index;
            index += 1;
            while index < bytes.len() && is_identifier_byte(bytes[index]) {
                index += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Word(sql[start..index].to_ascii_uppercase()),
            });
            continue;
        }
        tokens.push(Token {
            kind: TokenKind::Symbol(byte as char),
        });
        index += 1;
    }
    Ok(tokens)
}

fn quoted_value(bytes: &[u8], start: usize, quote: u8, escapes: bool) -> Result<(String, usize)> {
    let mut index = start + 1;
    let mut output = Vec::new();
    while index < bytes.len() {
        if escapes && bytes[index] == b'\\' && index + 1 < bytes.len() {
            output.push(bytes[index + 1]);
            index += 2;
        } else if bytes[index] == quote {
            if bytes.get(index + 1) == Some(&quote) {
                output.push(quote);
                index += 2;
            } else {
                let value = String::from_utf8(output)
                    .map_err(|_| refused("migration SQL contains invalid quoted text"))?;
                return Ok((value, index + 1));
            }
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    Err(refused("migration SQL has an unterminated quoted value"))
}

fn dollar_tag_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut end = start + 1;
    while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
        end += 1;
    }
    if bytes.get(end) != Some(&b'$') {
        return None;
    }
    if end > start + 1 && !is_identifier_start(bytes[start + 1]) {
        return None;
    }
    Some(end + 1)
}

fn is_identifier_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

fn is_identifier_byte(byte: u8) -> bool {
    is_identifier_start(byte) || byte.is_ascii_digit() || byte == b'$'
}

pub(super) fn split_statements(sql: &str) -> Result<Vec<Vec<Token>>> {
    let mut statements = Vec::new();
    let mut current = Vec::new();
    for token in tokenize(sql)? {
        if token.symbol(';') {
            if !current.is_empty() {
                statements.push(std::mem::take(&mut current));
            }
        } else {
            current.push(token);
        }
    }
    if !current.is_empty() {
        statements.push(current);
    }
    Ok(statements)
}

pub(super) fn words(statement: &[Token]) -> Vec<String> {
    statement
        .iter()
        .filter_map(|token| token.word().map(str::to_owned))
        .collect()
}

pub(super) fn ddl_words(statement: &[Token]) -> Vec<String> {
    statement
        .iter()
        .filter_map(|token| match &token.kind {
            TokenKind::Word(word) => Some(word.clone()),
            TokenKind::QuotedIdentifier(name) => Some(format!("\0{name}")),
            TokenKind::Literal(_) | TokenKind::Body(_) | TokenKind::Symbol(_) => None,
        })
        .collect()
}
