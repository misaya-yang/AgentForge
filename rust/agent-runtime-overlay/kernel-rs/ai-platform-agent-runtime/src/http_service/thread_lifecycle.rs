use std::collections::HashMap;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadArchiveParams;
use codex_app_server_protocol::ThreadArchiveResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadUnarchiveParams;
use codex_app_server_protocol::ThreadUnarchiveResponse;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_protocol::ThreadId;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use tracing::warn;
use uuid::Uuid;

use super::RuntimeHttpState;
use super::security::RuntimeError;
use super::security::SESSION_HEADER;
use super::security::TENANT_HEADER;
use super::security::USER_HEADER;
use super::security::authorize;
use super::security::required_header;
use super::threads::apply_model_limits;
use crate::PlatformThreadIdentity;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EmptyLifecycleRequest {}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ResumeThreadRequest {
    model: Option<String>,
    model_plane_base_url: Option<String>,
    model_context_window: Option<i64>,
    auto_compact_token_limit: Option<i64>,
    /// Preserve the model profile's hosted Responses search capability when
    /// reloading a Thread. This is profile data from Gateway, never inferred
    /// from the model id or user prompt.
    #[serde(default)]
    native_web_search_enabled: Option<bool>,
    base_instructions: Option<String>,
    developer_instructions: Option<String>,
    tool_policy: Option<crate::tool_policy::PlatformToolPolicy>,
}

pub(super) async fn resume_thread(
    State(state): State<RuntimeHttpState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Json(mut body): Json<ResumeThreadRequest>,
) -> Result<Json<ThreadResumeResponse>, RuntimeError> {
    let thread_id = authorize_thread_scope(&state, &headers, &thread_id).await?;
    let _thread_guard = state
        .thread_gate(thread_id)
        .try_lock_owned()
        .map_err(|_| RuntimeError::conflict("runtime_turn_recovering"))?;
    let foreign_owner:bool=sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM assistant_runtime_execution_owners o JOIN assistant_runs r ON r.run_id=o.run_id WHERE o.runtime_thread_id=$1 AND o.owner_id<>$2 AND r.status IN ('running','awaiting_approval'))")
        .bind(Uuid::parse_str(&thread_id.to_string()).map_err(|_|RuntimeError::bad_request("invalid_thread_id"))?).bind(state.store.instance_id)
        .fetch_one(&state.store.pool).await.map_err(|_|RuntimeError::unavailable("runtime_recovery_store_unavailable"))?;
    if foreign_owner
        || state
            .recovering_threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&thread_id)
    {
        return Err(RuntimeError::conflict("runtime_turn_recovering"));
    }
    let tool_policy = state
        .store
        .resolve_tool_policy(thread_id, body.tool_policy.take())
        .await
        .map_err(RuntimeError::from_store)?;
    body.native_web_search_enabled = body
        .native_web_search_enabled
        .map(|enabled| enabled && tool_policy.allows_native_search());
    let params = resume_params(thread_id, body)?;
    let result = state
        .requests
        .request_thread_resume(
            ClientRequest::ThreadResume {
                request_id: lifecycle_request_id("thread-resume", thread_id),
                params,
            },
            codex_app_server::host_runtime::AppServerThreadResumeOptions::new(
                tool_policy.kernel_policy(),
            ),
        )
        .await
        .map_err(|_| RuntimeError::unavailable("agent_kernel_unavailable"))?
        .map_err(|_| RuntimeError::bad_request("agent_thread_lifecycle_rejected"))?;
    serde_json::from_value(result)
        .map(Json)
        .map_err(|_| RuntimeError::internal("invalid_agent_thread_resume_response"))
}

