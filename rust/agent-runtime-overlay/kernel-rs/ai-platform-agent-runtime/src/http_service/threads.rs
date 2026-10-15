//! Thread lifecycle entry points: creation, session cleanup, and the
//! model-limit/memory-mode validation that guards them.

use std::collections::HashMap;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadMemoryMode;
use codex_app_server_protocol::ThreadMemoryModeSetParams;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_protocol::ThreadId;
use serde::Deserialize;
use serde::Serialize;

use super::RuntimeHttpState;
use super::security::RuntimeError;
use super::security::SESSION_HEADER;
use super::security::TENANT_HEADER;
use super::security::USER_HEADER;
use super::security::authorize;
use super::security::required_header;
use crate::PlatformThreadIdentity;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CreateThreadRequest {
    tenant_id: String,
    user_id: String,
    session_id: String,
    #[serde(default)]
    start: ThreadStartParams,
    /// Platform-selected memory behavior for this thread. Applied through
    /// the kernel's native thread/memoryMode/set request after the durable
    /// root identity is reserved.
    memory_mode: Option<ThreadMemoryMode>,
    tool_policy: Option<crate::tool_policy::PlatformToolPolicy>,
    /// Model capability data from the immutable platform snapshot. These are
    /// converted to the kernel's config keys before thread creation so child
    /// threads inherit the same context/compaction policy.
    model_context_window: Option<i64>,
    auto_compact_token_limit: Option<i64>,
}

pub(super) async fn create_thread(
    State(state): State<RuntimeHttpState>,
    headers: HeaderMap,
    Json(body): Json<CreateThreadRequest>,
) -> Result<Json<ThreadStartResponse>, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    if body.start.ephemeral == Some(true) {
        return Err(RuntimeError::bad_request("ephemeral_thread_not_supported"));
    }
    let tool_policy = body
        .tool_policy
        .ok_or_else(|| RuntimeError::bad_request("platform_tool_policy_required"))?
        .normalized()
        .map_err(RuntimeError::from_store)?;
    let mut start = body.start;
    let config = start.config.get_or_insert_with(HashMap::new);
    crate::tool_policy::apply_managed_feature_profile(config);
    if !tool_policy.allows_native_search() {
        config.insert("web_search".to_string(), serde_json::json!("disabled"));
    }
    apply_model_limits(
        &mut start,
        body.model_context_window,
        body.auto_compact_token_limit,
    )?;
    let memory_mode = body.memory_mode;
    validate_memory_mode(memory_mode)?;
    let root_thread_id = ThreadId::new();
    state
        .store
        .authorize_root(&PlatformThreadIdentity::new(
            root_thread_id,
            body.tenant_id,
            body.user_id,
            body.session_id,
        ))
        .await
        .map_err(RuntimeError::from_store)?;
    state
        .store
        .stage_host_startup(
            root_thread_id,
            tool_policy.clone(),
            start.dynamic_tools.clone().unwrap_or_default(),
        )
        .await
        .map_err(RuntimeError::from_store)?;
    let result = state
        .requests
        .request_thread_start(
            ClientRequest::ThreadStart {
                request_id: RequestId::String(format!("thread-start-{root_thread_id}")),
                params: start,
            },
            codex_app_server::host_runtime::AppServerThreadStartOptions::new(root_thread_id)
                .with_tool_policy(tool_policy.kernel_policy()),
        )
        .await;
    if !matches!(&result, Ok(Ok(_))) {
        state.store.clear_host_startup(root_thread_id).await;
    }
    let result = result
        .map_err(|_| RuntimeError::unavailable("agent_kernel_unavailable"))?
        .map_err(|_| RuntimeError::bad_request("agent_thread_start_rejected"))?;
    state
        .store
        .resolve_tool_policy(root_thread_id, Some(tool_policy))
        .await
        .map_err(RuntimeError::from_store)?;
    if let Some(mode) = memory_mode {
        state
            .requests
            .request(ClientRequest::ThreadMemoryModeSet {
                request_id: RequestId::String(format!("thread-memory-mode-{root_thread_id}")),
                params: ThreadMemoryModeSetParams {
                    thread_id: root_thread_id.to_string(),
                    mode,
                },
            })
            .await
            .map_err(|_| RuntimeError::unavailable("agent_kernel_unavailable"))?
            .map_err(|_| RuntimeError::bad_request("agent_thread_memory_mode_rejected"))?;
    }
    serde_json::from_value(result)
        .map(Json)
        .map_err(|_| RuntimeError::internal("invalid_agent_thread_start_response"))
}

