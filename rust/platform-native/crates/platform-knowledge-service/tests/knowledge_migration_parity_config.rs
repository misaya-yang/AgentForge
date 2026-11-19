//! Pure source contracts: metadata registry, chunking configuration and reranker factory.
use ai_platform_knowledge_service::{
    migration_knowledge_ingest::{
        chunking::{ChunkingConfig, validate_config},
        metadata::{normalize_patch, validate_fields},
    },
    migration_knowledge_query::rerank::{
        normalize_model, normalize_provider, resolve_dashscope_endpoint,
    },
};
use serde_json::json;
#[test]
fn metadata_registry_rejects_reserved_duplicate_and_type_changes() {
    let fields = vec![
        json!({"name":"author","label":"Author","type":"string"}),
        json!({"name":"priority","label":"Priority","type":"number"}),
        json!({"name":"published_at","label":"Published","type":"datetime"}),
    ];
    assert!(
        validate_fields(
            &[json!({"name":"tenant_id","label":"Tenant","type":"string"})],
            &[]
        )
        .unwrap_err()
        .to_string()
        .contains("reserved")
    );
    assert!(
        validate_fields(&[fields[0].clone(), fields[0].clone()], &[])
            .unwrap_err()
            .to_string()
            .contains("duplicated")
    );
    assert!(
        validate_fields(
            &[json!({"name":"author","label":"Author","type":"number"})],
            &fields
        )
        .unwrap_err()
        .to_string()
        .contains("immutable")
    );
}
#[test]
fn metadata_patch_is_typed_bounded_and_timezone_normalized() {
    let registry = json!({"version":1,"revision":4,"fields":[{"name":"author","label":"Author","type":"string"},{"name":"priority","label":"Priority","type":"number"},{"name":"published_at","label":"Published","type":"datetime"}]});
    let (patch, removed) = normalize_patch(
        &registry,
        &json!({"author":"Ada","priority":3,"published_at":"2026-08-29T08:00:00-04:00"}),
        &[],
    )
    .unwrap();
    assert!(removed.is_empty());
    assert_eq!(
        patch,
        json!({"author":"Ada","priority":3,"published_at":"2026-08-29T12:00:00Z"})
    );
    assert!(
        normalize_patch(
            &registry,
            &json!({"published_at":"2026-08-29T12:00:00"}),
            &[]
        )
        .unwrap_err()
        .to_string()
        .contains("timezone")
    );
    assert!(
        normalize_patch(&registry, &json!({"constructor":"x"}), &[])
            .unwrap_err()
            .to_string()
            .contains("unknown")
    );
    assert!(normalize_patch(&registry, &json!({"author":"x".repeat(2049)}), &[]).is_err());
    assert!(normalize_patch(&registry, &json!({"priority":true}), &[]).is_err());
}
#[test]
fn chunking_runtime_rejects_unsafe_source_fields() {
    for value in [
        json!({"page_marker":"(a+)+$"}),
        json!({"metadata_fields":[""]}),
        json!({"separators":[""]}),
        json!({"segmentation":{"max_tokens":500,"separator":"\\n"}}),
    ] {
        assert!(validate_config(&value).is_err(), "{value}");
    }
}
#[test]
fn chunking_config_chain_uses_real_parser_and_preserves_explicit_budgets() {
    let c = ChunkingConfig::from_value(
        &json!({"mode":"hierarchical","token_limit":400,"chunk_overlap":50,"use_token_count":true}),
    )
    .unwrap();
    assert_eq!(c.token_limit, 400);
    assert!(c.use_token_count);
    assert_eq!(c.child_token_limit, 400);
    // Archived production from_dict derives child * 4 (chunking.py:1159).
    // The old test_chunking_config_chain.py copied a stale child * 3 helper;
    // it did not invoke production from_dict. Keep runtime behavior authoritative.
    assert_eq!(c.parent_token_limit, 1600);
    assert_eq!(c.min_chunk_tokens, None);
    assert_eq!(c.max_chunk_tokens, None);
    let c=ChunkingConfig::from_value(&json!({"mode":"hierarchical","token_limit":400,"chunk_overlap":50,"use_token_count":true,"min_chunk_tokens":80,"max_chunk_tokens":400,"child_token_limit":400,"parent_token_limit":1600})).unwrap();
    assert_eq!(c.token_limit, 400);
    assert!(c.use_token_count);
    assert_eq!(c.min_chunk_tokens, Some(80));
    assert_eq!(c.max_chunk_tokens, Some(400));
    assert_eq!(c.child_token_limit, 400);
    assert_eq!(c.parent_token_limit, 1600);
}
#[test]
fn reranker_provider_inference_aliases_and_regional_endpoints() {
    for model in ["bge-reranker-v2-m3", "BAAI/bge-reranker-v2-m3"] {
        assert_eq!(normalize_provider("", model), "bge");
    }
    assert_eq!(normalize_model("dashscope", "gte-rerank"), "gte-rerank-v2");
    assert_eq!(normalize_model("dashscope", ""), "qwen3-rerank");
    assert_eq!(
        resolve_dashscope_endpoint(
            "qwen3-rerank",
            "",
            "https://workspace-123.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
        )
        .unwrap(),
        "https://workspace-123.ap-southeast-1.maas.aliyuncs.com/compatible-api/v1/reranks"
    );
    assert_eq!(
        resolve_dashscope_endpoint(
            "qwen3-rerank",
            "",
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1"
        )
        .unwrap(),
        "https://dashscope-intl.aliyuncs.com/api/v1/services/rerank/text-rerank/text-rerank"
    );
    assert!(resolve_dashscope_endpoint("qwen3-rerank", "", "").is_err());
}
