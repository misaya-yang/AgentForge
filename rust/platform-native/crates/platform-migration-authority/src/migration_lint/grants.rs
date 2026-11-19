use super::approved_027_grants::APPROVED_027_ROLE_GRANTS;
use super::sql_lexer::{Token, TokenKind, split_statements, tokenize};
use super::{
    APPROVED_OWNER_PRECONDITION, APPROVED_ROLE_GRANTS, ChangeSpec, ROLE_PREFIX_REGEX, Result,
};

fn token_signature(tokens: &[Token]) -> Vec<String> {
    tokens
        .iter()
        .map(|token| match &token.kind {
            TokenKind::Word(value) => format!("W:{value}"),
            TokenKind::QuotedIdentifier(value) => format!("Q:{value}"),
            TokenKind::Literal(value) => format!("L:{value}"),
            TokenKind::Body(value) => format!("B:{value}"),
            TokenKind::Symbol(value) => format!("S:{value}"),
        })
        .collect()
}

fn has_verified_owner_precondition(change: &ChangeSpec) -> bool {
    let Ok(expected) = tokenize(APPROVED_OWNER_PRECONDITION) else {
        return false;
    };
    let expected = token_signature(&expected);
    change.preconditions.iter().any(|condition| {
        tokenize(condition)
            .ok()
            .is_some_and(|tokens| token_signature(&tokens) == expected)
    })
}

fn normalized_grant_sql(sql: &str) -> String {
    sql.chars()
        .filter(|character| !character.is_ascii_whitespace())
        .flat_map(char::to_uppercase)
        .collect()
}

fn approved_grant_template(sql: &str, suffix: &str, service_owned: bool) -> bool {
    if sql.matches("%I").count() != 1 || sql.matches('%').count() != 1 {
        return false;
    }
    let normalized = normalized_grant_sql(sql);
    APPROVED_ROLE_GRANTS
        .iter()
        .chain(APPROVED_027_ROLE_GRANTS.iter())
        .chain(
            super::approved_service_grants::APPROVED_SERVICE_ROLE_GRANTS
                .iter()
                .filter(|_| service_owned),
        )
        .any(|(allowed, template)| {
            *allowed == suffix && normalized_grant_sql(template) == normalized
        })
}

pub(super) fn word_is(token: &Token, expected: &str) -> bool {
    token.word() == Some(expected)
}

fn symbol_is(token: &Token, expected: char) -> bool {
    token.symbol(expected)
}

fn literal_is(token: &Token, expected: &str) -> bool {
    matches!(&token.kind, TokenKind::Literal(value) if value == expected)
}

fn is_prefix_declaration(tokens: &[Token]) -> bool {
    tokens.len() == 19
        && word_is(&tokens[0], "DECLARE")
        && word_is(&tokens[1], "PREFIX")
        && word_is(&tokens[2], "TEXT")
        && symbol_is(&tokens[3], ':')
        && symbol_is(&tokens[4], '=')
        && word_is(&tokens[5], "LEFT")
        && symbol_is(&tokens[6], '(')
        && word_is(&tokens[7], "CURRENT_USER")
        && symbol_is(&tokens[8], ',')
        && word_is(&tokens[9], "LENGTH")
        && symbol_is(&tokens[10], '(')
        && word_is(&tokens[11], "CURRENT_USER")
        && symbol_is(&tokens[12], ')')
        && symbol_is(&tokens[13], '-')
        && word_is(&tokens[14], "LENGTH")
        && symbol_is(&tokens[15], '(')
        && literal_is(&tokens[16], "owner")
        && symbol_is(&tokens[17], ')')
        && symbol_is(&tokens[18], ')')
}

fn is_principal_declaration(tokens: &[Token]) -> bool {
    tokens.len() == 2 && word_is(&tokens[0], "PRINCIPAL") && word_is(&tokens[1], "TEXT")
}

fn is_fixed_owner_prefix_guard(tokens: &[Token]) -> bool {
    // These two additional guards occur in the measured fixed027 sources.
    // Only the inert error-message literal can vary; all executable tokens match.
    const HEADS: &[&str] = &[
        "IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'guard'",
        r"IF current_user NOT LIKE '%\_owner' ESCAPE '\' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'guard'",
    ];
    HEADS.iter().any(|head| {
        let Ok(expected) = tokenize(head) else {
            return false;
        };
        tokens.len() == expected.len()
            && matches!(tokens.last().map(|t| &t.kind), Some(TokenKind::Literal(_)))
            && token_signature(&tokens[..tokens.len() - 1])
                == token_signature(&expected[..expected.len() - 1])
    })
}