#[derive(Serialize)]
pub(super) struct SessionCleanupResponse {
    session_id: String,
    status: &'static str,
}

/// Tombstone the Runtime projection and root identity for one platform
/// session. Items remain append-only for audit/recovery, and scope is bound
/// both to the authenticated headers and the path session id.
pub(super) async fn cleanup_session(
    State(state): State<RuntimeHttpState>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<SessionCleanupResponse>, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    if session_id.is_empty()
        || session_id.len() > 255
        || session_id.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(RuntimeError::bad_request("invalid_session_id"));
    }
    let tenant_id = required_header(&headers, TENANT_HEADER)?;
    let user_id = required_header(&headers, USER_HEADER)?;
    let header_session_id = required_header(&headers, SESSION_HEADER)?;
    if [tenant_id.as_str(), user_id.as_str()]
        .into_iter()
        .any(|value| value.len() > 255 || value.bytes().any(|byte| byte.is_ascii_control()))
    {
        return Err(RuntimeError::bad_request("invalid_runtime_scope"));
    }
    if header_session_id != session_id {
        return Err(RuntimeError::not_found("session_not_found"));
    }
    let deleted = state
        .store
        .cleanup_session(&tenant_id, &user_id, &session_id)
        .await
        .map_err(RuntimeError::from_store)?;
    Ok(Json(SessionCleanupResponse {
        session_id,
        status: if deleted { "deleted" } else { "not_found" },
    }))
}

pub(super) fn validate_memory_mode(mode: Option<ThreadMemoryMode>) -> Result<(), RuntimeError> {
    if matches!(mode, Some(ThreadMemoryMode::Enabled)) {
        // The upstream memory generator persists into a process-local SQLite
        // home. This service is multi-tenant, so accepting `enabled` here
        // would create an unscoped cross-tenant memory store. Until the
        // platform memory contributor is wired to tenant-scoped storage,
        // fail closed rather than silently enabling it.
        return Err(RuntimeError::bad_request(
            "agent_memory_backend_unavailable",
        ));
    }
    Ok(())
}

/// Merge model capability limits into the official thread config map. The
/// platform sends camelCase fields while the kernel config loader owns the
/// snake_case keys. Conflicting values are rejected rather than silently
/// choosing one source of truth.
pub(super) fn apply_model_limits(
    start: &mut ThreadStartParams,
    model_context_window: Option<i64>,
    auto_compact_token_limit: Option<i64>,
) -> Result<(), RuntimeError> {
    if model_context_window.is_none() && auto_compact_token_limit.is_none() {
        return Ok(());
    }
    let context_window = model_context_window.ok_or_else(|| {
        RuntimeError::bad_request("model_context_window_required_for_compaction_policy")
    })?;
    if !(1..=10_000_000).contains(&context_window) {
        return Err(RuntimeError::bad_request("invalid_model_context_window"));
    }
    let compact_limit = auto_compact_token_limit.unwrap_or((context_window * 9) / 10);
    if !(1..=context_window).contains(&compact_limit) {
        return Err(RuntimeError::bad_request(
            "invalid_auto_compact_token_limit",
        ));
    }
    let config = start.config.get_or_insert_with(HashMap::new);
    insert_matching_config(config, "model_context_window", context_window)?;
    insert_matching_config(config, "model_auto_compact_token_limit", compact_limit)?;
    Ok(())
}

fn insert_matching_config(
    config: &mut HashMap<String, serde_json::Value>,
    key: &str,
    value: i64,
) -> Result<(), RuntimeError> {
    let json_value = serde_json::Value::from(value);
    if let Some(existing) = config.get(key) {
        if existing != &json_value {
            return Err(RuntimeError::bad_request(
                "conflicting_model_runtime_config",
            ));
        }
    } else {
        config.insert(key.to_string(), json_value);
    }
    Ok(())
}
