use super::*;
use syn::visit::Visit;
pub(super) fn line_count(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    text.split_inclusive([
        '\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}',
        '\u{2029}',
    ])
    .count()
        - text.as_bytes().windows(2).filter(|b| *b == b"\r\n").count()
}
fn is_test(p: &str) -> bool {
    p.starts_with("tests/")
        || p.contains("/tests/")
        || p.contains("/e2e/")
        || p.rsplit('/').next().is_some_and(|n| {
            n.starts_with("test_")
                || [".test.ts", ".test.tsx", ".spec.ts", ".spec.tsx"]
                    .iter()
                    .any(|s| n.ends_with(s))
        })
}
pub(super) fn loc(s: &Snapshot) -> Result<Value> {
    let mut rust = BTreeMap::new();
    let mut ts = BTreeMap::new();
    for (path, bytes) in &s.files {
        let target = if path.ends_with(".rs") {
            &mut rust
        } else if path.ends_with(".ts") || path.ends_with(".tsx") {
            &mut ts
        } else {
            continue;
        };
        target.insert(path.clone(), line_count(std::str::from_utf8(bytes)?));
    }
    fn totals(rows: &BTreeMap<String, usize>, production: bool) -> Value {
        let rows: Vec<_> = rows
            .iter()
            .filter(|(p, _)| !production || !is_test(p))
            .collect();
        json!({"files":rows.len(),"lines":rows.iter().map(|(_,n)|**n).sum::<usize>()})
    }
    fn oversized(rows: &BTreeMap<String, usize>, limit: usize) -> Value {
        let mut files: Vec<_> = rows
            .iter()
            .filter(|(_, n)| **n > limit)
            .map(|(p, n)| json!({"file":p,"lines":n,"unit":unit(p),"is_test":is_test(p)}))
            .collect();
        files.sort_by(|a, b| {
            b["lines"]
                .as_u64()
                .cmp(&a["lines"].as_u64())
                .then(a["file"].as_str().cmp(&b["file"].as_str()))
        });
        let mut units = BTreeMap::<String, usize>::new();
        for row in &files {
            *units
                .entry(row["unit"].as_str().unwrap().into())
                .or_default() += 1;
        }
        json!({"threshold_lines":limit,"count":files.len(),"production_count":files.iter().filter(|r|r["is_test"]==false).count(),"by_unit":units,"files":files})
    }
    let mut out = s.envelope("loc-baseline");
    out["thresholds"] = json!({"rust_new_file_max":800,"typescript_new_file_max":500,"policy":"Oversized files are no-growth; reviewed owner/reason/expiry required for exceptions."});
    out["totals"] = json!({"rust":totals(&rust,false),"rust_production":totals(&rust,true),"typescript":totals(&ts,false),"typescript_production":totals(&ts,true)});
    out["oversized_rust"] = oversized(&rust, 800);
    out["oversized_typescript"] = oversized(&ts, 500);
    out["rust_large_informational"] = oversized(&rust, 2000);
    out["methodology"] = json!([
        "Counts read from immutable Git blobs, not working tree.",
        "Version 2 measures native Rust source; historical Python ledger remains unchanged.",
        "Test classification is by path; no claims that ignored/live tests passed."
    ]);
    Ok(out)
}
#[derive(Default)]
pub(super) struct SourceFacts {
    pub literals: Vec<String>,
    pub includes: Vec<String>,
    pub paths: BTreeSet<String>,
    pub public_exports: BTreeSet<String>,
    pub functions: usize,
}
impl<'ast> Visit<'ast> for SourceFacts {
    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        use syn::parse::Parser;
        if node.path.is_ident("include_str") {
            if let Ok(path) = syn::parse2::<syn::LitStr>(node.tokens.clone()) {
                self.includes.push(path.value());
            }
        } else if let Ok(expressions) =
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated
                .parse2(node.tokens.clone())
        {
            // Macro input is opaque to syn's default visitor. SQL and format
            // macros still contain ordinary expressions, including literals.
            let mut nested = SourceFacts::default();
            for expression in &expressions {
                nested.visit_expr(expression);
            }
            self.literals.extend(nested.literals);
            self.includes.extend(nested.includes);
            self.paths.extend(nested.paths);
        }
        syn::visit::visit_macro(self, node);
    }
    fn visit_lit_str(&mut self, node: &'ast syn::LitStr) {
        self.literals.push(node.value());
    }
    fn visit_path(&mut self, node: &'ast syn::Path) {
        self.paths.insert(
            node.segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::"),
        );
        syn::visit::visit_path(self, node);
    }
    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        fn paths(tree: &syn::UseTree, prefix: &str, out: &mut BTreeSet<String>) {
            match tree {
                syn::UseTree::Path(p) => paths(&p.tree, &format!("{prefix}{}::", p.ident), out),
                syn::UseTree::Name(n) => {
                    out.insert(format!("{prefix}{}", n.ident));
                }
                syn::UseTree::Rename(n) => {
                    out.insert(format!("{prefix}{}", n.ident));
                }
                syn::UseTree::Glob(_) => {
                    out.insert(format!("{prefix}*"));
                }
                syn::UseTree::Group(g) => {
                    for t in &g.items {
                        paths(t, prefix, out)
                    }
                }
            }
        }
        paths(&node.tree, "", &mut self.paths);
        if matches!(node.vis, syn::Visibility::Public(_)) {
            paths(&node.tree, "", &mut self.public_exports);
        }
        syn::visit::visit_item_use(self, node);
    }
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        self.functions += 1;
        syn::visit::visit_item_fn(self, node);
    }
}
pub(super) fn source(text: &str) -> Result<SourceFacts> {
    let tree = syn::parse_file(text)?;
    let mut facts = SourceFacts::default();
    facts.visit_file(&tree);
    Ok(facts)
}
pub(super) fn sql_literals(s: &Snapshot, path: &str) -> Result<Vec<String>> {
    let facts = source(s.text(path)?)?;
    let mut literals = facts.literals;
    for include in facts.includes {
        let joined = Path::new(path)
            .parent()
            .ok_or("source parent missing")?
            .join(include);
        let mut normalized = PathBuf::new();
        for component in joined.components() {
            match component {
                std::path::Component::Normal(p) => normalized.push(p),
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    require(normalized.pop(), "include_str escapes snapshot")?;
                }
                _ => return Err("absolute include_str path is not an immutable source fact".into()),
            }
        }
        let name = normalized.to_str().ok_or("non-UTF8 include path")?;
        // include_str can load text fixtures as well as SQL. The SQL reference
        // classifier below decides relevance without dropping external queries.
        literals.push(s.text(name)?.to_owned());
    }
    Ok(literals)
}
fn dep_sections(value: &toml::Value, prefix: &str, out: &mut BTreeMap<String, Value>) {
    if let Some(table) = value.as_table() {
        for (k, v) in table {
            let path = if prefix.is_empty() {
                k.clone()
            } else {
                format!("{prefix}.{k}")
            };
            if ["dependencies", "dev-dependencies", "build-dependencies"].contains(&k.as_str()) {
                out.insert(path, serde_json::to_value(v).expect("TOML JSON"));
            } else if k == "target" || prefix.starts_with("target") {
                dep_sections(v, &path, out)
            }
        }
    }
}
pub(super) fn dependencies(s: &Snapshot) -> Result<Value> {
    let mut declared = BTreeMap::new();
    let mut packages = BTreeMap::new();
    for (path, bytes) in &s.files {
        if !path.ends_with("Cargo.toml") {
            continue;
        }
        let manifest: toml::Value = toml::from_str(std::str::from_utf8(bytes)?)?;
        let Some(name) = manifest
            .get("package")
            .and_then(|v| v.get("name"))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        let mut sections = BTreeMap::new();
        dep_sections(&manifest, "", &mut sections);
        declared.insert(
            name.to_owned(),
            json!({"manifest":path,"found":true,"dependency_sections":sections}),
        );
        packages.insert(
            name.replace('-', "_"),
            (
                name.to_owned(),
                path.trim_end_matches("Cargo.toml").to_owned(),
            ),
        );
    }
    let mut edges = BTreeMap::<(String, String), (BTreeSet<String>, BTreeSet<String>)>::new();
    for (path, bytes) in &s.files {
        if !path.ends_with(".rs") {
            continue;
        }
        let owner = packages
            .values()
            .filter(|(_, base)| path.starts_with(base))
            .max_by_key(|(_, base)| base.len())
            .map(|(name, _)| name.clone())
            .unwrap_or_else(|| unit(path));
        for import in source(std::str::from_utf8(bytes)?)?.paths {
            let top = import.split("::").next().unwrap_or("");
            if let Some((target, _)) = packages.get(top) {
                if target != &owner {
                    let row = edges.entry((owner.clone(), target.clone())).or_default();
                    row.0.insert(import);
                    row.1.insert(path.clone());
                }
            }
        }
    }
    let rows:Vec<_>=edges.into_iter().map(|((from,to),(modules,files))|json!({"from_unit":from,"to_unit":to,"imported_modules":modules,"importing_file_count":files.len(),"importing_files":files})).collect();
    let mut out = s.envelope("dependency-baseline");
    out["declared_dependencies"] = json!(declared);
    out["import_edges"] = json!({"edges":rows,"rule":"Cargo dependency and Rust AST facts; transport boundary decisions enforced by architecture-boundary gate."});
    out["source_units"] = json!(packages);
    Ok(out)
}
pub(super) fn skips(s: &Snapshot) -> Result<Value> {
    let mut rust = vec![];
    let mut ts = vec![];
    let ts_pattern = regex::Regex::new(r"\b(?:test|it|describe|suite)\s*\.\s*(skip|fixme|only)\b")?;
    for (path, bytes) in &s.files {
        let text = std::str::from_utf8(bytes);
        if path.ends_with(".rs") {
            rust.extend(crate::hygiene::scan_rust(path, text?)?.markers);
        } else if [".spec.ts", ".spec.tsx", ".test.ts", ".test.tsx"]
            .iter()
            .any(|s| path.ends_with(s))
        {
            let text = text?;
            for m in ts_pattern.captures_iter(text) {
                ts.push(json!({"file":path,"line":text[..m.get(0).unwrap().start()].bytes().filter(|b|*b==b'\n').count()+1,"kind":m[1].to_owned()}));
            }
        }
    }
    fn summary(rows: Vec<Value>) -> Value {
        let files: BTreeSet<_> = rows.iter().filter_map(|r| r["file"].as_str()).collect();
        let live = rows
            .iter()
            .filter(|r| {
                let reason = r["reason"].as_str().unwrap_or("").to_lowercase();
                [
                    "set run_",
                    "live",
                    "credentials",
                    "password",
                    "env",
                    "docker",
                    "not available locally",
                ]
                .iter()
                .any(|s| reason.contains(s))
            })
            .count();
        json!({"marker_count":rows.len(),"file_count":files.len(),"classification":{"conditional_live_or_env_gated":live,"unconditional_or_static":rows.len()-live},"markers":rows})
    }
    let mut out = s.envelope("skip-baseline");
    out["policy"] = json!(
        "Release-required gates must show zero unexpected skips. A named ignored test is not passing coverage."
    );
    out["rust"] = summary(rust);
    out["typescript"] = summary(ts);
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_line_counts() {
        assert_eq!(line_count("a\r\nb\u{2028}c"), 3);
        assert_eq!(line_count(""), 0);
    }
    #[test]
    fn rust_ast_facts() {
        let f = source("pub use dep::{A,B}; fn x(){let q=\"SELECT * FROM users\";dep::call();}")
            .unwrap();
        assert!(f.paths.contains("dep::A"));
        assert!(f.public_exports.contains("dep::B"));
        assert_eq!(f.literals, ["SELECT * FROM users"]);
    }
}
