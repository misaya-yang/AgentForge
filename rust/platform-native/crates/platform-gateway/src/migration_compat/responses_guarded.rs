//! Existing Responses acceptance/idempotency flow with guardrail hooks.
use super::*;
use crate::llm_api::guardrails::{Context as GuardContext, GuardrailService};
pub(super) async fn create_response(
    State(state): State<Arc<ResponsesState>>,
    request: Request,
) -> Response {
    let admission = state.auth.request(request.headers(), request.uri().path());
    let mut user = match admission.resolve().await {
        Ok(principal) => CompatUser::from_admitted(principal, request.headers()),
        Err(_) => {
            return response_error(
                401,
                "authentication_required",
                "Authentication and tenant identity are required.",
                None,
                "authentication_error",
            );
        }
    };
    user.apply_request_context(request.extensions());
    if !user.authenticated
        || user.user_id.is_empty()
        || user.tenant_id.is_empty()
        || user.user_id == "anonymous"
        || user.tenant_id == "public"
    {
        return response_error(
            401,
            "authentication_required",
            "Authentication and tenant identity are required.",
            None,
            "authentication_error",
        );
    }
    drop(admission);
    if request.uri().query().is_some_and(|v| !v.is_empty()) {
        return response_error(
            400,
            "unsupported_query_parameters",
            "Query parameters are not supported for this endpoint.",
            None,
            "invalid_request_error",
        );
    }
    if let Some(limiter) = &state.limiter {
        let context = RateLimitContext {
            user_id: Some(user.user_id.clone()),
            tenant_id: Some(user.tenant_id.clone()),
            user_tier: user.tier.clone(),
            operation: Some("assistant_chat".into()),
            ..Default::default()
        };
        let result = match state.rate_config.policies(
            &context,
            &request
                .extensions()
                .get::<crate::migration_host::rate::CountedDimensions>()
                .map(|v| v.0.clone())
                .unwrap_or_default(),
        ) {
            Ok(policies) => limiter.check_http_many(&policies).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(result) if !result.result.allowed => {
                let mut response = response_error(
                    429,
                    "rate_limit_exceeded",
                    "Rate limit exceeded.",
                    None,
                    "rate_limit_error",
                );
                for (k, v) in result.result.headers() {
                    if let (Ok(k), Ok(v)) = (
                        HeaderName::from_bytes(k.as_bytes()),
                        HeaderValue::from_str(&v),
                    ) {
                        response.headers_mut().insert(k, v);
                    }
                }
                return response;
            }
            Err(_) => {
                return response_error(
                    503,
                    "rate_limit_unavailable",
                    "Rate limiting is temporarily unavailable.",
                    None,
                    "server_error",
                );
            }
            _ => {}
        }
    }
    let idempotency_key =
        crate::migration_compat::common::header(request.headers(), "idempotency-key").to_owned();
    let forged_header = request.headers().keys().any(|key| {
        key.as_str().starts_with("x-agent-")
            && !matches!(key.as_str(), "x-agent-embed-token" | "x-agent-embed-origin")
    });
    let bytes = match to_bytes(request.into_body(), 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return response_error(
                413,
                "request_too_large",
                "Request body is too large.",
                None,
                "invalid_request_error",
            );
        }
    };
    let payload: Value = match serde_json::from_slice(&bytes) {
        Ok(payload) => payload,
        Err(_) => {
            return response_error(
                400,
                "invalid_json",
                "Invalid JSON body.",
                None,
                "invalid_request_error",
            );
        }
    };
    if !payload.is_object() {
        return response_error(
            400,
            "invalid_json_object",
            "Request body must be a JSON object.",
            None,
            "invalid_request_error",
        );
    }
    let forged_body = [
        "agent_id",
        "agent_version_id",
        "draft_revision",
        "publication_id",
        "channel",
        "resolved_snapshot",
        "runtime_envelope",
        "snapshot_hash",
        "spec_hash",
        "runtime_fingerprint",
    ]
    .iter()
    .any(|field| payload.get(*field).is_some());
    if forged_header || forged_body {
        return response_error(
            if forged_header { 400 } else { 422 },
            "agent_runtime_field_forbidden",
            "Client-supplied Agent runtime fields or headers are forbidden.",
            None,
            "invalid_request_error",
        );
    }
    let model = match payload["model"]
        .as_str()
        .filter(|v| !v.trim().is_empty() && v.chars().count() <= 255)
    {
        Some(model) => model,
        None => {
            return response_error(
                400,
                "invalid_model",
                "model must be a non-empty string.",
                Some("model"),
                "invalid_request_error",
            );
        }
    };
    if payload
        .get("expected_model_ref")
        .is_some_and(|v| !v.is_null() && !v.is_object())
    {
        return response_error(
            400,
            "invalid_expected_model_ref",
            "expected_model_ref must be an object.",
            Some("expected_model_ref"),
            "invalid_request_error",
        );
    }
    if payload.get("provider_id").is_some_and(|v| {
        !v.is_null()
            && v.as_str()
                .is_none_or(|v| v.trim().is_empty() || v.chars().count() > 255)
    }) {
        return response_error(
            400,
            "invalid_provider_id",
            "provider_id must be a non-empty string.",
            Some("provider_id"),
            "invalid_request_error",
        );
    }
    let provider = payload["provider_id"].as_str().map(str::trim);
    let model_record = match state
        .models
        .get_model(&user.tenant_id, model, provider)
        .await
    {
        Ok(Some(record)) => Value::Object(record),
        Ok(None) => {
            return response_error(
                400,
                "model_not_found",
                "The requested model was not found.",
                Some("model"),
                "invalid_request_error",
            );
        }
        Err(crate::model_service::ModelServiceError::ProviderAmbiguous) => {
            return response_error(
                409,
                "model_provider_required",
                "Specify provider_id for this model.",
                Some("provider_id"),
                "invalid_request_error",
            );
        }
        Err(_) => {
            return response_error(
                503,
                "model_authorization_unavailable",
                "Model authorization is temporarily unavailable.",
                Some("model"),
                "server_error",
            );
        }
    };
    let actor = Actor {
        tenant_id: user.tenant_id.clone(),
        user_id: user.user_id.clone(),
        roles: user.roles.clone(),
        tier: user.tier.clone(),
    };
    if !crate::migration_agent::entry::user_can_access_model(
        &actor,
        text(&model_record, "access_level"),
    ) {
        return response_error(
            403,
            "model_access_denied",
            "Access to the requested model is denied.",
            Some("model"),
            "permission_error",
        );
    }
    let parsed = match ResponsesRequest::after_model_authorization(payload, &idempotency_key) {
        Ok(parsed) => parsed,
        Err(error) => {
            let detail = error.detail.as_str().unwrap_or("");
            let code = error
                .detail
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or_else(|| {
                    if detail.contains("tool") {
                        "invalid_tools"
                    } else if detail.contains("instructions") {
                        "invalid_instructions"
                    } else if detail.contains("reasoning") {
                        "invalid_reasoning"
                    } else if detail.contains("max_output_tokens") {
                        "invalid_max_output_tokens"
                    } else if detail.contains("temperature") {
                        "invalid_temperature"
                    } else if detail.contains("stream") {
                        "invalid_stream"
                    } else {
                        "invalid_input"
                    }
                });
            return response_error(
                error.status,
                code,
                if detail.is_empty() {
                    "The Responses request is not supported by the Agent Runtime."
                } else {
                    detail
                },
                None,
                "invalid_request_error",
            );
        }
    };
    let guard_request = format!("guard_{}", uuid::Uuid::new_v4().simple());
    let guard_context = match GuardrailService::new(state.control.pool.clone())
        .inspect_input(&user.tenant_id, &user.user_id, &guard_request, &bytes)
        .await
    {
        Ok(g) => g,
        Err(e) => {
            return response_error(
                e.status,
                e.code,
                "Tenant guardrails rejected or could not inspect this request.",
                None,
                "guardrail_error",
            );
        }
    };
    let claim = match state
        .idempotency
        .begin(&user, &idempotency_key, &bytes)
        .await
    {
        Ok(BeginResponse::Replay(cached)) => {
            return match screen_response(guard_context.as_ref(), cached_response(cached, true))
                .await
            {
                Ok(r) => r,
                Err(e) => response_error(
                    e.status,
                    e.code,
                    "Tenant guardrails withheld the cached response.",
                    None,
                    "guardrail_error",
                ),
            };
        }
        Ok(BeginResponse::Claim { scope, digest }) => Some((scope, digest)),
        Ok(BeginResponse::Unkeyed) => None,
        Err(error) => {
            return response_error(
                error.status,
                error.detail.as_str().unwrap_or("idempotency_failed"),
                "The idempotent request could not proceed.",
                None,
                "invalid_request_error",
            );
        }
    };
    let recovery_claim = claim.clone();
    let recovery_session = parsed.session_id.clone();
    let mut accepted_run = None;
    let response=async {
    let scope = actor.scope(&parsed.session_id);
    let prepare_session = async {
        if let Some(session) = state
            .sessions
            .get(&parsed.session_id)
            .await
            .map_err(|_| CompatError::new(503, "Session storage unavailable"))?
        {
            if session.user_id != user.user_id || session.tenant_id != user.tenant_id {
                return Err(CompatError::new(404, "Session not found"));
            }
        } else {
            match state
                .sessions
                .create(CreateSession {
                    user_id: user.user_id.clone(),
                    tenant_id: user.tenant_id.clone(),
                    service_id: Some("__builtin_assistant__".into()),
                    session_id: Some(parsed.session_id.clone()),
                    fail_if_exists: true,
                    ..Default::default()
                })
                .await
            {
                Ok(_) => {}
                Err(_) => {
                    let session = state
                        .sessions
                        .get(&parsed.session_id)
                        .await
                        .map_err(|_| CompatError::new(503, "Session storage unavailable"))?;
                    if session
                        .is_none_or(|s| s.user_id != user.user_id || s.tenant_id != user.tenant_id)
                    {
                        return Err(CompatError::new(404, "Session not found"));
                    }
                }
            }
        }
        state
            .control
            .threads
            .bind_new_assignment(&scope, &state.control.options.kernel_revision)
            .await
            .map_err(|e| CompatError::value(e.status, e.detail))?;
        Ok::<_, CompatError>(())
    }
    .await;
    if prepare_session.is_err() {
        if let Some((scope, _)) = &claim {
            let _ = state.idempotency.abort(scope).await;
        }
        return response_error(
            503,
            "agent_runtime_session_unavailable",
            "Agent Runtime session storage is temporarily unavailable.",
            None,
            "server_error",
        );
    }
    let turn = state
        .control
        .start_turn(TurnRequest {
            actor,
            session_id: parsed.session_id.clone(),
            message: parsed.message.clone(),
            model_id: parsed.model.clone(),
            provider_id: parsed.provider_id.clone(),
            expected_model_ref: parsed
                .payload
                .get("expected_model_ref")
                .filter(|v| !v.is_null())
                .cloned(),
            reasoning_option: parsed.payload["reasoning"]["effort"]
                .as_str()
                .map(str::to_owned),
            max_tokens: parsed.payload["max_output_tokens"].as_i64(),
            temperature: parsed.payload["temperature"].as_f64(),
            developer_instructions: parsed.payload["instructions"].as_str().map(str::to_owned),
            readonly_capabilities: parsed.readonly_capabilities(),
            memory_mode: "off".into(),
            memory_profile: Some("off".into()),
            enable_dynamic_tools: parsed.tools["tool_names"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
                && parsed.tools["tool_choice"] != json!("none"),
            ..Default::default()
        })
        .await;
    let turn = match turn {
        Ok(turn) => turn,
        Err(error) => {
            if let Some((scope, _)) = &claim {
                let _ = state.idempotency.abort(scope).await;
            }
            return response_error(
                error.status,
                &text(&error.detail, "code").to_lowercase(),
                "Agent Runtime rejected the request.",
                None,
                if error.status >= 500 {
                    "server_error"
                } else {
                    "invalid_request_error"
                },
            );
        }
    };
    accepted_run=Some(turn.run_id.clone());
    let mut projection = ResponseProjection::new(&turn.run_id, &parsed);
    let events = state
        .control
        .stream_thread_events(
            scope,
            turn.runtime_thread_id,
            turn.after_sequence,
            1000,
            Some(turn.run_id.clone()),
        )
        .await;
    let mut events = match events {
        Ok(events) => events,
        Err(_) => {
            return accepted_observation_failure(
                &state.idempotency, claim.as_ref(), &turn.run_id, &parsed.session_id,
                503, "agent_runtime_event_stream_failed",
                "Agent Runtime event stream is temporarily unavailable.",
            ).await;
        }
    };
    if parsed.stream {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            for bytes in projection.initial_events() {
                if tx.send(Ok(Bytes::from(bytes))).await.is_err() {
                    return;
                }
            }
            loop {
                tokio::select! {_=tx.closed()=>return,event=events.recv()=>match event{Some(Ok(event))=>{for bytes in projection.observe(&event){if tx.send(Ok(Bytes::from(bytes))).await.is_err(){return;}}if projection.terminal.is_some(){return;}},Some(Err(_))=>{let _=tx.send(Err(std::io::Error::other("Agent Runtime event stream interrupted"))).await;return;},None=>break}}
            }
            if let Some(bytes) = projection.incomplete_event() {
                let _ = tx.send(Ok(Bytes::from(bytes))).await;
            }
        });
        return (
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
                ("x-accel-buffering", "no"),
            ],
            Body::from_stream(crate::migration_compat::common::ByteReceiver(rx)),
        )
            .into_response();
    }
    while let Some(event) = events.recv().await {
        match event {
            Ok(event) => {
                projection.observe(&event);
                if projection.terminal.is_some() {
                    break;
                }
            }
            Err(_) => {
                return accepted_observation_failure(
                    &state.idempotency, claim.as_ref(), &turn.run_id, &parsed.session_id,
                    503, "agent_runtime_event_stream_failed",
                    "Agent Runtime event stream is temporarily unavailable.",
                ).await;
            }
        }
    }
    if projection.terminal.is_none() {
        return accepted_observation_failure(
            &state.idempotency, claim.as_ref(), &turn.run_id, &parsed.session_id,
            502, "agent_runtime_stream_incomplete",
            "Agent Runtime ended without a terminal event.",
        ).await;
    }
    let error=projection.terminal.as_deref().filter(|status|*status!="completed").map(|status|json!({"code":status,"message":"The response could not be completed.","type":"server_error"}));
    let payload = projection.completed_response(error);
    if let Some((scope, digest)) = claim {
        let cached = CachedResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: payload.to_string().into_bytes(),
            digest,
        };
        if state.idempotency.finish(&scope, cached).await.is_err() {
            return response_error(
                503,
                "idempotency_store_unavailable",
                "Response persistence is temporarily unavailable.",
                None,
                "server_error",
            );
        }
    }
    Json(payload).into_response()
    }.await;
    let result = match screen_response(guard_context.as_ref(), response).await {
        Ok(response) => response,
        Err(e) => {
            if let Some(run) = accepted_run {
                accepted_observation_failure(&state.idempotency,recovery_claim.as_ref(),&run,&recovery_session,e.status,e.code,"The accepted turn output was withheld by guardrails; do not resubmit it as a new execution.").await
            } else {
                response_error(
                    e.status,
                    e.code,
                    "Tenant guardrails withheld the response.",
                    None,
                    "guardrail_error",
                )
            }
        }
    };
    with_guard_id(result, &guard_request, guard_context.is_some())
}

