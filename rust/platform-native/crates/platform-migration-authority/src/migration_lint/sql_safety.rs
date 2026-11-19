use super::grants::word_is;
use super::sql_lexer::{Token, TokenKind, ddl_words, tokenize, words};
use super::{ChangeSpec, Finding, Result, TransactionMode, push_finding};

pub(super) fn canonical_identifier(token: &Token) -> Option<String> {
    match &token.kind {
        TokenKind::Word(name) => Some(name.to_ascii_lowercase()),
        TokenKind::QuotedIdentifier(name) if name == &name.to_ascii_lowercase() => {
            Some(name.clone())
        }
        TokenKind::QuotedIdentifier(name) => Some(format!("\0{name}")),
        _ => None,
    }
}

pub(super) fn qualified_relation_at(tokens: &[Token], mut index: usize) -> Option<(String, usize)> {
    if tokens
        .get(index)
        .is_some_and(|token| word_is(token, "ONLY"))
    {
        index += 1;
    }
    let schema = canonical_identifier(tokens.get(index)?)?;
    if !tokens.get(index + 1).is_some_and(|token| token.symbol('.')) {
        return None;
    }
    let relation = canonical_identifier(tokens.get(index + 2)?)?;
    Some((format!("{schema}.{relation}"), index + 3))
}

pub(super) fn newly_created_table(statement: &[Token], change: &ChangeSpec) -> Option<String> {
    if change.transaction_mode != TransactionMode::Transactional
        || !statement
            .first()
            .is_some_and(|token| word_is(token, "CREATE"))
        || !statement
            .get(1)
            .is_some_and(|token| word_is(token, "TABLE"))
    {
        return None;
    }
    let (relation, after_name) = qualified_relation_at(statement, 2)?;
    // Only ordinary CREATE TABLE with a declared column list establishes an
    // empty table. IF NOT EXISTS and CREATE TABLE AS are deliberately excluded.
    statement
        .get(after_name)
        .is_some_and(|token| token.symbol('('))
        .then_some(relation)
}

pub(super) fn index_target_relation(statement: &[Token]) -> Option<String> {
    let index_word = statement.iter().position(|token| word_is(token, "INDEX"))?;
    let on_word = statement
        .iter()
        .enumerate()
        .skip(index_word + 1)
        .find(|(_, token)| token.word() == Some("ON"))
        .map(|(index, _)| index)?;
    qualified_relation_at(statement, on_word + 1).map(|(relation, _)| relation)
}

pub(super) fn plain_index_targets_new_empty_table(
    statements: &[Vec<Token>],
    statement_index: usize,
    change: &ChangeSpec,
) -> bool {
    if statement_index == 0 || change.transaction_mode != TransactionMode::Transactional {
        return false;
    }
    let Some(target) = index_target_relation(&statements[statement_index]) else {
        return false;
    };
    let mut cursor = statement_index;
    while cursor > 0 {
        cursor -= 1;
        let previous = &statements[cursor];
        let previous_words = ddl_words(previous);
        let previous_is_plain_index = starts_with(&previous_words, &["CREATE"])
            && previous_words.iter().any(|word| word == "INDEX")
            && !previous_words.iter().any(|word| word == "CONCURRENTLY")
            && index_target_relation(previous).as_deref() == Some(target.as_str());
        if previous_is_plain_index {
            continue;
        }
        return newly_created_table(previous, change).as_deref() == Some(target.as_str());
    }
    false
}

pub(super) fn has_phrase(words: &[String], phrase: &[&str]) -> bool {
    words
        .windows(phrase.len())
        .any(|window| window.iter().map(String::as_str).eq(phrase.iter().copied()))
}

pub(super) fn starts_with(words: &[String], phrase: &[&str]) -> bool {
    words.len() >= phrase.len()
        && words
            .iter()
            .take(phrase.len())
            .map(String::as_str)
            .eq(phrase.iter().copied())
}

