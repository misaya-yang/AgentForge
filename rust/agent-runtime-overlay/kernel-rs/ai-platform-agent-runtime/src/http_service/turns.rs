//! Turn start: lease verification, readonly capability payload validation,
//! and the kernel TurnStart dispatch.

use std::collections::HashMap;
use std::time::Instant;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::HeaderMap;
use codex_app_server_protocol::AdditionalContextEntry;
use codex_app_server_protocol::AdditionalContextKind;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ImageReference;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use codex_protocol::openai_models::ReasoningEffort;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use sha2::Sha256;
use uuid::Uuid;

use super::READONLY_TURN_MAX_ENTRIES;
use super::READONLY_TURN_TTL;
use super::ReadonlyTurnBinding;
use super::RuntimeHttpState;
use super::security::RuntimeError;
use super::security::SESSION_HEADER;
use super::security::TENANT_HEADER;
use super::security::USER_HEADER;
use super::security::required_header;
use super::thread_lifecycle::authorize_thread_scope;
use crate::readonly_capabilities::RuntimeCapabilityScope;
use crate::readonly_capabilities::render_turn_input;
use crate::readonly_capabilities::validate_platform_config;

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StartTurnRequest {
    pub(super) run_id: Uuid,
    pub(super) snapshot_id: Uuid,
    pub(super) lease_id: Uuid,
    pub(super) lease_signature: String,
    #[serde(default)]
    pub(super) capability_ceiling:
        Option<ai_platform_capability_contract::lease_v3::SignedEnvelope>,
    pub(super) message: String,
    #[serde(default)]
    pub(super) images: Vec<String>,
    pub(super) model: String,
    pub(super) effort: Option<String>,
    pub(super) capability_revision: i64,
    #[serde(default)]
    pub(super) readonly: Option<serde_json::Value>,
    #[serde(default)]
    pub(super) platform_config: Option<serde_json::Value>,
}

pub(super) async fn start_turn(
    State(state): State<RuntimeHttpState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<StartTurnRequest>,
) -> Result<Json<TurnStartResponse>, RuntimeError> {
    start_turn_mode(State(state), Path(thread_id), headers, Json(body), false).await
}