fn is_prefix_guard(tokens: &[Token]) -> bool {
    is_fixed_owner_prefix_guard(tokens)
        || (tokens.len() == 9
            && word_is(&tokens[0], "IF")
            && word_is(&tokens[1], "PREFIX")
            && symbol_is(&tokens[2], '!')
            && symbol_is(&tokens[3], '~')
            && literal_is(&tokens[4], ROLE_PREFIX_REGEX)
            && word_is(&tokens[5], "THEN")
            && word_is(&tokens[6], "RAISE")
            && word_is(&tokens[7], "EXCEPTION")
            && matches!(&tokens[8].kind, TokenKind::Literal(_)))
}

fn begins_with_prefix_guard(tokens: &[Token]) -> bool {
    tokens.first().is_some_and(|token| word_is(token, "BEGIN")) && is_prefix_guard(&tokens[1..])
}

fn is_principal_assignment(tokens: &[Token]) -> bool {
    tokens.len() == 7
        && word_is(&tokens[0], "PRINCIPAL")
        && symbol_is(&tokens[1], ':')
        && symbol_is(&tokens[2], '=')
        && word_is(&tokens[3], "PREFIX")
        && symbol_is(&tokens[4], '|')
        && symbol_is(&tokens[5], '|')
        && literal_is(&tokens[6], "gateway")
}

fn approved_execute_format(
    tokens: &[Token],
    principal_is_gateway: bool,
    service_owned: bool,
) -> bool {
    if tokens.len() < 7
        || !word_is(&tokens[0], "EXECUTE")
        || !word_is(&tokens[1], "FORMAT")
        || !symbol_is(&tokens[2], '(')
        || !matches!(&tokens[3].kind, TokenKind::Literal(_))
    {
        return false;
    }
    let Some(format_sql) = tokens[3].literal() else {
        return false;
    };
    if !symbol_is(&tokens[4], ',') || !symbol_is(tokens.last().expect("nonempty"), ')') {
        return false;
    }
    if tokens.len() == 7 && word_is(&tokens[5], "PRINCIPAL") && principal_is_gateway {
        return approved_grant_template(format_sql, "gateway", service_owned);
    }
    tokens.len() == 10
        && word_is(&tokens[5], "PREFIX")
        && symbol_is(&tokens[6], '|')
        && symbol_is(&tokens[7], '|')
        && matches!(&tokens[8].kind, TokenKind::Literal(value) if approved_grant_template(format_sql, value, service_owned))
}

pub(super) fn is_approved_role_prefix_grant_body(body: &str, change: &ChangeSpec) -> Result<bool> {
    if !has_verified_owner_precondition(change) {
        return Ok(false);
    }
    let statements = split_statements(body)?;
    if statements.len() < 4 || !is_prefix_declaration(&statements[0]) {
        return Ok(false);
    }
    let mut index = 1;
    let mut principal_declared = false;
    if is_principal_declaration(&statements[index]) {
        principal_declared = true;
        index += 1;
    }
    if statements
        .get(index)
        .is_some_and(|tokens| tokens.len() == 1 && word_is(&tokens[0], "BEGIN"))
    {
        index += 1;
        if !statements
            .get(index)
            .is_some_and(|tokens| is_prefix_guard(tokens))
        {
            return Ok(false);
        }
        index += 1;
    } else if statements
        .get(index)
        .is_some_and(|tokens| begins_with_prefix_guard(tokens))
    {
        index += 1;
    } else {
        return Ok(false);
    }
    if !statements.get(index).is_some_and(|tokens| {
        tokens.len() == 2 && word_is(&tokens[0], "END") && word_is(&tokens[1], "IF")
    }) {
        return Ok(false);
    }
    index += 1;
    let mut principal_is_gateway = false;
    let mut execute_count = 0usize;
    for (offset, statement) in statements[index..].iter().enumerate() {
        if statement.len() == 1 && word_is(&statement[0], "END") {
            return Ok(execute_count > 0 && index + offset + 1 == statements.len());
        }
        if is_principal_assignment(statement) {
            if !principal_declared || principal_is_gateway {
                return Ok(false);
            }
            principal_is_gateway = true;
            continue;
        }
        if approved_execute_format(
            statement,
            principal_is_gateway,
            ai_platform_migration_authority::source_tools::service_successors::recognizes_change(
                change,
            ),
        ) {
            execute_count += 1;
            continue;
        }
        return Ok(false);
    }
    Ok(false)
}
