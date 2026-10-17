//! Execution lifecycle endpoints: create (lease + descriptor + binding
//! verification, reservation, dispatch), read, and cancel.

use std::collections::BTreeMap;

use ai_platform_capability_contract::{
    CAPABILITY_EXECUTION_SCHEMA_VERSION, CapabilityEffect, CapabilityExecutionStatus,
    CapabilityExecutionV2, CreateCapabilityExecutionRequestV2, RuntimeCapabilityLeaseV1,
    validate_json_value,
};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use uuid::Uuid;

use super::execution_run::run_execution;
use super::{
    ExecutionScope, HttpError, WorkerState, authorize, descriptor_type, error, event_id,
    resource_binding_value, scope_matches, store_error, validate_execution_id,
    verify_descriptor_against_runtime_binding,
};
use crate::{CapabilityIdentity, NewExecution, StoreError, now_epoch_ms};

pub(super) async fn create_execution(
    State(state): State<WorkerState>,
    headers: HeaderMap,
    Json(request): Json<CreateCapabilityExecutionRequestV2>,
) -> Result<impl IntoResponse, HttpError> {
    let scope = authorize(&headers, &state)?;
    let trace_context = crate::trace_context::InternalTraceContext::from_headers(&headers);
    request.validate(now_epoch_ms()).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "execution_request_invalid",
            "execution request is invalid",
        )
    })?;
    request
        .lease
        .verify_signature(&state.lease_secret)
        .map_err(|_| {
            error(
                StatusCode::FORBIDDEN,
                "lease_invalid",
                "runtime capability lease is invalid",
            )
        })?;
    if !scope_matches(&scope, &request.lease.scope()) {
        return Err(error(
            StatusCode::FORBIDDEN,
            "scope_mismatch",
            "lease scope does not match forwarded identity",
        ));
    }
    let requested_descriptor = request.descriptor.clone();
    requested_descriptor.validate().map_err(|_| {
        error(
            StatusCode::FORBIDDEN,
            "capability_descriptor_invalid",
            "capability descriptor is invalid",
        )
    })?;
    if requested_descriptor.id != request.lease.capability_id
        || requested_descriptor.effect != request.lease.effect
    {
        return Err(store_error(StoreError::RuntimeBindingUnauthorized));
    }
    // Built-ins remain registry-authoritative. Dynamic descriptors are not
    // required to appear in this catalog; they are checked against PG below.
    let descriptor = if let Some(static_descriptor) =
        state.capability_registry.get(&request.lease.capability_id)
    {
        if static_descriptor != &requested_descriptor {
            return Err(store_error(StoreError::RuntimeBindingUnauthorized));
        }
        static_descriptor.clone()
    } else {
        requested_descriptor
    };
    if !state.admits_plugin_descriptor(&descriptor) {
        return Err(store_error(StoreError::RuntimeBindingUnauthorized));
    }
    let declared_type = descriptor_type(&descriptor)?;
    let runtime_binding = state
        .store
        .authorize_runtime_binding(
            &scope,
            &request.lease.run_id,
            request.lease.capability_revision,
            &CapabilityIdentity {
                capability_type: declared_type.to_string(),
                name: descriptor.name.clone(),
                capability_id: descriptor.id.clone(),
                version: descriptor.version.clone(),
                schema_hash: descriptor.schema_hash.clone(),
                effect: descriptor.effect,
                approval_policy: descriptor.approval_policy,
                connector_binding: request.connector_binding.clone(),
            },
        )
        .await
        .map_err(store_error)?;
    if runtime_binding.capability_revision != request.lease.capability_revision {
        return Err(store_error(StoreError::RuntimeBindingUnauthorized));
    }
    if runtime_binding.capability_type != declared_type
        && !(runtime_binding.capability_type == "connector"
            && descriptor
                .tags
                .iter()
                .any(|tag| tag == "binding-type:connector"))
    {
        return Err(store_error(StoreError::RuntimeBindingUnauthorized));
    }
    verify_descriptor_against_runtime_binding(
        &descriptor,
        &runtime_binding,
        request.connector_binding.as_ref(),
    )
    .map_err(store_error)?;
    let arguments = request.arguments;
    if descriptor.id == "search_knowledge_base"
        && let Some(requested_datasets) = arguments.get("dataset_ids")
    {
        let requested_datasets = requested_datasets.as_array().ok_or_else(|| {
            error(
                StatusCode::BAD_REQUEST,
                "capability_arguments_invalid",
                "dataset_ids must be an array",
            )
        })?;
        if requested_datasets.iter().any(|dataset| {
            dataset
                .as_str()
                .is_none_or(|dataset| !runtime_binding.bound_dataset_ids.contains(dataset))
        }) {
            return Err(error(
                StatusCode::FORBIDDEN,
                "runtime_resource_scope_mismatch",
                "requested datasets are outside the Runtime snapshot",
            ));
        }
    }
    validate_json_value(&descriptor.input_schema, &arguments).map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "capability_arguments_invalid",
            "capability arguments do not match the bound schema",
        )
    })?;
    if request.lease.effect != descriptor.effect {
        return Err(error(
            StatusCode::CONFLICT,
            "capability_binding_changed",
            "capability binding changed",
        ));
    }
    if descriptor.id == "update_user_memory" && runtime_binding.memory_policy.is_none() {
        return Err(store_error(StoreError::RuntimeBindingUnauthorized));
    }
    if let Some(policy) = &runtime_binding.memory_policy {
        policy
            .validate()
            .map_err(|_| store_error(StoreError::RuntimeBindingUnauthorized))?;
    }

    let execution_id = Uuid::now_v7().to_string();
    let events_url = format!("/internal/v2/capabilities/executions/{execution_id}/events");
    let execution = CapabilityExecutionV2 {
        schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.to_string(),
        execution_id: execution_id.clone(),
        lease_id: request.lease.lease_id.clone(),
        tenant_id: request.lease.tenant_id.clone(),
        user_id: request.lease.user_id.clone(),
        session_id: request.lease.session_id.clone(),
        run_id: request.lease.run_id.clone(),
        tool_call_id: request.lease.tool_call_id.clone(),
        attempt_id: request.lease.attempt_id.clone(),
        capability_id: request.lease.capability_id.clone(),
        capability_revision: request.lease.capability_revision,
        arguments_hash: request.lease.arguments_hash.clone(),
        idempotency_key: request.idempotency_key,
        effect: request.lease.effect,
        status: CapabilityExecutionStatus::Published,
        events_url,
        result: None,
        error: None,
    };
    execution.validate().map_err(|_| {
        error(
            StatusCode::BAD_REQUEST,
            "execution_request_invalid",
            "execution request is invalid",
        )
    })?;
    let mut resource_binding = resource_binding_value(
        &runtime_binding, &scope, &request.lease.run_id, &descriptor,
    );
    if let Some(active) = &state.plugin_activation
        && active.managed.contains(&descriptor.id)
    {
        resource_binding["plugin_activation"] = serde_json::json!({
            "activation_id": active.receipt.activation_id,
            "generation": active.receipt.generation,
            "catalog_sha256": active.receipt.catalog_sha256,
            "capability_version": descriptor.version,
        });
    }
    // The store invokes this only for a new reservation, under its owner and
    // identity locks. A duplicate/conflict never consumes admission capacity.
    let class = super::pools::ExecutionClass::for_descriptor(&descriptor);
    let target = super::pools::write_target(&descriptor, &scope, &resource_binding);
    let mut ticket = None;
    let mut admit = || {
        ticket = Some(state.execution_pools.admit(class, &scope.tenant_id, target.clone())
            .map_err(|_| StoreError::CapacityExhausted)?);
        Ok(())
    };
    let outcome = state
        .store
        .reserve_admitted_owned(
            NewExecution {
                execution,
                arguments,
                resource_binding,
                approval_policy: descriptor.approval_policy,
                approval_id: request.lease.approval_id,
                approval_status: if matches!(descriptor.effect, CapabilityEffect::Read) {
                    "not_required".to_string()
                } else {
                    "approved".to_string()
                },
            },
            headers
                .get("x-runtime-execution-owner")
                .and_then(|value| value.to_str().ok()),
            headers
                .get("x-runtime-execution-fence")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok()),
            &mut admit,
        )
        .await
        .map_err(store_error)?;
    drop(admit);
    let actual_id = outcome.record.execution.execution_id.clone();
    if outcome.created {
        let published_event_id = event_id(&actual_id, 1)?;
        state
            .store
            .append_event(
                &scope,
                &actual_id,
                &published_event_id,
                "published",
                CapabilityExecutionStatus::Published,
                BTreeMap::new(),
                None,
            )
            .await
            .map_err(store_error)?;
    }

    if outcome.created && !outcome.record.execution.status.is_terminal() {
        state.register_cancellation(&actual_id).await;
        tokio::spawn(trace_context.scope(run_execution(
            state.clone(),
            scope,
            actual_id.clone(),
            outcome.record.execution.capability_id.clone(),
            outcome.record.arguments.clone(),
            descriptor,
            ticket.expect("new committed reservation has an admission ticket"),
        )));
    }
    let current = state
        .store
        .get(&outcome.record.execution.scope(), &actual_id)
        .await;
    let response = current.unwrap_or(outcome.record).execution;
    Ok((
        if outcome.created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(response),
    ))
}

