//! Four explicitly scoped repository quality candidates, with native repair mappings.
use crate::*;
use syn::visit::Visit;
const EXPECTED: [(&str, &str, &str); 4] = [
    (
        "file-storage-barrel-only-export",
        "src/persistence/storage.py",
        "FileStorage",
    ),
    (
        "unused-get-langgraph-proxy",
        "src/api/deps.py",
        "get_langgraph_proxy",
    ),
    (
        "streaming-timeout-placeholder",
        "tests/proxy/test_streaming.py",
        "test_streaming_timeout_handling",
    ),
    (
        "streaming-connection-placeholder",
        "tests/proxy/test_streaming.py",
        "test_connection_error_handling",
    ),
];
struct References<'a> {
    symbol: &'a str,
    found: bool,
}
impl<'ast> Visit<'ast> for References<'_> {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        if ident == self.symbol {
            self.found = true;
        }
    }
}
// The removed Python persistence barrel and the active native upload service
// share a short name. Accept only the actual declared/imported native identity;
// an import of a resurrected legacy storage path still fails inside either file.
fn distinct_upload_identity(file: &Path, ast: &syn::File) -> bool {
    let declaration =
        file == Path::new("platform-native/crates/platform-gateway/src/migration_content/files.rs");
    let composition =
        file == Path::new("platform-native/crates/platform-gateway/src/migration_host/content.rs");
    if !declaration && !composition {
        return false;
    }
    fn imports(tree: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                imports(&p.tree, prefix, out);
                prefix.pop();
            }
            syn::UseTree::Group(g) => {
                for item in &g.items {
                    imports(item, prefix, out);
                }
            }
            syn::UseTree::Name(n) => {
                let mut p = prefix.clone();
                p.push(n.ident.to_string());
                out.push(p);
            }
            syn::UseTree::Rename(n) => {
                let mut p = prefix.clone();
                p.push(n.ident.to_string());
                if n.rename == "FileStorage" {
                    p.push("unexpected-alias".into());
                }
                out.push(p);
            }
            syn::UseTree::Glob(_) => {}
        }
    }
    fn upload_path(p: &[String]) -> bool {
        p == ["crate", "migration_content", "files", "FileStorage"]
    }
    struct Identity {
        declared: bool,
        imported: bool,
        bad: bool,
    }
    impl<'ast> Visit<'ast> for Identity {
        fn visit_item_struct(&mut self, item: &'ast syn::ItemStruct) {
            if item.ident == "FileStorage" {
                self.declared = item
                    .fields
                    .iter()
                    .any(|f| f.ident.as_ref().is_some_and(|i| i == "backend"));
            }
            syn::visit::visit_item_struct(self, item);
        }
        fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
            let mut paths = vec![];
            imports(&item.tree, &mut vec![], &mut paths);
            for p in paths {
                if p.iter()
                    .any(|s| s == "FileStorage" || s == "unexpected-alias")
                {
                    if upload_path(&p) {
                        self.imported = true;
                    } else {
                        self.bad = true;
                    }
                }
            }
            syn::visit::visit_item_use(self, item);
        }
        fn visit_path(&mut self, p: &'ast syn::Path) {
            let parts: Vec<_> = p.segments.iter().map(|s| s.ident.to_string()).collect();
            if parts.len() > 1 && parts.iter().any(|s| s == "FileStorage") && !upload_path(&parts) {
                self.bad = true;
            }
            syn::visit::visit_path(self, p);
        }
    }
    let mut identity = Identity {
        declared: false,
        imported: false,
        bad: false,
    };
    identity.visit_file(ast);
    !identity.bad
        && if declaration {
            identity.declared && !identity.imported
        } else {
            identity.imported && !identity.declared
        }
}
fn repair(file: &str, source: &str, name: &str, status: &str) -> Result<()> {
    let ast = syn::parse_file(source)?;
    struct Find<'a> {
        name: &'a str,
        found: Vec<syn::ItemFn>,
    }
    impl<'ast> Visit<'ast> for Find<'_> {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            if f.sig.ident == self.name {
                self.found.push(f.clone());
            }
            syn::visit::visit_item_fn(self, f);
        }
    }
    let mut find = Find {
        name,
        found: vec![],
    };
    find.visit_file(&ast);
    require(
        find.found.len() == 1,
        format!("native repair test absent/ambiguous: {file}:{name}"),
    )?;
    let f = &find.found[0];
    require(
        f.attrs
            .iter()
            .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"))
            && !f.block.stmts.is_empty(),
        "native repair must be real test function",
    )?;
    struct Proof<'a> {
        status: &'a str,
        assertions: usize,
        status_seen: bool,
        proxy_call: bool,
        slot_reference: bool,
        helper_calls: Vec<String>,
    }
    impl<'ast> Visit<'ast> for Proof<'_> {
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            let tokens = m.tokens.to_string();
            if m.path
                .segments
                .last()
                .is_some_and(|s| s.ident.to_string().starts_with("assert"))
            {
                self.assertions += 1;
                self.status_seen |= tokens.contains(self.status)
                    || tokens.contains(if self.status == "504" {
                        "GATEWAY_TIMEOUT"
                    } else {
                        "BAD_GATEWAY"
                    });
            }
        }
        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(path) = c.func.as_ref() {
                if let Some(segment) = path.path.segments.last() {
                    self.helper_calls.push(segment.ident.to_string());
                }
                self.proxy_call |= path
                    .path
                    .segments
                    .last()
                    .is_some_and(|s| s.ident.to_string().contains("proxy_streaming"));
            }
            syn::visit::visit_expr_call(self, c);
        }
        fn visit_expr_method_call(&mut self, c: &'ast syn::ExprMethodCall) {
            self.proxy_call |=
                c.method.to_string().contains("proxy_streaming") || c.method == "proxy";
            syn::visit::visit_expr_method_call(self, c);
        }
        fn visit_ident(&mut self, i: &'ast syn::Ident) {
            let s = i.to_string();
            self.slot_reference |= s.contains("slot") || s.contains("limit");
        }
    }
    let mut proof = Proof {
        status,
        assertions: 0,
        status_seen: false,
        proxy_call: false,
        slot_reference: false,
        helper_calls: vec![],
    };
    proof.visit_block(&f.block);
    // The production regression shares a local assertion helper. Inspect only
    // helpers actually called by the selected test, never unrelated functions.
    let mut visited = std::collections::BTreeSet::new();
    while let Some(name) = proof.helper_calls.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let mut helper = Find {
            name: &name,
            found: vec![],
        };
        helper.visit_file(&ast);
        if helper.found.len() == 1 {
            proof.visit_block(&helper.found[0].block);
        }
    }
    require(
        proof.assertions >= 2 && proof.status_seen && proof.proxy_call && proof.slot_reference,
        "native repair must call proxy transport and assert status plus streaming slot release",
    )?;
    Ok(())
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root();
    let path = args
        .values
        .get("--manifest")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("deploy/repository-quality/known-candidates.json"));
    let manifest = object(&path)?;
    require(
        manifest["schema_version"] == "ai-platform/repository-quality-candidates/v1",
        "candidate manifest schema invalid",
    )?;
    let rows = manifest["candidates"]
        .as_array()
        .ok_or("candidate rows missing")?;
    require(
        rows.len() == EXPECTED.len()
            && EXPECTED
                .iter()
                .all(|(id, _, _)| rows.iter().filter(|r| r["id"] == *id).count() == 1),
        "candidate set differs from explicit allowlist",
    )?;
    let mut repaired = vec![];
    let mut removed = vec![];
    let mut pending = vec![];
    let mut distinct_live_symbols = vec![];
    let files = walk(&root.join("rust"), harness::PRUNED, false)?;
    for (id, path, symbol) in EXPECTED {
        let row = rows.iter().find(|r| r["id"] == id).unwrap();
        require(
            row["path"] == path && row["symbol"] == symbol,
            "candidate provenance identity drift",
        )?;
        if row["classification"] == "self_proving_test" {
            let native_path = row["native_path"]
                .as_str()
                .ok_or("native test mapping required for repaired candidate")?;
            require(
                native_path.starts_with("rust/") && native_path.ends_with(".rs"),
                "native repair path must be Rust source",
            )?;
            let native_name = row["native_symbol"]
                .as_str()
                .ok_or("native repair symbol missing")?;
            let source = std::fs::read_to_string(safe(&root, native_path)?)?;
            repair(
                native_path,
                &source,
                native_name,
                if id.contains("timeout") { "504" } else { "502" },
            )?;
            repaired.push(id);
        } else {
            require(
                row["classification"] == "confirmed_dead",
                "candidate classification drift",
            )?;
            let mut hits = vec![];
            for path in &files {
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let ast = syn::parse_file(&std::fs::read_to_string(root.join("rust").join(path))?)?;
                let mut visitor = References {
                    symbol,
                    found: false,
                };
                visitor.visit_file(&ast);
                if visitor.found
                    && id == "file-storage-barrel-only-export"
                    && distinct_upload_identity(path, &ast)
                {
                    distinct_live_symbols.push(json!({"candidate":id,"file":path,"qualified_identity":"crate::migration_content::files::FileStorage","reason":"active native upload service, not removed src.persistence.storage barrel"}));
                } else if visitor.found {
                    hits.push(path.to_string_lossy().into_owned());
                }
            }
            if !hits.is_empty() {
                return Err(format!(
                    "confirmed-dead candidate gained native consumers: {id}: {hits:?}"
                )
                .into());
            }
            if row["disposition"] == "removed" {
                require(
                    !root.join(path).exists(),
                    "legacy dead candidate file remains",
                )?;
                removed.push(id);
            } else {
                pending.push(id);
            }
        }
    }
    Ok(
        json!({"result":if pending.is_empty(){"pass"}else{"blocked"},"repaired":repaired,"removed":removed,"pending_scoped_removal":pending,"distinct_live_symbols":distinct_live_symbols}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinct_live_upload_does_not_hide_resurrected_legacy_consumer() {
        let file =
            Path::new("platform-native/crates/platform-gateway/src/migration_host/content.rs");
        let active = syn::parse_file("use crate::migration_content::files::FileStorage; fn compose(){ let _=FileStorage{backend: backend()}; }").unwrap();
        assert!(distinct_upload_identity(file, &active));
        let legacy = syn::parse_file("use crate::persistence::storage::FileStorage; fn compose(){ let _=FileStorage::new(); }").unwrap();
        assert!(!distinct_upload_identity(file, &legacy));
        let collision = syn::parse_file("use crate::migration_content::files::FileStorage; fn bad(){ crate::persistence::storage::FileStorage::new(); }").unwrap();
        assert!(!distinct_upload_identity(file, &collision));
        assert!(!distinct_upload_identity(
            Path::new("elsewhere.rs"),
            &active
        ));
    }
    #[test]
    fn repair_requires_real_proxy_call_and_slot_assertion() {
        let source = "#[test] fn timeout(){let slots=limiter(); let response=proxy_streaming(); assert_eq!(response.status(),504);assert_eq!(slots.active(),0);}";
        assert!(repair("f.rs", source, "timeout", "504").is_ok());
        assert!(
            repair(
                "f.rs",
                "#[test]fn timeout(){assert_eq!(504,504);}",
                "timeout",
                "504"
            )
            .is_err()
        );
    }
}
