use super::*;
const IO: &[&str] = &[
    "sqlx",
    "redis",
    "reqwest",
    "hyper",
    "axum",
    "tokio",
    "std::fs",
    "std::net",
    "std::process",
    "aws_sdk_s3",
    "qdrant_client",
];
fn crate_name(s: &Snapshot, path: &str) -> Result<String> {
    let manifest = s
        .files
        .keys()
        .filter(|p| {
            p.ends_with("/Cargo.toml") && path.starts_with(p.trim_end_matches("Cargo.toml"))
        })
        .max_by_key(|p| p.len());
    if let Some(manifest) = manifest {
        let value: toml::Value = toml::from_str(s.text(manifest)?)?;
        if let Some(name) = value
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(toml::Value::as_str)
        {
            return Ok(name.replace('-', "_"));
        }
    }
    Ok(unit(path).replace('-', "_"))
}
fn module(s: &Snapshot, path: &str) -> Result<String> {
    let source = path.split_once("/src/").map(|(_, p)| p).unwrap_or(path);
    let name = source.trim_end_matches(".rs").replace('/', "::");
    let name = name.strip_suffix("::mod").unwrap_or(&name);
    if name == "lib" || name == "main" {
        crate_name(s, path)
    } else {
        Ok(format!("{}::{name}", crate_name(s, path)?))
    }
}
fn domain(path: &str) -> String {
    for d in [
        "quiz",
        "skills",
        "image",
        "memory",
        "sharing",
        "knowledge",
        "eval",
        "auth",
        "artifact",
        "agent-runtime",
        "agent-studio",
        "local-node",
    ] {
        let token = d.replace('-', "_");
        if path.contains(&format!("/{token}/"))
            || path.contains(&format!("/{token}_"))
            || path.contains(&format!("migration_{token}"))
        {
            return d.into();
        }
    }
    unit(path)
}
fn io(paths: &BTreeSet<String>) -> Vec<String> {
    IO.iter()
        .filter(|prefix| {
            paths
                .iter()
                .any(|p| p.as_str() == **prefix || p.starts_with(&format!("{prefix}::")))
        })
        .map(|p| p.to_string())
        .collect()
}
pub(super) fn consumption(s: &Snapshot) -> Result<Value> {
    let mut modules = BTreeMap::<String, Value>::new();
    let mut all = BTreeMap::new();
    for (path, bytes) in &s.files {
        if !path.ends_with(".rs") {
            continue;
        }
        let text = std::str::from_utf8(bytes)?;
        let facts = facts::source(text)?;
        let name = module(s, path)?;
        modules.insert(name.clone(),json!({"file":path,"loc":facts::line_count(text),"io_deps":io(&facts.paths),"public_reexports":facts.public_exports,"function_count":facts.functions,"domain":domain(path),"source_unit":unit(path),"consumer_owners":{}}));
        all.insert(name, (path.clone(), facts.paths));
    }
    let catalog: BTreeSet<_> = modules.keys().cloned().collect();
    let mut consumption = BTreeMap::<String, BTreeMap<String, BTreeSet<String>>>::new();
    for (name, (file, paths)) in &all {
        for path in paths {
            let expanded = if let Some(rest) = path.strip_prefix("crate::") {
                format!("{}::{rest}", crate_name(s, file)?)
            } else {
                path.clone()
            };
            let mut target = expanded.as_str();
            loop {
                if catalog.contains(target) {
                    consumption
                        .entry(unit(file))
                        .or_default()
                        .entry(target.into())
                        .or_default()
                        .insert(file.clone());
                    let row = modules.get_mut(target).unwrap();
                    let owner = unit(file);
                    if row["consumer_owners"].get(&owner).is_none() {
                        row["consumer_owners"][&owner] = json!([]);
                    }
                    let files = row["consumer_owners"][&owner].as_array_mut().unwrap();
                    let value = json!(file);
                    if !files.contains(&value) {
                        files.push(value)
                    }
                    break;
                }
                if let Some((prefix, _)) = target.rsplit_once("::") {
                    target = prefix
                } else {
                    break;
                }
            }
        }
        let _ = name;
    }
    let contracts: BTreeMap<_, _> = modules
        .iter()
        .filter(|(_, v)| {
            v["file"].as_str().is_some_and(|p| {
                p.split('/')
                    .any(|part| part.ends_with("-contract") || part.ends_with("-contracts"))
            })
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut out = s.envelope("core-consumption");
    out["schema_version"] = json!("arc04-native-core-inventory/v1");
    out["base_sha"] = json!(s.sha);
    out["modules"] = json!(modules);
    out["contracts_modules"] = json!(contracts);
    out["consumption"] = json!(consumption);
    out["knowledge_core_modules"] = json!(
        consumption
            .iter()
            .filter(|(owner, _)| owner.contains("knowledge"))
            .flat_map(|(_, m)| m.keys().cloned())
            .collect::<BTreeSet<_>>()
    );
    out["knowledge_core_module_count"] =
        json!(out["knowledge_core_modules"].as_array().unwrap().len());
    out["native_facade_exports"] = json!(
        modules
            .iter()
            .filter(|(_, v)| v["public_reexports"]
                .as_array()
                .is_some_and(|r| !r.is_empty()))
            .map(|(k, v)| (k.clone(), v["public_reexports"].clone()))
            .collect::<BTreeMap<_, _>>()
    );
    out["methodology"] = json!([
        "Rust syntax tree import/path and public re-export scan at one immutable commit.",
        "Native module and crate units supersede Python shim module identities; historical inventory is unchanged.",
        "Macro-generated imports and dynamically loaded symbols require separate review."
    ]);
    Ok(out)
}
fn owner(table: &str) -> String {
    let reference: Value =
        serde_json::from_str(include_str!("domain_reference.json")).expect("domain reference");
    let owners = &reference["historical_table_owner_claims"];
    if let Some(v) = owners[table].as_str() {
        v.into()
    } else if let Some((schema, leaf)) = table.split_once('.') {
        owners[leaf]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("schema:{schema}"))
    } else {
        "unknown".into()
    }
}
pub(super) fn persistence(s: &Snapshot) -> Result<Value> {
    let (objects, _, _) = data::ddl(s)?;
    let mut known: BTreeSet<_> = objects
        .iter()
        .filter(|(_, v)| v["kinds"].as_array().unwrap().iter().any(|k| k == "TABLE"))
        .map(|(k, _)| k.clone())
        .collect();
    let moved = regex::Regex::new(
        r"(?i)ALTER\s+TABLE\s+(?:IF\s+EXISTS\s+)?([a-z_][\w.]*)\s+SET\s+SCHEMA\s+([a-z_]\w*)",
    )?;
    for (path, bytes) in &s.files {
        if path.starts_with("database/") && path.ends_with(".sql") {
            for c in moved.captures_iter(std::str::from_utf8(bytes)?) {
                known.insert(format!(
                    "{}.{}",
                    c[2].to_lowercase(),
                    c[1].rsplit('.').next().unwrap().to_lowercase()
                ));
            }
        }
    }
    let mut modules = BTreeMap::new();
    let mut cross = BTreeMap::new();
    for path in s.files.keys() {
        if !path.ends_with(".rs") {
            continue;
        }
        let mut reads = BTreeSet::new();
        let mut writes = BTreeSet::new();
        for literal in facts::sql_literals(s, path)? {
            let (r, w) = data::flags(&literal);
            if r {
                reads.extend(data::refs(&literal));
            }
            if w {
                writes.extend(data::refs(&literal));
            }
        }
        if reads.is_empty() && writes.is_empty() {
            continue;
        }
        let annotate = |names: &BTreeSet<String>| {
            names.iter().map(|name|{let table=data::resolve(name,&known);json!({"table":name,"resolved_table":table,"owner":table.as_deref().map(owner).unwrap_or_else(||"unresolved".into()),"known_table":table.is_some()})}).collect::<Vec<_>>()
        };
        let read = annotate(&reads);
        let write = annotate(&writes);
        let home = domain(path);
        let foreign: BTreeSet<_> = write
            .iter()
            .filter_map(|r| r["owner"].as_str())
            .filter(|o| *o != home && *o != "unknown" && *o != "unresolved")
            .map(str::to_owned)
            .collect();
        let read_foreign: BTreeSet<_> = read
            .iter()
            .filter_map(|r| r["owner"].as_str())
            .filter(|o| {
                *o != home && *o != "unknown" && *o != "unresolved" && !foreign.contains(*o)
            })
            .map(str::to_owned)
            .collect();
        let name = module(s, path)?;
        if !foreign.is_empty() {
            cross.insert(name.clone(),json!({"home_domain":home,"writes_into":foreign,"tables":write.iter().filter(|r|foreign.contains(r["owner"].as_str().unwrap())).map(|r|r["table"].clone()).collect::<Vec<_>>()}));
        }
        modules.insert(name,json!({"file":path,"home_domain":home,"read":read,"write":write,"cross_domain_write_owners":foreign,"cross_domain_read_only_owners":read_foreign}));
    }
    let mut out = s.envelope("persistence-sql");
    out["schema_version"] = json!("arc04-native-persistence-sql/v1");
    out["known_table_count"] = json!(known.len());
    out["modules"] = json!(modules);
    out["cross_domain_writes"] = json!(cross);
    out["caveats"] = json!([
        "Static Rust strings only; dynamic SQL may be incomplete.",
        "Unresolved CTE/table references are retained, not silently dropped.",
        "Historical table-owner map is a documented claim, not current grants proof."
    ]);
    Ok(out)
}
pub(super) fn domains(s: &Snapshot) -> Result<Value> {
    let imports = consumption(s)?;
    let sql = persistence(s)?;
    let reference: Value = serde_json::from_str(include_str!("domain_reference.json"))?;
    let mut domains = BTreeMap::new();
    for (domain, decision) in reference["historical_domain_decisions"]
        .as_object()
        .unwrap()
    {
        let own: BTreeMap<_, _> = imports["modules"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| v["domain"] == *domain)
            .map(|(m, v)| (m.clone(), v.clone()))
            .collect();
        let tables: BTreeMap<_, _> = sql["modules"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| v["home_domain"] == *domain)
            .map(|(m, v)| (m.clone(), v.clone()))
            .collect();
        let loc: usize = own
            .values()
            .map(|v| v["loc"].as_u64().unwrap_or(0) as usize)
            .sum();
        let deps: BTreeSet<_> = own
            .values()
            .flat_map(|v| {
                v["io_deps"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
            })
            .collect();
        domains.insert(domain.clone(),json!({"module_count":own.len(),"total_loc":loc,"direct_io_deps":deps,"modules":own,"sql_tables":tables,"historical_owner_decision":decision,"decision_status":"Historical rationale retained for review; native file and consumer evidence generated above."}));
    }
    let mut out = s.envelope("domain-ownership");
    out["schema_version"] = json!("arc04-native-domain-ownership/v1");
    out["domains"] = json!(domains);
    out["sources"] = json!(["core-consumption.json", "persistence-sql.json"]);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_claims_preserved() {
        assert_eq!(owner("users"), "auth");
        assert_eq!(owner("gateway.users"), "auth");
        assert_eq!(owner("x.unmapped"), "schema:x");
        assert_eq!(owner("unmapped"), "unknown");
    }
}