pub(super) fn lease_for_record(execution: &CapabilityExecutionV2) -> RuntimeCapabilityLeaseV1 {
    let issued = now_epoch_ms();
    RuntimeCapabilityLeaseV1 {
        schema_version: ai_platform_capability_contract::RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION
            .into(),
        lease_id: execution.lease_id.clone(),
        tenant_id: execution.tenant_id.clone(),
        user_id: execution.user_id.clone(),
        session_id: execution.session_id.clone(),
        run_id: execution.run_id.clone(),
        tool_call_id: execution.tool_call_id.clone(),
        attempt_id: execution.attempt_id.clone(),
        capability_id: execution.capability_id.clone(),
        capability_revision: execution.capability_revision,
        arguments_hash: execution.arguments_hash.clone(),
        effect: execution.effect,
        approval_id: None,
        issued_at_epoch_ms: issued,
        expires_at_epoch_ms: issued
            .saturating_add(ai_platform_capability_contract::MAX_CAPABILITY_LEASE_TTL_MS),
        nonce: execution.execution_id.clone(),
        signature: String::new(),
    }
}

pub(super) async fn get_execution(
    State(state): State<WorkerState>,
    headers: HeaderMap,
    Path(execution_id): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    let scope = authorize(&headers, &state)?;
    validate_execution_id(&execution_id)?;
    let execution = state
        .store
        .get(&scope, &execution_id)
        .await
        .map_err(store_error)?;
    Ok(Json(execution.execution))
}

pub(super) async fn cancel_execution(
    State(state): State<WorkerState>,
    headers: HeaderMap,
    Path(execution_path): Path<String>,
) -> Result<impl IntoResponse, HttpError> {
    let scope = authorize(&headers, &state)?;
    let execution_id = execution_path.strip_suffix(":cancel").ok_or_else(|| {
        error(
            StatusCode::BAD_REQUEST,
            "execution_id_invalid",
            "cancel path is invalid",
        )
    })?;
    validate_execution_id(execution_id)?;
    let cancelled = state
        .store
        .cancel_owned(
            &scope,
            execution_id,
            headers
                .get("x-runtime-execution-owner")
                .and_then(|value| value.to_str().ok()),
            headers
                .get("x-runtime-execution-fence")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse().ok()),
        )
        .await
        .map_err(store_error)?;
    state.request_cancel(execution_id).await;
    Ok(Json(cancelled.execution))
}