pub(super) async fn start_turn_mode(
    State(state): State<RuntimeHttpState>,
    Path(thread_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<StartTurnRequest>,
    recovery: bool,
) -> Result<Json<TurnStartResponse>, RuntimeError> {
    let thread_id = authorize_thread_scope(&state, &headers, &thread_id).await?;
    state
        .store
        .check_write_health(thread_id)
        .map_err(RuntimeError::from_store)?;
    validate_start_turn_request(&body)?;
    let tenant_id = required_header(&headers, TENANT_HEADER)?;
    let user_id = required_header(&headers, USER_HEADER)?;
    let session_id = required_header(&headers, SESSION_HEADER)?;
    super::lease_verification::verify_turn(
        &body.lease_signature,
        body.capability_ceiling.as_ref(),
        &crate::PlatformThreadIdentity::new(
            thread_id,
            tenant_id.clone(),
            user_id.clone(),
            session_id.clone(),
        ),
        &body.run_id.to_string(),
        &body.snapshot_id.to_string(),
        &body.lease_id.to_string(),
        body.capability_revision as u64,
        &body.model,
    )
    .map_err(|_| RuntimeError::bad_request("runtime_lease_signature_not_admitted"))?;
    let trace_context = crate::trace_context::InternalTraceContext::from_headers(&headers);
    let readonly_input = body
        .readonly
        .as_ref()
        .map(|payload| {
            render_turn_input(
                &RuntimeCapabilityScope {
                    tenant_id: tenant_id.clone(),
                    user_id: user_id.clone(),
                    session_id: session_id.clone(),
                    capability_revision: body.capability_revision,
                    snapshot_id: body.snapshot_id.to_string(),
                },
                payload,
            )
        })
        .transpose()
        .map_err(|_| RuntimeError::bad_request("invalid_readonly_capability_payload"))?;
    if let Some(platform_config) = body.platform_config.as_ref() {
        validate_platform_config(
            &RuntimeCapabilityScope {
                tenant_id: tenant_id.clone(),
                user_id: user_id.clone(),
                session_id: session_id.clone(),
                capability_revision: body.capability_revision,
                snapshot_id: body.snapshot_id.to_string(),
            },
            platform_config,
        )
        .map_err(|_| RuntimeError::bad_request("invalid_platform_runtime_config"))?;
        if body
            .readonly
            .as_ref()
            .and_then(|value| value.get("platform_config"))
            != Some(platform_config)
        {
            return Err(RuntimeError::bad_request(
                "platform_runtime_config_mismatch",
            ));
        }
    } else {
        return Err(RuntimeError::bad_request("platform_runtime_config_missing"));
    }
    let authorized = sqlx::query_scalar::<_, Option<chrono::DateTime<chrono::Utc>>>(
        r#"
        SELECT MAX(LEAST(l.expires_at,COALESCE(s.expires_at,l.expires_at)))
              FROM assistant_runtime_model_leases AS l
              JOIN assistant_runtime_snapshots AS s
                ON s.snapshot_id = l.snapshot_id
               AND s.run_id = l.run_id
               AND s.tenant_id = l.tenant_id
               AND s.user_id = l.user_id
               AND s.session_id = l.session_id
              JOIN assistant_runs AS r
                ON r.run_id = l.run_id
             WHERE l.lease_id = $1
               AND l.snapshot_id = $2
               AND l.run_id = $3
               AND l.runtime_thread_id = $4
               AND l.tenant_id = $5
               AND l.user_id = $6
               AND l.session_id = $7
               AND l.model_id = $8
               AND s.capability_revision = $9
               AND l.status = 'active'
               AND l.expires_at > NOW()
               AND r.status = 'running'
               AND r.engine = 'agent_runtime'
               AND NOT EXISTS (
                   SELECT 1
                     FROM assistant_runtime_snapshot_revocations AS rev
                    WHERE rev.snapshot_id = l.snapshot_id
               )
        "#,
    )
    .bind(body.lease_id)
    .bind(body.snapshot_id)
    .bind(body.run_id)
    .bind(
        Uuid::parse_str(&thread_id.to_string())
            .map_err(|_| RuntimeError::bad_request("invalid_thread_id"))?,
    )
    .bind(&tenant_id)
    .bind(&user_id)
    .bind(&session_id)
    .bind(&body.model)
    .bind(body.capability_revision)
    .fetch_one(&state.store.pool)
    .await
    .map_err(|_| RuntimeError::unavailable("runtime_lease_store_unavailable"))?;
    let lease_expiry =
        authorized.ok_or_else(|| RuntimeError::not_found("runtime_turn_lease_not_found"))?;
    let transport_budget = super::transport_budget::admit_authenticated(
        &headers,
        super::transport_budget::ADMISSION_CAP,
        Some(lease_expiry.timestamp_millis().max(0) as u64),
    )
    .map_err(|_| RuntimeError::bad_request("runtime_transport_budget_invalid"))?;
    if !recovery && state.store.execution_claim(body.run_id).is_none() {
        state
            .store
            .claim_execution(
                body.run_id,
                Some(
                    super::execution_context::encode(&body, &transport_budget)
                        .map_err(RuntimeError::bad_request)?,
                ),
            )
            .await
            .map_err(RuntimeError::from_store)?;
    }
    let execution_claim = state
        .store
        .assert_execution(body.run_id)
        .await
        .map_err(RuntimeError::from_store)?;
    if let Some(payload) = body.readonly.clone() {
        let mut bindings = state
            .readonly_by_turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        bindings.retain(|_, binding| now.duration_since(binding.created_at) < READONLY_TURN_TTL);
        if bindings.len() >= READONLY_TURN_MAX_ENTRIES
            && let Some(oldest) = bindings
                .iter()
                .min_by_key(|(_, binding)| binding.created_at)
                .map(|(key, _)| key.clone())
        {
            bindings.remove(&oldest);
        }
        bindings.insert(
            body.run_id.to_string(),
            ReadonlyTurnBinding {
                snapshot_id: body.snapshot_id.to_string(),
                capability_revision: body.capability_revision,
                payload,
                trace_context: trace_context.clone(),
                transport_budget: transport_budget.clone(),
                created_at: now,
            },
        );
    }

    let mut metadata = std::collections::HashMap::new();
    metadata.insert(
        "ai_platform_execution_owner".to_string(),
        execution_claim.owner_id.to_string(),
    );
    metadata.insert(
        "ai_platform_execution_fence".to_string(),
        execution_claim.fence.to_string(),
    );
    metadata.insert(
        "ai_platform_lease_id".to_string(),
        body.lease_id.to_string(),
    );
    metadata.insert(
        "ai_platform_lease_signature".to_string(),
        body.lease_signature,
    );
    metadata.insert(
        "ai_platform_scope_sha256".to_string(),
        runtime_scope_sha256(&tenant_id, &user_id, &session_id),
    );
    trace_context.extend_model_metadata(&mut metadata, &body.run_id.to_string());
    let mut turn_telemetry = crate::telemetry_model::start_turn_for_tenant(
        &mut metadata,
        &body.run_id.to_string(),
        &tenant_id,
    );
    let mut input = Vec::new();
    if !recovery {
        input.push(UserInput::Text {
            text: body.message,
            text_elements: Vec::new(),
        });
        input.extend(body.images.into_iter().map(|image_url| UserInput::Image {
            image: ImageReference::Inline { url: image_url },
            detail: None,
        }));
    }
    let additional_context = readonly_input.flatten().map(|value| {
        HashMap::from([(
            "ai_platform_readonly".to_string(),
            AdditionalContextEntry {
                value,
                kind: AdditionalContextKind::Untrusted,
            },
        )])
    });
    let params = TurnStartParams {
        thread_id: thread_id.to_string(),
        input,
        additional_context,
        responsesapi_client_metadata: Some(metadata),
        model: Some(body.model),
        effort: body
            .effort
            .as_deref()
            .map(parse_reasoning_effort)
            .transpose()?,
        ..Default::default()
    };
    let result = state
        .requests
        .request_turn_start(
            ClientRequest::TurnStart {
                request_id: RequestId::String(format!("turn-start-{}", body.run_id)),
                params,
            },
            if recovery {
                codex_app_server::host_runtime::AppServerTurnStartOptions::recovery(
                    body.run_id.to_string(),
                )
            } else {
                codex_app_server::host_runtime::AppServerTurnStartOptions::new(
                    body.run_id.to_string(),
                )
            },
        )
        .await
        .map_err(|_| RuntimeError::unavailable("agent_kernel_unavailable"))?
        .map_err(|_| RuntimeError::bad_request("agent_turn_start_rejected"))?;
    turn_telemetry.admitted();
    let response: TurnStartResponse = serde_json::from_value(result)
        .map_err(|_| RuntimeError::internal("invalid_agent_turn_start_response"))?;
    if response.turn.id != body.run_id.to_string() {
        return Err(RuntimeError::internal("agent_turn_id_mismatch"));
    }
    Ok(Json(response))
}

fn validate_start_turn_request(body: &StartTurnRequest) -> Result<(), RuntimeError> {
    if body.message.is_empty()
        || body.message.len() > 1_000_000
        || body.model.is_empty()
        || body.model.len() > 255
        || body.capability_revision < 1
        || body.images.len() > 5
        || body.images.iter().map(String::len).sum::<usize>() > 15 * 1024 * 1024
        || body.images.iter().any(|image| !valid_inline_image(image))
        || (!body
            .lease_signature
            .starts_with(ai_platform_capability_contract::lease_v3::ENVELOPE_PREFIX)
            && (body.lease_signature.len() != 67
                || !body.lease_signature.starts_with("v1:")
                || !body.lease_signature[3..]
                    .bytes()
                    .all(|value| value.is_ascii_hexdigit() && !value.is_ascii_uppercase())))
        || body.lease_signature.len()
            > ai_platform_capability_contract::lease_v3::MAX_WIRE_BYTES + 32
    {
        return Err(RuntimeError::bad_request("invalid_turn_start_request"));
    }
    Ok(())
}

fn valid_inline_image(image: &str) -> bool {
    let Some((header, encoded)) = image.split_once(',') else {
        return false;
    };
    let magic = match header {
        "data:image/png;base64" => "iVBORw0KGgo",
        "data:image/jpeg;base64" => "/9j/",
        "data:image/gif;base64" => "R0lGOD",
        "data:image/webp;base64" => "UklGR",
        _ => return false,
    };
    encoded.len() >= magic.len()
        && encoded.len() <= 3 * 1024 * 1024
        && encoded.len() % 4 == 0
        && encoded.starts_with(magic)
        && encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'='))
}

