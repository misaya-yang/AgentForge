//! Actual process parsing/provider-key selection; no global environment mutation.
use ai_platform_knowledge_service::{process::ProcessSettings, settings::ProviderKeys};
use std::collections::BTreeMap;
fn settings(extra: &[(&str, &str)]) -> ProcessSettings {
    let mut values: BTreeMap<String, String> = [
        ("ENVIRONMENT", "test"),
        ("AI_PLATFORM_INTERNAL_TOKEN", "synthetic-token"),
        ("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "synthetic-proof"),
        ("INTERNAL_COMM_STATE_BACKEND", "memory"),
        ("INTERNAL_IDEMPOTENCY_BACKEND", "memory"),
        ("KNOWLEDGE_RUNTIME_ROLE", "api"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    values.extend(extra.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    ProcessSettings::from_values(values, std::env::current_dir().unwrap()).unwrap()
}
#[test]
fn qa_original_environment_names_and_empty_alias_fallthrough() {
    let s = settings(&[("DASHSCOPE_API_KEY", "primary"), ("Aliyun_KEY", "legacy")]);
    assert_eq!(s.configuration.provider_keys.qa_key("dashscope"), "primary");
    let s = settings(&[("DASHSCOPE_API_KEY", ""), ("Aliyun_KEY", "legacy")]);
    assert_eq!(s.configuration.provider_keys.qa_key("dashscope"), "legacy");
    let s = settings(&[("LLM_API_KEY", "generic"), ("DASHSCOPE_API_KEY", "primary")]);
    assert_eq!(s.configuration.provider_keys.qa_key("dashscope"), "generic");
}
#[test]
fn provider_specific_fallbacks_do_not_cross_provider_boundaries() {
    let s = settings(&[
        ("DEEPSEEK_KEY", "deepseek-fallback"),
        ("GOOGLE_API_KEY", "google-fallback"),
    ]);
    assert_eq!(
        s.configuration.provider_keys.qa_key("deepseek"),
        "deepseek-fallback"
    );
    assert_eq!(
        s.configuration.provider_keys.qa_key("gemini"),
        "google-fallback"
    );
    let s = settings(&[("DASHSCOPE_API_KEY", "dashscope-only")]);
    assert_eq!(s.configuration.provider_keys.qa_key("gemini"), "");
    assert_eq!(ProviderKeys::default().qa_key("custom"), "");
}
#[test]
fn parsing_each_supplied_environment_snapshot_observes_new_values() {
    assert_eq!(
        settings(&[("GOOGLE_API_KEY", "first")])
            .configuration
            .provider_keys
            .google_api_key,
        "first"
    );
    assert_eq!(
        settings(&[("GOOGLE_API_KEY", "second")])
            .configuration
            .provider_keys
            .google_api_key,
        "second"
    );
    assert_eq!(settings(&[]).configuration.provider_keys.google_api_key, "");
    assert_eq!(
        settings(&[("LLM_MODEL", "qwen3.7-plus")])
            .configuration
            .qa
            .expect("process settings must include QA configuration")
            .model,
        "qwen3.7-plus"
    );
    assert_eq!(
        settings(&[("LLM_MODEL", "")])
            .configuration
            .qa
            .expect("process settings must include QA configuration")
            .model,
        ""
    );
}
#[test]
fn domain_endpoints_use_shared_resolver_and_keep_explicit_embedding_override() {
    let s = settings(&[
        ("DASHSCOPE_API_KEY", "shared"),
        ("DASHSCOPE_EMBEDDING_API_KEY", " embedding "),
        ("DASHSCOPE_OCR_API_KEY", " ocr "),
        (
            "DASHSCOPE_BASE_URL",
            "https://generic.invalid/compatible-mode/v1",
        ),
        (
            "DASHSCOPE_EMBEDDING_BASE_URL",
            "https://embedding.invalid/api/v1/",
        ),
        ("KNOWLEDGE_EMBEDDINGS__DASHSCOPE_API_KEY", "explicit"),
    ]);
    let keys = &s.configuration.provider_keys;
    assert_eq!(keys.dashscope_embedding_api_key, "embedding");
    assert_eq!(keys.dashscope_ocr_api_key, "ocr");
    assert_eq!(
        keys.dashscope_embedding_base_url,
        "https://embedding.invalid/api/v1"
    );
    assert_eq!(
        keys.dashscope_ocr_base_url,
        "https://generic.invalid/api/v1"
    );
    assert_eq!(s.configuration.embeddings.dashscope_api_key, "explicit");
}
