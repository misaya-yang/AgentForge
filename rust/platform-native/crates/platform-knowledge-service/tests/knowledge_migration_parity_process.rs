//! F-33 process migration: credentials are rejected, selectors remain explicit.
use ai_platform_knowledge_service::{process::ProcessSettings,settings::ProviderKeys};
use std::collections::BTreeMap;
fn parse(extra: &[(&str, &str)]) -> ai_platform_knowledge_service::Result<ProcessSettings> {
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
    ProcessSettings::from_values(values, std::env::current_dir().unwrap())
}

#[test] fn former_provider_key_aliases_and_precedence_inputs_cannot_reenter_knowledge() {
    for keys in [vec![("DASHSCOPE_API_KEY","primary"),("Aliyun_KEY","legacy")],vec![("DASHSCOPE_API_KEY",""),("Aliyun_KEY","legacy")],vec![("LLM_API_KEY","generic"),("DASHSCOPE_API_KEY","primary")],vec![("DEEPSEEK_KEY","deepseek-fallback"),("GOOGLE_API_KEY","google-fallback")]] {assert!(parse(&keys).is_err());}
    assert_eq!(ProviderKeys::default().qa_key("custom"),"");
    assert_eq!(std::mem::size_of::<ProviderKeys>(),0);
}
#[test] fn each_snapshot_rejects_keys_without_retaining_old_values_and_preserves_model_selection() {
    assert!(parse(&[("GOOGLE_API_KEY","first")]).is_err());assert!(parse(&[("GOOGLE_API_KEY","second")]).is_err());
    assert!(parse(&[]).is_ok());
    assert_eq!(parse(&[("LLM_MODEL","qwen3.7-plus")]).unwrap().configuration.qa.unwrap().model,"qwen3.7-plus");
    assert_eq!(parse(&[("LLM_MODEL","")]).unwrap().configuration.qa.unwrap().model,"");
}
#[test] fn former_domain_endpoints_and_keys_require_gateway_registration() {
    for key in ["DASHSCOPE_API_KEY","DASHSCOPE_EMBEDDING_API_KEY","DASHSCOPE_OCR_API_KEY","DASHSCOPE_BASE_URL","DASHSCOPE_EMBEDDING_BASE_URL","KNOWLEDGE_EMBEDDINGS__DASHSCOPE_API_KEY"] {assert!(parse(&[(key,"synthetic-retired-setting")]).is_err(),"{key}");}
}
