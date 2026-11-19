use serde_json::Value;
#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    depth: i32,
}
/// Lexer for compiled native statements; strings/comments are opaque. Unknown
/// syntax is rejected before any enabled-family mutation, never sent as legacy.
fn tokens(sql: &str) -> Result<Vec<Token<'_>>, &'static str> {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut depth = 0;
    let mut out = vec![];
    while i < b.len() {
        if b[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if b[i..].starts_with(b"--") {
            i += 2;
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if b[i..].starts_with(b"/*") {
            let mut level = 1;
            i += 2;
            while i < b.len() && level > 0 {
                if b[i..].starts_with(b"/*") {
                    level += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    level -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if level != 0 {
                return Err("unclosed SQL comment");
            }
            continue;
        }
        if b[i] == b'\'' || b[i] == b'"' {
            let quote = b[i];
            i += 1;
            loop {
                if i >= b.len() {
                    return Err("unclosed SQL quote");
                }
                if b[i] == quote {
                    i += 1;
                    if i < b.len() && b[i] == quote {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
        } else if b[i] == b'$' && i + 1 < b.len() && !b[i + 1].is_ascii_digit() {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            if i >= b.len() || b[i] != b'$' {
                return Err("invalid SQL dollar quote");
            }
            i += 1;
            let tag = &sql[start..i];
            let n = sql[i..].find(tag).ok_or("unclosed SQL dollar quote")?;
            i += n + tag.len();
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$' {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else {
            // Byte slicing must never bisect an unsupported UTF-8 identifier.
            // Quoted strings/identifiers and comments remain opaque UTF-8.
            if !b[i].is_ascii() {
                return Err("unsupported unquoted SQL identifier");
            }
            i += 1;
        }
        let text = &sql[start..i];
        if text.starts_with('$')
            && text.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
            && (!text[1..].bytes().all(|c| c.is_ascii_digit())
                || !text[1..].parse::<u16>().is_ok_and(|n| n > 0))
        {
            return Err("invalid SQL parameter");
        }
        if text == ")" {
            depth -= 1;
            if depth < 0 {
                return Err("unbalanced SQL parentheses");
            }
        }
        out.push(Token {
            text,
            start,
            end: i,
            depth,
        });
        if text == "(" {
            depth += 1;
        }
    }
    if depth != 0 {
        return Err("unbalanced SQL parentheses");
    }
    Ok(out)
}
fn is(t: &Token<'_>, word: &str) -> bool {
    t.text.eq_ignore_ascii_case(word)
}
fn literal(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}
fn identifier(s: &str) -> bool {
    s.bytes()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == b'_')
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}
/// PostgreSQL folds only unquoted identifiers. Quotes preserve case; accepting
/// a differently cased quoted relation would select a different real table.
fn relation_identifier(token: &Token<'_>) -> Option<String> {
    let name = if let Some(inner) = token
        .text
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
    {
        inner.replace("\"\"", "\"")
    } else {
        token.text.to_ascii_lowercase()
    };
    identifier(&name).then_some(name)
}
fn relation_name(tokens: &[Token<'_>]) -> Option<String> {
    match tokens {
        [table] => relation_identifier(table),
        [schema, dot, table] if dot.text == "." => Some(format!(
            "{}.{}",
            relation_identifier(schema)?,
            relation_identifier(table)?
        )),
        _ => None,
    }
}
#[derive(Debug)]
pub struct Statement {
    pub family: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub tail: String,
    pub returning: String,
}
pub fn parse_statement(family: &str, sql: &str) -> Result<Statement, &'static str> {
    if !super::super::append::FAMILIES.contains(&family) {
        return Err("unknown append family");
    }
    let ts = tokens(sql)?;
    if ts.len() < 8 || !is(&ts[0], "insert") || !is(&ts[1], "into") {
        return Err("append adapter requires INSERT VALUES");
    }
    let open = ts
        .iter()
        .position(|t| t.text == "(")
        .ok_or("missing insert columns")?;
    let table = relation_name(&ts[2..open]).ok_or("invalid append table identifier")?;
    if table != family && table != family.split('.').next_back().unwrap() {
        return Err("append family/table mismatch");
    }
    let close = (open + 1..ts.len())
        .find(|&i| ts[i].text == ")" && ts[i].depth == 0)
        .ok_or("missing column end")?;
    // Use lexer boundaries rather than splitting raw text: nested comments,
    // whitespace and quoted identifiers cannot rename/duplicate a column.
    let mut columns = Vec::new();
    for (index, token) in ts[open + 1..close].iter().enumerate() {
        if index % 2 == 1 {
            if token.text != "," || token.depth != 1 {
                return Err("invalid append column separator");
            }
        } else {
            let column = relation_identifier(token).ok_or("invalid append columns")?;
            if token.depth != 1 || columns.contains(&column) {
                return Err("invalid append columns");
            }
            columns.push(column);
        }
    }
    if columns.is_empty() || (close - open) % 2 != 0 {
        return Err("invalid append columns");
    }
    if !ts.get(close + 1).is_some_and(|token| is(token, "values")) {
        return Err("append adapter requires VALUES, not SELECT");
    }
    let conflict = (close + 2..ts.len())
        .find(|&i| {
            ts[i].depth == 0 && is(&ts[i], "on") && ts.get(i + 1).is_some_and(|t| is(t, "conflict"))
        })
        .ok_or("append adapter requires ON CONFLICT")?;
    let returning = (conflict + 2..ts.len()).find(|&i| ts[i].depth == 0 && is(&ts[i], "returning"));
    if ts.iter().any(|t| t.text == ";") {
        return Err("multiple/trailing SQL statements forbidden");
    }
    let mut rows = vec![];
    let mut p = close + 2;
    while p < conflict {
        if ts[p].text != "(" || ts[p].depth != 0 {
            return Err("malformed append VALUES");
        }
        let end = (p + 1..conflict)
            .find(|&i| ts[i].text == ")" && ts[i].depth == 0)
            .ok_or("malformed append VALUES end")?;
        let mut expressions = vec![];
        let mut start = ts[p].end;
        for t in &ts[p + 1..end] {
            if t.text == "," && t.depth == 1 {
                expressions.push(sql[start..t.start].trim().to_owned());
                start = t.end;
            }
        }
        expressions.push(sql[start..ts[end].start].trim().to_owned());
        if expressions.len() != columns.len()
            || expressions.iter().any(|x| match tokens(x) {
                Ok(ts) => ts.is_empty() || (ts.len() == 1 && is(&ts[0], "default")),
                Err(_) => true,
            })
        {
            return Err("VALUES column/default mismatch");
        }
        rows.push(expressions);
        p = end + 1;
        if p < conflict {
            if ts[p].text != "," {
                return Err("malformed VALUES separator");
            }
            p += 1;
            if p >= conflict {
                return Err("trailing VALUES separator");
            }
        }
    }
    if rows.is_empty() {
        return Err("empty append VALUES");
    }
    let tail = sql[ts[conflict].start..].trim().to_owned();
    let returning = returning
        .map(|i| sql[ts[i].end..].trim().to_owned())
        .unwrap_or_else(|| "1".into());
    Ok(Statement {
        family: family.into(),
        columns,
        rows,
        tail,
        returning,
    })
}
impl Statement {
    pub fn rewrite(&self, types: &Value) -> Result<String, &'static str> {
        let typed_rows = self
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .zip(&self.columns)
                    .map(|(expr, col)| -> Result<String, &'static str> {
                        let ty = types
                            .get(col)
                            .and_then(Value::as_str)
                            .ok_or("unknown admitted append column/type")?;
                        // These are pg_catalog.format_type outputs returned by authority;
                        // no type identifier or SQL comes from request bodies.
                        Ok(format!("({expr})::{ty}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(|v| format!("({})", v.join(",")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let columns = self.columns.join(",");
        // The only original update-only parameter is INGEST_TRACE's owner guard.
        // Keep its original text type and binding slot. Any new policy with
        // another extra parameter must first extend and test this contract.
        let mut parameters = vec![];
        for token in tokens(&self.tail)? {
            if let Some(n) = token
                .text
                .strip_prefix('$')
                .and_then(|n| n.parse::<usize>().ok())
            {
                if self.rows.iter().flatten().all(|e| {
                    tokens(e)
                        .map(|ts| ts.iter().all(|t| t.text != token.text))
                        .unwrap_or(false)
                }) {
                    if n != 31 || self.family != "gateway.agent_traces" {
                        return Err("unadmitted update-only parameter");
                    }
                    if !parameters.contains(&n) {
                        parameters.push(n);
                    }
                }
            }
        }
        let params: String = if parameters.is_empty() {
            "'{}'::jsonb".into()
        } else {
            "jsonb_build_object('p31',$31::text)".into()
        };
        let returned = self
            .returning
            .replace("(xmax=0)", "(af_result.result->>'inserted')::boolean")
            .replace("(xmax = 0)", "(af_result.result->>'inserted')::boolean");
        let alias = self.family.split('.').next_back().unwrap();
        Ok(format!(
            "WITH af_input({columns}) AS (VALUES {}), af_result AS MATERIALIZED (SELECT partition_ops.write_json({},to_jsonb(af_input),'statement',NULL,{}, {params}) AS result FROM af_input) SELECT {returned} FROM af_result CROSS JOIN LATERAL jsonb_populate_record(NULL::{},partition_ops.require_payload(af_result.result)) AS {alias} WHERE af_result.result->'row'<>'null'::jsonb",
            typed_rows.join(","),
            literal(&self.family),
            literal(&self.tail),
            self.family
        ))
    }
}
/// Only range comparisons are copied; equality/order/group expressions keep
/// their exact original meaning. Date casts get bounds in the session timezone.
pub fn pruning(sql: &str) -> Result<String, &'static str> {
    let ts = tokens(sql)?;
    let mut additions = vec![];
    for (i, t) in ts.iter().enumerate() {
        if !is(t, "created_at") {
            continue;
        }
        let mut j = i + 1;
        let mut date = false;
        if ts.get(j).is_some_and(|t| t.text == ":")
            && ts.get(j + 1).is_some_and(|t| t.text == ":")
            && ts.get(j + 2).is_some_and(|t| is(t, "date"))
        {
            date = true;
            j += 3;
        }
        if !ts.get(j).is_some_and(|t| matches!(t.text, ">" | "<")) {
            continue;
        }
        let cmp_start = j;
        j += 1;
        let eq = ts.get(j).is_some_and(|t| t.text == "=");
        if eq {
            j += 1;
        }
        let rhs_start = ts.get(j).ok_or("missing pruning bound")?.start;
        let depth = t.depth;
        let mut end = j;
        while end < ts.len()
            && ts[end].depth >= depth
            && !((ts[end].depth == depth)
                && (is(&ts[end], "and")
                    || is(&ts[end], "or")
                    || is(&ts[end], "group")
                    || is(&ts[end], "order")
                    || is(&ts[end], "limit")))
        {
            end += 1;
        }
        let rhs = sql[rhs_start..ts.get(end).map(|t| t.start).unwrap_or(sql.len())].trim();
        if rhs.is_empty() {
            return Err("empty pruning bound");
        }
        let qualifier = if i >= 2 && ts[i - 1].text == "." {
            format!("{}.", ts[i - 2].text)
        } else {
            String::new()
        };
        let cmp = &sql[ts[cmp_start].start..ts[j - 1].end];
        let bound = if date {
            match (ts[cmp_start].text, eq) {
                (">", true) => (">=", format!("({rhs})::date::timestamptz")),
                (">", false) => (">=", format!("(({rhs})::date+1)::timestamptz")),
                ("<", true) => ("<", format!("(({rhs})::date+1)::timestamptz")),
                _ => ("<", format!("({rhs})::date::timestamptz")),
            }
        } else {
            (cmp, rhs.into())
        };
        additions.push((
            t.start,
            ts.get(end).map(|t| t.start).unwrap_or(sql.len()),
            format!(
                " AND COALESCE({qualifier}created_at,'-infinity'::timestamptz) {} {} ",
                bound.0, bound.1
            ),
        ));
    }
    // Add beside each existing predicate so OR/parenthesized branches retain
    // their scope; do not append bounds to the entire query.
    let mut result = sql.to_owned();
    for (_, at, addition) in additions.into_iter().rev() {
        result.insert_str(at, &addition);
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn values_are_typed_and_conflict_policy_is_exact() {
        let s = "INSERT INTO billing_events(event_id,status) VALUES($1,'x,y'),($2,'z') ON CONFLICT(event_id) DO NOTHING RETURNING event_id";
        let p = parse_statement("gateway.billing_events", s).unwrap();
        assert_eq!(p.rows.len(), 2);
        let r = p
            .rewrite(&json!({"event_id":"text","status":"character varying(20)"}))
            .unwrap();
        assert!(r.contains("($1)::text"));
        assert!(r.contains("ON CONFLICT(event_id) DO NOTHING RETURNING event_id"));
        assert!(!r.contains("INSERT INTO"));
    }
    #[test]
    fn injected_and_unsupported_sql_fails_before_mutation() {
        for s in [
            "INSERT INTO billing_events(event_id) SELECT $1 ON CONFLICT DO NOTHING",
            "INSERT INTO other(event_id) VALUES($1) ON CONFLICT DO NOTHING",
            "INSERT INTO billing_events(event_id) VALUES($1) ON CONFLICT DO NOTHING; DELETE FROM billing_events",
            "INSERT INTO billing_events(event_id,event_id) VALUES($1,$2) ON CONFLICT DO NOTHING",
            "INSERT INTO billing_events(event_id) VALUES(DEFAULT) ON CONFLICT DO NOTHING",
        ] {
            assert!(parse_statement("gateway.billing_events", s).is_err(), "{s}");
        }
    }
    #[test]
    fn owner_guard_binding_is_preserved() {
        let p=parse_statement("gateway.agent_traces","INSERT INTO agent_traces(trace_id) VALUES($1::uuid) ON CONFLICT(trace_id) DO UPDATE SET status=EXCLUDED.status WHERE $31::text IS NULL RETURNING trace_id").unwrap();
        assert!(
            p.rewrite(&json!({"trace_id":"uuid"}))
                .unwrap()
                .contains("jsonb_build_object('p31',$31::text)")
        );
        let p=parse_statement("gateway.billing_events","INSERT INTO billing_events(event_id) VALUES($1) ON CONFLICT(event_id) DO UPDATE SET status=$2 RETURNING event_id").unwrap();
        assert!(p.rewrite(&json!({"event_id":"text"})).is_err());
    }
    #[test]
    fn unknown_types_and_unbalanced_input_are_errors() {
        let p = parse_statement(
            "gateway.billing_events",
            "INSERT INTO billing_events(event_id) VALUES($1) ON CONFLICT DO NOTHING",
        )
        .unwrap();
        assert!(p.rewrite(&json!({})).is_err());
        assert!(tokens("'unterminated").is_err());
        assert!(tokens("(/* nested /*x*/").is_err());
    }
    #[test]
    fn pruning_preserves_nullable_and_session_date_predicates() {
        let s=pruning("SELECT * FROM gateway.usage_records WHERE created_at::date >= $1 AND created_at::date <= $2 ORDER BY created_at").unwrap();
        assert!(s.contains("created_at::date >= $1"));
        assert!(
            s.contains("COALESCE(created_at,'-infinity'::timestamptz) >= ($1)::date::timestamptz")
        );
        assert!(s.contains("(($2)::date+1)::timestamptz"));
        assert!(s.ends_with("ORDER BY created_at"));
    }
}

#[cfg(test)]
mod native_contract_tests {
    use super::*;
    #[test]
    fn every_actual_native_upsert_parses_with_unchanged_columns_and_exact_tail() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("native_statements.json")).unwrap();
        assert_eq!(cases.len(), 12);
        for case in cases {
            let family = case["family"].as_str().unwrap();
            let parsed = parse_statement(family, case["sql"].as_str().unwrap()).unwrap();
            assert_eq!(parsed.tail, case["statement_tail"].as_str().unwrap());
            let types = Value::Object(
                parsed
                    .columns
                    .iter()
                    .map(|c| (c.clone(), Value::String("text".into())))
                    .collect(),
            );
            let projected = parsed.rewrite(&types).unwrap();
            assert!(projected.contains("partition_ops.write_json("));
            assert!(projected.contains(&literal(&parsed.tail)));
            if family == "gateway.usage_records" && case["action"] == "update" {
                assert!(projected.contains("af_result.result->>'inserted'"));
                assert!(parsed.tail.contains("pricing_snapshot"));
                assert!(parsed.tail.contains("IS NOT DISTINCT FROM"));
            }
            if family == "gateway.agent_traces" && case["action"] == "update" {
                assert!(projected.contains("jsonb_build_object('p31',$31::text)"));
                assert!(parsed.tail.contains("tenant_id = EXCLUDED.tenant_id"));
            }
        }
    }
    #[test]
    fn malformed_truncated_column_lists_do_not_panic() {
        for query in [
            "INSERT INTO billing_events(a,b,c)",
            "INSERT INTO billing_events(event_id) VALUES($1)",
            "INSERT INTO billing_events(event_id) VALUES($1), ON CONFLICT DO NOTHING",
        ] {
            assert!(parse_statement("gateway.billing_events", query).is_err());
        }
    }
    #[test]
    fn pruning_keeps_function_bounds_aliases_and_or_branch_scope() {
        let query = "SELECT * FROM usage_records u WHERE (u.created_at>=to_timestamp($1::double precision/1000) AND u.tenant_id=$2) OR (u.created_at<$3 AND u.tenant_id=$4) ORDER BY u.created_at";
        let rewritten = pruning(query).unwrap();
        assert!(rewritten.contains("COALESCE(u.created_at,'-infinity'::timestamptz) >= to_timestamp($1::double precision/1000)"));
        assert!(rewritten.contains("COALESCE(u.created_at,'-infinity'::timestamptz) < $3"));
        assert_eq!(rewritten.matches(" OR ").count(), 1);
        assert!(rewritten.contains("u.tenant_id=$2) OR (u.created_at<$3"));
    }
}

#[cfg(test)]
mod boundary_regressions {
    use super::*;
    use serde_json::json;
    #[test]
    fn identifier_trivia_is_parsed_but_statement_policy_and_binding_are_exact() {
        let tail = "ON CONFLICT(event_id) DO NOTHING RETURNING event_id";
        let prefix = "INSERT /* ; ON CONFLICT fake */ INTO GATEWAY /* outer /* inner */ end */ . \"billing_events\"(EVENT_ID /* ignored , */, \"status\") VALUES ($1, 'opaque , ) ; -- $31'),($2, $tag$,,); ON CONFLICT $tag$) ";
        let sql = format!("{prefix}{tail}");
        let parsed = parse_statement("gateway.billing_events", &sql).unwrap();
        assert_eq!(parsed.columns, ["event_id", "status"]);
        assert_eq!(parsed.tail, tail);
        assert_eq!(parsed.rows.len(), 2);
        let rewritten = parsed
            .rewrite(&json!({"event_id":"text","status":"text"}))
            .unwrap();
        assert!(rewritten.contains("($1)::text"));
        assert!(rewritten.contains("($2)::text"));
        assert!(rewritten.contains(&literal(tail)));
        assert!(!rewritten.contains("jsonb_build_object('p31'"));
        assert!(rewritten.contains("partition_ops.write_json('gateway.billing_events'"));
    }
    #[test]
    fn family_alias_case_lookalikes_and_malformed_columns_still_fail_closed() {
        for table in [
            "gateway.usage_records",
            "other.billing_events",
            "\"Gateway\".billing_events",
            "gateway.\"BILLING_EVENTS\"",
            "billing_events AS b",
            "gateway..billing_events",
            "gateway.billing_events.extra",
        ] {
            let sql = format!("INSERT INTO {table}(event_id) VALUES($1) ON CONFLICT DO NOTHING");
            assert!(
                parse_statement("gateway.billing_events", &sql).is_err(),
                "{sql}"
            );
        }
        for columns in [
            "event_id,",
            ",event_id",
            "event_id status",
            "event_id,EVENT_ID",
            "event_id,\"event_id\"",
            "\"Event_Id\"",
            "event_id::text",
            "",
            "123",
        ] {
            let sql =
                format!("INSERT INTO billing_events({columns}) VALUES($1) ON CONFLICT DO NOTHING");
            assert!(
                parse_statement("gateway.billing_events", &sql).is_err(),
                "{sql}"
            );
        }
    }
    #[test]
    fn expressions_preserve_opaque_literals_but_not_empty_default_or_invalid_binds() {
        for expression in [
            "/* empty */",
            "/* trivia */ DEFAULT",
            "$0",
            "$65536",
            "$1x",
            "é",
        ] {
            let sql = format!(
                "INSERT INTO billing_events(event_id) VALUES({expression}) ON CONFLICT DO NOTHING"
            );
            assert!(
                parse_statement("gateway.billing_events", &sql).is_err(),
                "{sql}"
            );
        }
        for expression in [
            "'é; DEFAULT $0'",
            "$tag$é; DEFAULT $0$tag$",
            "COALESCE($1,'x,y')",
        ] {
            let sql = format!(
                "INSERT INTO billing_events(event_id) VALUES({expression}) ON CONFLICT DO NOTHING"
            );
            let statement = parse_statement("gateway.billing_events", &sql).unwrap();
            assert_eq!(statement.rows[0][0], expression);
            assert!(statement.rewrite(&json!({"event_id":"text"})).is_ok());
        }
        let p = parse_statement("gateway.billing_events", "INSERT INTO billing_events(event_id) VALUES($1) ON CONFLICT(event_id) DO UPDATE SET event_id=$31").unwrap();
        assert!(p.rewrite(&json!({"event_id":"text"})).is_err());
        assert!(
            parse_statement(
                "gateway.billing_events",
                "INSERT INTO billing_events(event_id) VALUES($1) ON CONFLICT DO NOTHING; SELECT $1"
            )
            .is_err()
        );
    }
    #[test]
    fn provider_batch_fixture_binds_the_actual_family_call_instead_of_matching_only_a_tail() {
        let source = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../platform-gateway-foundation/src/provider_batch_settlement.rs"
        ));
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("native_statements.json")).unwrap();
        let batch = cases
            .iter()
            .filter(|c| {
                c["source"] == "platform-gateway-foundation/src/provider_batch_settlement.rs"
            })
            .collect::<Vec<_>>();
        assert_eq!(batch.len(), 2);
        for case in batch {
            let family = case["family"].as_str().unwrap();
            let sql = case["sql"].as_str().unwrap();
            assert!(
                source.contains(&format!(
                    "append_store::execute(&mut **tx, \"{family}\", sqlx::query(\"{sql}\")"
                )),
                "{family} fixture does not describe its real call"
            );
            assert_eq!(parse_statement(family, sql).unwrap().family, family);
        }
        // The original fixture associated usage SQL with the checkpoint family.
        // It remains forbidden; only the source-bound fixture was repaired.
        let usage = cases
            .iter()
            .find(|c| {
                c["source"] == "platform-gateway-foundation/src/provider_batch_settlement.rs"
                    && c["family"] == "gateway.usage_records"
            })
            .unwrap()["sql"]
            .as_str()
            .unwrap();
        assert_eq!(
            parse_statement("gateway.price_book_v2_accounting_checkpoints", usage).unwrap_err(),
            "append family/table mismatch"
        );
    }
}
