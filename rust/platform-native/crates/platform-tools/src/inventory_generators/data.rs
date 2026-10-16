use super::*;
use regex::Regex;
pub(super) fn refs(sql: &str) -> BTreeSet<String> {
    static RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:FROM|JOIN|INTO|UPDATE|TABLE|TRUNCATE)\s+(?:ONLY\s+)?((?:[a-z_][a-z0-9_]*\.)?[a-z_][a-z0-9_]*)").unwrap()
    });
    RE.captures_iter(sql)
        .map(|c| c[1].to_lowercase())
        .filter(|n| {
            ![
                "select",
                "where",
                "group",
                "order",
                "limit",
                "offset",
                "returning",
                "set",
                "values",
                "distinct",
                "lateral",
                "generate_series",
                "unnest",
                "jsonb",
                "json",
                "information_schema",
                "pg_catalog",
            ]
            .contains(&n.rsplit('.').next().unwrap_or(""))
        })
        .collect()
}
pub(super) fn flags(sql: &str) -> (bool, bool) {
    static WRITE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(?:INSERT\s+INTO|UPDATE|DELETE\s+FROM|TRUNCATE(?:\s+TABLE)?|MERGE\s+INTO)\b",
        )
        .unwrap()
    });
    static READ: std::sync::LazyLock<Regex> =
        std::sync::LazyLock::new(|| Regex::new(r"(?i)\b(?:SELECT|FROM|JOIN)\b").unwrap());
    let write = WRITE.is_match(sql);
    (READ.is_match(sql) || write, write)
}
pub(super) fn resolve(reference: &str, tables: &BTreeSet<String>) -> Option<String> {
    if tables.contains(reference) {
        return Some(reference.into());
    }
    let leaf = reference.rsplit('.').next()?;
    if tables.contains(leaf) {
        return Some(leaf.into());
    }
    let matches: Vec<_> = tables
        .iter()
        .filter(|t| t.rsplit('.').next() == Some(leaf))
        .collect();
    (matches.len() == 1).then(|| matches[0].clone())
}
fn bodies(text: &str) -> Vec<(String, String)> {
    let re =
        Regex::new(r"(?is)CREATE\s+(?:OR\s+REPLACE\s+)?FUNCTION\s+([a-z_][\w.]*).*?AS\s*\$(\w*)\$")
            .unwrap();
    let mut rows = vec![];
    let mut offset = 0;
    while let Some(c) = re.captures(&text[offset..]) {
        let end = offset + c.get(0).unwrap().end();
        let delimiter = format!("${}$", &c[2]);
        let Some(stop) = text[end..].find(&delimiter) else {
            break;
        };
        rows.push((c[1].to_lowercase(), text[end..end + stop].into()));
        offset = end + stop + delimiter.len();
    }
    rows
}
pub(super) fn ddl(s: &Snapshot) -> Result<(BTreeMap<String, Value>, Vec<Value>, Vec<Value>)> {
    let create = Regex::new(
        r"(?im)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(TABLE|VIEW|MATERIALIZED\s+VIEW|SEQUENCE|FUNCTION|TYPE|SCHEMA|INDEX|UNIQUE\s+INDEX)\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][\w.]*)",
    )?;
    let grant = Regex::new(
        r#"(?im)^\s*GRANT\s+([\w, ]+?)\s+ON\s+([\w ]+?)\s+([\w"., ]+?)\s+TO\s+([\w"]+)"#,
    )?;
    let table = Regex::new(r"(?i)^\s*CREATE\s+TABLE\s+(?:IF\s+NOT\s+EXISTS\s+)?([A-Za-z_][\w.]*)")?;
    let serial = Regex::new(r#"(?i)^\s*"?([A-Za-z_]\w*)"?\s+(?:BIG|SMALL)?SERIAL\b"#)?;
    let alter = Regex::new(
        r#"(?i)^\s*ALTER\s+TABLE\s+(?:ONLY\s+)?([A-Za-z_][\w.]*)\s+ADD\s+COLUMN\s+(?:IF\s+NOT\s+EXISTS\s+)?"?([A-Za-z_]\w*)"?\s+(?:BIG|SMALL)?SERIAL\b"#,
    )?;
    let mut objects = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>, String)>::new();
    let mut grants = vec![];
    let mut sequences = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>)>::new();
    for (path, bytes) in &s.files {
        if !(path == "database/schema.sql" || path.starts_with("database/migrations/"))
            || !path.ends_with(".sql")
        {
            continue;
        }
        let text = std::str::from_utf8(bytes)?;
        for c in create.captures_iter(text) {
            let kind = c[1]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("_")
                .to_uppercase();
            let row = objects
                .entry(c[2].to_lowercase())
                .or_insert_with(|| (BTreeSet::new(), BTreeSet::new(), path.clone()));
            row.0.insert(kind);
            row.1.insert(path.clone());
        }
        for c in grant.captures_iter(text) {
            grants.push(json!({"privileges":c[1].split_whitespace().collect::<Vec<_>>().join(" "),"on":c[2].trim().to_lowercase(),"object":c[3].trim_matches('"').to_lowercase(),"role":c[4].trim_matches('"').to_lowercase(),"source":path}));
        }
        let mut current = None;
        for line in text.lines() {
            if let Some(c) = table.captures(line) {
                current = Some(c[1].to_lowercase());
                continue;
            }
            if line.trim_start().starts_with(')') {
                current = None;
                continue;
            }
            let pair = if let (Some(t), Some(c)) = (&current, serial.captures(line)) {
                Some((t.clone(), c[1].to_lowercase()))
            } else {
                alter
                    .captures(line)
                    .map(|c| (c[1].to_lowercase(), c[2].to_lowercase()))
            };
            if let Some((t, col)) = pair {
                let row = sequences.entry(format!("{t}_{col}_seq")).or_default();
                row.0.insert(path.clone());
                row.1.insert(format!("{t}.{col}"));
            }
        }
    }
    grants.sort_by_key(|g| {
        ["object", "role", "privileges", "source"].map(|k| g[k].as_str().unwrap_or("").to_owned())
    });
    Ok((objects.into_iter().map(|(name,(kinds,sources,first))|(name.clone(),json!({"name":name,"kinds":kinds,"defined_in":sources,"first_defined_in":first,"schema_qualified":name.contains('.')}))).collect(),grants,sequences.into_iter().map(|(name,(sources,columns))|json!({"name":name,"kind":"implicit (SERIAL column)","defined_in":sources,"columns":columns})).collect()))
}
pub(super) fn build(s: &Snapshot) -> Result<Value> {
    let (objects, grants, sequences) = ddl(s)?;
    let names = |kind: &str| -> Vec<String> {
        objects
            .iter()
            .filter(|(_, r)| r["kinds"].as_array().unwrap().iter().any(|k| k == kind))
            .map(|(n, _)| n.clone())
            .collect()
    };
    let tables: BTreeSet<_> = names("TABLE").into_iter().collect();
    let mut access = BTreeMap::<String, BTreeMap<&str, BTreeSet<String>>>::new();
    let mut written = BTreeMap::<String, BTreeSet<String>>::new();
    let mut read = BTreeMap::<String, BTreeSet<String>>::new();
    for (path, bytes) in &s.files {
        if path.ends_with(".rs") && !path.contains("/tests/") {
            for literal in facts::sql_literals(s, path)? {
                let (r, w) = flags(&literal);
                for reference in refs(&literal) {
                    let Some(t) = resolve(&reference, &tables) else {
                        continue;
                    };
                    let row = access.entry(t).or_default();
                    if r {
                        row.entry("readers").or_default().insert(unit(path));
                        row.entry("reader_files").or_default().insert(path.clone());
                    }
                    if w {
                        row.entry("writers").or_default().insert(unit(path));
                        row.entry("writer_files").or_default().insert(path.clone());
                    }
                }
            }
        } else if path.ends_with(".sql")
            && (path == "database/schema.sql" || path.starts_with("database/migrations/"))
        {
            for (function, body) in bodies(std::str::from_utf8(bytes)?) {
                for stmt in body.split(';') {
                    let (r, w) = flags(stmt);
                    for reference in refs(stmt) {
                        if let Some(t) = resolve(&reference, &tables) {
                            if w {
                                written
                                    .entry(t.clone())
                                    .or_default()
                                    .insert(function.clone());
                            }
                            if r {
                                read.entry(t).or_default().insert(function.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    let empty = BTreeSet::new();
    let table_rows:Vec<_>=tables.iter().map(|name|{let get=|key|access.get(name).and_then(|r|r.get(key)).unwrap_or(&empty);json!({"table":name,"schema_qualified_in_ddl":objects[name]["schema_qualified"],"defined_in":objects[name]["defined_in"],"static_readers":get("readers"),"static_writers":get("writers"),"reader_files":get("reader_files"),"writer_files":get("writer_files"),"function_writers":written.get(name).unwrap_or(&empty),"cross_unit_write":get("writers")})}).collect();
    let mut views = names("VIEW");
    views.extend(names("MATERIALIZED_VIEW"));
    views.sort();
    views.dedup();
    let funcs = names("FUNCTION");
    let explicit = names("SEQUENCE");
    let types = names("TYPE");
    let schemas = names("SCHEMA");
    let mut out = s.envelope("data-access-inventory");
    out["purpose"] = json!(
        "Static Rust SQL-literal and SQL-function attribution, not runtime access proof. Do not narrow grants from this heuristic alone."
    );
    out["postgresql"] = json!({"ddl_sources":s.files.keys().filter(|p|p.as_str()=="database/schema.sql"||p.starts_with("database/migrations/")&&p.ends_with(".sql")).collect::<Vec<_>>(),"object_counts":{"tables":tables.len(),"functions":funcs.len(),"views":views.len(),"sequences_explicit":explicit.len(),"sequences_implicit_serial":sequences.len(),"types":types.len(),"schemas_declared":schemas.len(),"grants_found":grants.len()},"schemas_declared":schemas,"functions":funcs,"views":views,"sequences_explicit":explicit,"sequences_implicit_serial":sequences,"types":types,"grants":grants,"tables":table_rows,"function_mediated_access":{"function_written_tables":written,"function_read_tables":read},"tables_with_multiple_native_unit_writers":table_rows.iter().filter(|r|r["static_writers"].as_array().unwrap().len()>1).map(|r|r["table"].clone()).collect::<Vec<_>>()});
    for (kind, tokens) in [
        (
            "qdrant",
            vec![
                "kb_",
                "knowledge_lexical",
                "knowledge_scope",
                "vector_collections",
            ],
        ),
        (
            "redis",
            vec![
                "session:",
                "cache:",
                "health:",
                "service:",
                "task:",
                "guest_session:",
                "ratelimit:",
            ],
        ),
        (
            "object_storage",
            vec!["uploads", "key_prefix", "data/files"],
        ),
    ] {
        let mut rows = vec![];
        for token in tokens {
            let evidence: Vec<_> = s
                .files
                .iter()
                .filter(|(p, b)| {
                    p.ends_with(".rs") && std::str::from_utf8(b).is_ok_and(|t| t.contains(token))
                })
                .map(|(p, _)| json!({"file":p,"source_unit":unit(p)}))
                .collect();
            rows.push(json!({"namespace_token":token,"evidence":evidence}));
        }
        out[kind] = json!({"method":"native source namespace evidence, not inferred exclusive ownership","namespaces":rows});
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ambiguous_leaf_refuses() {
        let set = BTreeSet::from(["a.users".into(), "b.users".into()]);
        assert_eq!(resolve("users", &set), None);
        assert_eq!(resolve("a.users", &set), Some("a.users".into()));
    }
    #[test]
    fn sql_function_delimiters() {
        assert_eq!(bodies("CREATE FUNCTION f() RETURNS void AS $body$ INSERT INTO x VALUES(1); $body$ LANGUAGE sql;")[0].0,"f");
    }
}