fn with_guard_id(mut response: Response, id: &str, enabled: bool) -> Response {
    if enabled {
        if let Ok(id) = HeaderValue::from_str(id) {
            response.headers_mut().insert("x-guardrail-request-id", id);
        }
    }
    response
}
async fn screen_response(
    context: Option<&GuardContext>,
    response: Response,
) -> Result<Response, crate::llm_api::LlmError> {
    let Some(g) = context.filter(|g| g.output_enabled()) else {
        return Ok(response);
    };
    if !response.status().is_success() {
        return Ok(response);
    }
    let start = Instant::now();
    let _permit = g.buffer_permit().await?;
    let (mut parts, body) = response.into_parts();
    let sse = parts
        .headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .is_some_and(|s| s.starts_with("text/event-stream"));
    let bytes = match to_bytes(body, g.output_limit()).await {
        Ok(b) => b,
        Err(_) => {
            g.reject_output_limit(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX))
                .await?;
            return Err(crate::llm_api::LlmError::new(
                503,
                "llm_guardrails_observation_incomplete",
            ));
        }
    };
    g.inspect_output(
        &bytes,
        sse,
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
    )
    .await?;
    parts.headers.remove("content-length");
    Ok(Response::from_parts(parts, Body::from(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn absent_policy_does_not_poll_or_buffer_existing_responses_stream() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let polls = Arc::new(AtomicUsize::new(0));
        let seen = polls.clone();
        let stream = futures_util::stream::poll_fn(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
            std::task::Poll::<Option<Result<Bytes, std::io::Error>>>::Pending
        });
        let response = Response::new(Body::from_stream(stream));
        let _ = screen_response(None, response).await.unwrap();
        assert_eq!(polls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn completed_responses_output_is_screened_and_rejected_acceptance_stays_fenced() {
        let context = crate::llm_api::guardrails::test_context();
        let response =
            Json(json!({"id":"resp_run","output_text":"person@example.test"})).into_response();
        let rejected = screen_response(Some(&context), response).await.unwrap_err();
        assert_eq!(rejected.status, 403);
        let store = ResponsesIdempotency::new(None);
        let claim = ("scope".into(), "original-digest".into());
        let failure = accepted_observation_failure(
            &store,
            Some(&claim),
            "run-id",
            "session-id",
            rejected.status,
            rejected.code,
            "Output withheld by guardrails; accepted execution remains fenced.",
        )
        .await;
        assert_eq!(failure.status(), 403);
        assert_eq!(failure.headers()["x-run-id"], "run-id");
        let cached = store.cached("scope").await.unwrap().unwrap();
        assert_eq!(cached.status, 403);
        assert_eq!(cached.digest, "original-digest");
        assert!(!String::from_utf8_lossy(&cached.body).contains("person@example.test"));
    }
}