/// Interrupts one active turn after checking the platform-owned thread
/// identity. The local capability handler is cancelled before the typed
/// request is forwarded so an interrupted turn cannot leave a Worker call
/// waiting past the kernel's terminal/interrupted item.
pub(super) async fn interrupt_turn(
    State(state): State<RuntimeHttpState>,
    Path((thread_id, turn_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(_body): Json<EmptyLifecycleRequest>,
) -> Result<Json<TurnInterruptResponse>, RuntimeError> {
    let thread_id = authorize_thread_scope(&state, &headers, &thread_id).await?;
    if turn_id.is_empty() || turn_id.len() > 255 {
        return Err(RuntimeError::bad_request("invalid_turn_id"));
    }
    let belongs_to_thread: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM assistant_runtime_snapshots WHERE runtime_thread_id=$1 AND run_id::text=$2)",
    )
    .bind(Uuid::parse_str(&thread_id.to_string()).map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?)
    .bind(&turn_id)
    .fetch_one(&state.store.pool)
    .await
    .map_err(|_| RuntimeError::unavailable("runtime_lease_store_unavailable"))?;
    if !belongs_to_thread {
        return Err(RuntimeError::not_found("turn_not_found"));
    }
    let run_id =
        Uuid::parse_str(&turn_id).map_err(|_| RuntimeError::bad_request("invalid_turn_id"))?;
    let mut transaction = state
        .store
        .pool
        .begin()
        .await
        .map_err(|_| RuntimeError::unavailable("runtime_cancel_store_unavailable"))?;
    let cancelled: bool = sqlx::query_scalar("SELECT cancel_runtime_execution($1,$2,$3,$4,$5)")
        .bind(run_id)
        .bind(
            Uuid::parse_str(&thread_id.to_string())
                .map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?,
        )
        .bind(required_header(&headers, TENANT_HEADER)?)
        .bind(required_header(&headers, USER_HEADER)?)
        .bind(required_header(&headers, SESSION_HEADER)?)
        .fetch_one(&mut *transaction)
        .await
        .map_err(|_| RuntimeError::unavailable("runtime_cancel_store_unavailable"))?;
    let saved = sqlx::query("SELECT status,EXTRACT(EPOCH FROM COALESCE(finished_at,updated_at))::double precision AS finished FROM assistant_runs WHERE run_id=$1")
        .bind(run_id).fetch_one(&mut *transaction).await.map_err(|_| RuntimeError::unavailable("runtime_cancel_store_unavailable"))?;
    if saved.try_get::<String, _>("status").ok().as_deref() == Some("cancelled") {
        let event = crate::AssistantTurnEventV1 {
            schema_version: "assistant-turn-contract/v1".to_string(),
            event_type: "cancelled".to_string(),
            data: json!({"run_id":run_id,"thread_id":thread_id,"session_id":required_header(&headers,SESSION_HEADER)?,"status":"cancelled","terminal_reason":"client_interrupt"}),
            timestamp: saved
                .try_get("finished")
                .map_err(|_| RuntimeError::unavailable("runtime_cancel_store_unavailable"))?,
        };
        let payload = serde_json::to_value(event)
            .map_err(|_| RuntimeError::internal("runtime_cancel_event_invalid"))?;
        let payload_hash =
            crate::postgres_store::payload_hash(&payload).map_err(RuntimeError::from_store)?;
        let event_key = format!("compat/control/{run_id}/cancelled");
        let digest = Sha256::digest(event_key.as_bytes());
        let mut event_bytes = [0u8; 16];
        event_bytes.copy_from_slice(&digest[..16]);
        let event_id = Uuid::from_bytes(event_bytes);
        sqlx::query("SELECT append_assistant_runtime_item($1,$1,$2,$3,$4,$5,$6,$7,NULL,'compat/v1/cancelled','assistant_turn_event_v1','cancelled',$8,$9)")
            .bind(Uuid::parse_str(&thread_id.to_string()).map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?)
            .bind(required_header(&headers,TENANT_HEADER)?).bind(required_header(&headers,USER_HEADER)?).bind(required_header(&headers,SESSION_HEADER)?)
            .bind(event_id).bind(event_key).bind(&turn_id).bind(payload).bind(payload_hash)
            .execute(&mut *transaction).await.map_err(|_| RuntimeError::unavailable("runtime_cancel_receipt_unavailable"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| RuntimeError::unavailable("runtime_cancel_store_unavailable"))?;
    state.cancel_turn(&turn_id);
    let locally_owned = Uuid::parse_str(&turn_id)
        .ok()
        .and_then(|run| state.store.execution_claim(run))
        .is_some();
    let recovering = state
        .recovering_threads
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&thread_id);
    if cancelled && locally_owned && !recovering {
        // Durable intent is already authoritative; a process-local interrupt
        // notification may be unavailable during a concurrent Core shutdown.
        let _ = request_typed::<TurnInterruptResponse>(
            &state,
            ClientRequest::TurnInterrupt {
                request_id: RequestId::String(format!("turn-interrupt-{thread_id}-{turn_id}")),
                params: TurnInterruptParams {
                    thread_id: thread_id.to_string(),
                    turn_id,
                },
            },
            "invalid_agent_turn_interrupt_response",
        )
        .await;
    }
    serde_json::from_value(json!({}))
        .map(Json)
        .map_err(|_| RuntimeError::internal("invalid_agent_turn_interrupt_response"))
}

pub(super) fn resume_params(
    thread_id: ThreadId,
    body: ResumeThreadRequest,
) -> Result<ThreadResumeParams, RuntimeError> {
    let ResumeThreadRequest {
        model,
        model_plane_base_url,
        model_context_window,
        auto_compact_token_limit,
        native_web_search_enabled,
        base_instructions,
        developer_instructions,
        tool_policy: _,
    } = body;
    validate_instructions(base_instructions.as_deref(), "base_instructions")?;
    validate_instructions(developer_instructions.as_deref(), "developer_instructions")?;
    let (model, model_plane_base_url) = match (model, model_plane_base_url) {
        (None, None)
            if model_context_window.is_none()
                && auto_compact_token_limit.is_none()
                && base_instructions.is_none()
                && developer_instructions.is_none() =>
        {
            return Ok(ThreadResumeParams {
                thread_id: thread_id.to_string(),
                ..Default::default()
            });
        }
        (None, None) => return Err(RuntimeError::bad_request("invalid_thread_resume_config")),
        (Some(model), Some(model_plane_base_url)) => (model, model_plane_base_url),
        _ => return Err(RuntimeError::bad_request("invalid_thread_resume_config")),
    };
    if model.is_empty()
        || model.len() > 255
        || model_plane_base_url.len() > 2048
        || !valid_model_plane_base_url(&model_plane_base_url)
    {
        return Err(RuntimeError::bad_request("invalid_thread_resume_config"));
    }
    let provider_id = "ai-platform-gateway";
    let mut config = HashMap::<String, Value>::new();
    config.insert("model_provider".to_string(), provider_id.into());
    config.insert("model".to_string(), model.clone().into());
    config.insert(
        "model_providers".to_string(),
        json!({
            (provider_id): {
                "name": "AI Platform Gateway Model Plane",
                "base_url": model_plane_base_url,
                "env_key": "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
                "wire_api": "responses",
                "requires_openai_auth": false,
                "supports_websockets": false,
                "request_max_retries": 0,
                "stream_max_retries": 0,
            }
        }),
    );
    if let Some(enabled) = native_web_search_enabled {
        config.insert(
            "web_search".to_string(),
            if enabled {
                "live".into()
            } else {
                "disabled".into()
            },
        );
    }
    config.insert(
        "features".to_string(),
        json!({
            "standalone_web_search": false,
            "multi_agent_v2": {
                "enabled": true,
                "max_concurrent_threads_per_session": 6,
            },
        }),
    );
    crate::tool_policy::apply_managed_feature_profile(&mut config);
    let mut params = ThreadResumeParams {
        thread_id: thread_id.to_string(),
        model: Some(model),
        model_provider: Some(provider_id.to_string()),
        config: Some(config),
        base_instructions,
        developer_instructions,
        ..Default::default()
    };
    let mut limit_start = ThreadStartParams {
        config: params.config.take(),
        ..Default::default()
    };
    apply_model_limits(
        &mut limit_start,
        model_context_window,
        auto_compact_token_limit,
    )?;
    params.config = limit_start.config;
    Ok(params)
}

fn validate_instructions(value: Option<&str>, field: &str) -> Result<(), RuntimeError> {
    if value.is_some_and(|value| value.trim().is_empty() || value.len() > 256 * 1024) {
        return Err(RuntimeError::bad_request(if field == "base_instructions" {
            "invalid_base_instructions"
        } else {
            "invalid_developer_instructions"
        }));
    }
    Ok(())
}

pub(super) fn valid_model_plane_base_url(value: &str) -> bool {
    let remainder = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"));
    let Some((authority, path)) = remainder.and_then(|value| value.split_once('/')) else {
        return false;
    };
    !authority.is_empty()
        && !authority.contains('@')
        && !authority.chars().any(char::is_whitespace)
        && path == "internal/v1/agent-model-plane"
}

#[cfg(test)]
#[path = "thread_lifecycle_tests.rs"]
mod tests;

pub(super) async fn archive_thread(
    State(state): State<RuntimeHttpState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Json(_body): Json<EmptyLifecycleRequest>,
) -> Result<Json<ThreadArchiveResponse>, RuntimeError> {
    let thread_id = authorize_thread_scope(&state, &headers, &thread_id).await?;
    request_typed(
        &state,
        ClientRequest::ThreadArchive {
            request_id: lifecycle_request_id("thread-archive", thread_id),
            params: ThreadArchiveParams {
                thread_id: thread_id.to_string(),
            },
        },
        "invalid_agent_thread_archive_response",
    )
    .await
}

pub(super) async fn unarchive_thread(
    State(state): State<RuntimeHttpState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Json(_body): Json<EmptyLifecycleRequest>,
) -> Result<Json<ThreadUnarchiveResponse>, RuntimeError> {
    let thread_id = authorize_thread_scope(&state, &headers, &thread_id).await?;
    request_typed(
        &state,
        ClientRequest::ThreadUnarchive {
            request_id: lifecycle_request_id("thread-unarchive", thread_id),
            params: ThreadUnarchiveParams {
                thread_id: thread_id.to_string(),
            },
        },
        "invalid_agent_thread_unarchive_response",
    )
    .await
}

pub(super) async fn authorize_thread_scope(
    state: &RuntimeHttpState,
    headers: &HeaderMap,
    thread_id: &str,
) -> Result<ThreadId, RuntimeError> {
    authorize(headers, &state.internal_token)?;
    let thread_id = ThreadId::from_string(thread_id)
        .map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?;
    let identity = PlatformThreadIdentity::new(
        thread_id,
        required_header(headers, TENANT_HEADER)?,
        required_header(headers, USER_HEADER)?,
        required_header(headers, SESSION_HEADER)?,
    );
    state
        .store
        .verify_root_identity(&identity)
        .await
        .map_err(|_| RuntimeError::not_found("thread_not_found"))?;
    Ok(thread_id)
}

async fn request_typed<T>(
    state: &RuntimeHttpState,
    request: ClientRequest,
    invalid_response_code: &'static str,
) -> Result<Json<T>, RuntimeError>
where
    T: serde::de::DeserializeOwned,
{
    let result = state
        .requests
        .request(request)
        .await
        .map_err(|_| RuntimeError::unavailable("agent_kernel_unavailable"))?
        .map_err(|error| {
            warn!(
                provider_code = error.code,
                provider_message = %error.message,
                "Agent rejected a platform thread lifecycle request"
            );
            RuntimeError::bad_request("agent_thread_lifecycle_rejected")
        })?;
    serde_json::from_value(result)
        .map(Json)
        .map_err(|_| RuntimeError::internal(invalid_response_code))
}

fn lifecycle_request_id(operation: &str, thread_id: ThreadId) -> RequestId {
    RequestId::String(format!("{operation}-{thread_id}-{}", Uuid::now_v7()))
}