fn parse_reasoning_effort(value: &str) -> Result<ReasoningEffort, RuntimeError> {
    match value {
        "none" => Ok(ReasoningEffort::None),
        "minimal" => Ok(ReasoningEffort::Minimal),
        "low" => Ok(ReasoningEffort::Low),
        "medium" => Ok(ReasoningEffort::Medium),
        "high" => Ok(ReasoningEffort::High),
        "xhigh" => Ok(ReasoningEffort::XHigh),
        "max" => Ok(ReasoningEffort::Max),
        "ultra" => Ok(ReasoningEffort::Ultra),
        _ => Err(RuntimeError::bad_request("invalid_reasoning_effort")),
    }
}

fn runtime_scope_sha256(tenant_id: &str, user_id: &str, session_id: &str) -> String {
    let mut digest = Sha256::new();
    for value in [tenant_id, user_id, session_id] {
        let bytes = value.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    let output = digest.finalize();
    format!("{output:x}")
}

#[cfg(test)]
mod turn_request_tests {
    use super::super::TextProjectionState;
    use super::super::capability_dispatch::capability_result_detail;
    use super::super::capability_dispatch::capability_status_name;
    use super::super::capability_dispatch::capability_worker_enabled;
    use super::super::projection::terminal_admission_failure_event;
    use super::super::threads::apply_model_limits;
    use super::super::threads::validate_memory_mode;
    use super::*;
    use crate::V1ProjectionContext;
    use crate::capability_execution::CapabilityExecutionOutcome;
    use ai_platform_capability_contract::CapabilityExecutionStatus;
    use codex_app_server_protocol::AgentMessageDeltaNotification;
    use codex_app_server_protocol::ItemCompletedNotification;
    use codex_app_server_protocol::ServerNotification;
    use codex_app_server_protocol::ThreadItem;
    use codex_app_server_protocol::ThreadMemoryMode;
    use codex_app_server_protocol::ThreadStartParams;
    use codex_app_server_protocol::Turn;
    use codex_app_server_protocol::TurnCompletedNotification;
    use codex_app_server_protocol::TurnItemsView;
    use codex_app_server_protocol::TurnStatus;
    use serde_json::json;

    fn request(signature: String) -> StartTurnRequest {
        StartTurnRequest {
            run_id: Uuid::nil(),
            snapshot_id: Uuid::nil(),
            lease_id: Uuid::nil(),
            lease_signature: signature,
            capability_ceiling: None,
            message: "hello".to_string(),
            images: Vec::new(),
            model: "qwen3.7-plus".to_string(),
            effort: Some("minimal".to_string()),
            capability_revision: 1,
            readonly: None,
            platform_config: None,
        }
    }

    #[test]
    fn signed_turn_request_contract_is_bounded() {
        let valid = request(format!("v1:{}", "a".repeat(64)));
        assert!(validate_start_turn_request(&valid).is_ok());
        assert!(parse_reasoning_effort("minimal").is_ok());
        assert!(parse_reasoning_effort("invented").is_err());
        assert_eq!(runtime_scope_sha256("tenant", "user", "session").len(), 64);
        assert_ne!(
            runtime_scope_sha256("tenant", "user", "session"),
            runtime_scope_sha256("tenant", "users", "ession")
        );

        let mut with_image = request(format!("v1:{}", "a".repeat(64)));
        with_image.images = vec!["data:image/png;base64,iVBORw0KGgo=".to_string()];
        assert!(validate_start_turn_request(&with_image).is_ok());
        with_image.images = vec!["data:text/plain;base64,aGVsbG8=".to_string()];
        assert!(validate_start_turn_request(&with_image).is_err());

        let uppercase = request(format!("v1:{}", "A".repeat(64)));
        assert!(validate_start_turn_request(&uppercase).is_err());
        let oversized = request(format!("v1:{}", "a".repeat(65)));
        assert!(validate_start_turn_request(&oversized).is_err());
    }

    #[test]
    fn capability_worker_requires_explicit_true_flag() {
        assert!(capability_worker_enabled(Some("true")));
        assert!(capability_worker_enabled(Some("TRUE")));
        assert!(!capability_worker_enabled(None));
        assert!(!capability_worker_enabled(Some("false")));
        assert!(!capability_worker_enabled(Some("1")));
        assert!(!capability_worker_enabled(Some(" true ")));
    }

    #[test]
    fn capability_result_status_does_not_treat_failed_response_as_success() {
        let failed = CapabilityExecutionOutcome {
            response: serde_json::json!({
                "contentItems": [{"type": "inputText", "text": "failed"}],
                "success": false
            }),
            status: CapabilityExecutionStatus::Failed,
            raw_result: None,
        };
        assert_eq!(capability_status_name(failed.status), "failed");
        assert_eq!(
            capability_result_detail(&failed),
            "capability_execution_failed"
        );
        assert_ne!(capability_status_name(failed.status), "succeeded");
    }

    #[test]
    fn model_limits_are_profile_driven_and_conflicts_fail_closed() {
        let mut start = ThreadStartParams::default();
        assert!(
            apply_model_limits(&mut start, Some(1_000_000), Some(900_000)).is_ok(),
            "valid model limits"
        );
        let config = start.config.expect("limits should be placed in config");
        assert_eq!(config["model_context_window"], 1_000_000);
        assert_eq!(config["model_auto_compact_token_limit"], 900_000);

        let mut conflicting = ThreadStartParams {
            config: Some(HashMap::from([(
                "model_context_window".to_string(),
                serde_json::json!(272_000),
            )])),
            ..Default::default()
        };
        assert!(apply_model_limits(&mut conflicting, Some(1_000_000), None).is_err());
        assert!(apply_model_limits(&mut ThreadStartParams::default(), None, Some(10)).is_err());
        assert!(
            apply_model_limits(&mut ThreadStartParams::default(), Some(1_000), Some(1_001))
                .is_err()
        );
    }

    #[test]
    fn official_thread_instructions_use_stable_camel_case_fields() {
        let start = ThreadStartParams {
            base_instructions: Some("platform system contract".to_string()),
            developer_instructions: Some("platform developer contract".to_string()),
            ..Default::default()
        };
        let value = serde_json::to_value(start).expect("thread params should serialize");
        assert_eq!(value["baseInstructions"], "platform system contract");
        assert_eq!(
            value["developerInstructions"],
            "platform developer contract"
        );
        assert!(!value.as_object().unwrap().contains_key("base_instructions"));
    }

    #[test]
    fn memory_mode_does_not_enable_unscoped_local_storage() {
        assert!(validate_memory_mode(None).is_ok());
        assert!(validate_memory_mode(Some(ThreadMemoryMode::Disabled)).is_ok());
        assert!(validate_memory_mode(Some(ThreadMemoryMode::Enabled)).is_err());
    }

    #[test]
    fn subagent_projection_deduplicates_starts_and_repairs_late_receiver_identity() {
        let data = json!({
            "run_id": "turn-a",
            "agent_id": "child-a",
            "status": "running",
        });
        let mut state = TextProjectionState::default();
        let first = state.normalize_subagent_events(vec![crate::AssistantTurnEventV1::new(
            "subagent_started",
            data.clone(),
        )]);
        assert_eq!(first.len(), 1);
        let duplicate = state.normalize_subagent_events(vec![crate::AssistantTurnEventV1::new(
            "subagent_started",
            data,
        )]);
        assert!(duplicate.is_empty());
        let finish = state.normalize_subagent_events(vec![crate::AssistantTurnEventV1::new(
            "subagent_finished",
            json!({
                "run_id": "turn-a",
                "agent_id": "child-late",
                "status": "completed",
            }),
        )]);
        assert_eq!(finish.len(), 2);
        assert_eq!(finish[0].event_type, "subagent_started");
        assert_eq!(finish[1].event_type, "subagent_finished");
    }

    #[test]
    fn terminal_admission_failure_is_an_explicit_non_durable_failure() {
        let event = terminal_admission_failure_event(
            &TurnCompletedNotification {
                thread_id: "thread-a".to_string(),
                turn: Turn {
                    id: "turn-a".to_string(),
                    root_turn_id: None,
                    items: Vec::new(),
                    items_view: TurnItemsView::NotLoaded,
                    status: TurnStatus::Interrupted,
                    error: None,
                    started_at: Some(1),
                    completed_at: Some(2),
                    duration_ms: Some(1_000),
                },
            },
            &V1ProjectionContext {
                tenant_id: "tenant-a".to_string(),
                user_id: "user-a".to_string(),
                session_id: "session-a".to_string(),
            },
        );
        assert_eq!(event.event_type, "run_error");
        assert_eq!(event.data["status"], "failed");
        assert_eq!(event.data["durable"], false);
        assert_eq!(
            event.data["terminal_envelope"]["exit_reason"],
            "terminal_admission_failed"
        );
    }

    fn completed_message(item_id: &str, text: &str) -> ServerNotification {
        ServerNotification::ItemCompleted(ItemCompletedNotification {
            item: ThreadItem::AgentMessage {
                id: item_id.to_string(),
                text: text.to_string(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            completed_at_ms: 1,
        })
    }

    #[test]
    fn completed_agent_message_falls_back_only_when_no_delta_was_seen() {
        let context = V1ProjectionContext {
            tenant_id: "tenant".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
        };
        let mut state = TextProjectionState::default();
        let fallback = state
            .fallback_event(&completed_message("message-1", "hello"), &context)
            .expect("completed item fallback");
        assert_eq!(fallback.event_type, "text_delta");
        assert_eq!(fallback.data["content"], "hello");
        assert!(
            state
                .fallback_event(&completed_message("message-1", "hello"), &context)
                .is_none()
        );

        let delta = ServerNotification::AgentMessageDelta(AgentMessageDeltaNotification {
            thread_id: "thread-1".to_string(),
            turn_id: "turn-1".to_string(),
            item_id: "message-2".to_string(),
            delta: "streamed".to_string(),
        });
        assert!(state.fallback_event(&delta, &context).is_none());
        assert!(
            state
                .fallback_event(&completed_message("message-2", "streamed"), &context)
                .is_none()
        );
    }
}
