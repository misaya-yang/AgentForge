//! AST-based native equivalent of gateway_kb_boundary_gate.py. Knowledge-owned
//! implementation modules are outside the Gateway data-plane scan boundary.
use regex::Regex;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use syn::visit::{self, Visit};
const TABLES: &str = "child_chunks|confluence_connections|confluence_image_sync|confluence_pages|confluence_space_bindings|confluence_sync_tasks|confluence_webhooks|dataset_collection_bindings|dataset_keyword_tables|dataset_permissions|dataset_process_rules|dataset_queries|datasets|document_permissions|document_pipeline_executions|document_summaries|document_versions|documents|embedding_migration_progress|embedding_migrations|embedding_vector_cache|image_segments|kb_bm25_v2_lifecycle|kb_eval_golden|kb_eval_golden_release|segment_images|segments|version_retention_policies";
fn sql_patterns() -> (Regex, Regex) {
    (Regex::new(&format!(r#"(?i)\b(?:FROM|INTO|UPDATE|JOIN|REFERENCES|(?:ALTER|CREATE|DROP|TRUNCATE)\s+TABLE(?:\s+IF\s+(?:NOT\s+)?EXISTS)?|CREATE\s+(?:UNIQUE\s+)?INDEX(?:\s+IF\s+NOT\s+EXISTS)?\s+\S+\s+ON)\s+(?:ONLY\s+)?(?:[`"]?knowledge[`"]?\s*\.\s*)?[`"]?(?:{TABLES})\b[`"]?"#)).unwrap(),Regex::new(&format!(r#"(?i)(?:to_regclass\(\s*['"](?:knowledge\.)?|\btable_name\s*=\s*['"]|\btablename\s*=\s*['"])(?:{TABLES})\b[`"]?"#)).unwrap())
}
fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = vec![];
    for entry in fs::read_dir(root).map_err(|e| format!("{}: {e}", root.display()))? {
        let p = entry.map_err(|e| e.to_string())?.path();
        if p.is_dir() {
            out.extend(files(&p)?)
        } else if p.extension().is_some_and(|s| s == "rs") {
            out.push(p)
        }
    }
    out.sort();
    Ok(out)
}
fn serialized(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("derive")
            && a.parse_args_with(
                syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
            )
            .is_ok_and(|ps| {
                ps.iter().any(|p| {
                    p.segments.last().is_some_and(|s| {
                        matches!(s.ident.to_string().as_str(), "Deserialize" | "Serialize")
                    })
                })
            })
    })
}
#[derive(Default)]
struct Models(BTreeSet<String>);
impl<'a> Visit<'a> for Models {
    fn visit_item_struct(&mut self, n: &'a syn::ItemStruct) {
        if serialized(&n.attrs) {
            self.0.insert(n.ident.to_string());
        }
        visit::visit_item_struct(self, n)
    }
    fn visit_item_enum(&mut self, n: &'a syn::ItemEnum) {
        if serialized(&n.attrs) {
            self.0.insert(n.ident.to_string());
        }
        visit::visit_item_enum(self, n)
    }
}
struct Check<'a> {
    path: &'a str,
    models: &'a BTreeSet<String>,
    sql: Regex,
    metadata: Regex,
    names: Regex,
    findings: Vec<String>,
}
impl Check<'_> {
    fn schema(&mut self, id: &syn::Ident, attrs: &[syn::Attribute]) {
        if !serialized(attrs) {
            return;
        }
        let name = id.to_string();
        if self.models.contains(&name) {
            self.findings.push(format!("{}:{}: '{name}' mirrors a knowledge-service API contract class (proxy KB bodies through; PRD T8.4 rule 2)",self.path,id.span().start().line));
        } else if self.names.is_match(&name) {
            self.findings.push(format!(
                "{}:{}: KB request schema '{name}' defined in gateway (PRD T8.4 rule 2)",
                self.path,
                id.span().start().line
            ));
        }
    }
}
impl<'a> Visit<'a> for Check<'_> {
    fn visit_lit_str(&mut self, n: &'a syn::LitStr) {
        let s = n.value();
        if self.sql.is_match(&s) || self.metadata.is_match(&s) {
            self.findings.push(format!("{}:{}: KB-table SQL in gateway data plane (knowledge-service owns this storage; PRD T8.4 rule 1)",self.path,n.span().start().line));
        }
    }
    fn visit_item_struct(&mut self, n: &'a syn::ItemStruct) {
        self.schema(&n.ident, &n.attrs);
        visit::visit_item_struct(self, n)
    }
    fn visit_item_enum(&mut self, n: &'a syn::ItemEnum) {
        self.schema(&n.ident, &n.attrs);
        visit::visit_item_enum(self, n)
    }
    fn visit_macro(&mut self, n: &'a syn::Macro) {
        fn tokens(c: &mut Check<'_>, ts: proc_macro2::TokenStream) {
            for t in ts {
                match t {
                    proc_macro2::TokenTree::Group(g) => tokens(c, g.stream()),
                    proc_macro2::TokenTree::Literal(l) => {
                        if let Ok(mut s) = syn::parse_str::<syn::LitStr>(&l.to_string()) {
                            s.set_span(l.span());
                            c.visit_lit_str(&s)
                        }
                    }
                    _ => {}
                }
            }
        }
        tokens(self, n.tokens.clone());
    }
}
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let base = root.join("rust/platform-native/crates");
    let mut models = Models::default();
    for owner in [
        "platform-knowledge-service/src",
        "platform-gateway-integrations/src/migration_knowledge_ingest",
        "platform-gateway-integrations/src/migration_knowledge_query",
    ] {
        for p in files(&base.join(owner))? {
            let source = fs::read_to_string(&p).map_err(|e| e.to_string())?;
            let tree = syn::parse_file(&source).map_err(|e| format!("{}: {e}", p.display()))?;
            models.visit_file(&tree);
        }
    }
    let mut findings = BTreeSet::new();
    for owner in [
        "platform-gateway/src",
        "platform-gateway-foundation/src",
        "platform-gateway-integrations/src/migration_adapters",
        "platform-gateway-integrations/src/migration_content",
        "platform-gateway-integrations/src/migration_local_node",
        "platform-gateway-integrations/src/chatgpt_plan",
    ] {
        for p in files(&base.join(owner))? {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string();
            let result = fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|s| syn::parse_file(&s).map_err(|e| e.to_string()));
            match result {
                Err(e) => {
                    findings.insert(format!("{rel}:0: unreadable ({e})"));
                }
                Ok(tree) => {
                    let (sql, metadata) = sql_patterns();
                    let mut c=Check{path:&rel,models:&models.0,sql,metadata,names:Regex::new(r"^(?:Dataset\w*(?:Schema|Request|Payload)|Segment\w*(?:Schema|Request|Payload)|Document\w*(?:Schema|Request|Payload)|Retrieve\w*(?:Schema|Request|Payload)|KBSearchRequest|KBQueryRequest|Confluence\w*(?:Schema|CreateSchema|UpdateSchema))$").unwrap(),findings:vec![]};
                    c.visit_file(&tree);
                    findings.extend(c.findings);
                }
            }
        }
    }
    Ok(findings.into_iter().collect())
}
pub fn run(args: &[String]) -> u8 {
    let root = match args {
        [] => std::env::current_dir().unwrap_or_default(),
        [flag, path] if flag == "--repo-root" => PathBuf::from(path),
        _ => {
            eprintln!("Usage: ai-platform-openapi gateway-kb-boundary [--repo-root ROOT]");
            return 2;
        }
    };
    match check(&root) {
        Err(e) => {
            eprintln!("Gateway KB boundary gate: BLOCKED ({e})");
            2
        }
        Ok(findings) => {
            for f in &findings {
                println!("{f}");
            }
            if findings.is_empty() {
                println!("Gateway KB boundary gate: PASS");
                0
            } else {
                println!(
                    "Gateway KB boundary gate: FAIL ({} violations)",
                    findings.len()
                );
                1
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sql_identifier_matrix() {
        let (a, b) = sql_patterns();
        for sql in [
            "SELECT * FROM knowledge.datasets",
            "UPDATE `documents` SET x=1",
            "CREATE UNIQUE INDEX idx ON segments(id)",
            "to_regclass('knowledge.documents')",
            "table_name='datasets'",
        ] {
            assert!(a.is_match(sql) || b.is_match(sql), "{sql}")
        }
        assert!(!a.is_match("SELECT * FROM assistant_sessions"));
    }
    #[test]
    fn macro_sql_is_not_hidden() {
        let tree =
            syn::parse_file(r##"fn f(){sqlx::query!(r#"SELECT * FROM knowledge.datasets"#);} "##)
                .unwrap();
        let (a, b) = sql_patterns();
        let names = BTreeSet::new();
        let mut c = Check {
            path: "fixture.rs",
            models: &names,
            sql: a,
            metadata: b,
            names: Regex::new("^DatasetRequest$").unwrap(),
            findings: vec![],
        };
        c.visit_file(&tree);
        assert_eq!(c.findings.len(), 1);
    }
    fn inspect(source: &str, owner: &str) -> Vec<String> {
        let mut models = Models::default();
        models.visit_file(&syn::parse_file(owner).unwrap());
        let (sql, metadata) = sql_patterns();
        let mut check = Check { path: "fixture.rs", models: &models.0, sql, metadata,
            names: Regex::new(r"^(?:Dataset\w*(?:Schema|Request|Payload)|Segment\w*(?:Schema|Request|Payload)|Document\w*(?:Schema|Request|Payload)|Retrieve\w*(?:Schema|Request|Payload)|KBSearchRequest|KBQueryRequest|Confluence\w*(?:Schema|CreateSchema|UpdateSchema))$").unwrap(), findings: vec![] };
        check.visit_file(&syn::parse_file(source).unwrap());
        check.findings
    }
    #[test]
    fn source_metadata_ddl_and_verbs_are_caught() {
        for value in [
            "SELECT to_regclass('knowledge.datasets')",
            "SELECT 1 FROM information_schema.columns WHERE table_name = 'segments'",
            "ALTER TABLE document_summaries ADD COLUMN stale BOOLEAN",
            "CREATE INDEX idx_segment_images ON segment_images(segment_id)",
            "SELECT * FROM \"knowledge\".\"datasets\"",
            "UPDATE documents SET status = 'x'",
            "DELETE FROM segments WHERE document_id = $1",
            "INSERT INTO confluence_pages (id) VALUES ($1)",
            "SELECT 1 FROM dataset_permissions",
            "JOIN image_segments USING (segment_id)",
            "INSERT INTO segment_images (segment_id) VALUES ($1)",
            "DELETE FROM document_summaries WHERE document_id = $1",
        ] {
            assert_eq!(
                inspect(&format!("fn probe() {{ let sql = {value:?}; }}"), "").len(),
                1,
                "{value}"
            );
        }
    }
    #[test]
    fn schema_mirror_and_heuristic_preserve_source_boundary() {
        let model = "#[derive(Deserialize)] struct RagasJudgeSelector { model: String }";
        assert!(inspect(model, model)[0].contains("mirrors a knowledge-service API"));
        assert!(
            inspect(
                "#[derive(Deserialize)] struct DatasetCreateSchema {name: String}",
                ""
            )[0]
            .contains("DatasetCreateSchema")
        );
        assert!(inspect("#[derive(Deserialize)] struct PresignedUploadRequest {filename:String} #[derive(Deserialize)] struct ConfluenceReadRequest {page_id:String}", "").is_empty());
    }
}
