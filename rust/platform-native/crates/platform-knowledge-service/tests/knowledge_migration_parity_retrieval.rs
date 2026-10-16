use ai_platform_knowledge_service::migration_knowledge_ingest::dataset_reads::revision_fingerprint;
use ai_platform_knowledge_service::migration_knowledge_query::{
    config::{self, ResolvedConfig, RetrieveRequest},
    parents::{self, StructuralSettings},
    types::{Candidate, Dataset},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
fn resolved(
    retrieval: Value,
) -> Result<
    ResolvedConfig,
    ai_platform_knowledge_service::migration_knowledge_query::types::QueryError,
> {
    config::resolve(
        &Dataset {
            index_config: json!({"retrieval":retrieval}),
            ..Default::default()
        },
        &RetrieveRequest {
            query: "query".into(),
            ..Default::default()
        },
    )
}
fn candidate(id: &str, score: f64, metadata: Value) -> Candidate {
    Candidate {
        segment_id: id.into(),
        document_id: "doc-1".into(),
        text: format!("block body of {id}"),
        metadata,
        point_id: None,
        dense_score: None,
        bm25_score: None,
        fusion_score: Some(score),
        rerank_score: None,
        final_score: score,
        sources: ["bm25".into()].into_iter().collect(),
    }
}
#[test]
fn parent_and_summary_settings_preserve_disabled_defaults_and_invalid_shapes() {
    for p in [Value::Null, json!(false), json!({"enabled":false})] {
        assert!(
            resolved(json!({"parent_child":p,"summary_index":p}))
                .unwrap()
                .parent_child
                .is_none()
        );
        assert!(
            resolved(json!({"summary_index":p}))
                .unwrap()
                .prepend_summary
                .is_none()
        );
    }
    let c = resolved(json!({"parent_child":true,"summary_index":true})).unwrap();
    let p = c.parent_child.unwrap();
    assert_eq!(p.return_mode, "parent");
    assert_eq!(p.fanout_top_k, 48);
    assert!(!p.configured_fanout);
    assert_eq!(c.prepend_summary, Some(true));
    let c=resolved(json!({"parent_child":{"enabled":true,"return_mode":"CONTEXT","fanout_top_k":12},"summary_index":{"enabled":true,"prepend_summary":false}})).unwrap();
    let p = c.parent_child.unwrap();
    assert_eq!(p.return_mode, "context");
    assert_eq!(p.fanout_top_k, 12);
    assert!(p.configured_fanout);
    assert_eq!(c.prepend_summary, Some(false));
    for p in [
        json!("yes"),
        json!({"return_mode":"hybrid"}),
        json!({"fanout_top_k":0}),
        json!({"fanout_top_k":-3}),
        json!({"fanout_top_k":2001}),
        json!({"fanout_top_k":"48"}),
        json!({"fanout_top_k":true}),
    ] {
        assert!(resolved(json!({"parent_child":p})).is_err(), "{p}");
    }
    for s in [json!(1), json!({"prepend_summary":"yes"})] {
        assert!(resolved(json!({"summary_index":s})).is_err());
    }
}
#[test]
fn structural_settings_validate_exact_source_cases() {
    for s in [Value::Null, json!(false), json!({"enabled":false})] {
        assert!(
            parents::parse_structural_settings(&json!({"structural_routing":s}))
                .unwrap()
                .is_none()
        );
    }
    let default = parents::parse_structural_settings(&json!({"structural_routing":true}))
        .unwrap()
        .unwrap();
    assert_eq!(default.boost, 0.15);
    assert_eq!(default.min_affinity, 0.25);
    let custom=parents::parse_structural_settings(&json!({"structural_routing":{"enabled":true,"mode":"heading_priority","boost":0.4,"min_affinity":0.0}})).unwrap().unwrap();
    assert_eq!(custom.boost, 0.4);
    for s in [
        json!([]),
        json!({"mode":"document_first"}),
        json!({"boost":1.5}),
        json!({"boost":-0.1}),
        json!({"boost":true}),
        json!({"min_affinity":"wide"}),
    ] {
        assert!(parents::parse_structural_settings(&json!({"structural_routing":s})).is_err());
    }
}
#[test]
fn summary_merge_preserves_max_score_source_union_and_original_body() {
    for (block_score, summary_score, expected) in [(0.8, 0.3, 0.8), (0.2, 0.9, 0.9)] {
        let b = candidate("b-1", block_score, json!({}));
        let mut s = candidate(
            "s-1",
            summary_score,
            json!({"metadata":{"is_summary":true,"original_chunk_id":"b-1"}}),
        );
        s.text = "summary text".into();
        s.sources = ["vector".into()].into_iter().collect();
        let mut c = BTreeMap::from([("b-1".into(), b), ("s-1".into(), s)]);
        assert_eq!(
            parents::merge_summary_siblings(&mut c, true),
            json!({"enabled":true,"merged":1,"unresolved":0})
        );
        assert_eq!(c.len(), 1);
        assert_eq!(c["b-1"].final_score, expected);
        assert_eq!(c["b-1"].fusion_score, Some(expected));
        assert_eq!(c["b-1"].text, "block body of b-1");
        assert_eq!(c["b-1"].metadata["_summary_prefix"], "summary text");
        assert_eq!(c["b-1"].metadata["_summary_hit"], true);
        assert_eq!(c["b-1"].metadata["_summary_segment_id"], "s-1");
        assert_eq!(
            c["b-1"].sources,
            ["bm25".into(), "vector".into()].into_iter().collect()
        );
    }
}
#[test]
fn unresolved_summary_passthrough_and_prefix_optout() {
    let summary = candidate(
        "s",
        0.4,
        json!({"metadata":{"is_summary":true,"original_chunk_id":"b"}}),
    );
    let mut c = BTreeMap::from([("s".into(), summary.clone())]);
    let stats = parents::merge_summary_siblings(&mut c, true);
    assert_eq!(stats["unresolved"], 1);
    assert!(c.contains_key("s"));
    c.insert("b".into(), candidate("b", 0.5, json!({})));
    parents::merge_summary_siblings(&mut c, false);
    assert!(c["b"].metadata.get("_summary_prefix").is_none());
}
#[test]
fn structural_boost_affinity_clamping_and_empty_query() {
    let mut c = BTreeMap::from([
        (
            "a".into(),
            candidate("a", 0.5, json!({"section_header":"Deployment Network"})),
        ),
        (
            "b".into(),
            candidate("b", 0.6, json!({"section_header":"Billing FAQ"})),
        ),
        ("c".into(), candidate("c", 0.4, json!({}))),
    ]);
    let stats = parents::apply_structural_routing(
        &mut c,
        "deployment network",
        &StructuralSettings {
            boost: 0.4,
            min_affinity: 0.25,
        },
    );
    assert_eq!(stats["candidates_with_breadcrumb"], 2);
    assert_eq!(stats["boosted"], 1);
    assert!((c["a"].final_score - 0.9).abs() < 1e-12);
    assert_eq!(c["b"].final_score, 0.6);
    assert_eq!(c["c"].final_score, 0.4);
    assert_eq!(c["a"].metadata["_structural_route"]["affinity"], 1.0);
    parents::apply_structural_routing(
        &mut c,
        "network",
        &StructuralSettings {
            boost: 1.,
            min_affinity: 0.25,
        },
    );
    assert_eq!(c["a"].fusion_score, Some(1.));
    let old = c["a"].final_score;
    assert_eq!(
        parents::apply_structural_routing(
            &mut c,
            "   ",
            &StructuralSettings {
                boost: 0.15,
                min_affinity: 0.25
            }
        )["boosted"],
        0
    );
    assert_eq!(c["a"].final_score, old);
}
#[test]
fn dataset_fingerprint_tracks_authority_and_effective_config_not_secrets() {
    let base = json!({"dataset_id":"dataset-a","tenant_id":"tenant-a","content_revision":7,"embedding_config":{"api_key":"secret-a","base_url":"https://user:password@embedding.example/v1?api_key=secret"},"index_config":{"retrieval":{"mode":"hybrid","score_threshold":0.2,"rerank":{"enabled":false,"api_key":"secret-a"}}}});
    let initial = revision_fingerprint(&base).unwrap();
    assert!(initial.starts_with("sha256:"));
    let mut changed = base.clone();
    changed["content_revision"] = json!(8);
    assert_ne!(revision_fingerprint(&changed).unwrap(), initial);
    changed = base.clone();
    changed["index_config"]["retrieval"]["mode"] = json!("bm25");
    assert_ne!(revision_fingerprint(&changed).unwrap(), initial);
    changed = base.clone();
    changed["embedding_config"]["api_key"] = json!("secret-b");
    changed["index_config"]["retrieval"]["rerank"]["api_key"] = json!("secret-b");
    assert_eq!(revision_fingerprint(&changed).unwrap(), initial);
    changed["content_revision"] = Value::Null;
    assert!(revision_fingerprint(&changed).is_none());
}
#[test]
fn dataset_fingerprint_includes_nested_rrf_weights_and_lexical_profile() {
    let mut base = json!({"dataset_id":"dataset-a","content_revision":7,"index_config":{"retrieval":{"fusion":{"rrf_weights":{"vector":0.8,"keyword":0.2}},"lexical":{"active_version":"lexical_v1","bm25_v2":{"shadow_write_enabled":true,"k":1.2,"b":0.75,"avg_len":256,"tokenizer":"multilingual","language":"none"}}}}});
    let first = revision_fingerprint(&base);
    base["index_config"]["retrieval"]["fusion"]["rrf_weights"] =
        json!({"vector":0.2,"keyword":0.8});
    assert_ne!(revision_fingerprint(&base), first);
    let second = revision_fingerprint(&base);
    base["index_config"]["retrieval"]["lexical"]["bm25_v2"]["avg_len"] = json!(384);
    assert_ne!(revision_fingerprint(&base), second);
}

#[test]
fn public_and_persisted_retrieval_bounds_refuse_amplification() {
    for (key, number) in [
        ("top_k", 101),
        ("vector_top_k", 1001),
        ("keyword_top_k", 1001),
        ("candidate_top_k", 2001),
        ("keyword_candidate_k", 501),
        ("rerank_top_n", 1001),
        ("rrf_k", 10001),
    ] {
        let mut value = json!({"query":"q"});
        value[key] = json!(number);
        let request: RetrieveRequest = serde_json::from_value(value.clone()).unwrap();
        assert!(
            config::resolve(&Dataset::default(), &request).is_err(),
            "{value}"
        );
        assert!(resolved(value).is_err());
    }
    let request = RetrieveRequest {
        query: "q".repeat(4097),
        ..Default::default()
    };
    assert!(config::resolve(&Dataset::default(), &request).is_err());
    for threshold in [-0.1, 1.1] {
        let request = RetrieveRequest {
            query: "q".into(),
            score_threshold: Some(threshold),
            ..Default::default()
        };
        assert!(config::resolve(&Dataset::default(), &request).is_err());
    }
}
#[test]
fn parent_fanout_widens_each_leg_without_shrinking() {
    for (fanout, vector, keyword, candidate, pool, expected) in [
        (48, 10, 10, 30, 50, (48, 48, 48, 50)),
        (8, 100, 60, 200, 300, (100, 60, 200, 300)),
        (2000, 1, 1, 1, 1, (1000, 1000, 2000, 500)),
    ] {
        let result=resolved(json!({"parent_child":{"fanout_top_k":fanout},"vector_top_k":vector,"keyword_top_k":keyword,"candidate_top_k":candidate,"keyword_candidate_k":pool})).unwrap();
        assert_eq!(
            (
                result.vector_k,
                result.keyword_k,
                result.candidate_k,
                result.keyword_pool_k
            ),
            expected
        );
    }
}

#[test]
fn lexical_profile_defaults_fingerprints_roundtrip_and_fail_closed() {
    use ai_platform_knowledge_service::migration_knowledge_query::lexical_v2::LexicalProfile;
    let legacy = LexicalProfile::from_dataset(&Dataset::default()).unwrap();
    assert!(!legacy.active);
    assert!(!legacy.shadow);
    let profile = |k: f64| Dataset {
        index_config: json!({"retrieval":{"lexical":{"active_version":"lexical_v1","bm25_v2":{"shadow_write_enabled":true,"k":k,"b":0.75,"avg_len":256,"tokenizer":"multilingual","language":"none"}}}}),
        ..Default::default()
    };
    let first = LexicalProfile::from_dataset(&profile(1.2)).unwrap();
    let changed = LexicalProfile::from_dataset(&profile(1.5)).unwrap();
    assert_ne!(first.schema_fingerprint(), changed.schema_fingerprint());
    let persisted = first.collection_profile();
    assert_eq!(persisted["bm25_v2"]["field"], "bm25_v2");
    assert_eq!(persisted["bm25_v2"]["model"], "qdrant/bm25");
    assert_eq!(persisted["bm25_v2"]["tokenizer"], "multilingual");
    assert_eq!(persisted["bm25_v2"]["language"], "none");
    assert_eq!(
        persisted["filtering"]["required_payload_indexes"],
        json!(["tenant_id", "dataset_id"])
    );
    assert_eq!(
        LexicalProfile::from_collection(&persisted)
            .unwrap()
            .collection_profile(),
        persisted
    );
    let mut invalid = profile(1.2);
    invalid.index_config["retrieval"]["lexical"]["active_version"] = json!("bm25_v2");
    invalid.index_config["retrieval"]["lexical"]["bm25_v2"]["shadow_write_enabled"] = json!(false);
    assert!(LexicalProfile::from_dataset(&invalid).is_err());
    let mut invalid = profile(1.2);
    invalid.index_config["retrieval"]["lexical"]["bm25_v2"]["typo_k"] = json!(1.2);
    assert!(LexicalProfile::from_dataset(&invalid).is_err());
}
