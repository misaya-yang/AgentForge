//! Private capability authority for genuine SDK runs. No Codex thread, turn,
//! server request, model call, or process-local approval waiter is fabricated.

use ai_platform_capability_contract::{
    ApprovalPolicy, CapabilityEffect, CapabilityScopeV2, canonical_json_hash,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{Postgres, Row, Transaction};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::RuntimeHttpState;
use super::security::{
    RuntimeError, SESSION_HEADER, TENANT_HEADER, USER_HEADER, authorize, required_header,
};
use crate::approval_control::persist_tool_approval;
use crate::capability_execution::{
    CapabilityCallIdentity, ReadonlyCapabilityBinding, execute_capability_call_with_existing,
};
use crate::capability_worker::CapabilityWorkerClient;
use crate::readonly_capabilities::{
    RuntimeCapabilityScope, render_turn_input, resolve_dynamic_capability_descriptor,
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SdkCapabilityRequest {
    tool_call_id: String,
    tool: String,
    #[serde(default)]
    namespace: Option<String>,
    arguments: Value,
}

struct SdkAuthority {
    scope: CapabilityScopeV2,
    run_id: Uuid,
    owner_id: Uuid,
    fence: i64,
}

impl SdkAuthority {
    async fn assert(&self, tx: &mut Transaction<'_, Postgres>, admission: bool)
        -> Result<(), RuntimeError>
    {
        // Short preparation/result transactions serialize per run. SDK owner
        // heartbeats are never held across the Worker network request.
        let run = sqlx::query("SELECT run_id FROM assistant_runs WHERE run_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND engine IN ('openai_agents','claude_agent_sdk') FOR UPDATE")
            .bind(self.run_id).bind(&self.scope.tenant_id).bind(&self.scope.user_id)
            .bind(&self.scope.session_id).fetch_optional(&mut **tx).await
            .map_err(|_| RuntimeError::conflict("sdk_execution_authority_lost"))?;
        if run.is_none() {
            return Err(RuntimeError::conflict("sdk_execution_authority_lost"));
        }
        let allowed: bool = sqlx::query_scalar(
            "SELECT assistant.assert_sdk_execution($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(self.run_id).bind(&self.scope.tenant_id).bind(&self.scope.user_id)
        .bind(&self.scope.session_id).bind(self.owner_id).bind(self.fence).bind(admission)
        .fetch_one(&mut **tx).await
        .map_err(|_| RuntimeError::conflict("sdk_execution_authority_lost"))?;
        if !allowed {
            return Err(RuntimeError::conflict("sdk_execution_authority_lost"));
        }
        Ok(())
    }
}

fn safe_identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && value.bytes().all(|byte|
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/'))
}

fn validate_request(body: &SdkCapabilityRequest) -> Result<(), RuntimeError> {
    if !safe_identifier(&body.tool_call_id, 160) || !safe_identifier(&body.tool, 160)
        || body.namespace.as_deref().is_some_and(|value| !safe_identifier(value, 64))
        || !body.arguments.is_object()
        || serde_json::to_vec(&body.arguments).map_or(true, |bytes| bytes.len() > 64 * 1024)
    {
        return Err(RuntimeError::bad_request("sdk_capability_request_invalid"));
    }
    Ok(())
}

fn failure(body: &SdkCapabilityRequest, run_id: Uuid, status: &str, detail: &str) -> Value {
    json!({
        "status": status, "run_id": run_id, "tool_call_id": body.tool_call_id,
        "tool": body.tool, "execution_id": null, "receipt": null,
        "response": {"success": false,
            "contentItems": [{"type": "inputText", "text": detail}]},
    })
}

fn unknown(body: &SdkCapabilityRequest, run_id: Uuid) -> Value {
    failure(body, run_id, "side_effect_unknown",
        "The action may already have executed. Do not retry with a new call ID; inspect the original execution receipt.")
}

async fn execution_id(
    tx: &mut Transaction<'_, Postgres>,
    authority: &SdkAuthority,
    body: &SdkCapabilityRequest,
    binding: &ReadonlyCapabilityBinding,
    arguments_hash: &str,
) -> Result<Option<Uuid>, RuntimeError> {
    let row = sqlx::query("SELECT execution_id,capability_id,capability_revision,arguments_sha256,arguments FROM assistant_capability_executions WHERE run_id=$1 AND tool_call_id=$2 AND attempt_id=$2 AND tenant_id=$3 AND user_id=$4 AND session_id=$5")
        .bind(authority.run_id).bind(&body.tool_call_id).bind(&authority.scope.tenant_id)
        .bind(&authority.scope.user_id).bind(&authority.scope.session_id)
        .fetch_optional(&mut **tx).await
        .map_err(|_| RuntimeError::internal("sdk_execution_receipt_lookup_failed"))?;
    let Some(row) = row else { return Ok(None); };
    if row.try_get::<String, _>("capability_id").map_err(invalid_row)? != binding.expected_tool.id
        || row.try_get::<i64, _>("capability_revision").map_err(invalid_row)? != binding.capability_revision as i64
        || row.try_get::<String, _>("arguments_sha256").map_err(invalid_row)?.trim_end() != arguments_hash
        || row.try_get::<Value, _>("arguments").map_err(invalid_row)? != body.arguments
    {
        return Err(RuntimeError::conflict("sdk_execution_receipt_mismatch"));
    }
    Ok(Some(row.try_get("execution_id").map_err(invalid_row)?))
}

fn invalid_row(_: sqlx::Error) -> RuntimeError {
    RuntimeError::internal("sdk_capability_record_invalid")
}

async fn save_response(
    tx: &mut Transaction<'_, Postgres>,
    run_id: Uuid,
    call_id: &str,
    execution_id: Option<Uuid>,
    response: &Value,
) -> Result<Value, RuntimeError> {
    sqlx::query_scalar("UPDATE assistant.assistant_sdk_tool_calls SET execution_id=COALESCE(execution_id,$3),response=COALESCE(response,$4),completed_at=COALESCE(completed_at,clock_timestamp()) WHERE run_id=$1 AND tool_call_id=$2 RETURNING response")
        .bind(run_id).bind(call_id).bind(execution_id).bind(response)
        .fetch_one(&mut **tx).await.map_err(invalid_row)
}

pub(super) async fn execute_sdk_capability(
    State(state): State<RuntimeHttpState>,
    headers: HeaderMap,
    Path(run_id): Path<Uuid>,
    Json(body): Json<SdkCapabilityRequest>,
) -> Result<Json<Value>, RuntimeError> {
    authorize(&headers, &state.internal_token)?;
    validate_request(&body)?;
    let authority = SdkAuthority {
        scope: CapabilityScopeV2 {
            tenant_id: required_header(&headers, TENANT_HEADER)?,
            user_id: required_header(&headers, USER_HEADER)?,
            session_id: required_header(&headers, SESSION_HEADER)?,
        },
        run_id,
        owner_id: Uuid::parse_str(&required_header(&headers, "x-execution-owner-id")?)
            .map_err(|_| RuntimeError::bad_request("sdk_execution_owner_invalid"))?,
        fence: required_header(&headers, "x-execution-fence")?.parse::<i64>()
            .ok().filter(|value| *value > 0)
            .ok_or_else(|| RuntimeError::bad_request("sdk_execution_fence_invalid"))?,
    };
    let arguments_hash = canonical_json_hash(&body.arguments)
        .map_err(|_| RuntimeError::bad_request("sdk_capability_arguments_invalid"))?;
    let arguments_hash = arguments_hash.trim_start_matches("sha256:").to_string();
    let mut tx = state.store.pool.begin().await.map_err(invalid_row)?;
    // Lock order is always run -> SDK authority -> invocation -> approval.
    authority.assert(&mut tx, true).await?;
    let row = sqlx::query("SELECT snapshot_id,capability_revision,snapshot,snapshot_sha256,expires_at FROM assistant.assistant_sdk_runs WHERE run_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND engine IN ('openai_agents','claude_agent_sdk')")
        .bind(run_id).bind(&authority.scope.tenant_id).bind(&authority.scope.user_id)
        .bind(&authority.scope.session_id).fetch_one(&mut *tx).await.map_err(invalid_row)?;
    let verified_expiry:DateTime<Utc>=row.try_get("expires_at").map_err(invalid_row)?;
    let transport_budget=super::transport_budget::admit_authenticated(&headers,super::transport_budget::ADMISSION_CAP,Some(verified_expiry.timestamp_millis().max(0)as u64)).map_err(|_|RuntimeError::bad_request("runtime_transport_budget_invalid"))?;
    let snapshot: Value = row.try_get("snapshot").map_err(invalid_row)?;
    let stored_hash: String = row.try_get("snapshot_sha256").map_err(invalid_row)?;
    if canonical_json_hash(&snapshot)
        .map_err(|_| RuntimeError::conflict("sdk_snapshot_invalid"))?
        .trim_start_matches("sha256:") != stored_hash.trim_end()
    {
        return Err(RuntimeError::conflict("sdk_snapshot_hash_mismatch"));
    }
    let revision: i64 = row.try_get("capability_revision").map_err(invalid_row)?;
    let snapshot_id: Uuid = row.try_get("snapshot_id").map_err(invalid_row)?;
    let payload = snapshot.get("readonly_capabilities")
        .ok_or_else(|| RuntimeError::conflict("sdk_snapshot_capabilities_missing"))?;
    // Pure readonly-payload validation: no kernel lineage/model metadata is used.
    render_turn_input(&RuntimeCapabilityScope {
        tenant_id: authority.scope.tenant_id.clone(),
        user_id: authority.scope.user_id.clone(),
        session_id: authority.scope.session_id.clone(),
        capability_revision: revision,
        snapshot_id: snapshot_id.to_string(),
    }, payload).map_err(|_| RuntimeError::conflict("sdk_snapshot_capabilities_invalid"))?;
    let (allowlist, expected_tool, descriptor, effect) =
        resolve_dynamic_capability_descriptor(payload, body.namespace.as_deref(), &body.tool)
            .map_err(|_| RuntimeError::bad_request("sdk_capability_not_authorized"))?;
    if effect == CapabilityEffect::Read && descriptor.approval_policy != ApprovalPolicy::Never {
        return Err(RuntimeError::bad_request("sdk_read_approval_policy_unsupported"));
    }
    let binding = ReadonlyCapabilityBinding {
        capability_revision: revision.try_into()
            .map_err(|_| RuntimeError::conflict("sdk_snapshot_revision_invalid"))?,
        allowlist, expected_tool, descriptor,
    };
    let new_call: bool = sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM assistant.assistant_sdk_tool_calls WHERE run_id=$1 AND tool_call_id=$2)")
        .bind(run_id).bind(&body.tool_call_id).fetch_one(&mut *tx).await.map_err(invalid_row)?;
    if new_call {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM assistant.assistant_sdk_tool_calls WHERE run_id=$1")
            .bind(run_id).fetch_one(&mut *tx).await.map_err(invalid_row)?;
        if count >= 128 {
            return Err(RuntimeError::conflict("sdk_capability_call_budget_exhausted"));
        }
    }
    sqlx::query("INSERT INTO assistant.assistant_sdk_tool_calls (run_id,tool_call_id,tool_name,namespace,arguments,arguments_sha256) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT (run_id,tool_call_id) DO NOTHING")
        .bind(run_id).bind(&body.tool_call_id).bind(&body.tool).bind(&body.namespace)
        .bind(&body.arguments).bind(&arguments_hash).execute(&mut *tx).await.map_err(invalid_row)?;
    let invocation = sqlx::query("SELECT tool_name,namespace,arguments,arguments_sha256,response,execution_id,dispatch_started_at FROM assistant.assistant_sdk_tool_calls WHERE run_id=$1 AND tool_call_id=$2 FOR UPDATE")
        .bind(run_id).bind(&body.tool_call_id).fetch_one(&mut *tx).await.map_err(invalid_row)?;
    // Check immutable identity before reusing any approval, response or read.
    if invocation.try_get::<String, _>("tool_name").map_err(invalid_row)? != body.tool
        || invocation.try_get::<Option<String>, _>("namespace").map_err(invalid_row)? != body.namespace
        || invocation.try_get::<Value, _>("arguments").map_err(invalid_row)? != body.arguments
        || invocation.try_get::<String, _>("arguments_sha256").map_err(invalid_row)?.trim_end() != arguments_hash
    {
        return Err(RuntimeError::conflict("sdk_capability_call_identity_changed"));
    }
    if let Some(response) = invocation.try_get::<Option<Value>, _>("response").map_err(invalid_row)? {
        tx.commit().await.map_err(invalid_row)?;
        return Ok(Json(response));
    }
    let existing = execution_id(&mut tx, &authority, &body, &binding, &arguments_hash).await?;
    let saved_execution: Option<Uuid> = invocation.try_get("execution_id").map_err(invalid_row)?;
    if saved_execution.is_some() && saved_execution != existing {
        return Err(RuntimeError::conflict("sdk_execution_receipt_mismatch"));
    }
    let started: Option<DateTime<Utc>> = invocation.try_get("dispatch_started_at").map_err(invalid_row)?;
    if started.is_some() && existing.is_none() && effect != CapabilityEffect::Read {
        // No second create after the first request may have crossed the boundary.
        let response = save_response(&mut tx, run_id, &body.tool_call_id, None, &unknown(&body, run_id)).await?;
        tx.commit().await.map_err(invalid_row)?;
        return Ok(Json(response));
    }
    let mut approval_id = None;
    if effect != CapabilityEffect::Read {
        let approval = persist_tool_approval(&mut tx, &authority.scope, run_id,
            &body.tool_call_id, &binding.expected_tool.id, &body.arguments).await
            .map_err(|_| RuntimeError::conflict("sdk_capability_approval_invalid"))?;
        approval_id = Some(approval.approval_id);
        sqlx::query("UPDATE assistant.assistant_sdk_tool_calls SET approval_id=$3 WHERE run_id=$1 AND tool_call_id=$2 AND (approval_id IS NULL OR approval_id=$3)")
            .bind(run_id).bind(&body.tool_call_id).bind(approval.approval_id)
            .execute(&mut *tx).await.map_err(invalid_row)?;
        // A consumed approval is usable only to observe its original receipt.
        let can_observe = approval.status == "consumed" && existing.is_some();
        if !can_observe && (approval.expires_at <= Utc::now()
            || !matches!(approval.status.as_str(), "pending" | "approved"))
        {
            let mut response = failure(&body, run_id, "failed", "capability approval rejected, cancelled, or expired");
            response["approval_id"] = json!(approval.approval_id);
            response["approval_status"] = json!(if approval.expires_at <= Utc::now() && approval.status == "pending" {"expired"} else {approval.status.as_str()});
            let response = save_response(&mut tx, run_id, &body.tool_call_id, existing, &response).await?;
            tx.commit().await.map_err(invalid_row)?;
            return Ok(Json(response));
        }
        if approval.status == "pending" {
            let response = json!({
                "status": "approval_required", "approval_id": approval.approval_id,
                "run_id": run_id, "tool_call_id": body.tool_call_id, "tool": body.tool,
                "arguments": body.arguments, "expires_at": approval.expires_at,
            });
            tx.commit().await.map_err(invalid_row)?;
            return Ok(Json(response));
        }
    }
    if !super::capability_dispatch::capability_worker_enabled(
        std::env::var("AI_PLATFORM_CAPABILITY_WORKER_ENABLED").ok().as_deref())
        || (effect != CapabilityEffect::Read && !super::capability_dispatch::capability_worker_enabled(
            std::env::var("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED").ok().as_deref()))
    {
        return Err(RuntimeError::unavailable("sdk_capability_worker_not_ready"));
    }
    let worker_url = std::env::var("AI_PLATFORM_CAPABILITY_WORKER_URL")
        .map_err(|_| RuntimeError::unavailable("sdk_capability_worker_not_ready"))?;
    let secret = super::lease_verification::sdk_capability_authority(&mut tx,&authority.scope,run_id,snapshot_id,authority.owner_id,authority.fence).await
        .map_err(|_| RuntimeError::unavailable("sdk_capability_worker_not_ready"))?;
    let worker = CapabilityWorkerClient::new(state.capability_client.clone(), &worker_url,
        state.internal_token.to_string())
        .map_err(|_| RuntimeError::unavailable("sdk_capability_worker_not_ready"))?
        .with_execution_owner(authority.owner_id.to_string(), authority.fence)
        .with_transport_budget(transport_budget)
        .with_trace_context(crate::trace_context::InternalTraceContext::from_headers(&headers), run_id.to_string());
    authority.assert(&mut tx, true).await?;
    sqlx::query("UPDATE assistant.assistant_sdk_tool_calls SET dispatch_started_at=COALESCE(dispatch_started_at,clock_timestamp()),execution_id=COALESCE(execution_id,$3) WHERE run_id=$1 AND tool_call_id=$2")
        .bind(run_id).bind(&body.tool_call_id).bind(existing)
        .execute(&mut *tx).await.map_err(invalid_row)?;
    tx.commit().await.map_err(invalid_row)?;
    let call = CapabilityCallIdentity {
        scope: authority.scope.clone(), run_id: run_id.to_string(),
        tool_call_id: body.tool_call_id.clone(), tool: body.tool.clone(), arguments: body.arguments.clone(),
    };
    let existing_string = existing.map(|id| id.to_string());
    let approval_string = approval_id.map(|id| id.to_string());
    // Worker reservation and dispatch atomically recheck the same owner/fence.
    // Dropping this HTTP future leaves the durable invocation/Worker receipt.
    let outcome = execute_capability_call_with_existing(&worker, &call, &binding,
        &secret, effect, approval_string.as_deref(), &CancellationToken::new(),
        existing_string.as_deref()).await;
    let mut tx = state.store.pool.begin().await.map_err(invalid_row)?;
    authority.assert(&mut tx, false).await?;
    let receipt_id = execution_id(&mut tx, &authority, &body, &binding, &arguments_hash).await?;
    let mut response = match outcome {
        Ok(outcome) => json!({
            "status": outcome.status, "run_id": run_id, "tool_call_id": body.tool_call_id,
            "tool": body.tool, "execution_id": receipt_id,
            "receipt": outcome.raw_result, "response": outcome.response,
        }),
        Err(_) if effect != CapabilityEffect::Read => unknown(&body, run_id),
        Err(_) => return Err(RuntimeError::unavailable("sdk_capability_execution_unavailable")),
    };
    if let Some(id) = approval_id {
        let status: Option<String> = sqlx::query_scalar("SELECT status FROM assistant_tool_approvals WHERE approval_id=$1 AND run_id=$2")
            .bind(id).bind(run_id).fetch_optional(&mut *tx).await.map_err(invalid_row)?;
        response["approval_id"] = json!(id);
        response["approval_status"] = json!(status);
    }
    response["execution_id"] = json!(receipt_id);
    let response = save_response(&mut tx, run_id, &body.tool_call_id, receipt_id, &response).await?;
    tx.commit().await.map_err(invalid_row)?;
    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_request_rejects_authority_and_codex_fields() {
        for field in ["thread_id", "turn_id", "approval_id", "capability_revision", "snapshot", "owner_id"] {
            let mut value = json!({"tool_call_id":"sdk-call", "tool":"search", "arguments":{}});
            value[field] = json!("injected");
            assert!(serde_json::from_value::<SdkCapabilityRequest>(value).is_err());
        }
    }

    #[test]
    fn sdk_request_requires_bounded_identity_and_object_arguments() {
        for value in [
            json!({"tool_call_id":"", "tool":"search", "arguments":{}}),
            json!({"tool_call_id":"sdk-call", "tool":"search", "arguments":[]}),
            json!({"tool_call_id":"a".repeat(161), "tool":"search", "arguments":{}}),
            json!({"tool_call_id":"sdk-call", "tool":"search", "namespace":"a".repeat(65), "arguments":{}}),
            json!({"tool_call_id":"sdk-call", "tool":"search", "arguments":{"x":"x".repeat(65536)}}),
        ] {
            let request: SdkCapabilityRequest = serde_json::from_value(value).unwrap();
            assert!(validate_request(&request).is_err());
        }
        let request: SdkCapabilityRequest = serde_json::from_value(json!({
            "tool_call_id":"sdk-call", "tool":"search", "namespace":"mcp", "arguments":{}
        })).unwrap();
        assert!(validate_request(&request).is_ok());
    }
}