pub(super) fn changes_authorization_identity(statement: &[Token]) -> bool {
    let words = words(statement);
    words.iter().enumerate().any(|(index, word)| {
        if word == "RESET"
            && (words.get(index + 1).is_some_and(|next| next == "ROLE")
                || (words.get(index + 1).is_some_and(|next| next == "SESSION")
                    && words
                        .get(index + 2)
                        .is_some_and(|next| next == "AUTHORIZATION")))
        {
            return true;
        }
        if word != "SET" {
            return false;
        }
        let mut cursor = index + 1;
        if words
            .get(cursor)
            .is_some_and(|next| next == "LOCAL" || next == "SESSION")
        {
            if words.get(cursor).is_some_and(|next| next == "SESSION")
                && words
                    .get(cursor + 1)
                    .is_some_and(|next| next == "AUTHORIZATION")
            {
                return true;
            }
            cursor += 1;
        }
        words.get(cursor).is_some_and(|next| next == "ROLE")
    })
}

pub(super) fn controls_transaction_boundary(
    statement: &[Token],
    allow_plpgsql_begin: bool,
) -> bool {
    let words = words(statement);
    words.iter().enumerate().any(|(index, word)| {
        if matches!(
            word.as_str(),
            "COMMIT" | "ROLLBACK" | "SAVEPOINT" | "RELEASE"
        ) {
            return true;
        }
        if word == "BEGIN" {
            return !allow_plpgsql_begin && index == 0;
        }
        word == "START"
            && words
                .get(index + 1)
                .is_some_and(|next| next == "TRANSACTION")
    })
}

pub(super) fn changes_search_path(statement: &[Token]) -> bool {
    let words = words(statement);
    words.iter().enumerate().any(|(index, word)| {
        if word != "SET" && word != "RESET" {
            return false;
        }
        let mut cursor = index + 1;
        if words
            .get(cursor)
            .is_some_and(|next| next == "LOCAL" || next == "SESSION")
        {
            cursor += 1;
        }
        words.get(cursor).is_some_and(|next| next == "SEARCH_PATH")
    })
}

pub(super) fn starts_constraint_kind(words: &[String]) -> bool {
    matches!(
        words.first().map(String::as_str),
        Some("CHECK" | "UNIQUE" | "EXCLUDE")
    ) || starts_with(words, &["FOREIGN", "KEY"])
        || starts_with(words, &["PRIMARY", "KEY"])
}

pub(super) fn column_default_may_rewrite(statement: &[Token]) -> bool {
    let Some(index) = statement
        .iter()
        .position(|token| token.word() == Some("DEFAULT"))
    else {
        return false;
    };
    let Some(value) = statement.get(index + 1) else {
        return true;
    };
    match &value.kind {
        TokenKind::Literal(_) | TokenKind::Body(_) => false,
        TokenKind::Word(word) if matches!(word.as_str(), "NULL" | "TRUE" | "FALSE") => false,
        TokenKind::Symbol(byte) if byte.is_ascii_digit() => false,
        TokenKind::Symbol('-') => !statement.get(index + 2).is_some_and(
            |next| matches!(&next.kind, TokenKind::Symbol(byte) if byte.is_ascii_digit()),
        ),
        _ => true,
    }
}

