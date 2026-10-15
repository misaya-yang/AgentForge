//! Validation boundary for nested retrieval presets used by the A/B workbench.
use serde_json::Value;
fn number(v: &Value, path: &str) -> Option<f64> {
    v.pointer(path)
        .and_then(Value::as_f64)
        .filter(|v| v.is_finite())
}
pub fn valid(v: &Value) -> bool {
    if !["vector", "keyword", "hybrid"].contains(&v["mode"].as_str().unwrap_or(""))
        || !["rrf", "weighted"].contains(&v["fusion"]["strategy"].as_str().unwrap_or(""))
        || !["dashscope", "cohere", "jina", "bge", "custom"]
            .contains(&v["rerank"]["provider"].as_str().unwrap_or(""))
    {
        return false;
    }
    for path in [
        "/vector/enabled",
        "/keyword/enabled",
        "/rerank/enabled",
        "/mmr/enabled",
        "/multimodal/enabled",
        "/multimodal/image_search_enabled",
        "/multimodal/use_separate_thresholds",
        "/multimodal/vlm_rerank_enabled",
    ] {
        if !v.pointer(path).is_some_and(Value::is_boolean) {
            return false;
        }
    }
    for (path, max) in [
        ("/top_k", 100),
        ("/vector/top_k", 1000),
        ("/keyword/top_k", 1000),
        ("/keyword/candidate_pool_size", 500),
        ("/fusion/rrf_k", 10000),
    ] {
        if !v
            .pointer(path)
            .and_then(Value::as_u64)
            .is_some_and(|n| n >= 1 && n <= max)
        {
            return false;
        }
    }
    for path in [
        "/fusion/alpha",
        "/mmr/lambda",
        "/keyword/bm25_b",
        "/multimodal/image_score_threshold",
        "/multimodal/text_score_threshold",
        "/multimodal/vlm_rerank_weight",
    ] {
        if !number(v, path).is_some_and(|n| (0.0..=1.0).contains(&n)) {
            return false;
        }
    }
    for path in [
        "/score_threshold",
        "/vector/score_threshold",
        "/rerank/score_threshold",
        "/mmr/similarity_threshold",
    ] {
        let Some(value) = v.pointer(path) else {
            return false;
        };
        if !value.is_null() && !value.as_f64().is_some_and(|n| (0.0..=1.0).contains(&n)) {
            return false;
        }
    }
    if !v["rerank"]["top_n"].is_null()
        && !v["rerank"]["top_n"]
            .as_u64()
            .is_some_and(|n| (1..=1000).contains(&n))
    {
        return false;
    }
    if !v["rerank"]["model"]
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.chars().count() <= 256)
    {
        return false;
    }
    if !v["multimodal"]["content_type_filter"].is_null()
        && !v["multimodal"]["content_type_filter"]
            .as_str()
            .is_some_and(|s| s.chars().count() <= 64)
    {
        return false;
    }
    for path in ["/keyword/bm25_k1", "/multimodal/image_boost"] {
        if !number(v, path).is_some_and(|n| (0.0..=10.0).contains(&n)) {
            return false;
        }
    }
    let Some(weights) = v["fusion"]["rrf_weights"].as_object() else {
        return false;
    };
    weights.len() <= 16
        && weights.iter().all(|(name, weight)| {
            !name.is_empty()
                && name.chars().count() <= 64
                && weight.as_f64().is_some_and(|n| (0.0..=100.0).contains(&n))
        })
}
