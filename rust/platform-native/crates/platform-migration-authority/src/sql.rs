//! Lexical transaction guard; SQL function bodies and quoted strings are inert.
use crate::{AuthorityError, Result};
use regex::Regex;
use std::sync::LazyLock;
static TRANSACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(BEGIN\b|START\s+TRANSACTION\b|COMMIT\b|END\b|ROLLBACK\b|SAVEPOINT\b|RELEASE(?:\s+SAVEPOINT)?\b|ABORT\b|PREPARE\s+TRANSACTION\b|SET\s+TRANSACTION\b|SET\s+SESSION\s+CHARACTERISTICS\s+AS\s+TRANSACTION\b)(?:\s+.*)?$").unwrap()
});
static DOLLAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\$(?:[A-Za-z_][A-Za-z0-9_]*)?\$").unwrap());

pub fn strip_sql_bodies(sql: &str) -> Result<String> {
    let bytes = sql.as_bytes();
    let mut masked = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"--") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if depth != 0 {
                return Err(AuthorityError::Refused(
                    "SQL contains an unterminated block comment".into(),
                ));
            }
        } else if matches!(bytes[i], b'\'' | b'"') {
            let quote = bytes[i];
            let escape = quote == b'\''
                && i > 0
                && matches!(bytes[i - 1], b'e' | b'E')
                && !sql[..i - 1]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_');
            i += 1;
            let mut closed = false;
            while i < bytes.len() {
                if bytes[i] == quote {
                    i += 1;
                    if i < bytes.len() && bytes[i] == quote {
                        i += 1;
                        continue;
                    }
                    closed = true;
                    break;
                }
                if escape && bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if !closed {
                return Err(AuthorityError::Refused(
                    "SQL contains an unterminated quoted string".into(),
                ));
            }
        } else if bytes[i] == b'$' {
            if let Some(delimiter) = DOLLAR.find(&sql[i..]) {
                let delimiter = delimiter.as_str();
                let body_start = i + delimiter.len();
                let end = sql[body_start..].find(delimiter).ok_or_else(|| {
                    AuthorityError::Refused("SQL contains an unterminated dollar quote".into())
                })?;
                i = body_start + end + delimiter.len();
            } else {
                i += 1;
                continue;
            }
        } else {
            i += 1;
            continue;
        }
        masked[start..i].fill(b' ');
    }
    // All masked spans are complete strings/comments; untouched UTF-8 is valid.
    String::from_utf8(masked).map_err(|_| AuthorityError::Refused("invalid SQL encoding".into()))
}
pub fn reject_embedded_transactions(sql: &str, name: &str) -> Result<()> {
    for statement in strip_sql_bodies(sql)?.split(';') {
        let normalized = statement.split_whitespace().collect::<Vec<_>>().join(" ");
        if TRANSACTION.is_match(&normalized) {
            return Err(AuthorityError::Refused(format!(
                "change {name} contains transaction control; the runner owns transactions"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transaction_guard_preserves_real_boundaries() {
        for sql in [
            "BEGIN;",
            "-- $$\nCOMMIT;",
            "SELECT '$$';ROLLBACK;",
            "/* outer /* inner */ */ START TRANSACTION;",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY;",
        ] {
            assert!(reject_embedded_transactions(sql, "test").is_err(), "{sql}");
        }
        for sql in [
            "DO $$ BEGIN NULL; END $$;",
            "SELECT 'BEGIN;COMMIT';",
            "SELECT E'\\\'BEGIN;';",
            "CREATE TABLE a (b text);",
        ] {
            assert!(reject_embedded_transactions(sql, "test").is_ok(), "{sql}");
        }
        for sql in ["SELECT '", "/* missing", "DO $body$ missing"] {
            assert!(strip_sql_bodies(sql).is_err());
        }
    }
}
