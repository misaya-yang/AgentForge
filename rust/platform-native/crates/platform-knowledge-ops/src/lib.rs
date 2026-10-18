//! Native operator commands. Import/backfill writes remain explicitly selected;
//! retired sparse migration is permanently read-only. Never invokes Python.
pub mod backfill;
pub mod bakeoff;
pub mod golden;
pub mod lexical_shadow;
pub mod model_session;
pub mod qdrant_upgrade;
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn error(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, message.to_owned()).into()
}
pub fn fail<T>(message: &str) -> Result<T> {
    Err(error(message))
}
#[derive(Default)]
pub struct Args {
    pub options: BTreeMap<String, String>,
    pub flags: Vec<String>,
    pub positional: Vec<String>,
}
impl Args {
    pub fn parse(args: &[String], flags: &[&str]) -> Result<Self> {
        let mut out = Self::default();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a == "--" {
                out.positional.extend_from_slice(&args[i + 1..]);
                break;
            }
            if a.starts_with("--") {
                let key = a.trim_start_matches("--");
                if let Some((key, value)) = key.split_once('=') {
                    if flags.contains(&key) {
                        return fail("flag does not accept a value");
                    }
                    out.options.insert(key.into(), value.into());
                } else if flags.contains(&key) {
                    out.flags.push(key.into());
                } else {
                    let value = args
                        .get(i + 1)
                        .filter(|v| !v.starts_with("--"))
                        .ok_or_else(|| error("option requires a value"))?;
                    out.options.insert(key.into(), value.clone());
                    i += 1;
                }
            } else {
                out.positional.push(a.clone());
            }
            i += 1;
        }
        Ok(out)
    }
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.options.get(key).map(String::as_str).unwrap_or(default)
    }
    pub fn has(&self, key: &str) -> bool {
        self.flags.iter().any(|s| s == key)
    }
    pub fn validate(&self, options: &[&str], max_positionals: usize) -> Result<()> {
        if self
            .options
            .keys()
            .any(|key| !options.contains(&key.as_str()))
        {
            return fail("unrecognized command option");
        }
        if self.positional.len() > max_positionals {
            return fail("unexpected positional argument");
        }
        Ok(())
    }
}
pub fn environment() -> BTreeMap<String, String> {
    let path = std::env::var("ENV_FILE").unwrap_or_else(|_| ".env".into());
    let mut values: BTreeMap<String, String> = dotenvy::from_path_iter(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .collect();
    values.extend(std::env::vars());
    values
}
pub async fn postgres(values: &BTreeMap<String, String>) -> Result<sqlx::PgPool> {
    let required = |key: &str| {
        values
            .get(key)
            .filter(|s| !s.is_empty())
            .cloned()
            .ok_or_else(|| error(&format!("PostgreSQL configuration missing {key}")))
    };
    let options = sqlx::postgres::PgConnectOptions::new()
        .host(
            values
                .get("POSTGRES_HOST")
                .map(String::as_str)
                .unwrap_or("127.0.0.1"),
        )
        .port(required("POSTGRES_PORT")?.parse()?)
        .username(&required("POSTGRES_USER")?)
        .password(&required("POSTGRES_PASSWORD")?)
        .database(&required("POSTGRES_DB")?);
    Ok(sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .map_err(|_| error("PostgreSQL connection failed"))?)
}
pub fn build_rerank_rows() -> Result<Vec<Value>> {
    let data: Value = serde_json::from_str(include_str!("rerank_corpus.json"))?;
    let mut rows = vec![];
    for spec in data["_CASE_SPECS"]
        .as_array()
        .ok_or_else(|| error("case specifications invalid"))?
    {
        let family = spec["family"].as_str().unwrap();
        let language = spec["language"].as_str().unwrap();
        let ids = spec["candidates"].as_array().unwrap();
        let mut candidates = vec![];
        for id in ids {
            let key = id.as_str().unwrap();
            let text = data["CORPUS"][key]
                .as_str()
                .ok_or_else(|| error("candidate absent from fixed corpus"))?;
            candidates.push(json!({"id":id,"text":text}));
        }
        rows.push(json!({"case_id":format!("kb.bakeoff.{family}.{language}"),"query":spec["query"],"language":language,"cross_lingual":spec["cross_lingual"],"relevance":spec["relevance"],"candidates":candidates,"metadata":{"version":"rerank-bakeoff-v1","family":family,"owner":"kb-eval","provenance":"bake-off corpus, human-review-pending"}}));
    }
    Ok(rows)
}
fn write_jsonl(path: &Path, rows: &[Value]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = String::new();
    for row in rows {
        text.push_str(&ai_platform_eval::canonical::canonical(row));
        text.push('\n');
    }
    std::fs::write(path, text)?;
    Ok(())
}
pub async fn run(args: &[String]) -> Result<()> {
    let Some(command) = args.first() else {
        return fail(
            "usage: platform-knowledge-ops <import-kb-eval-golden|build-rerank-bakeoff-cases|migrate-sparse-vectors> [options]",
        );
    };
    match command.as_str() {
        "--help" | "help" => {
            println!(
                "platform-knowledge-ops commands: import-kb-eval-golden, build-rerank-bakeoff-cases, reranker-bakeoff, chinese-lexical-shadow-eval, migrate-sparse-vectors, backfill-bm25-v2, serving-profile-plan, qdrant-upgrade"
            );
            Ok(())
        }
        "serving-profile-plan" => {
            let args = Args::parse(&args[1..], &[])?;
            args.validate(&["dataset"], 0)?;
            let path = args.get("dataset", "");
            if path.is_empty() {
                return fail("--dataset is required");
            }
            let metadata = std::fs::metadata(path)?;
            if metadata.len() > 1024 * 1024 {
                return fail("dataset configuration exceeds byte budget");
            }
            let dataset: ai_platform_knowledge_service::migration_knowledge_query::types::Dataset =
                serde_json::from_slice(&std::fs::read(path)?)?;
            let profile=ai_platform_knowledge_service::migration_knowledge_query::serving_profile::Profile::from_dataset(&dataset)?.ok_or_else(||error("serving_v2 profile is required"))?;
            let mut collection = json!({"vectors":{"size":dataset.embedding_dimension,"distance":"Cosine"},"sparse_vectors":{"bm25":{"modifier":"idf"}},"hnsw_config":{"m":16,"ef_construct":200},"metadata":{"knowledge_serving":{"schema":"knowledge-serving/v1","index_fingerprint":ai_platform_knowledge_service::migration_knowledge_query::lexical_v2::fingerprint(&serde_json::to_value(&profile.index)?)} }});
            profile.collection_config(&mut collection);
            if let Some(scope) = profile.shared_scope(&dataset) {
                collection["metadata"]["knowledge_scope"] = scope;
                collection["sharding_method"] = json!("custom");
            } else {
                collection["metadata"]["knowledge_scope"] = json!({"schema_version":1,"dataset_id":dataset.dataset_id,"tenant_id":dataset.tenant_id});
            }
            println!(
                "{}",
                json!({"schema":"knowledge-serving-plan/v1","activation":"requires_real_promotion","fingerprint":profile.fingerprint(&dataset),"collection":collection,"shard_selector":profile.shard_key(),"payload_indexes":{"tenant_id":{"type":"keyword","is_tenant":true},"dataset_id":"keyword","document_id":"keyword","segment_id":"keyword","enabled":"bool","content_type":"keyword","level":"integer","source_version":"integer","_rollback_backup":"bool","metadata.source_path":{"type":"keyword","prefix":{}}}})
            );
            Ok(())
        }
        "qdrant-upgrade" => qdrant_upgrade::run(&args[1..]).await,
        "reranker-bakeoff" => bakeoff::run(&args[1..]).await,
        "backfill-bm25-v2" => backfill::run(&args[1..]).await,
        "chinese-lexical-shadow-eval" => lexical_shadow::run(&args[1..]),
        "build-rerank-bakeoff-cases" => {
            let args = Args::parse(&args[1..], &["stdout"])?;
            args.validate(&["out"], 0)?;
            let rows = build_rerank_rows()?;
            if args.has("stdout") {
                for row in rows {
                    println!("{}", ai_platform_eval::canonical::canonical(&row));
                }
            } else {
                let path = Path::new(args.get(
                    "out",
                    "tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl",
                ));
                write_jsonl(path, &rows)?;
                bakeoff::load(path)?;
                println!(
                    "wrote {} cases (6 zh / 6 en) -> {}",
                    rows.len(),
                    path.display()
                );
            }
            Ok(())
        }
        "migrate-sparse-vectors" => {
            let args = Args::parse(&args[1..], &["dry-run"])?;
            args.validate(&["collection", "batch-size", "qdrant-url"], 0)?;
            if !args.has("dry-run") {
                return fail(
                    "legacy sparse-vector migration is permanently retired; use platform-knowledge-ops backfill-bm25-v2",
                );
            }
            let collection = args.get("collection", "kb_default_1024").trim();
            if collection.is_empty() || args.get("batch-size", "100").parse::<usize>()? == 0 {
                return fail("collection and positive batch-size are required");
            }
            let env = environment();
            let mut url = reqwest::Url::parse(
                args.get(
                    "qdrant-url",
                    env.get("QDRANT_URL")
                        .map(String::as_str)
                        .unwrap_or("http://localhost:6333"),
                ),
            )?;
            url.path_segments_mut()
                .map_err(|_| error("invalid Qdrant URL"))?
                .pop_if_empty()
                .push("collections")
                .push(collection);
            let response: Value = reqwest::Client::new()
                .get(url)
                .timeout(std::time::Duration::from_secs(120))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            let info = &response["result"];
            let vectors = &info["config"]["params"]["vectors"];
            let dense = if vectors.get("size").is_some() {
                vectors
            } else {
                vectors
                    .get("")
                    .or_else(|| vectors.get("default"))
                    .or_else(|| {
                        vectors
                            .as_object()
                            .filter(|m| m.len() == 1)
                            .and_then(|m| m.values().next())
                    })
                    .unwrap_or(&Value::Null)
            };
            println!(
                "{}",
                json!({"collection":collection,"points_count":info["points_count"],"dense_vector_size":dense["size"],"distance":dense["distance"],"status":"retired","replacement":"platform-knowledge-ops backfill-bm25-v2"})
            );
            Ok(())
        }
        "import-kb-eval-golden" => {
            let args = Args::parse(&args[1..], &["dry-run", "pin-release"])?;
            args.validate(&["version", "split", "release-key", "note"], 1)?;
            let path = args
                .positional
                .first()
                .map(String::as_str)
                .unwrap_or("tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl");
            let (cases, _) = ai_platform_eval::snapshot(path)?;
            let report = ai_platform_eval::rag::validate_cases(&cases);
            if report["valid"] != true {
                return fail(&format!("invalid golden cases: {}", report["errors"]));
            }
            let version =
                golden::resolve_version(&cases, args.options.get("version").map(String::as_str))?;
            let split = args.get("split", "growth");
            let rows: Vec<_> = cases
                .iter()
                .map(|c| golden::case_to_row(c, &version, split, None))
                .collect::<Result<_>>()?;
            let mut plan = json!({"file":path,"version":version,"cases":cases.len(),"frozen":rows.iter().filter(|r|r["split"]=="frozen").count(),"growth":rows.iter().filter(|r|r["split"]=="growth").count()});
            if args.has("dry-run") {
                plan["mode"] = json!("dry-run");
            } else {
                let pool = postgres(&environment()).await?;
                let store = golden::GoldenStore { pool: pool.clone() };
                let counts = store.import_cases(&cases, &version, split, None).await?;
                for (k, v) in counts.as_object().unwrap() {
                    plan[k] = v.clone();
                }
                if args.has("pin-release") {
                    store
                        .pin_release(
                            &version,
                            args.get("release-key", "current"),
                            args.get("note", ""),
                        )
                        .await?;
                }
                pool.close().await;
                plan["mode"] = json!("imported");
                plan["pinned"] = json!(args.has("pin-release"));
            }
            println!("{plan}");
            Ok(())
        }
        _ => fail("unknown knowledge operator command"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generated_jsonl_contains_objects_not_quoted_json() {
        let rows = build_rerank_rows().unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "native-kb-cases-{}-{stamp}.jsonl",
            std::process::id()
        ));
        write_jsonl(&path, &rows).unwrap();
        let parsed = bakeoff::load(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(parsed.len(), 12);
    }
    #[test]
    fn corpus_has_fixed_bilingual_cases() {
        let rows = build_rerank_rows().unwrap();
        assert_eq!(rows.len(), 12);
        assert_eq!(rows.iter().filter(|r| r["language"] == "zh").count(), 6);
        assert_eq!(rows[0]["case_id"], "kb.bakeoff.refund.zh");
        assert_eq!(rows[0]["candidates"][0]["id"], "refund-near-monthly");
    }
    #[tokio::test]
    async fn retired_migration_refuses_before_network() {
        assert!(
            run(&[
                "migrate-sparse-vectors".into(),
                "--qdrant-url".into(),
                "invalid".into()
            ])
            .await
            .unwrap_err()
            .to_string()
            .contains("permanently retired")
        );
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;
    #[test]
    fn argument_validation_precedes_operator_work() {
        let args = Args::parse(
            &["--out=report.json".into(), "--allow-live".into()],
            &["allow-live"],
        )
        .unwrap();
        assert_eq!(args.get("out", ""), "report.json");
        assert!(args.has("allow-live"));
        assert!(args.validate(&["out"], 0).is_ok());
        assert!(args.validate(&[], 0).is_err());
        assert!(Args::parse(&["--allow-live=false".into()], &["allow-live"]).is_err());
        let args = Args::parse(&["--".into(), "--filename".into()], &[]).unwrap();
        assert_eq!(args.positional, vec!["--filename"]);
        assert!(args.validate(&[], 0).is_err());
    }
}

pub mod promotion;
