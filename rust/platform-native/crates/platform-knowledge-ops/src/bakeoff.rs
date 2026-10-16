//! Fixed-candidate reranker evaluation. Report-only; never changes serving defaults.
use super::{Args, Result, error, fail};
use ai_platform_knowledge_service::{
    migration_knowledge_query::rerank::{RerankProviderConfig, Reranker},
    native_cross_encoder::NativeCrossEncoder,
    native_rerank::{NativeBgeReranker, NativeLocalReranker},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
#[derive(Clone)]
pub struct Case {
    pub id: String,
    pub query: String,
    pub language: String,
    pub cross: bool,
    pub candidates: Vec<(String, String)>,
    pub relevance: BTreeMap<String, i64>,
}
pub fn load(path: &Path) -> Result<Vec<Case>> {
    let (rows, _) = ai_platform_eval::snapshot(path)?;
    parse(&rows)
}
pub fn parse(rows: &[Value]) -> Result<Vec<Case>> {
    let mut seen = BTreeSet::new();
    let mut cases = vec![];
    for row in rows {
        let id = row["case_id"].as_str().unwrap_or("").trim();
        if id.is_empty() || !seen.insert(id) {
            return fail("case_id missing or duplicated");
        }
        let candidates = row["candidates"]
            .as_array()
            .ok_or_else(|| error("case candidates missing"))?
            .iter()
            .map(|c| {
                Ok((
                    c["id"]
                        .as_str()
                        .ok_or_else(|| error("candidate id invalid"))?
                        .into(),
                    c["text"]
                        .as_str()
                        .ok_or_else(|| error("candidate text invalid"))?
                        .into(),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        if candidates.is_empty() {
            return fail("case candidates must be non-empty");
        }
        let raw = row["relevance"]
            .as_object()
            .filter(|r| !r.is_empty())
            .ok_or_else(|| error("case relevance must be non-empty"))?;
        let mut relevance = BTreeMap::new();
        for (id, grade) in raw {
            let grade = grade
                .as_i64()
                .or_else(|| grade.as_f64().map(|n| n as i64))
                .or_else(|| grade.as_str().and_then(|s| s.parse().ok()))
                .ok_or_else(|| error("relevance grade invalid"))?;
            if grade > 0 {
                relevance.insert(id.clone(), grade);
            }
        }
        cases.push(Case {
            id: id.into(),
            query: row["query"].as_str().unwrap_or("").trim().into(),
            language: row["language"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("en")
                .into(),
            cross: row["cross_lingual"].as_bool().unwrap_or(false),
            candidates,
            relevance,
        });
    }
    if cases.is_empty() {
        return fail("no bakeoff cases");
    }
    Ok(cases)
}
pub fn dcg(grades: &[i64], k: usize) -> f64 {
    grades
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, g)| (2f64.powf(*g as f64) - 1.) / ((i + 2) as f64).log2())
        .sum()
}
fn metrics(cases: &[&Case], scores: &BTreeMap<String, Vec<f64>>, k: usize) -> Value {
    let (mut ndcg, mut recall, mut mrr) = (0., 0., 0.);
    for case in cases {
        let scores = &scores[&case.id];
        let mut order: Vec<_> = (0..scores.len()).collect();
        order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then(a.cmp(b)));
        let grades: Vec<_> = order
            .iter()
            .map(|i| *case.relevance.get(&case.candidates[*i].0).unwrap_or(&0))
            .collect();
        let mut ideal = grades.clone();
        ideal.sort_by(|a, b| b.cmp(a));
        let idcg = dcg(&ideal, k);
        if idcg > 0. {
            ndcg += dcg(&grades, k) / idcg;
        }
        if !case.relevance.is_empty() {
            recall += grades.iter().take(k).filter(|g| **g > 0).count() as f64
                / case.relevance.len() as f64;
        }
        if let Some(i) = grades.iter().position(|g| *g > 0) {
            mrr += 1. / (i + 1) as f64;
        }
    }
    let n = cases.len().max(1) as f64;
    let round = |v: f64| (v / n * 1e6).round() / 1e6;
    json!({"cases":cases.len(),"ndcg_at_k":round(ndcg),"recall_at_k":round(recall),"mrr":round(mrr)})
}
pub fn gate(results: &[Value]) -> Value {
    let mut decision = json!({"promotable":false,"winner":null,"reason":""});
    let Some(base) = results.iter().find(|r| r["adapter"] == "identity") else {
        decision["reason"] = json!("identity baseline missing from results");
        return decision;
    };
    if base["slices"].get("language:zh").is_none() {
        decision["reason"] = json!("no language:zh cases in the bake-off set");
        return decision;
    }
    let mut eligible: Vec<_> = results
        .iter()
        .filter(|r| r["eligible"] == true && r["adapter"] != "identity")
        .collect();
    eligible.sort_by(|a, b| {
        b["slices"]["overall"]["ndcg_at_k"]
            .as_f64()
            .unwrap_or(0.)
            .total_cmp(&a["slices"]["overall"]["ndcg_at_k"].as_f64().unwrap_or(0.))
    });
    let Some(winner) = eligible.first() else {
        decision["reason"] = json!("no eligible challenger (all errored or none ran)");
        return decision;
    };
    let zh = winner["slices"]["language:zh"]["ndcg_at_k"]
        .as_f64()
        .unwrap_or(0.)
        > base["slices"]["language:zh"]["ndcg_at_k"]
            .as_f64()
            .unwrap_or(0.)
            + 1e-9;
    let en = base["slices"].get("language:en").is_none()
        || winner["slices"]["language:en"]["ndcg_at_k"]
            .as_f64()
            .unwrap_or(0.)
            >= base["slices"]["language:en"]["ndcg_at_k"]
                .as_f64()
                .unwrap_or(0.)
                - 1e-9;
    decision["winner"] = winner["adapter"].clone();
    decision["zh_net_win"] = json!(zh);
    decision["en_no_regression"] = json!(en);
    decision["promotable"] = json!(zh && en);
    if !(zh && en) {
        decision["reason"] = json!(format!(
            "challenger {} failed {}",
            winner["adapter"].as_str().unwrap_or(""),
            if !zh {
                "zh net win"
            } else {
                "en no-regression"
            }
        ));
    }
    decision
}
fn native(env: &BTreeMap<String, String>) -> Result<NativeLocalReranker> {
    let cwd = std::env::current_dir()?;
    let path = |key: &str, model: &str, revision: &str| -> PathBuf {
        let path = env
            .get(key)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/models").join(model).join(revision));
        if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        }
    };
    Ok(NativeLocalReranker {
        m3: NativeBgeReranker::new(path(
            "KNOWLEDGE_NATIVE__BGE_MODEL_DIRECTORY",
            "bge-reranker-v2-m3",
            ai_platform_knowledge_service::native_rerank::MODEL_REVISION,
        ))?,
        large: NativeBgeReranker::large(path(
            "KNOWLEDGE_NATIVE__BGE_LARGE_MODEL_DIRECTORY",
            "bge-reranker-large",
            ai_platform_knowledge_service::native_rerank::LARGE_MODEL_REVISION,
        ))?,
        cross_encoder: NativeCrossEncoder::new(path(
            "KNOWLEDGE_NATIVE__CROSS_ENCODER_MODEL_DIRECTORY",
            "ms-marco-MiniLM-L-12-v2",
            ai_platform_knowledge_service::native_cross_encoder::MODEL_REVISION,
        ))?,
    })
}
pub async fn run(args: &[String]) -> Result<()> {
    let args = Args::parse(args, &["allow-live"])?;
    args.validate(&["cases", "out", "top-k", "adapters"], 0)?;
    let path = args
        .options
        .get("cases")
        .ok_or_else(|| error("--cases required"))?;
    let out = args
        .options
        .get("out")
        .ok_or_else(|| error("--out required"))?;
    let cases = load(Path::new(path))?;
    let k = args.get("top-k", "10").parse::<usize>()?;
    let specs: Vec<_> = args
        .get("adapters", "identity,bge")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !specs
        .iter()
        .any(|s| s.split(':').next() == Some("identity"))
    {
        return fail("identity baseline must be part of --adapters");
    }
    let env = super::environment();
    for spec in &specs {
        let provider = spec.split(':').next().unwrap().to_lowercase();
        let key = match provider.as_str() {
            "dashscope" => Some("DASHSCOPE_API_KEY"),
            "cohere" => Some("COHERE_API_KEY"),
            "jina" => Some("JINA_API_KEY"),
            "identity" | "bge" => None,
            _ => return fail("unknown adapter provider"),
        };
        if let Some(key) = key {
            if !args.has("allow-live") {
                return fail("live adapters require --allow-live (paid API calls)");
            }
            if env.get(key).is_none_or(|v| v.trim().is_empty()) {
                return fail(&format!("{key} is not set"));
            }
        }
    }
    let mut providers = BTreeMap::new();
    for (provider, key, endpoint) in [
        ("dashscope", "DASHSCOPE_API_KEY", ""),
        (
            "cohere",
            "COHERE_API_KEY",
            "https://api.cohere.com/v2/rerank",
        ),
    ] {
        providers.insert(
            provider.into(),
            RerankProviderConfig {
                api_key: env.get(key).cloned().unwrap_or_default(),
                endpoint: if provider == "dashscope" {
                    env.get("DASHSCOPE_RERANK_BASE_URL")
                        .cloned()
                        .unwrap_or_default()
                } else {
                    endpoint.into()
                },
                request_schema: if provider == "dashscope" {
                    env.get("DASHSCOPE_RERANK_REQUEST_SCHEMA")
                        .cloned()
                        .unwrap_or_else(|| "auto".into())
                } else {
                    "flat".into()
                },
                instruct: if provider == "dashscope" {
                    env.get("DASHSCOPE_RERANK_INSTRUCT").cloned()
                } else {
                    None
                },
            },
        );
    }
    let client = reqwest::Client::new();
    let mut reranker =
        Reranker::new(client.clone(), providers).with_local_backend(Arc::new(native(&env)?));
    reranker.dashscope_base_url = env.get("DASHSCOPE_BASE_URL").cloned();
    let mut results = vec![];
    for spec in specs {
        let (provider, model) = spec.split_once(':').unwrap_or((spec, ""));
        let provider = provider.to_lowercase();
        let name = if provider == "identity" {
            "identity".into()
        } else if provider == "jina" {
            format!(
                "jina:{}",
                if model.is_empty() {
                    "jina-reranker-v3"
                } else {
                    model
                }
            )
        } else {
            format!(
                "{provider}:{}",
                if model.is_empty() { "default" } else { model }
            )
        };
        let mut scores = BTreeMap::new();
        let mut errors = vec![];
        for case in &cases {
            let output:Result<Vec<f64>>=async{if provider=="identity"{return Ok((0..case.candidates.len()).map(|i|(case.candidates.len()-i)as f64).collect());}let docs:Vec<_>=case.candidates.iter().map(|c|c.1.clone()).collect();let mut values=vec![f64::NEG_INFINITY;docs.len()];if provider=="jina"{let response:Value=client.post("https://api.jina.ai/v1/rerank").bearer_auth(&env["JINA_API_KEY"]).timeout(std::time::Duration::from_secs(60)).json(&json!({"model":if model.is_empty(){"jina-reranker-v3"}else{model},"query":case.query,"documents":docs,"top_n":docs.len()})).send().await?.error_for_status()?.json().await?;if let Some(hits)=response["results"].as_array(){for hit in hits{if let Some(i)=hit["index"].as_u64().map(|i|i as usize).filter(|i|*i<values.len()){values[i]=hit["relevance_score"].as_f64().unwrap_or(0.);}}}}else{for hit in reranker.rerank(&provider,model,&case.query,&docs,docs.len()).await?{if hit.index<values.len(){values[hit.index]=hit.relevance_score;}}}Ok(values)}.await;
            match output{Ok(values)=>{scores.insert(case.id.clone(),values);},Err(_)=>errors.push(json!({"case_id":case.id,"error":"adapter inference or provider request failed"}))}
        }
        let scored: Vec<_> = cases
            .iter()
            .filter(|c| scores.contains_key(&c.id))
            .collect();
        let mut slices = json!({"overall":metrics(&scored,&scores,k)});
        for language in scored.iter().map(|c| &c.language).collect::<BTreeSet<_>>() {
            slices[format!("language:{language}")] = metrics(
                &scored
                    .iter()
                    .copied()
                    .filter(|c| &c.language == language)
                    .collect::<Vec<_>>(),
                &scores,
                k,
            );
        }
        let cross: Vec<_> = scored.iter().copied().filter(|c| c.cross).collect();
        if !cross.is_empty() {
            slices["cross_lingual"] = metrics(&cross, &scores, k);
        }
        results.push(
            json!({"adapter":name,"eligible":errors.is_empty(),"errors":errors,"slices":slices}),
        );
    }
    let report = json!({"schema":"rerank-bakeoff-v1","generated_at":chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),"params":{"k":k,"total_cases":cases.len()},"gate":gate(&results),"adapters":results});
    write_reports(Path::new(out), &report, "Reranker bake-off report")?;
    println!(
        "winner={} promotable={}",
        report["gate"]["winner"], report["gate"]["promotable"]
    );
    Ok(())
}
pub fn write_reports(out: &Path, report: &Value, title: &str) -> Result<()> {
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        out.with_extension("json"),
        serde_json::to_string_pretty(report)?,
    )?;
    let mut text = format!(
        "# {title}\n\nGenerated: {}\n\n",
        report["generated_at"].as_str().unwrap_or("")
    );
    if let Some(gate) = report.get("gate") {
        text.push_str(&format!(
            "## Gate\n\n```json\n{}\n```\n\n",
            serde_json::to_string_pretty(gate)?
        ));
    }
    text.push_str("| adapter / leg | eligible | slice | cases | nDCG | recall | MRR |\n| --- | --- | --- | --- | --- | --- | --- |\n");
    for entry in report
        .get("adapters")
        .or_else(|| report.get("legs"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = entry
            .get("adapter")
            .or_else(|| entry.get("leg"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Some(slices) = entry["slices"].as_object() {
            for (slice, m) in slices {
                text.push_str(&format!(
                    "| {name} | {} | {slice} | {} | {:.4} | {:.4} | {:.4} |\n",
                    entry["eligible"],
                    m["cases"],
                    m["ndcg_at_k"].as_f64().unwrap_or(0.),
                    m["recall_at_k"].as_f64().unwrap_or(0.),
                    m["mrr"].as_f64().unwrap_or(0.)
                ));
            }
        }
        if let Some(reason) = entry["skip_reason"].as_str() {
            text.push_str(&format!("\n- {name}: {reason}\n"));
        }
        if let Some(errors) = entry["errors"].as_array() {
            for e in errors {
                text.push_str(&format!("\n- {name}, {}: {}\n", e["case_id"], e["error"]));
            }
        }
    }
    text.push_str("\nNo serving defaults are changed by this report.\n");
    std::fs::write(out.with_extension("md"), text)?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_corpus_identity_is_imperfect() {
        let cases = parse(&super::super::build_rerank_rows().unwrap()).unwrap();
        let scores = cases
            .iter()
            .map(|c| {
                (
                    c.id.clone(),
                    (0..c.candidates.len())
                        .map(|i| (c.candidates.len() - i) as f64)
                        .collect(),
                )
            })
            .collect();
        let m = metrics(&cases.iter().collect::<Vec<_>>(), &scores, 10);
        assert!(m["ndcg_at_k"].as_f64().unwrap() < 1.);
        assert_eq!(m["recall_at_k"], 1.);
    }
}

#[cfg(test)]
#[path = "eval_migration_parity_bakeoff.rs"]
mod eval_migration_parity_bakeoff;
