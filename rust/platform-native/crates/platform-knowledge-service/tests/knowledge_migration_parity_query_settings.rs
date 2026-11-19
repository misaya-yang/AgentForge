//! Pure resolver assertions from query preset and fusion validation source suites.
use ai_platform_knowledge_service::{
    migration_knowledge_ingest::dataset_reads::revision_fingerprint,
    migration_knowledge_query::{
        config::{self, ResolvedConfig, RetrieveRequest},
        types::{Dataset, QueryError},
    },
};
use serde_json::{Value, json};
fn resolve(defaults: Value) -> Result<ResolvedConfig, QueryError> {
    config::resolve(
        &Dataset {
            index_config: json!({"retrieval":defaults}),
            ..Default::default()
        },
        &RetrieveRequest {
            query: "q".into(),
            ..Default::default()
        },
    )
}
#[test]
fn absent_disabled_presets_are_unconfigured() {
    for value in [
        json!({}),
        json!({"query_rewrite":null}),
        json!({"query_rewrite":false}),
        json!({"multi_query_expansion":{"enabled":false}}),
        json!({"hyde":{"enabled":false}}),
    ] {
        assert_eq!(config::parse_presets(&value).unwrap(), json!({}));
        assert_eq!(resolve(value).unwrap().presets, json!({}));
    }
}
#[test]
fn preset_shorthands_echo_inert_defaults_and_normalized_names() {
    assert_eq!(
        config::parse_presets(&json!({"query_rewrite":true,"hyde":true})).unwrap(),
        json!({"query_rewrite":{"enabled":true,"applied":false,"reason":"eval_gate_pending","preset":"pronoun_resolution"},"hyde":{"enabled":true,"applied":false,"reason":"flag_only"}})
    );
    assert_eq!(
        config::parse_presets(
            &json!({"multi_query_expansion":{"enabled":true,"preset":" LLM_Paraphrase "}})
        )
        .unwrap()["multi_query_expansion"]["preset"],
        "llm_paraphrase"
    );
    for preset in ["pronoun_resolution", "multi_turn_merge"] {
        assert_eq!(
            config::parse_presets(&json!({"query_rewrite":{"preset":preset}})).unwrap()["query_rewrite"]
                ["preset"],
            preset
        );
    }
}
#[test]
fn preset_unknown_names_and_malformed_shapes_fail_before_recall() {
    for value in [
        json!({"query_rewrite":{"enabled":true,"preset":"gpt-forever"}}),
        json!({"multi_query_expansion":{"enabled":true,"preset":"gpt-forever"}}),
    ] {
        assert!(
            config::parse_presets(&value)
                .unwrap_err()
                .to_string()
                .contains("preset")
        );
        assert!(resolve(value).is_err());
    }
    assert!(
        config::parse_presets(&json!({"hyde":{"enabled":true,"preset":"x"}}))
            .unwrap_err()
            .to_string()
            .contains("flag-only")
    );
    for value in [
        json!({"query_rewrite":"yes"}),
        json!({"query_rewrite":{"enabled":"true"}}),
        json!({"hyde":[1]}),
        json!({"multi_query_expansion":{"enabled":1}}),
    ] {
        assert!(config::parse_presets(&value).is_err());
        assert!(resolve(value).is_err());
    }
}
#[test]
fn preset_flags_and_scalar_shorthand_change_cache_fingerprint() {
    let fingerprint = |retrieval: Value| {
        revision_fingerprint(
            &json!({"dataset_id":"kb-demo","content_revision":7,"embedding_provider":"dashscope","embedding_model":"text-embedding-v3","embedding_dimension":1024,"index_config":{"retrieval":retrieval}}),
        )
    };
    let off = fingerprint(json!({}));
    let on = fingerprint(json!({"query_rewrite":{"enabled":true,"preset":"multi_turn_merge"}}));
    let other =
        fingerprint(json!({"query_rewrite":{"enabled":true,"preset":"pronoun_resolution"}}));
    assert_ne!(off, on);
    assert_ne!(on, other);
    assert_ne!(off, fingerprint(json!({"hyde":true})));
}
#[test]
fn rrf_weighting_requires_explicit_configuration() {
    assert!(!resolve(json!({})).unwrap().weighted_rrf);
    for value in [
        json!({"adaptive_weights":true}),
        json!({"dense_weight":0.5,"bm25_weight":0.5}),
        json!({"fusion":{"rrf_weights":{"dense":0.7,"bm25":0.3}}}),
    ] {
        assert!(resolve(value).unwrap().weighted_rrf);
    }
}
#[test]
fn nested_rrf_aliases_and_canonical_weighted_alpha_are_preserved() {
    for (weights, expected) in [
        (json!({"vector":0.8,"keyword":0.2}), (0.8, 0.2)),
        (json!({"dense":0.7,"bm25":0.3}), (0.7, 0.3)),
    ] {
        let result =
            resolve(json!({"fusion":{"strategy":"rrf","rrf_k":42,"rrf_weights":weights}})).unwrap();
        assert_eq!(result.fusion_method, "rrf");
        assert_eq!(result.rrf_k, 42);
        assert!((result.dense_weight - expected.0).abs() < 1e-12);
        assert!((result.bm25_weight - expected.1).abs() < 1e-12);
    }
    let result = resolve(json!({"fusion":{"strategy":"weighted","alpha":0.8}})).unwrap();
    assert_eq!(result.fusion_method, "weighted");
    assert!((result.dense_weight - 0.8).abs() < 1e-12);
    assert!((result.bm25_weight - 0.2).abs() < 1e-12);
}
#[test]
fn persisted_invalid_fusion_parameters_fail_closed() {
    for value in [
        json!({"rrf_k":0}),
        json!({"rrf_k":-1}),
        json!({"rrf_k":60.5}),
        json!({"fusion":{"rrf_weights":{"vector":0.0,"keyword":0.0}}}),
        json!({"dense_weight":-1.0,"bm25_weight":1.0}),
    ] {
        assert!(resolve(value.clone()).is_err(), "{value}");
    }
    // Non-finite floats are not legal wire JSON; reject at parsing rather than
    // constructing serde_json::Value null and falsely claiming NaN validation.
    for wire in [
        r#"{"query":"q","rrf_weights":{"vector":NaN}}"#,
        r#"{"query":"q","bm25_weight":Infinity}"#,
    ] {
        assert!(serde_json::from_str::<RetrieveRequest>(wire).is_err());
    }
}
