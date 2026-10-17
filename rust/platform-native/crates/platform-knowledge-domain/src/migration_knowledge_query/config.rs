use super::types::{Dataset, QueryError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
fn top_k() -> usize {
    5
}
fn cascade() -> String {
    "cascade".into()
}
fn ten() -> usize {
    10
}
fn yes() -> bool {
    true
}
fn hybrid() -> String {
    "hybrid".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrieveRequest {
    #[serde(default)]
    pub hierarchical: bool,
    #[serde(default = "cascade")]
    pub hierarchical_strategy: String,
    #[serde(default = "top_k")]
    pub l1_top_k: usize,
    #[serde(default = "ten")]
    pub l2_top_k: usize,
    #[serde(default = "yes")]
    pub include_context: bool,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
    #[serde(skip)]
    pub telemetry_source: String,
    pub query: String,
    #[serde(default = "top_k")]
    pub top_k: usize,
    #[serde(default = "hybrid")]
    pub mode: String,
    pub document_id: Option<String>,
    pub dense_weight: Option<f64>,
    pub bm25_weight: Option<f64>,
    pub fusion_method: Option<String>,
    pub rrf_k: Option<usize>,
    pub alpha: Option<f64>,
    pub score_threshold: Option<f64>,
    pub vector_top_k: Option<usize>,
    pub keyword_top_k: Option<usize>,
    pub candidate_top_k: Option<usize>,
    pub keyword_candidate_k: Option<usize>,
    pub fusion: Option<String>,
    pub rrf_weights: Option<Value>,
    pub rerank: Option<bool>,
    pub rerank_model: Option<String>,
    pub rerank_top_n: Option<usize>,
    pub mmr: Option<bool>,
    pub mmr_lambda: Option<f64>,
    pub mmr_threshold: Option<f64>,
    pub source_type_filter: Option<String>,
    pub language_filter: Option<String>,
    pub metadata_filter: Option<Value>,
}
impl Default for RetrieveRequest {
    fn default() -> Self {
        Self {
            hierarchical: false,
            hierarchical_strategy: cascade(),
            l1_top_k: 5,
            l2_top_k: 10,
            include_context: true,
            extra: Default::default(),
            telemetry_source: String::new(),
            query: String::new(),
            top_k: 5,
            mode: "hybrid".into(),
            document_id: None,
            dense_weight: None,
            bm25_weight: None,
            fusion_method: None,
            rrf_k: None,
            alpha: None,
            score_threshold: None,
            vector_top_k: None,
            keyword_top_k: None,
            candidate_top_k: None,
            keyword_candidate_k: None,
            fusion: None,
            rrf_weights: None,
            rerank: None,
            rerank_model: None,
            rerank_top_n: None,
            mmr: None,
            mmr_lambda: None,
            mmr_threshold: None,
            source_type_filter: None,
            language_filter: None,
            metadata_filter: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct ParentSettings {
    pub return_mode: String,
    pub fanout_top_k: usize,
    pub configured_fanout: bool,
}
#[derive(Clone, Debug)]
pub struct ResolvedConfig {
    pub mode: String,
    pub vector_k: usize,
    pub keyword_k: usize,
    pub candidate_k: usize,
    pub keyword_pool_k: usize,
    pub fusion_method: String,
    pub dense_weight: f64,
    pub bm25_weight: f64,
    pub weighted_rrf: bool,
    pub rrf_k: usize,
    pub threshold: f64,
    pub rerank: bool,
    pub rerank_provider: String,
    pub rerank_model: String,
    pub rerank_top_n: Option<usize>,
    pub mmr: bool,
    pub mmr_lambda: f64,
    pub mmr_threshold: Option<f64>,
    pub mmr_fill_policy: String,
    pub parent_child: Option<ParentSettings>,
    pub prepend_summary: Option<bool>,
    pub presets: Value,
}
fn validation(message: impl Into<String>) -> QueryError {
    QueryError::Validation(message.into())
}
pub fn server_owned_config(value: &Value) -> Result<()> {
    match value {
        Value::Object(m) => {
            for (k, v) in m {
                if [
                    "api_key",
                    "base_url",
                    "endpoint",
                    "api_base",
                    "token",
                    "authorization",
                    "headers",
                    "api_key_env",
                ]
                .contains(&k.trim().to_lowercase().as_str())
                {
                    return Err(validation(
                        "embedding/rerank credentials and endpoints are server-owned",
                    ));
                }
                server_owned_config(v)?
            }
        }
        Value::Array(a) => {
            for v in a {
                server_owned_config(v)?
            }
        }
        _ => {}
    }
    Ok(())
}
fn at<'a>(v: &'a Value, path: &str) -> &'a Value {
    let mut p = v;
    for key in path.split('.') {
        p = &p[key]
    }
    p
}
pub fn validate_bounds(v: &Value) -> Result<()> {
    for (path, max) in [
        ("top_k", 100),
        ("vector_top_k", 1000),
        ("keyword_top_k", 1000),
        ("candidate_top_k", 2000),
        ("keyword_candidate_k", 500),
        ("rerank_top_n", 1000),
        ("rrf_k", 10000),
        ("vector.top_k", 1000),
        ("keyword.top_k", 1000),
        ("keyword.candidate_pool_size", 500),
        ("fusion.rrf_k", 10000),
        ("rerank.top_n", 1000),
    ] {
        let n = at(v, path);
        if !n.is_null() && !n.as_u64().is_some_and(|n| n >= 1 && n <= max) {
            return Err(validation(format!(
                "{path} must be an integer between 1 and {max}"
            )));
        }
    }
    for path in [
        "dense_weight",
        "bm25_weight",
        "alpha",
        "score_threshold",
        "mmr_lambda",
        "mmr_threshold",
        "vector.score_threshold",
        "fusion.dense_weight",
        "fusion.bm25_weight",
        "fusion.alpha",
        "rerank.score_threshold",
        "mmr.lambda",
        "mmr.lambda_mult",
        "mmr.threshold",
        "mmr.similarity_threshold",
    ] {
        let n = at(v, path);
        if !n.is_null()
            && !n
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n))
        {
            return Err(validation(format!(
                "{path} must be finite and between 0 and 1"
            )));
        }
    }
    for path in ["rrf_weights", "fusion.rrf_weights"] {
        let n = at(v, path);
        if n.is_null() {
            continue;
        }
        let m = n
            .as_object()
            .ok_or_else(|| validation("rrf_weights must be an object"))?;
        if m.len() > 16
            || m.iter().any(|(k, v)| {
                k.trim().is_empty()
                    || k.chars().count() > 64
                    || !v
                        .as_f64()
                        .is_some_and(|n| n.is_finite() && (0.0..=100.0).contains(&n))
            })
            || (!m.is_empty() && !m.values().any(|v| v.as_f64().unwrap_or(0.0) > 0.0))
        {
            return Err(validation(
                "rrf_weights must contain at most 16 finite nonnegative weights including a positive weight",
            ));
        }
    }
    if let Some(raw) = at(v, "mmr.fill_policy").as_str() {
        if !["strict", "fill"].contains(&raw.trim().to_lowercase().as_str()) {
            return Err(validation("mmr.fill_policy must be strict or fill"));
        }
    } else if !at(v, "mmr.fill_policy").is_null() {
        return Err(validation("mmr.fill_policy must be strict or fill"));
    }
    if !at(v, "mmr.strict_diversity").is_null() && !at(v, "mmr.strict_diversity").is_boolean() {
        return Err(validation("mmr.strict_diversity must be a boolean"));
    }
    Ok(())
}
pub fn parse_presets(defaults: &Value) -> Result<Value> {
    let mut report = serde_json::Map::new();
    for (key, default, allowed, reason) in [
        (
            "query_rewrite",
            "pronoun_resolution",
            &["pronoun_resolution", "multi_turn_merge"][..],
            "eval_gate_pending",
        ),
        (
            "multi_query_expansion",
            "lexical_synonym",
            &["lexical_synonym", "llm_paraphrase"][..],
            "eval_gate_pending",
        ),
        ("hyde", "", &[][..], "flag_only"),
    ] {
        let raw = &defaults[key];
        if raw.is_null() || raw == &Value::Bool(false) {
            continue;
        }
        if !raw.is_object() && raw != &Value::Bool(true) {
            return Err(validation(format!("{key} must be a boolean or object")));
        }
        let enabled = if raw.is_object() {
            match raw.get("enabled") {
                None => true,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| validation(format!("{key}.enabled must be boolean")))?,
            }
        } else {
            true
        };
        if key == "hyde" && raw.get("preset").is_some() {
            return Err(validation("hyde is flag-only and takes no preset"));
        }
        let preset = raw["preset"]
            .as_str()
            .unwrap_or(default)
            .trim()
            .to_lowercase();
        if key != "hyde" && !allowed.contains(&preset.as_str()) {
            return Err(validation(format!("invalid {key}.preset")));
        }
        if enabled {
            let mut entry = json!({"enabled":true,"applied":false,"reason":reason});
            if key != "hyde" {
                entry["preset"] = json!(preset)
            }
            report.insert(key.into(), entry);
        }
    }
    Ok(Value::Object(report))
}
pub fn resolve(dataset: &Dataset, request: &RetrieveRequest) -> Result<ResolvedConfig> {
    if !(1..=4096).contains(&request.query.trim().chars().count()) {
        return Err(validation("query must contain 1-4096 characters"));
    }
    if request.extra.iter().any(|(k, v)| match k.as_str() {
        "include_images"
        | "include_associated_images"
        | "multimodal_rerank"
        | "image_search_enabled"
        | "use_separate_thresholds" => v.as_bool() == Some(true),
        "vlm_rerank_weight" | "image_boost" | "image_score_threshold" => !v.is_null(),
        "content_type_filter" => v
            .as_str()
            .is_some_and(|s| !s.is_empty() && !s.eq_ignore_ascii_case("text")),
        _ => false,
    }) {
        return Err(validation(
            "multimodal retrieval is not enabled for this release",
        ));
    }
    if !["cascade", "parallel", "adaptive"].contains(&request.hierarchical_strategy.as_str())
        || !(1..=100).contains(&request.l1_top_k)
        || !(1..=200).contains(&request.l2_top_k)
    {
        return Err(validation(
            "invalid hierarchical strategy or candidate bounds",
        ));
    }
    validate_bounds(&serde_json::to_value(request).map_err(|_| validation("invalid request"))?)?;
    let d = &dataset.index_config["retrieval"];
    validate_bounds(d)?;
    server_owned_config(&dataset.embedding_config)?;
    server_owned_config(&d["rerank"])?;
    let locked = ["enforce_config", "locked", "lock"]
        .iter()
        .any(|k| d[*k].as_bool() == Some(true));
    let empty = RetrieveRequest::default();
    let r = if locked { &empty } else { request };
    let mode = if locked {
        d["mode"].as_str().unwrap_or("hybrid")
    } else {
        r.mode.as_str()
    };
    let mode = match mode.to_lowercase().as_str() {
        "keyword" | "bm25" => "bm25",
        "vector" | "dense" => "dense",
        "hybrid" => "hybrid",
        _ => return Err(validation("mode must be dense|bm25|hybrid")),
    }
    .to_string();
    let fusion = &d["fusion"];
    let mut method = r
        .fusion_method
        .as_deref()
        .or(r.fusion.as_deref())
        .or(fusion["strategy"].as_str())
        .or(fusion["method"].as_str())
        .or(d["fusion_method"].as_str())
        .or(fusion.as_str())
        .unwrap_or("rrf")
        .to_lowercase();
    if method == "alpha" {
        method = "weighted".into()
    }
    if method != "weighted" && method != "rrf" {
        method = "rrf".into()
    }
    let mut dw = d["dense_weight"].as_f64().unwrap_or(0.5);
    let mut bw = d["bm25_weight"].as_f64().unwrap_or(0.5);
    if let Some(a) = fusion["alpha"].as_f64() {
        dw = a;
        bw = 1.0 - a
    } else {
        dw = fusion["dense_weight"].as_f64().unwrap_or(dw);
        bw = fusion["bm25_weight"].as_f64().unwrap_or(bw)
    }
    fn weights(v: &Value, d: &mut f64, b: &mut f64) {
        *d = v["vector"].as_f64().or(v["dense"].as_f64()).unwrap_or(*d);
        *b = v["keyword"].as_f64().or(v["bm25"].as_f64()).unwrap_or(*b)
    }
    if method == "rrf" {
        weights(&d["rrf_weights"], &mut dw, &mut bw);
        weights(&fusion["rrf_weights"], &mut dw, &mut bw)
    }
    if let Some(a) = r.alpha {
        dw = a;
        bw = 1.0 - a
    } else {
        dw = r.dense_weight.unwrap_or(dw);
        bw = r.bm25_weight.unwrap_or(bw)
    }
    if method == "rrf" {
        if let Some(w) = &r.rrf_weights {
            weights(w, &mut dw, &mut bw)
        }
    }
    if dw < 0.0 || bw < 0.0 || !dw.is_finite() || !bw.is_finite() || dw + bw <= 0.0 {
        return Err(validation(
            "fusion weights must be finite and non-negative with a positive value",
        ));
    }
    let weighted_rrf = r.dense_weight.is_some()
        || r.bm25_weight.is_some()
        || r.alpha.is_some()
        || r.rrf_weights
            .as_ref()
            .is_some_and(|v| v.as_object().is_some_and(|v| !v.is_empty()))
        || ["dense_weight", "bm25_weight", "rrf_weights"]
            .iter()
            .any(|k| !d[*k].is_null())
        || ["dense_weight", "bm25_weight", "rrf_weights", "alpha"]
            .iter()
            .any(|k| !fusion[*k].is_null())
        || d["adaptive_weights"].as_bool() == Some(true);
    if mode == "hybrid" && d["adaptive_weights"].as_bool().unwrap_or(true) {
        (dw, bw) = super::lexical::language_weights(&request.query, dw, bw)
    }
    let mut vk = r
        .vector_top_k
        .or(d["vector_top_k"].as_u64().map(|v| v as usize))
        .unwrap_or(request.top_k.max(12));
    let mut kk = r
        .keyword_top_k
        .or(d["keyword_top_k"].as_u64().map(|v| v as usize))
        .unwrap_or(request.top_k.max(12));
    let mut ck = r
        .candidate_top_k
        .or(d["candidate_top_k"].as_u64().map(|v| v as usize))
        .unwrap_or((request.top_k * 2).max(24))
        .max(request.top_k)
        .min(2000);
    let mut pk = r
        .keyword_candidate_k
        .or(d["keyword_candidate_k"].as_u64().map(|v| v as usize))
        .unwrap_or((kk * 3).max(50))
        .max(kk)
        .min(500);
    let p = &d["parent_child"];
    let parent_child =
        if p.is_null() || p == &Value::Bool(false) || p["enabled"].as_bool() == Some(false) {
            None
        } else {
            if !p.is_object() && p != &Value::Bool(true) {
                return Err(validation("parent_child must be a boolean or object"));
            }
            let m = p["return_mode"]
                .as_str()
                .unwrap_or("parent")
                .trim()
                .to_lowercase();
            if !["parent", "context"].contains(&m.as_str()) {
                return Err(validation(
                    "parent_child.return_mode must be parent or context",
                ));
            }
            let n = p["fanout_top_k"].as_u64().unwrap_or(48) as usize;
            if !p["fanout_top_k"].is_null()
                && (!p["fanout_top_k"].is_u64() || !(1..=2000).contains(&n))
            {
                return Err(validation(
                    "parent_child.fanout_top_k must be an integer between 1 and 2000",
                ));
            }
            let fanout = n.max(request.top_k);
            vk = vk.max(fanout.min(1000));
            kk = kk.max(fanout.min(1000));
            ck = ck.max(fanout.min(2000));
            pk = pk.max(fanout.min(500));
            Some(ParentSettings {
                return_mode: m,
                fanout_top_k: n,
                configured_fanout: !p["fanout_top_k"].is_null(),
            })
        };
    let s = &d["summary_index"];
    let prepend_summary =
        if s.is_null() || s == &Value::Bool(false) || s["enabled"].as_bool() == Some(false) {
            None
        } else {
            if !s.is_object() && s != &Value::Bool(true) {
                return Err(validation("summary_index must be a boolean or object"));
            }
            Some(match s.get("prepend_summary") {
                None => true,
                Some(x) => x
                    .as_bool()
                    .ok_or_else(|| validation("summary_index.prepend_summary must be boolean"))?,
            })
        };
    let rk = &d["rerank"];
    let mmr = &d["mmr"];
    let mmr_policy = mmr["fill_policy"]
        .as_str()
        .map(|s| s.trim().to_lowercase())
        .unwrap_or_else(|| {
            if mmr["strict_diversity"].as_bool() == Some(false) {
                "fill".into()
            } else {
                "strict".into()
            }
        });
    Ok(ResolvedConfig {
        mode,
        vector_k: vk,
        keyword_k: kk,
        candidate_k: ck,
        keyword_pool_k: pk,
        fusion_method: method,
        dense_weight: dw,
        bm25_weight: bw,
        weighted_rrf,
        rrf_k: r
            .rrf_k
            .or(fusion["rrf_k"].as_u64().map(|n| n as usize))
            .or(d["rrf_k"].as_u64().map(|n| n as usize))
            .unwrap_or(60),
        threshold: r
            .score_threshold
            .or(d["score_threshold"].as_f64())
            .unwrap_or(0.0),
        rerank: r.rerank.unwrap_or_else(|| {
            rk.as_bool()
                .unwrap_or_else(|| rk["enabled"].as_bool().unwrap_or(false))
        }),
        rerank_provider: rk["provider"].as_str().unwrap_or("").into(),
        rerank_model: r
            .rerank_model
            .clone()
            .or(rk["model"].as_str().map(str::to_owned))
            .unwrap_or_else(|| "qwen3-rerank".into()),
        rerank_top_n: r.rerank_top_n.or(rk["top_n"].as_u64().map(|v| v as usize)),
        mmr: r.mmr.unwrap_or_else(|| {
            mmr.as_bool()
                .unwrap_or_else(|| mmr["enabled"].as_bool().unwrap_or(false))
        }),
        mmr_lambda: r.mmr_lambda.or(mmr["lambda"].as_f64()).unwrap_or(0.5),
        mmr_threshold: r.mmr_threshold.or(mmr["threshold"].as_f64()),
        mmr_fill_policy: mmr_policy,
        parent_child,
        prepend_summary,
        presets: parse_presets(d)?,
    })
}
