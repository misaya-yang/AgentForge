use crate::{Result, cli::Args};
use serde_json::{Value, json};
pub fn rows(version: &str) -> Vec<Value> {
    let families: Vec<Value> =
        serde_json::from_str(include_str!("seed_families.json")).expect("frozen seed family data");
    let mut rows = vec![];
    for spec in families {
        let family = spec["family"].as_str().unwrap();
        let base = json!({"owner":"kb-eval","version":version,"provenance":"seed-machine-generated, human-review-pending","family":family,"corpus_language":"en"});
        for language in ["en", "zh"] {
            let mut metadata = base.clone();
            metadata["language"] = json!(language);
            metadata["cross_lingual"] = json!(language == "zh");
            rows.push(json!({"case_id":format!("kb.seed.{family}.retrieval.{language}"),"track":"retrieval_only","query":spec[format!("query_{language}")],"relevance":spec["relevance"],"metadata":metadata}));
        }
        let mut metadata = base;
        metadata["language"] = json!("zh");
        metadata["answer_language"] = json!("en");
        metadata["cross_lingual"] = json!(true);
        rows.push(json!({"case_id":format!("kb.seed.{family}.answer.xl"),"track":"answer_aware","query":spec["query_zh"],"relevance":spec["answer_relevance"],"reference_answer":spec["answer_en"],"metadata":metadata}));
    }
    rows
}
pub fn render(rows: &[Value]) -> Vec<u8> {
    format!(
        "{}\n",
        rows.iter()
            .map(crate::canonical::canonical)
            .collect::<Vec<_>>()
            .join("\n")
    )
    .into_bytes()
}
pub fn import_payload(rows: &[Value]) -> Value {
    json!({"examples":rows.iter().map(|row|{let mut metadata=row["metadata"].clone();metadata["track"]=row["track"].clone();metadata["relevance"]=row["relevance"].clone();metadata["golden_schema"]=json!("kb-golden-qa/v1");json!({"case_id":row["case_id"],"split":"regression","input":{"query":row["query"]},"expected_output":if row["reference_answer"].is_string(){json!({"reference":row["reference_answer"]})}else{json!({})},"expected_trajectory":{},"assertions":[],"metadata":metadata})}).collect::<Vec<_>>(),"mode":"skip_duplicates"})
}
pub fn run(args: Args) -> Result<Value> {
    args.reject_unknown(&["output", "emit", "version", "check", "force"])?;
    if !args.positional.is_empty() {
        return Err("unexpected positional arguments".into());
    }
    let rows = rows(args.get("version", "2026-08-28.1"));
    let validation = crate::rag::validate_cases(&rows);
    if validation["valid"] != true {
        return Err(format!(
            "seed rows fail validate_rag_cases: {}",
            validation["errors"]
        )
        .into());
    }
    match args.get("emit", "golden") {
        "eval-import" => return Ok(import_payload(&rows)),
        "golden" => (),
        _ => return Err("--emit must be golden or eval-import".into()),
    }
    let path = std::path::Path::new(args.get(
        "output",
        "tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl",
    ));
    let bytes = render(&rows);
    if args.options.contains_key("check") {
        if !path.is_file() {
            return Err(format!("seed check failed: {} is missing", path.display()).into());
        }
        if std::fs::read(path)? != bytes {
            return Err(format!(
                "seed check failed: {} drifted from the seed source",
                path.display()
            )
            .into());
        }
        return Ok(json!({"check":"ok","output":path,"case_count":rows.len()}));
    }
    if path.exists() && !args.options.contains_key("force") {
        return Err(format!("refusing to overwrite {} without --force", path.display()).into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?
    }
    std::fs::write(path, bytes)?;
    Ok(json!({"written":path,"case_count":rows.len()}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seed_bytes_match_frozen_fixture() {
        assert_eq!(
            render(&rows("2026-08-28.1")),
            include_bytes!("../../../../../tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl")
        );
    }
}
