//! Static database-policy evidence. Native SQL literals replace former Python
//! source units; live ACL verification remains mandatory before freezing.
use crate::{AuthorityError, Result};
use regex::Regex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
fn fail(s: &str) -> AuthorityError {
    AuthorityError::Refused(s.into())
}
fn re(s: &str) -> Regex {
    Regex::new(s).unwrap()
}
fn leaf(s: &str) -> &str {
    s.rsplit('.').next().unwrap_or(s)
}
fn walk(path: &Path, suffix: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !path.exists() {
        return Ok(paths);
    }
    for e in std::fs::read_dir(path)? {
        let e = e?;
        let kind = e.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if !["target", ".git", "node_modules", "vendor"]
                .contains(&e.file_name().to_string_lossy().as_ref())
            {
                paths.extend(walk(&e.path(), suffix)?);
            }
        } else if e.path().extension().is_some_and(|x| x == suffix) {
            paths.push(e.path());
        }
    }
    paths.sort();
    Ok(paths)
}
pub fn serialized(value: &Value) -> Result<Vec<u8>> {
    let mut value = value.clone();
    value.sort_all_objects();
    let mut bytes =
        serde_json::to_vec_pretty(&value).map_err(|_| fail("policy serialization failed"))?;
    bytes.push(b'\n');
    Ok(bytes)
}
fn schema_for_table(name: &str) -> Result<String> {
    let d: Value = serde_json::from_str(include_str!("policy_domains.json")).unwrap();
    for (key, schema) in [
        ("knowledge_overrides", "knowledge"),
        ("assistant_overrides", "assistant"),
    ] {
        if d[key].as_array().unwrap().iter().any(|v| v == leaf(name)) {
            return Ok(schema.into());
        }
    }
    if let Some(domain) = d["owners"][name]
        .as_str()
        .or_else(|| d["owners"][leaf(name)].as_str())
    {
        if let Some(schema) = d["domains"][domain].as_str() {
            return Ok(schema.into());
        }
    }
    if let Some((schema, _)) = name.split_once('.') {
        if ["assistant", "gateway", "knowledge", "public"].contains(&schema) {
            return Ok(schema.into());
        }
    }
    Err(fail(&format!("table {name} has unresolved owner domain")))
}
fn function_schema(name: &str) -> String {
    if name.starts_with("knowledge.")
        || [
            "knowledge_",
            "segments_",
            "guard_dataset",
            "prune_kb_",
            "record_kb_",
            "update_document",
            "update_segment",
        ]
        .iter()
        .any(|p| leaf(name).starts_with(p))
    {
        return "knowledge".into();
    }
    if name.starts_with("assistant.")
        || [
            "assistant_",
            "append_assistant",
            "reserve_assistant",
            "dispatch_assistant",
            "ensure_assistant",
            "import_assistant",
            "issue_assistant",
            "complete_assistant",
            "update_assistant",
            "update_memory",
        ]
        .iter()
        .any(|p| leaf(name).starts_with(p))
    {
        return "assistant".into();
    }
    "gateway".into()
}
fn roles(path: &str) -> Vec<&'static str> {
    if path.contains("/ai-platform-agent-runtime/src/") {
        return vec!["runtime"];
    }
    if path.contains("/ai-platform-capability-worker/src/") {
        return vec!["capability_worker"];
    }
    // These modules are composed into Knowledge processes, despite living in
    // the gateway-integrations crate. Shared source evidence is attributed to
    // both principals, exactly as the historical shared repository scanner was.
    if path.contains("/migration_knowledge_ingest/") {
        if [
            "/worker.rs",
            "/streaming.rs",
            "/vision.rs",
            "/structured.rs",
        ]
        .iter()
        .any(|name| path.ends_with(name))
        {
            return vec!["knowledge_worker"];
        }
        return vec!["knowledge_api", "knowledge_worker"];
    }
    if path.contains("/migration_knowledge_query/") {
        if ["/indexer.rs", "/hierarchy.rs"]
            .iter()
            .any(|name| path.ends_with(name))
        {
            return vec!["knowledge_worker"];
        }
        if path.contains("embedding") || path.contains("qdrant") || path.contains("index") {
            return vec!["knowledge_api", "knowledge_worker"];
        }
        return vec!["knowledge_api"];
    }
    if path.contains("/platform-knowledge-service/src/") {
        if path.ends_with("/metrics.rs") || path.ends_with("/lifecycle.rs") {
            return vec!["knowledge_api", "knowledge_worker"];
        }
        if [
            "/workers/",
            "/worker_",
            "/ingestion",
            "/indexing",
            "/parsing",
            "/embedding",
            "/backfill",
        ]
        .iter()
        .any(|p| path.contains(p))
        {
            return vec!["knowledge_worker"];
        }
        if path.contains("/repositories/") || path.contains("/persistence/") {
            return vec!["knowledge_api", "knowledge_worker"];
        }
        return vec!["knowledge_api"];
    }
    if path.contains("/platform-gateway") || path.contains("/platform-auth/src/") {
        return vec!["gateway"];
    }
    Vec::new()
}
/// Rust string scanner handles matching raw delimiters and escaped quoted strings.
fn literals(source: &str) -> Vec<String> {
    let b = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'r' {
            let mut j = i + 1;
            while j < b.len() && b[j] == b'#' {
                j += 1;
            }
            if j < b.len() && b[j] == b'"' {
                let close = format!("\"{}", "#".repeat(j - i - 1));
                if let Some(end) = source[j + 1..].find(&close) {
                    out.push(source[j + 1..j + 1 + end].into());
                    i = j + 1 + end + close.len();
                    continue;
                }
            }
        }
        if b[i] == b'"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    i += 1;
                    if let Ok(v) = serde_json::from_str::<String>(&source[start..i]) {
                        out.push(v);
                    }
                    break;
                }
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    out
}
fn refs(sql: &str) -> BTreeSet<String> {
    static REFERENCES: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        re(
            r"(?i)\b(?:FROM|JOIN|INTO|UPDATE|TABLE|TRUNCATE)\s+(?:ONLY\s+)?((?:[a-z_][a-z0-9_]*\.)?[a-z_][a-z0-9_]*)",
        )
    });
    REFERENCES
        .captures_iter(sql)
        .map(|c| leaf(&c[1]).to_lowercase())
        .collect()
}
#[derive(Default)]
struct Access {
    privileges: BTreeSet<String>,
    evidence: BTreeSet<String>,
}
#[derive(Default)]
struct Table {
    defined: BTreeSet<String>,
    aliases: BTreeSet<String>,
    functions: BTreeSet<String>,
}
#[derive(Default)]
struct Sequence {
    defined: BTreeSet<String>,
    columns: BTreeSet<String>,
}
fn close_paren(s: &str, start: usize) -> Result<usize> {
    let mut depth = 0;
    let mut quote = None;
    for (off, c) in s[start..].char_indices() {
        if let Some(q) = quote {
            if q == c {
                quote = None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
            if depth == 0 {
                return Ok(start + off);
            }
        }
    }
    Err(fail("unterminated CREATE FUNCTION arguments"))
}
fn arguments(s: &str) -> String {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        if c == '(' || c == '[' {
            depth += 1;
        } else if c == ')' || c == ']' {
            depth -= 1;
        } else if c == ',' && depth == 0 {
            pieces.push(&s[start..i]);
            start = i + 1;
        }
    }
    if !s[start..].trim().is_empty() {
        pieces.push(&s[start..]);
    }
    pieces
        .into_iter()
        .filter_map(|piece| {
            let value = re(r"(?i)\s+DEFAULT\s+|\s*=")
                .split(piece)
                .next()
                .unwrap()
                .trim()
                .to_lowercase();
            let mut tokens = value.split_whitespace().collect::<Vec<_>>();
            if tokens.first() == Some(&"out") {
                return None;
            }
            if tokens
                .first()
                .is_some_and(|p| ["in", "inout", "variadic"].contains(p))
            {
                tokens.remove(0);
            }
            if tokens.len() > 1 && re("^[a-z_][a-z0-9_]*$").is_match(tokens[0]) {
                tokens.remove(0);
            }
            let rendered = tokens.join(" ");
            let base = re(r"^([a-z]+)\([0-9, ]+\)$")
                .captures(&rendered)
                .map(|c| c[1].to_owned())
                .unwrap_or(rendered.clone());
            let canonical = match base.as_str() {
                "varchar" => "character varying",
                "char" => "character",
                "int" | "int4" => "integer",
                "int8" => "bigint",
                "bool" => "boolean",
                "timestamptz" => "timestamp with time zone",
                "timestamp" => "timestamp without time zone",
                _ => &rendered,
            };
            if canonical.is_empty() {
                None
            } else {
                Some(canonical.to_owned())
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}
fn function_bodies(text: &str) -> Vec<(String, String)> {
    let pattern =
        re(r"(?is)CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+([a-z_][\w.]*).*?AS\s*\$(\w*)\$");
    let mut result = Vec::new();
    let mut offset = 0;
    while let Some(c) = pattern.captures(&text[offset..]) {
        let end = offset + c.get(0).unwrap().end();
        let delimiter = format!("${}$", &c[2]);
        if let Some(stop) = text[end..].find(&delimiter) {
            result.push((c[1].to_lowercase(), text[end..end + stop].to_owned()));
            offset = end + stop + delimiter.len();
        } else {
            break;
        }
    }
    result
}
pub fn build(
    root: &Path,
    source_sha: Option<&str>,
    pending: bool,
) -> Result<(Value, Value, Value)> {
    if !pending && !source_sha.is_some_and(|s| re("^[0-9a-f]{40}$").is_match(s)) {
        return Err(fail("source revision must be full lowercase Git SHA"));
    }
    let mut sql_files = walk(&root.join("database/migrations"), "sql")?;
    if root.join("database/schema.sql").is_file() {
        sql_files.push(root.join("database/schema.sql"));
    }
    // Function definitions follow schema.sql first, then ordered migrations.
    sql_files.sort_by_key(|p| (p != &root.join("database/schema.sql"), p.clone()));
    let mut tables: BTreeMap<String, Table> = BTreeMap::new();
    let mut functions = BTreeSet::new();
    let mut views = BTreeSet::new();
    let mut sequences: BTreeMap<String, Sequence> = BTreeMap::new();
    let mut definitions: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut bodies = Vec::new();
    let create = re(
        r"(?im)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(TABLE|VIEW|MATERIALIZED\s+VIEW|SEQUENCE|FUNCTION|TYPE|SCHEMA|INDEX|UNIQUE\s+INDEX)\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-z_][\w.]*)",
    );
    let table_line = re(r"(?i)^\s*CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([a-z_][\w.]*)");
    let serial = re(r#"(?i)^\s*"?([a-z_]\w*)"?\s+(?:BIG|SMALL)?SERIAL\b"#);
    let alter = re(
        r#"(?i)^\s*ALTER\s+TABLE\s+(?:ONLY\s+)?([a-z_][\w.]*)\s+ADD\s+COLUMN\s+(?:IF\s+NOT\s+EXISTS\s+)?"?([a-z_]\w*)"?\s+(?:BIG|SMALL)?SERIAL\b"#,
    );
    let function = re(r"(?i)CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+([a-z_][a-z0-9_.]*)\s*\(");
    for path in &sql_files {
        let text = std::fs::read_to_string(path)?;
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        for c in create.captures_iter(&text) {
            let kind = c[1].to_uppercase();
            let name = c[2].to_lowercase();
            let key = leaf(&name).to_owned();
            match kind.as_str() {
                "TABLE" => {
                    if [
                        "schema_migrations",
                        "schema_migrations_meta",
                        "platform_schema_baselines",
                        "platform_schema_changes",
                        "platform_schema_change_attempts",
                    ]
                    .contains(&key.as_str())
                    {
                        continue;
                    }
                    let table = tables.entry(key).or_default();
                    table.defined.insert(relative.clone());
                    table.aliases.insert(name);
                }
                "FUNCTION" => {
                    functions.insert(key);
                }
                "VIEW" | "MATERIALIZED VIEW" => {
                    views.insert(key);
                }
                _ => {}
            }
        }
        let mut current = None;
        let mut record = |table: &str, column: &str| {
            let table = table.to_lowercase();
            let column = column.to_lowercase();
            let seq = sequences
                .entry(format!("{table}_{column}_seq"))
                .or_default();
            seq.defined.insert(relative.clone());
            seq.columns.insert(format!("{table}.{column}"));
        };
        for line in text.lines() {
            if let Some(c) = table_line.captures(line) {
                current = Some(c[1].to_owned());
                continue;
            }
            if let Some(table) = &current {
                if line.trim_start().starts_with(')') {
                    current = None;
                    continue;
                }
                if let Some(c) = serial.captures(line) {
                    record(table, &c[1]);
                    continue;
                }
            }
            if let Some(c) = alter.captures(line) {
                record(&c[1], &c[2]);
            }
        }
        bodies.extend(function_bodies(&text));
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("_rollback.sql")
        {
            for c in function.captures_iter(&text) {
                let start = c.get(0).unwrap().end();
                let end = close_paren(&text, start - 1)?;
                definitions.insert(
                    leaf(&c[1]).to_lowercase(),
                    (arguments(&text[start..end]), relative.clone()),
                );
            }
        }
    }
    let writes =
        re(r"(?i)\b(INSERT\s+INTO|UPDATE|DELETE\s+FROM|TRUNCATE(?:\s+TABLE)?|MERGE\s+INTO)\b");
    for (name, body) in bodies {
        for statement in body.split(';') {
            if writes.is_match(statement) {
                for table in refs(statement) {
                    if let Some(t) = tables.get_mut(&table) {
                        t.functions.insert(name.clone());
                    }
                }
            }
        }
    }
    let mut access: BTreeMap<String, BTreeMap<String, Access>> = BTreeMap::new();
    let mut callers: BTreeMap<String, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
    let function_patterns: Vec<_> = functions
        .iter()
        .map(|name| (name, re(&format!(r"(?i)\b{}\s*\(", regex::escape(name)))))
        .collect();
    let sqlish = re(r"(?i)\b(SELECT|INSERT\s+INTO|UPDATE|DELETE\s+FROM)\b");
    let select = re(r"(?i)\bSELECT\b");
    let conflict = re(r"(?i)\bON\s+CONFLICT\b");
    let targets = [
        ("INSERT", re(r"(?i)\bINSERT\s+INTO\s+([a-z_][a-z0-9_.]*)")),
        ("UPDATE", re(r"(?i)\bUPDATE\s+([a-z_][a-z0-9_.]*)\s+SET\b")),
        ("DELETE", re(r"(?i)\bDELETE\s+FROM\s+([a-z_][a-z0-9_.]*)")),
    ];
    for path in walk(&root.join("rust"), "rs")? {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        if relative.contains("/tests/") || relative.ends_with("_tests.rs") {
            continue;
        }
        let role_set = roles(&relative);
        if role_set.is_empty() {
            continue;
        }
        for literal in literals(&std::fs::read_to_string(path)?) {
            if !sqlish.is_match(&literal) {
                continue;
            }
            let mut record = |table: &str, privilege: &str| {
                if tables.contains_key(table) {
                    for role in &role_set {
                        let contract = access
                            .entry(table.into())
                            .or_default()
                            .entry((*role).into())
                            .or_default();
                        contract.privileges.insert(privilege.into());
                        contract.evidence.insert(relative.clone());
                    }
                }
            };
            if select.is_match(&literal) {
                for table in refs(&literal) {
                    record(&table, "SELECT");
                }
            }
            for (privilege, pattern) in &targets {
                for c in pattern.captures_iter(&literal) {
                    let table = leaf(&c[1]).to_lowercase();
                    record(&table, privilege);
                    if *privilege == "INSERT" && conflict.is_match(&literal) {
                        record(&table, "UPDATE");
                    }
                }
            }
            for (function, pattern) in &function_patterns {
                if pattern.is_match(&literal) {
                    for role in &role_set {
                        callers
                            .entry((*function).clone())
                            .or_default()
                            .entry((*role).into())
                            .or_default()
                            .insert(relative.clone());
                    }
                }
            }
        }
    }
    let mut table_rows = Vec::new();
    let mut ownership = Vec::new();
    let mut grants = Vec::new();
    let order = ["SELECT", "INSERT", "UPDATE", "DELETE"];
    for (name, table) in &tables {
        let schema = schema_for_table(name)?;
        let mut readers = BTreeSet::new();
        let mut writers = BTreeSet::new();
        let mut reader_files = BTreeSet::new();
        let mut writer_files = BTreeSet::new();
        let mut accesses = Vec::new();
        if let Some(contracts) = access.get(name) {
            for (role, contract) in contracts {
                let units: Vec<_> = contract
                    .evidence
                    .iter()
                    .map(|p| format!("{role}:{p}"))
                    .collect();
                if contract.privileges.contains("SELECT") {
                    readers.extend(units.clone());
                }
                if ["INSERT", "UPDATE", "DELETE"]
                    .iter()
                    .any(|p| contract.privileges.contains(*p))
                {
                    writers.extend(units.clone());
                    writer_files.extend(contract.evidence.clone());
                }
                reader_files.extend(contract.evidence.clone());
                let privileges: Vec<_> = order
                    .iter()
                    .filter(|p| contract.privileges.contains(**p))
                    .copied()
                    .collect();
                accesses.push(json!({"units":units,"roles":[role],"privileges":privileges,"evidence":contract.evidence}));
            }
        }
        table_rows.push(json!({"table":name,"schema":schema,"defined_in":table.defined,"function_writers":table.functions,"aliases":table.aliases,"static_readers":readers,"static_writers":writers,"reader_files":reader_files,"writer_files":writer_files}));
        ownership.push(json!({"kind":"table","inventory_name":name,"schema":schema,"name":name,"owner":"owner","evidence":table.defined}));
        grants.push(json!({"inventory_kind":"table","inventory_name":name,"schema":schema,"name":name,"owner":"owner","evidence":table.defined,"access":accesses,"function_only":!table.functions.is_empty()&&writers.is_empty(),"function_evidence":table.functions}));
    }
    for name in &functions {
        let (args, defined) = definitions
            .get(name)
            .ok_or_else(|| fail(&format!("function {name} has no parseable definition")))?;
        let schema = function_schema(name);
        ownership.push(json!({"kind":"function","inventory_name":name,"schema":schema,"name":name,"identity_arguments":args,"owner":"owner","evidence":[defined]}));
        let accesses:Vec<_>=callers.get(name).into_iter().flat_map(|m|m.iter()).map(|(role,paths)|json!({"units":paths.iter().map(|p|format!("{role}:{p}")).collect::<Vec<_>>(),"roles":[role],"privileges":["EXECUTE"],"evidence":paths})).collect();
        grants.push(json!({"inventory_kind":"function","inventory_name":name,"schema":schema,"name":name,"identity_arguments":args,"owner":"owner","evidence":[defined],"access":accesses}));
    }
    for name in &views {
        ownership.push(json!({"kind":"view","inventory_name":name,"schema":"gateway","name":name,"owner":"owner","evidence":["database/schema.sql","database/migrations"]}));
        grants.push(json!({"inventory_kind":"view","inventory_name":name,"schema":"gateway","name":name,"owner":"owner","evidence":["database/schema.sql","database/migrations"],"access":[{"units":[format!("gateway:view:{name}")],"roles":["gateway"],"privileges":["SELECT"],"evidence":[format!("reviewed-view:{name}")]}]}));
    }
    let mut sequence_rows = Vec::new();
    for (name, sequence) in sequences {
        let column = sequence
            .columns
            .first()
            .ok_or_else(|| fail("sequence parent missing"))?;
        let parent = leaf(column.split_once('.').unwrap().0);
        let table = table_rows
            .iter()
            .find(|t| t["table"] == parent)
            .ok_or_else(|| fail("sequence has unknown parent"))?;
        let schema = &table["schema"];
        let roles: Vec<_> = access
            .get(parent)
            .into_iter()
            .flat_map(|m| m.iter())
            .filter(|(_, c)| c.privileges.contains("INSERT"))
            .map(|(r, _)| r)
            .collect();
        sequence_rows.push(json!({"name":name,"kind":"implicit (SERIAL column)","columns":sequence.columns,"defined_in":sequence.defined,"schema":schema,"parent_table":parent}));
        ownership.push(json!({"kind":"sequence","inventory_name":name,"schema":schema,"name":name,"owner":"owner","evidence":sequence.defined}));
        grants.push(json!({"inventory_kind":"sequence","inventory_name":name,"schema":schema,"name":name,"owner":"owner","evidence":sequence.defined,"access":roles.iter().map(|r|json!({"units":[format!("{r}:insert:{parent}")],"roles":[r],"privileges":["USAGE"],"evidence":[format!("serial-parent:{}.{}",schema.as_str().unwrap(),parent)]})).collect::<Vec<_>>()}));
    }
    let state = if pending {
        "pending-live-freeze"
    } else {
        "source-commit-bound-pending-live-acl"
    };
    let sha = if pending { None } else { source_sha };
    let inventory = json!({"schema":"arc03-data-access-policy/v1","state":state,"source_git_sha":sha,"postgresql":{"tables":table_rows,"views":views,"functions":functions,"types":[],"sequences_explicit":[],"sequences_implicit_serial":sequence_rows}});
    let digest = format!("{:x}", Sha256::digest(serialized(&inventory)?));
    ownership.sort_by_key(|v| {
        ["kind", "schema", "name", "identity_arguments"]
            .map(|k| v[k].as_str().unwrap_or("").to_owned())
    });
    grants.sort_by_key(|v| {
        ["inventory_kind", "inventory_name"].map(|k| v[k].as_str().unwrap_or("").to_owned())
    });
    Ok((
        inventory,
        json!({"schema":"arc03-object-ownership/v1","state":state,"source_git_sha":sha,"inventory_sha256":digest,"objects":ownership}),
        json!({"schema_version":"arc03-grants-policy/v1","state":state,"source_git_sha":sha,"inventory_sha256":digest,"objects":grants}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literals_preserve_sql() {
        assert_eq!(
            literals(
                "const A: &str = r###\"SELECT 'x' FROM a\"###; const B: &str = \"SELECT \\\"b\\\"\";"
            ),
            vec!["SELECT 'x' FROM a", "SELECT \"b\""]
        );
    }
    #[test]
    fn knowledge_sql_is_not_gateway_owned() {
        assert_eq!(
            roles(
                "rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_ingest/worker.rs"
            ),
            vec!["knowledge_worker"]
        );
        assert_eq!(
            roles(
                "rust/platform-native/crates/platform-gateway-integrations/src/migration_knowledge_ingest/sql.rs"
            ),
            vec!["knowledge_api", "knowledge_worker"]
        );
        assert_eq!(
            roles("rust/platform-native/crates/platform-knowledge-service/src/confluence/sql.rs"),
            vec!["knowledge_api"]
        );
    }
    #[test]
    fn canonical_arguments() {
        assert_eq!(
            arguments("p_id UUID, p_text VARCHAR(64), OUT answer int"),
            "uuid, character varying"
        );
    }
    #[test]
    fn builds_reviewed_native_policy_from_source() {
        let root =
            std::env::temp_dir().join(format!("native-policy-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("database/migrations")).unwrap();
        let code = root.join("rust/platform-native/crates/platform-gateway/src");
        std::fs::create_dir_all(&code).unwrap();
        std::fs::write(root.join("database/schema.sql"), "CREATE TABLE users (\n id SERIAL,\n name text\n);\nCREATE FUNCTION audit_user(p_id int) RETURNS void AS $$ UPDATE users SET name = 'x'; $$ LANGUAGE SQL;\n").unwrap();
        std::fs::write(code.join("store.rs"), r###"const SQL: &str = r#"INSERT INTO gateway.users(name) VALUES('x') ON CONFLICT DO NOTHING RETURNING id"#; const READ: &str = "SELECT * FROM gateway.users";"###).unwrap();
        let result = build(&root, Some(&"a".repeat(40)), false);
        let _ = std::fs::remove_dir_all(&root);
        let (inventory, ownership, grants) = result.unwrap();
        assert_eq!(inventory["postgresql"]["tables"][0]["table"], "users");
        assert_eq!(
            inventory["postgresql"]["sequences_implicit_serial"][0]["name"],
            "users_id_seq"
        );
        let inv = serialized(&inventory).unwrap();
        super::super::render_contract::load_policy_bytes(&inv, &serialized(&ownership).unwrap())
            .unwrap();
        let sql = super::super::grants::generate(
            &inv,
            &serialized(&grants).unwrap(),
            crate::DEFAULT_ROLE_PREFIX,
        )
        .unwrap();
        assert!(sql.contains("GRANT SELECT, INSERT, UPDATE ON TABLE"));
        assert!(sql.contains("GRANT USAGE ON SEQUENCE"));
    }
    #[test]
    fn domains_fail_closed() {
        assert_eq!(schema_for_table("users").unwrap(), "gateway");
        assert!(schema_for_table("unreviewed_table").is_err());
    }
}
