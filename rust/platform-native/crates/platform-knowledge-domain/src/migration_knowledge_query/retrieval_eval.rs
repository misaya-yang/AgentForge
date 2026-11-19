//! Application retrieval-evaluation API and canonical duplicate-aware IR metrics.
//! These functions consume caller-labelled cases; they contain no fixtures or oracle data.
use super::{
    config::{RetrieveRequest, validate_bounds},
    service::RetrievalService,
    types::{QueryError, Result, UserContext},
};
use crate::observation_redaction::redact_preview;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn invalid(message: &str) -> QueryError {
    QueryError::Validation(message.into())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryRetrievalJudgement {
    pub query_id: String,
    pub retrieved: Vec<String>,
    pub relevance: BTreeMap<String, f64>,
}
fn ranked_grades(retrieved: &[String], grades: &BTreeMap<String, f64>) -> Vec<f64> {
    let mut seen = BTreeSet::new();
    retrieved
        .iter()
        .map(|id| {
            if seen.insert(id) {
                grades.get(id).copied().unwrap_or(0.0)
            } else {
                0.0
            }
        })
        .collect()
}
fn dcg(grades: &[f64], k: usize) -> f64 {
    grades
        .iter()
        .take(k)
        .enumerate()
        .filter(|(_, g)| **g > 0.0)
        .map(|(i, g)| g / ((i + 2) as f64).log2())
        .sum()
}
fn metrics(retrieved: &[String], relevance: &BTreeMap<String, f64>, k: Option<usize>) -> Value {
    let all = ranked_grades(retrieved, relevance);
    let cutoff = k.unwrap_or(all.len());
    let top = &all[..all.len().min(cutoff)];
    let relevant = relevance.values().filter(|g| **g > 0.0).count();
    let mut hits = 0usize;
    let mut rr = 0.0;
    let mut precisions = 0.0;
    for (i, grade) in top.iter().enumerate() {
        if *grade > 0.0 {
            hits += 1;
            precisions += hits as f64 / (i + 1) as f64;
            if rr == 0.0 {
                rr = 1.0 / (i + 1) as f64;
            }
        }
    }
    let denominator = k.map_or(relevant, |k| relevant.min(k));
    let mut ideal: Vec<_> = relevance.values().copied().collect();
    ideal.sort_by(|a, b| b.total_cmp(a));
    let ideal_dcg = dcg(&ideal, cutoff);
    json!({"hit_rate":if hits>0{1.0}else{0.0},"precision_at_k":if cutoff>0{hits as f64/cutoff as f64}else{0.0},"recall_at_k":if relevant>0{hits as f64/relevant as f64}else{0.0},"mrr":rr,"ndcg_at_k":if ideal_dcg>0.0{(dcg(&all,cutoff)/ideal_dcg).clamp(0.0,1.0)}else{0.0},"map":if denominator>0{precisions/denominator as f64}else{0.0}})
}
pub fn score_single_query(
    retrieved: &[String],
    relevance: &BTreeMap<String, f64>,
    ks: &[usize],
) -> Result<Value> {
    if relevance.values().any(|g| !g.is_finite() || *g < 0.0) {
        return Err(invalid("relevance grades must be finite and non-negative"));
    }
    let mut by_k = Map::new();
    for k in ks {
        by_k.insert(k.to_string(), metrics(retrieved, relevance, Some(*k)));
    }
    let full = metrics(retrieved, relevance, None);
    Ok(
        json!({"reciprocal_rank":full["mrr"],"average_precision":full["map"],"retrieved_count":retrieved.len(),"unique_retrieved_count":retrieved.iter().collect::<BTreeSet<_>>().len(),"by_k":by_k}),
    )
}
pub fn evaluate_retrieval(judgements: &[QueryRetrievalJudgement], ks: &[usize]) -> Result<Value> {
    if ks.is_empty() || ks.iter().any(|k| !(1..=100).contains(k)) {
        return Err(invalid("k_values must be between 1 and 100"));
    }
    let ks: Vec<_> = ks
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let names = [
        "hit_rate",
        "precision_at_k",
        "recall_at_k",
        "mrr",
        "ndcg_at_k",
        "map",
    ];
    let mut report = json!({"k_values":ks,"metrics":{},"per_query":{}});
    for k in &ks {
        report["metrics"][k.to_string()] = json!({"k":k,"num_queries":judgements.len()});
        for name in names {
            report["metrics"][k.to_string()][name] = json!(0.0)
        }
    }
    for j in judgements {
        if report["per_query"].get(&j.query_id).is_some() {
            return Err(invalid("duplicate query_id"));
        }
        let detail = score_single_query(&j.retrieved, &j.relevance, &ks)?;
        for k in &ks {
            for name in names {
                let value = report["metrics"][k.to_string()][name]
                    .as_f64()
                    .unwrap_or(0.0)
                    + detail["by_k"][k.to_string()][name].as_f64().unwrap_or(0.0);
                report["metrics"][k.to_string()][name] = json!(value)
            }
        }
        report["per_query"][&j.query_id] = detail;
    }
    for k in &ks {
        for name in names {
            let sum = report["metrics"][k.to_string()][name]
                .as_f64()
                .unwrap_or(0.0);
            report["metrics"][k.to_string()][name] = json!(sum / judgements.len().max(1) as f64);
        }
    }
    Ok(report)
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationCase {
    pub query: String,
    pub case_id: Option<String>,
    #[serde(default)]
    pub relevant_segment_ids: Vec<String>,
    #[serde(default)]
    pub relevance: BTreeMap<String, f64>,
}
pub struct EvaluationRequest {
    pub retrieval: RetrieveRequest,
    pub cases: Vec<EvaluationCase>,
    pub k_values: Vec<usize>,
    pub return_retrieved: bool,
    pub requested_config: Value,
}
fn nested(
    data: &mut Map<String, Value>,
    name: &str,
    allowed: &[&str],
) -> Result<Option<Map<String, Value>>> {
    let Some(raw) = data.get(name).filter(|v| v.is_object()) else {
        return Ok(None);
    };
    let map = raw.as_object().ok_or_else(|| invalid("invalid preset"))?;
    if map.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(invalid("unsupported retrieval preset fields"));
    }
    Ok(data.remove(name).and_then(|v| v.as_object().cloned()))
}
fn promote(data: &mut Map<String, Value>, block: &Map<String, Value>, from: &str, to: &str) {
    if let Some(value) = block.get(from).filter(|v| !v.is_null()) {
        data.entry(to.to_owned()).or_insert_with(|| value.clone());
    }
}
impl EvaluationRequest {
    pub fn parse(value: Value) -> Result<Self> {
        let mut data = value
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("evaluation request must be an object"))?;
        if let Some(v) = nested(
            &mut data,
            "vector",
            &["enabled", "top_k", "score_threshold"],
        )? {
            promote(&mut data, &v, "top_k", "vector_top_k");
            if data.get("score_threshold").is_none_or(Value::is_null) {
                if let Some(t) = v.get("score_threshold").filter(|v| !v.is_null()) {
                    data.insert("score_threshold".into(), t.clone());
                }
            }
        }
        if let Some(v) = nested(
            &mut data,
            "keyword",
            &[
                "enabled",
                "top_k",
                "candidate_pool_size",
                "bm25_k1",
                "bm25_b",
            ],
        )? {
            promote(&mut data, &v, "top_k", "keyword_top_k");
            promote(&mut data, &v, "candidate_pool_size", "keyword_candidate_k");
        }
        if let Some(v) = nested(
            &mut data,
            "fusion",
            &["strategy", "rrf_k", "rrf_weights", "alpha"],
        )? {
            for (a, b) in [
                ("strategy", "fusion_method"),
                ("rrf_k", "rrf_k"),
                ("rrf_weights", "rrf_weights"),
                ("alpha", "alpha"),
            ] {
                promote(&mut data, &v, a, b)
            }
        }
        if let Some(v) = nested(
            &mut data,
            "rerank",
            &["enabled", "provider", "model", "top_n", "score_threshold"],
        )? {
            for (a, b) in [
                ("enabled", "rerank"),
                ("model", "rerank_model"),
                ("top_n", "rerank_top_n"),
            ] {
                promote(&mut data, &v, a, b)
            }
        }
        if let Some(v) = nested(
            &mut data,
            "mmr",
            &["enabled", "lambda", "similarity_threshold"],
        )? {
            for (a, b) in [
                ("enabled", "mmr"),
                ("lambda", "mmr_lambda"),
                ("similarity_threshold", "mmr_threshold"),
            ] {
                promote(&mut data, &v, a, b)
            }
        }
        if let Some(v) = nested(
            &mut data,
            "multimodal",
            &[
                "enabled",
                "image_search_enabled",
                "image_score_threshold",
                "text_score_threshold",
                "use_separate_thresholds",
                "image_boost",
                "vlm_rerank_enabled",
                "vlm_rerank_weight",
                "content_type_filter",
            ],
        )? {
            let enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(false);
            if enabled {
                data.entry("include_images".to_owned())
                    .or_insert(json!(true));
            }
            for (a, b) in [
                ("image_search_enabled", "image_search_enabled"),
                ("use_separate_thresholds", "use_separate_thresholds"),
                ("vlm_rerank_enabled", "multimodal_rerank"),
                ("content_type_filter", "content_type_filter"),
            ] {
                promote(&mut data, &v, a, b)
            }
            if enabled {
                for key in ["image_score_threshold", "image_boost", "vlm_rerank_weight"] {
                    promote(&mut data, &v, key, key)
                }
            }
        }
        let mut cases: Vec<EvaluationCase> = serde_json::from_value(
            data.remove("cases")
                .ok_or_else(|| invalid("cases are required"))?,
        )
        .map_err(|_| invalid("invalid retrieval evaluation cases"))?;
        let ks: Vec<usize> =
            serde_json::from_value(data.remove("k_values").unwrap_or(json!([1, 3, 5, 10])))
                .map_err(|_| invalid("invalid k_values"))?;
        let return_retrieved = data
            .remove("return_retrieved")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| invalid("return_retrieved must be boolean"))
            })
            .transpose()?
            .unwrap_or(true);
        if !(1..=20).contains(&cases.len())
            || ks.is_empty()
            || ks.len() > 8
            || ks.iter().any(|k| !(1..=100).contains(k))
        {
            return Err(invalid("retrieval evaluation bounds exceeded"));
        }
        if data
            .get("query")
            .is_some_and(|v| v.as_str().is_none_or(|q| q.chars().count() > 4096))
        {
            return Err(invalid("invalid ignored query field"));
        }
        data.insert("query".into(), json!("evaluation"));
        let mut ids = BTreeSet::new();
        for (i, case) in cases.iter_mut().enumerate() {
            case.query = case.query.trim().to_owned();
            let id = case
                .case_id
                .as_deref()
                .map(str::trim)
                .map(str::to_owned)
                .unwrap_or_else(|| format!("case_{i}"));
            if id.is_empty()
                || id.chars().count() > 128
                || !ids.insert(id.clone())
                || case.query.is_empty()
                || case.query.chars().count() > 4096
                || case.relevant_segment_ids.len() > 500
                || case.relevance.len() > 500
            {
                return Err(invalid("invalid or duplicate retrieval case identity"));
            }
            case.case_id = Some(id);
            let mut segments = BTreeSet::new();
            for id in &mut case.relevant_segment_ids {
                *id = id.trim().to_owned();
                if id.is_empty() || id.chars().count() > 256 || !segments.insert(id.clone()) {
                    return Err(invalid(
                        "relevant segment IDs must be unique 1-256 characters",
                    ));
                }
            }
            let mut grades = BTreeMap::new();
            for (id, g) in &case.relevance {
                let id = id.trim();
                if id.is_empty()
                    || id.chars().count() > 256
                    || !g.is_finite()
                    || !(0.0..=5.0).contains(g)
                {
                    return Err(invalid("invalid graded relevance"));
                }
                grades.insert(id.to_owned(), *g);
            }
            case.relevance = grades;
        }
        validate_bounds(&Value::Object(data.clone()))?;
        let request: RetrieveRequest = serde_json::from_value(Value::Object(data.clone()))
            .map_err(|_| invalid("invalid retrieval configuration"))?;
        if request.extra.keys().any(|k| {
            ![
                "include_images",
                "include_associated_images",
                "multimodal_rerank",
                "content_type_filter",
                "image_search_enabled",
                "vlm_rerank_weight",
                "image_boost",
                "image_score_threshold",
                "use_separate_thresholds",
            ]
            .contains(&k.as_str())
        }) {
            return Err(invalid("unsupported retrieval evaluation field"));
        }
        for field in [
            "dense_weight",
            "bm25_weight",
            "alpha",
            "score_threshold",
            "mmr_lambda",
            "mmr_threshold",
            "vlm_rerank_weight",
            "image_score_threshold",
        ] {
            if let Some(v) = data.get(field).filter(|v| !v.is_null()) {
                if !v
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
                {
                    return Err(invalid("evaluation weight or threshold is outside 0..1"));
                }
            }
        }
        for (field, max) in [
            ("document_id", 256),
            ("mode", 32),
            ("fusion_method", 32),
            ("fusion", 32),
            ("rerank_model", 256),
            ("content_type_filter", 64),
            ("source_type_filter", 128),
            ("language_filter", 64),
        ] {
            if data
                .get(field)
                .and_then(Value::as_str)
                .is_some_and(|s| s.chars().count() > max)
            {
                return Err(invalid("retrieval evaluation text field exceeds limit"));
            }
        }
        if request.metadata_filter.as_ref().is_some_and(|v| {
            serde_json::to_vec(v).map_or(true, |s| s.len() > 16384)
                || v.as_object().is_none_or(|m| m.len() > 64)
        }) {
            return Err(invalid(
                "metadata_filter must be an object bounded by 64 fields and 16 KiB",
            ));
        }
        let mut requested =
            serde_json::to_value(&request).map_err(|_| invalid("invalid request"))?;
        if let Some(m) = requested.as_object_mut() {
            m.remove("query");
            m.retain(|_, v| !v.is_null());
            m.entry("rrf_weights".to_owned()).or_insert(json!({}));
            for key in [
                "include_images",
                "include_associated_images",
                "multimodal_rerank",
                "image_search_enabled",
                "use_separate_thresholds",
            ] {
                m.entry(key.to_owned()).or_insert(json!(false));
            }
        }
        Ok(Self {
            retrieval: request,
            cases,
            k_values: ks
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect(),
            return_retrieved,
            requested_config: requested,
        })
    }
}
pub fn bounded_metadata(value: &Value, depth: usize, budget: &mut usize) -> Value {
    if *budget == 0 {
        return json!("[truncated]");
    }
    *budget -= 1;
    if depth >= 5 {
        return json!("[truncated]");
    }
    match value {
        Value::String(s) => json!(redact_preview(s, 500)),
        Value::Object(m) => {
            let sensitive=regex::Regex::new(r"(?i)(authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password)").expect("static secret-key pattern");
            let mut out = Map::new();
            for (i, (k, v)) in m.iter().enumerate() {
                if i >= 64 || *budget == 0 {
                    out.insert("__truncated_entries__".into(), json!(m.len() - i));
                    break;
                }
                *budget -= 1;
                let key = redact_preview(k, 128);
                let next = if sensitive.is_match(&key) {
                    json!("[redacted]")
                } else {
                    bounded_metadata(v, depth + 1, budget)
                };
                out.insert(key, next);
            }
            Value::Object(out)
        }
        Value::Array(a) => {
            let mut out = Vec::new();
            for v in a.iter().take(64) {
                if *budget == 0 {
                    break;
                }
                out.push(bounded_metadata(v, depth + 1, budget))
            }
            if a.len() > out.len() {
                out.push(json!(format!(
                    "[truncated {} entries]",
                    a.len() - out.len()
                )))
            }
            Value::Array(out)
        }
        _ => value.clone(),
    }
}
impl RetrievalService {
    pub async fn evaluate_cases(
        &self,
        user: &UserContext,
        id: &str,
        request: EvaluationRequest,
    ) -> Result<Value> {
        if user.anonymous()
            || !(user.user_tier.trim().eq_ignore_ascii_case("admin")
                || user
                    .roles
                    .iter()
                    .any(|r| r.trim().eq_ignore_ascii_case("admin")))
        {
            return Err(QueryError::Forbidden);
        }
        self.store
            .require_dataset_access(user, id, "editor")
            .await?;
        let fetch = request
            .retrieval
            .top_k
            .max(request.k_values.iter().copied().max().unwrap_or(1));
        let mut judgements = Vec::new();
        let mut cases = Vec::new();
        let mut evidence = Vec::new();
        let mut budget = 4096;
        for case in &request.cases {
            let mut r = request.retrieval.clone();
            r.query = case.query.clone();
            r.top_k = fetch;
            let (mut results, mut metadata) = self.retrieve(user, id, r).await?;
            let supplied = results.len();
            results.truncate(fetch);
            metadata["pipeline"] = json!(if request.retrieval.hierarchical {
                "hierarchical"
            } else {
                "standard"
            });
            metadata["evaluation_window"] = json!({"requested_fetch_k":fetch,"provider_retrieved_count":supplied,"evaluated_retrieved_count":results.len(),"truncated":supplied>results.len()});
            let ranked: Vec<_> = results.iter().map(|r| r.segment_id.clone()).collect();
            let mut seen = BTreeSet::new();
            let mut duplicate_seen = BTreeSet::new();
            let mut duplicates = Vec::new();
            for id in &ranked {
                if !seen.insert(id.clone()) && duplicate_seen.insert(id.clone()) {
                    duplicates.push(id.clone())
                }
            }
            let mut relevance: BTreeMap<_, _> = case
                .relevant_segment_ids
                .iter()
                .map(|id| (id.clone(), 1.0))
                .collect();
            relevance.extend(case.relevance.clone());
            let case_id = case.case_id.as_deref().unwrap_or("");
            evidence.push(json!({"case_id":case_id,"provider_retrieved_count":supplied,"retrieved_count":ranked.len(),"unique_retrieved_count":seen.len(),"duplicate_segment_ids":duplicates,"retrieval_metadata":bounded_metadata(&metadata,0,&mut budget)}));
            if request.return_retrieved {
                let rows:Vec<_>=results.iter().map(|r|json!({"segment_id":r.segment_id,"document_id":r.document_id,"score":r.score,"relevant":relevance.get(&r.segment_id).copied().unwrap_or(0.0)>0.0,"relevance_grade":relevance.get(&r.segment_id).copied().unwrap_or(0.0),"metadata":bounded_metadata(&r.metadata,0,&mut budget),"content_type":r.content_type,"level":if request.retrieval.hierarchical{r.metadata["level"].clone()}else{Value::Null}})).collect();
                cases.push(json!({"case_id":case_id,"query":case.query,"retrieved":rows}));
            }
            judgements.push(QueryRetrievalJudgement {
                query_id: case_id.into(),
                retrieved: ranked,
                relevance,
            });
        }
        let report = evaluate_retrieval(&judgements, &request.k_values)?;
        let mut requested_budget = 512;
        let mut result = json!({"dataset_id":id,"num_cases":judgements.len(),"k_values":report["k_values"],"metrics":report["metrics"],"requested_config":bounded_metadata(&request.requested_config,0,&mut requested_budget),"case_metadata":evidence});
        if let Some(k) = request.k_values.last() {
            result["primary_metrics"] = report["metrics"][k.to_string()].clone()
        }
        if request.return_retrieved {
            result["cases"] = json!(cases);
            result["per_query"] = report["per_query"].clone();
        }
        Ok(result)
    }
}
pub fn presets() -> Value {
    let definitions = [
        (
            "fast",
            "快速 (Fast)",
            "仅向量检索，无重排。延迟最低，适合高 QPS / 低延迟场景。",
            "实时问答、聊天补全、对延迟敏感的在线服务",
            0.2,
            false,
            false,
            0.5,
        ),
        (
            "balanced",
            "均衡 (Balanced)",
            "混合检索 (RRF) + 重排。质量与延迟的推荐平衡点。",
            "大多数企业知识库的默认选择",
            0.3,
            true,
            false,
            0.5,
        ),
        (
            "accurate",
            "精确 (Accurate)",
            "混合检索 + 重排 + 更高阈值。牺牲少量召回换取更高精度。",
            "对准确性要求高、可容忍略低召回的场景",
            0.35,
            true,
            false,
            0.5,
        ),
        (
            "diverse",
            "多样 (Diverse)",
            "混合 + 重排 + MMR 去重多样化。减少冗余、覆盖更多角度。",
            "多文档摘要、浏览式探索、需要多角度信息",
            0.3,
            true,
            true,
            0.5,
        ),
        (
            "sota",
            "最优 (SOTA)",
            "混合 + 重排(top_n=10) + 轻度 MMR(λ=0.7)。最高质量配置。",
            "离线评测基线、对质量要求最高的关键场景",
            0.3,
            true,
            true,
            0.7,
        ),
    ];
    let presets:Vec<_>=definitions.into_iter().map(|(name,label,summary,recommended,threshold,rerank,mmr,lambda)|json!({"name":name,"label":label,"summary":summary,"recommended_for":recommended,"config":{"mode":if name=="fast"{"vector"}else{"hybrid"},"top_k":5,"score_threshold":threshold,"vector":{"enabled":true,"top_k":20,"score_threshold":null},"keyword":{"enabled":true,"top_k":20,"candidate_pool_size":200,"bm25_k1":1.2,"bm25_b":0.75},"fusion":{"strategy":"rrf","rrf_k":60,"rrf_weights":{"vector":1.0,"keyword":1.0},"alpha":if name=="fast"{0.75}else{0.6}},"rerank":{"enabled":rerank,"provider":"dashscope","model":"qwen3-rerank","top_n":if name=="sota"{Some(10)}else{None},"score_threshold":null},"mmr":{"enabled":mmr,"lambda":lambda,"similarity_threshold":null},"multimodal":{"enabled":false,"image_search_enabled":false,"image_score_threshold":0.2,"text_score_threshold":0.3,"use_separate_thresholds":false,"image_boost":1.0,"vlm_rerank_enabled":false,"vlm_rerank_weight":0.4,"content_type_filter":null}}})).collect();
    json!({"presets":presets,"recommended_default":"balanced","notes":{"rrf_k":"RRF 常数默认 60（Elastic/Cormack'09，近优且不敏感）。注意不同向量库口径不同（如 Qdrant 默认 k=2 且零基 rank）。","mmr":"MMR 默认关闭；仅建议用于多文档摘要/浏览类查询。lambda=0.5（LangChain 默认，0=最多多样、1=最相关）。","rerank_top_n":"建议一阶段召回 50-100，重排后保留 final top_k（5-10）。","score_threshold":"相似度阈值建议 0.2-0.35；仅在有重排时生效更稳妥。"}})
}
