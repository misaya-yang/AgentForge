use codex_app_server_protocol::ThreadResumeParams;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;

use super::ResumeThreadRequest;
use super::resume_params;

#[test]
fn empty_resume_keeps_the_upstream_persisted_metadata_path() {
    let thread_id = ThreadId::new();

    let actual = resume_params(thread_id, ResumeThreadRequest::default())
        .unwrap_or_else(|_| panic!("valid resume"));

    assert_eq!(
        actual,
        ThreadResumeParams {
            thread_id: thread_id.to_string(),
            ..Default::default()
        }
    );
}

#[test]
fn platform_resume_reinstates_the_private_responses_provider() {
    let thread_id = ThreadId::new();

    let actual = resume_params(
        thread_id,
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some(
                "http://gateway:8080/internal/v1/agent-model-plane".to_string(),
            ),
            ..Default::default()
        },
    )
    .unwrap_or_else(|_| panic!("valid platform resume"));

    assert_eq!(actual.thread_id, thread_id.to_string());
    assert_eq!(actual.model.as_deref(), Some("qwen3.7-plus"));
    assert_eq!(
        actual.model_provider.as_deref(),
        Some("ai-platform-gateway")
    );
    let config = actual.config.expect("platform config");
    assert_eq!(config["model"], "qwen3.7-plus");
    assert_eq!(config["model_provider"], "ai-platform-gateway");
    assert_eq!(
        config["model_providers"]["ai-platform-gateway"]["base_url"],
        "http://gateway:8080/internal/v1/agent-model-plane"
    );
    assert_eq!(
        config["model_providers"]["ai-platform-gateway"]["env_key"],
        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN"
    );
    assert_eq!(
        config["model_providers"]["ai-platform-gateway"]["supports_websockets"],
        false
    );
    assert!(!config.contains_key("web_search"));
    assert_eq!(config["features"]["standalone_web_search"], false);
    assert_eq!(config["features"]["multi_agent_v2"]["enabled"], true);
}

#[test]
fn platform_resume_preserves_profile_declared_native_web_search() {
    let actual = resume_params(
        ThreadId::new(),
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some(
                "http://gateway:8080/internal/v1/agent-model-plane".to_string(),
            ),
            native_web_search_enabled: Some(true),
            ..Default::default()
        },
    )
    .unwrap_or_else(|_| panic!("valid platform resume"));
    let config = actual.config.expect("platform config");
    assert_eq!(config["web_search"], "live");
    assert_eq!(config["features"]["standalone_web_search"], false);
}

#[test]
fn incomplete_or_untrusted_platform_resume_config_is_rejected() {
    let thread_id = ThreadId::new();
    for request in [
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: None,
            ..Default::default()
        },
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some(
                "https://user@example.test/internal/v1/agent-model-plane".to_string(),
            ),
            ..Default::default()
        },
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some("https://example.test/v1/responses".to_string()),
            ..Default::default()
        },
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some("http:///internal/v1/agent-model-plane".to_string()),
            ..Default::default()
        },
    ] {
        assert!(resume_params(thread_id, request).is_err());
    }
}

#[test]
fn platform_resume_carries_model_context_and_compaction_limits() {
    let result = resume_params(
        ThreadId::new(),
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some(
                "http://gateway:8080/internal/v1/agent-model-plane".to_string(),
            ),
            model_context_window: Some(1_000_000),
            auto_compact_token_limit: Some(900_000),
            ..Default::default()
        },
    );
    assert!(result.is_ok(), "valid platform resume");
    let actual = match result {
        Ok(value) => value,
        Err(_) => unreachable!("checked above"),
    };
    let config = actual.config.expect("platform config");
    assert_eq!(config["model_context_window"], 1_000_000);
    assert_eq!(config["model_auto_compact_token_limit"], 900_000);
}

#[test]
fn platform_resume_rebinds_stable_system_and_developer_instructions() {
    let result = resume_params(
        ThreadId::new(),
        ResumeThreadRequest {
            model: Some("qwen3.7-plus".to_string()),
            model_plane_base_url: Some(
                "http://gateway:8080/internal/v1/agent-model-plane".to_string(),
            ),
            base_instructions: Some("platform system contract".to_string()),
            developer_instructions: Some("platform developer contract".to_string()),
            ..Default::default()
        },
    );
    assert!(result.is_ok(), "valid platform resume");
    let actual = match result {
        Ok(value) => value,
        Err(_) => unreachable!("checked above"),
    };
    assert_eq!(
        actual.base_instructions.as_deref(),
        Some("platform system contract")
    );
    assert_eq!(
        actual.developer_instructions.as_deref(),
        Some("platform developer contract")
    );
    assert!(
        resume_params(
            ThreadId::new(),
            ResumeThreadRequest {
                model: Some("qwen3.7-plus".to_string()),
                model_plane_base_url: Some(
                    "http://gateway:8080/internal/v1/agent-model-plane".to_string(),
                ),
                developer_instructions: Some("   ".to_string()),
                ..Default::default()
            }
        )
        .is_err()
    );
}