pub(super) fn timeout_override(statement: &[Token], words: &[String]) -> Option<&'static str> {
    for (index, word) in words.iter().enumerate() {
        if word == "SET" || word == "RESET" {
            let mut cursor = index + 1;
            if words
                .get(cursor)
                .is_some_and(|value| value == "LOCAL" || value == "SESSION")
            {
                cursor += 1;
            }
            if matches!(words.get(cursor).map(String::as_str), Some("LOCK_TIMEOUT")) {
                return Some("migration_sql_overrides_lock_timeout");
            }
            if matches!(
                words.get(cursor).map(String::as_str),
                Some("STATEMENT_TIMEOUT")
            ) {
                return Some("migration_sql_overrides_statement_timeout");
            }
            if words.get(cursor).is_some_and(|value| value == "ALL") && word == "RESET" {
                return Some("migration_sql_resets_authority_timeouts");
            }
        }
        if word == "DISCARD" && words.get(index + 1).is_some_and(|value| value == "ALL") {
            return Some("migration_sql_resets_authority_timeouts");
        }
    }
    for (index, token) in statement.iter().enumerate() {
        if !token
            .name()
            .is_some_and(|name| name.eq_ignore_ascii_case("set_config"))
        {
            continue;
        }
        if !statement
            .get(index + 1)
            .is_some_and(|next| next.symbol('('))
        {
            return Some("migration_sql_dynamically_controls_authority_timeouts");
        }
        let mut cursor = index + 2;
        let first_argument_start = cursor;
        let mut nesting = 0usize;
        while cursor < statement.len() {
            if statement[cursor].symbol('(') {
                nesting += 1;
            } else if statement[cursor].symbol(')') {
                if nesting == 0 {
                    break;
                }
                nesting -= 1;
            } else if statement[cursor].symbol(',') && nesting == 0 {
                break;
            }
            cursor += 1;
        }
        if cursor == statement.len() || cursor == first_argument_start {
            return Some("migration_sql_dynamically_controls_authority_timeouts");
        }
        let first_argument = &statement[first_argument_start..cursor];
        if first_argument.len() != 1 {
            return Some("migration_sql_dynamically_controls_authority_timeouts");
        }
        if let Some(setting) = first_argument[0].literal() {
            if setting.eq_ignore_ascii_case("lock_timeout") {
                return Some("migration_sql_overrides_lock_timeout");
            }
            if setting.eq_ignore_ascii_case("statement_timeout") {
                return Some("migration_sql_overrides_statement_timeout");
            }
            if setting.eq_ignore_ascii_case("role")
                || setting.eq_ignore_ascii_case("session_authorization")
            {
                return Some("migration_sql_cannot_change_authorization_identity");
            }
            if setting.eq_ignore_ascii_case("search_path") {
                return Some("migration_sql_cannot_change_search_path");
            }
        } else {
            return Some("migration_sql_dynamically_controls_authority_timeouts");
        }
    }
    for (index, token) in statement.iter().enumerate() {
        if let TokenKind::QuotedIdentifier(name) = &token.kind {
            if matches!(
                name.to_ascii_uppercase().as_str(),
                "LOCK_TIMEOUT" | "STATEMENT_TIMEOUT"
            ) && statement
                .iter()
                .take(index)
                .any(|previous| previous.word() == Some("SET") || previous.word() == Some("RESET"))
            {
                return Some("migration_sql_overrides_authority_timeout");
            }
        }
    }
    None
}

pub(super) fn index_name(words: &[String], index_word: usize) -> Option<String> {
    let mut cursor = index_word + 1;
    while matches!(
        words.get(cursor).map(String::as_str),
        Some("CONCURRENTLY" | "IF" | "NOT" | "EXISTS")
    ) {
        cursor += 1;
    }
    words.get(cursor).cloned()
}

pub(super) fn split_timeout_override_in_body(
    body: &str,
    statement_index: usize,
    allow_role_prefix_grants: bool,
    findings: &mut Vec<Finding>,
) -> Result<()> {
    let nested = tokenize(body)?;
    let nested_words = words(&nested);
    if let Some(code) = timeout_override(&nested, &nested_words) {
        push_finding(findings, "blocked", code, statement_index);
    }
    if nested_words.iter().any(|word| word == "EXECUTE") && !allow_role_prefix_grants {
        push_finding(
            findings,
            "blocked",
            "dynamic_sql_inside_migration_body_is_not_reviewable",
            statement_index,
        );
    }
    Ok(())
}
