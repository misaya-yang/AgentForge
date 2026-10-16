//! Real Rust AST test hygiene plus the unchanged TypeScript test checks.
use crate::*;
use std::{
    collections::BTreeSet,
    time::{SystemTime, UNIX_EPOCH},
};
use syn::{parse::Parser, visit::Visit};
fn date(days: i64) -> String {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}
fn line(text: &str, at: usize) -> usize {
    text[..at].bytes().filter(|b| *b == b'\n').count() + 1
}
fn literal(expr: &syn::Expr) -> Option<Value> {
    match expr {
        syn::Expr::Lit(v) => match &v.lit {
            syn::Lit::Bool(v) => Some(json!(v.value)),
            syn::Lit::Str(v) => Some(json!(v.value())),
            syn::Lit::Int(v) => v.base10_parse::<i64>().ok().map(|v| json!(v)),
            syn::Lit::Float(v) => v.base10_parse::<f64>().ok().map(|v| json!(v)),
            syn::Lit::Char(v) => Some(json!(v.value().to_string())),
            _ => None,
        },
        syn::Expr::Binary(v) => {
            let a = literal(&v.left)?;
            let b = literal(&v.right)?;
            let result = match &v.op {
                syn::BinOp::And(_) => a.as_bool()? && b.as_bool()?,
                syn::BinOp::Or(_) => a.as_bool()? || b.as_bool()?,
                syn::BinOp::Eq(_) => a == b,
                syn::BinOp::Ne(_) => a != b,
                syn::BinOp::Lt(_) => a.as_i64()? < b.as_i64()?,
                syn::BinOp::Le(_) => a.as_i64()? <= b.as_i64()?,
                syn::BinOp::Gt(_) => a.as_i64()? > b.as_i64()?,
                syn::BinOp::Ge(_) => a.as_i64()? >= b.as_i64()?,
                _ => return None,
            };
            Some(json!(result))
        }
        syn::Expr::Unary(v) if matches!(v.op, syn::UnOp::Not(_)) => {
            Some(json!(!literal(&v.expr)?.as_bool()?))
        }
        syn::Expr::Paren(v) => literal(&v.expr),
        _ => None,
    }
}
// Restrict structural self-comparison to a single local name. Calls, indexing,
// dereferences and overloaded expressions may have effects and are not folded.
fn same_local(a: &syn::Expr, b: &syn::Expr) -> bool {
    match (a, b) {
        (syn::Expr::Paren(a), b) => same_local(&a.expr, b),
        (a, syn::Expr::Paren(b)) => same_local(a, &b.expr),
        (syn::Expr::Path(a), syn::Expr::Path(b)) if a.qself.is_none() && b.qself.is_none() => a
            .path
            .get_ident()
            .zip(b.path.get_ident())
            .is_some_and(|(a, b)| a == b),
        _ => false,
    }
}
fn proving(mac: &syn::Macro) -> bool {
    let Some(name) = mac.path.segments.last().map(|s| s.ident.to_string()) else {
        return false;
    };
    let Ok(args) = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated
        .parse2(mac.tokens.clone())
    else {
        return false;
    };
    match name.as_str() {
        "assert" => args.first().is_some_and(|a| literal(a) == Some(json!(true))
            || matches!(a, syn::Expr::Binary(b) if matches!(b.op, syn::BinOp::Eq(_)) && same_local(&b.left, &b.right))),
        "assert_eq" => {
            args.len() >= 2 && (same_local(&args[0], &args[1]) || literal(&args[0]).is_some_and(|a| literal(&args[1]) == Some(a)))
        }
        "assert_ne" => {
            args.len() >= 2
                && literal(&args[0]).is_some_and(|a| literal(&args[1]).is_some_and(|b| a != b))
        }
        _ => false,
    }
}
#[derive(Default)]
pub(crate) struct Scan {
    failures: Vec<Value>,
    warnings: Vec<Value>,
    pub(crate) markers: Vec<Value>,
    functions: usize,
}
pub(crate) fn scan_rust(file: &str, text: &str) -> Result<Scan> {
    let ast = syn::parse_file(text).map_err(|e| format!("{file}: Rust syntax error: {e}"))?;
    struct Visitor<'a> {
        file: &'a str,
        text: &'a str,
        scan: Scan,
    }
    impl<'ast> Visit<'ast> for Visitor<'_> {
        fn visit_item_fn(&mut self, f: &'ast syn::ItemFn) {
            let is_test = f
                .attrs
                .iter()
                .any(|a| a.path().segments.last().is_some_and(|s| s.ident == "test"));
            if is_test {
                self.scan.functions += 1;
                let name = f.sig.ident.to_string();
                let at = self.text.find(&format!("fn {name}")).unwrap_or(0);
                let n = line(self.text, at);
                let finding =
                    |issue: &str| json!({"file":self.file,"line":n,"test":name,"issue":issue});
                if f.block.stmts.is_empty() {
                    self.scan.failures.push(finding("empty test body"));
                }
                let macro_of = |s: &syn::Stmt| match s {
                    syn::Stmt::Macro(m) => Some(m.mac.clone()),
                    syn::Stmt::Expr(syn::Expr::Macro(m), _) => Some(m.mac.clone()),
                    _ => None,
                };
                if !f.block.stmts.is_empty()
                    && f.block
                        .stmts
                        .iter()
                        .all(|s| macro_of(s).is_some_and(|m| proving(&m)))
                {
                    self.scan
                        .failures
                        .push(finding("self-proving assertion-only test"));
                }
                if f.block.stmts.iter().any(|s| {
                    macro_of(s).is_some_and(|m| {
                        m.path.is_ident("todo") || m.path.is_ident("unimplemented")
                    })
                }) {
                    self.scan.failures.push(finding("placeholder test body"));
                }
                struct Assertions(bool);
                impl<'ast> Visit<'ast> for Assertions {
                    fn visit_macro(&mut self, m: &'ast syn::Macro) {
                        if m.path.segments.last().is_some_and(|s| {
                            [
                                "assert",
                                "assert_eq",
                                "assert_ne",
                                "assert_matches",
                                "panic",
                            ]
                            .iter()
                            .any(|n| s.ident == *n)
                        }) {
                            self.0 = true;
                        }
                    }
                }
                let mut assertions = Assertions(false);
                assertions.visit_block(&f.block);
                if !assertions.0 && !f.block.stmts.is_empty() {
                    self.scan
                        .warnings
                        .push(finding("no assertion token; may assert through helpers"));
                }
                for attr in &f.attrs {
                    if attr.path().is_ident("ignore") {
                        let reason = if let syn::Meta::NameValue(v) = &attr.meta {
                            literal(&v.value).and_then(|v| v.as_str().map(str::to_owned))
                        } else {
                            None
                        };
                        if reason.as_deref().is_none_or(|s| s.trim().is_empty()) {
                            self.scan
                                .failures
                                .push(finding("ignored test without reason"));
                        }
                        self.scan.markers.push(json!({"file":self.file,"line":n,"kind":"rust.ignore","target":name,"reason":reason}));
                    }
                }
            }
            syn::visit::visit_item_fn(self, f);
        }
    }
    let mut visitor = Visitor {
        file,
        text,
        scan: Scan::default(),
    };
    visitor.visit_file(&ast);
    Ok(visitor.scan)
}
fn scan_js(file: &str, text: &str) -> Result<Scan> {
    let mut scan = Scan::default();
    for (suffix, issue) in [
        ("only", ".only( focused test"),
        ("fixme", ".fixme( disabled test"),
    ] {
        let re = regex::Regex::new(&format!(r"\b(?:test|it|describe)\s*\.\s*{suffix}\s*\("))?;
        for m in re.find_iter(text) {
            scan.failures
                .push(json!({"file":file,"line":line(text,m.start()),"issue":issue}));
        }
    }
    let empty = regex::Regex::new(
        r#"(?s)\b(?:test|it)\s*\(\s*(?P<title>["'`][^"'`]*["'`])\s*,\s*(?:(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>|(?:async\s+)?function(?:\s+[A-Za-z_$][\w$]*)?\s*\([^)]*\))\s*\{(?P<body>(?:\s|;|//[^\n]*(?:\n|$)|/\*.*?\*/)*)\}\s*(?:,\s*(?:[0-9_]+\s*)?)?\)"#,
    )?;
    for m in empty.captures_iter(text) {
        let title = &m["title"];
        scan.failures.push(json!({"file":file,"line":line(text,m.get(0).unwrap().start()),"test":&title[1..title.len()-1],"issue":"empty test body"}));
    }
    for m in regex::Regex::new(r"\b(?:test|it|describe|suite)\s*\.\s*(skip|fixme|only)\b")?
        .captures_iter(text)
    {
        scan.markers
            .push(json!({"file":file,"line":line(text,m.get(0).unwrap().start()),"kind":&m[1]}));
    }
    Ok(scan)
}
fn marker_key(v: &Value, rust: bool) -> String {
    serde_json::to_string(&if rust {
        json!([v["file"], v["kind"], v["target"], v["reason"]])
    } else {
        json!([v["file"], v["kind"]])
    })
    .unwrap()
}
fn baseline(current: &BTreeMap<&str, Vec<Value>>, baseline: &Value) -> Result<Value> {
    require(
        matches!(
            baseline["schema"].as_str(),
            Some("ai-gateway/baseline/skip-baseline/v1" | "ai-gateway/baseline/skip-baseline/v2")
        ),
        "skip baseline schema invalid",
    )?;
    let mut unexpected = vec![];
    let mut retired = 0;
    let mut counts = BTreeMap::new();
    for language in ["rust", "typescript"] {
        let empty = vec![];
        let old = if language == "rust" && baseline.get("rust").is_none() {
            &empty
        } else {
            let old = baseline[language]["markers"]
                .as_array()
                .ok_or("skip markers missing")?;
            require(
                baseline[language]["marker_count"].as_u64() == Some(old.len() as u64),
                "skip marker count mismatch",
            )?;
            old
        };
        let mut remaining = BTreeMap::<String, usize>::new();
        for row in old {
            *remaining
                .entry(marker_key(row, language == "rust"))
                .or_default() += 1;
        }
        for row in &current[language] {
            let number = remaining
                .entry(marker_key(row, language == "rust"))
                .or_default();
            if *number > 0 {
                *number -= 1;
            } else {
                let mut row = row.clone();
                row["language"] = json!(language);
                unexpected.push(row);
            }
        }
        retired += remaining.values().sum::<usize>();
        counts.insert(
            language,
            json!({"baseline":old.len(),"current":current[language].len()}),
        );
    }
    Ok(json!({"counts":counts,"unexpected":unexpected,"retired":retired}))
}
fn exceptions(
    failures: Vec<Value>,
    entries: &[Value],
    today: &str,
) -> Result<(Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>)> {
    let mut enforced = vec![];
    let mut allowed = vec![];
    let mut expired = vec![];
    let mut matched = BTreeSet::new();
    for entry in entries {
        for key in ["file", "test", "owner", "reason", "expires"] {
            require(
                entry[key].as_str().is_some_and(|s| !s.is_empty()),
                "allowlist missing required field",
            )?;
        }
        let expires = entry["expires"].as_str().unwrap();
        require(
            super::evidence::timestamp(&format!("{expires}T00:00:00Z")),
            "invalid allowlist expiry",
        )?;
    }
    for mut failure in failures {
        if let Some((i, entry)) = entries
            .iter()
            .enumerate()
            .find(|(_, e)| e["file"] == failure["file"] && e["test"] == failure["test"])
        {
            matched.insert(i);
            failure["allowlist_owner"] = entry["owner"].clone();
            failure["expires"] = entry["expires"].clone();
            if entry["expires"].as_str().unwrap() < today {
                expired.push(failure);
            } else {
                allowed.push(failure);
            }
        } else {
            enforced.push(failure);
        }
    }
    let stale = entries
        .iter()
        .enumerate()
        .filter(|(i, _)| !matched.contains(i))
        .map(|(_, e)| json!({"file":e["file"],"test":e["test"],"owner":e["owner"]}))
        .collect();
    Ok((enforced, allowed, expired, stale))
}
pub fn run(args: &Args) -> Result<Value> {
    match inner(args) {
        Ok(value) => Ok(value),
        Err(error) => {
            let value = json!({"gate":"hygiene-check","tier":"L0","result":"error","scan_error":error.to_string()});
            let path = args
                .values
                .get("--evidence-out")
                .map(PathBuf::from)
                .unwrap_or_else(|| args.root().join("tmp/gate-evidence/hygiene-check.json"));
            write_json(&path, &value)?;
            Ok(value)
        }
    }
}
fn inner(args: &Args) -> Result<Value> {
    if args.flag("--selftest") {
        selftest()?;
        return Ok(json!({"result":"pass","selftest":"hygiene-check"}));
    }
    let root = args.root();
    let allowlist = args
        .values
        .get("--allowlist")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("scripts/harness/hygiene_allowlist.json"));
    let entries = if allowlist.exists() {
        object(&allowlist)?["entries"]
            .as_array()
            .ok_or("allowlist entries missing")?
            .clone()
    } else {
        vec![]
    };
    let mut failures = vec![];
    let mut warnings = vec![];
    let mut markers = BTreeMap::from([("rust", vec![]), ("typescript", vec![])]);
    let mut rust_files = 0;
    let mut ts_files = 0;
    let prune = [
        "node_modules",
        "dist",
        "build",
        "__pycache__",
        ".venv",
        "venv",
        ".pytest_cache",
        ".ruff_cache",
        ".mypy_cache",
        ".playwright",
        "test-results",
        "playwright-report",
        "tmp",
        "target",
    ];
    for directory in ["rust", "web", "sdk"] {
        let start = root.join(directory);
        if !start.exists() {
            continue;
        }
        require(
            start.is_dir() && !start.is_symlink(),
            "hygiene scan root must be real directory",
        )?;
        for relative in walk(&start, &prune, true)? {
            let path = start.join(relative);
            require(
                !path.is_symlink(),
                "hygiene refuses incomplete symlink scan",
            )?;
            let file = path.strip_prefix(&root)?.to_string_lossy().into_owned();
            let mut scan = if directory == "rust" && path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path)?;
                let scan = scan_rust(&file, &text)?;
                if scan.functions > 0 {
                    rust_files += 1;
                }
                markers
                    .get_mut("rust")
                    .unwrap()
                    .extend(scan.markers.clone());
                scan
            } else if directory != "rust"
                && regex::Regex::new(r"\.(test|spec)\.tsx?$")?.is_match(&file)
            {
                ts_files += 1;
                let scan = scan_js(&file, &std::fs::read_to_string(&path)?)?;
                markers
                    .get_mut("typescript")
                    .unwrap()
                    .extend(scan.markers.clone());
                scan
            } else {
                continue;
            };
            failures.append(&mut scan.failures);
            warnings.append(&mut scan.warnings);
        }
    }
    require(
        rust_files > 0 && ts_files > 0,
        "Rust and TypeScript test scans must both be nonempty",
    )?;
    let baseline_path = args
        .values
        .get("--skip-baseline")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            root.join("docs/architecture/baselines/2026-08-post-rag/skip-baseline.json")
        });
    let skips = baseline(&markers, &object(&baseline_path)?)?;
    let today = date((SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() / 86400) as i64);
    let (enforced, allowed, expired, stale) = exceptions(failures, &entries, &today)?;
    let value = json!({"gate":"hygiene-check","tier":"L0","scanned":{"rust_test_files":rust_files,"typescript_test_files":ts_files},"failures":enforced,"allowlisted":allowed,"expired_entries":expired,"stale_entries":stale,"no_assertion_warnings":warnings,"skip_baseline":skips,"result":if enforced.is_empty()&&expired.is_empty()&&stale.is_empty()&&skips["unexpected"].as_array().unwrap().is_empty(){"pass"}else{"fail"}});
    write_json(
        &args
            .values
            .get("--evidence-out")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("tmp/gate-evidence/hygiene-check.json")),
        &value,
    )?;
    Ok(value)
}
fn selftest() -> Result<()> {
    let scan = scan_rust(
        "fixture.rs",
        r#"#[test]fn empty(){} #[test]fn obvious(){assert!(true);} #[test]#[ignore]fn ignored(){let actual=1;assert_eq!(actual,1);} #[test]fn real(){let actual=1;assert_eq!(actual,1);}"#,
    )?;
    require(
        scan.failures.len() == 3 && scan.functions == 4,
        "Rust hygiene selftest failed",
    )?;
    require(
        scan_js(
            "fixture.test.ts",
            "test.only('x',()=>{});test('empty',()=>{/*todo*/});",
        )?
        .failures
        .len()
            == 2,
        "TypeScript hygiene selftest failed",
    )?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_ast_checks() {
        selftest().unwrap();
        assert!(scan_rust("bad.rs", "fn broken(").is_err());
    }
    #[test]
    fn skip_drift_and_allowlist_expiry() {
        let current = BTreeMap::from([
            (
                "rust",
                vec![json!({"file":"x","kind":"rust.ignore","target":"t","reason":"why"})],
            ),
            ("typescript", vec![]),
        ]);
        let b = json!({"schema":"ai-gateway/baseline/skip-baseline/v1","typescript":{"markers":[],"marker_count":0}});
        assert_eq!(
            baseline(&current, &b).unwrap()["unexpected"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let entries = vec![
            json!({"file":"x","test":"t","owner":"team","reason":"why","expires":"2020-01-01"}),
        ];
        let (_, a, e, s) =
            exceptions(vec![json!({"file":"x","test":"t"})], &entries, "2026-10-15").unwrap();
        assert!(a.is_empty() && s.is_empty());
        assert_eq!(e.len(), 1);
    }
    #[test]
    fn utc_date_conversion() {
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(20741), "2026-10-15");
    }
}
