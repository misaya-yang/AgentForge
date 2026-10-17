use super::*;
fn pools() -> ExecutionPools {
    let limit = PoolLimit {
        concurrency: 1,
        queue_capacity: 3,
    };
    ExecutionPools::new(ExecutionPoolConfig {
        reads: limit,
        writes: limit,
        office: limit,
        sandbox: limit,
    })
    .unwrap()
}
fn granted(ticket: &mut ExecutionTicket) -> bool {
    ticket.ready.try_recv().is_ok()
}

#[test]
fn classes_have_independent_capacity_and_full_queue_rejects() {
    let pools = pools();
    let mut python = pools.admit(ExecutionClass::Sandbox, "a", None).unwrap();
    assert!(granted(&mut python));
    let _q1 = pools.admit(ExecutionClass::Sandbox, "a", None).unwrap();
    let _q2 = pools.admit(ExecutionClass::Sandbox, "a", None).unwrap();
    let _q3 = pools.admit(ExecutionClass::Sandbox, "a", None).unwrap();
    assert!(pools.admit(ExecutionClass::Sandbox, "b", None).is_err());
    let mut read = pools.admit(ExecutionClass::Read, "b", None).unwrap();
    assert!(granted(&mut read));
}

#[test]
fn drr_alternates_tenants_and_does_not_drain_one_backlog() {
    let pools = pools();
    let running = pools.admit(ExecutionClass::Read, "a", None).unwrap();
    let mut a1 = pools.admit(ExecutionClass::Read, "a", None).unwrap();
    let mut a2 = pools.admit(ExecutionClass::Read, "a", None).unwrap();
    let mut b1 = pools.admit(ExecutionClass::Read, "b", None).unwrap();
    drop(running);
    assert!(granted(&mut a1));
    assert!(!granted(&mut b1));
    drop(a1);
    assert!(granted(&mut b1));
    assert!(!granted(&mut a2));
    drop(b1);
    assert!(granted(&mut a2));
}

#[test]
fn cancellation_returns_queue_and_running_capacity() {
    let pools = pools();
    let running = pools.admit(ExecutionClass::Read, "a", None).unwrap();
    let queued = pools.admit(ExecutionClass::Read, "a", None).unwrap();
    let mut next = pools.admit(ExecutionClass::Read, "b", None).unwrap();
    drop(queued);
    drop(running);
    assert!(granted(&mut next));
    drop(next);
    let state = pools.0.lock().unwrap();
    assert!(state.entries.is_empty());
    assert!(state.targets.is_empty());
    assert_eq!(state.pools[&ExecutionClass::Read].running, 0);
    assert_eq!(state.pools[&ExecutionClass::Read].waiting, 0);
}

#[test]
fn target_lock_crosses_pools_and_blocked_head_does_not_block_other_target() {
    let pools = pools();
    let office = pools
        .admit(ExecutionClass::Office, "a", Some("a/page".into()))
        .unwrap();
    let mut blocked = pools
        .admit(ExecutionClass::Write, "a", Some("a/page".into()))
        .unwrap();
    let mut other = pools
        .admit(ExecutionClass::Write, "a", Some("a/other".into()))
        .unwrap();
    assert!(!granted(&mut blocked));
    assert!(granted(&mut other));
    drop(other);
    assert!(!granted(&mut blocked));
    drop(office);
    assert!(granted(&mut blocked));
}

#[test]
fn zero_concurrency_is_rejected() {
    let mut config = ExecutionPoolConfig::default();
    config.reads.concurrency = 0;
    assert!(ExecutionPools::new(config).is_err());
}
#[test]
fn classifies_builtin_python_and_office_before_read_effect() {
    let registry = crate::platform_catalog::worker_capability_catalog_with_writes(true).unwrap();
    assert_eq!(
        ExecutionClass::for_descriptor(&registry["execute_python_code"]),
        ExecutionClass::Sandbox
    );
    assert_eq!(
        ExecutionClass::for_descriptor(&registry["mcp_docgen__preview_document"]),
        ExecutionClass::Office
    );
    assert_eq!(
        ExecutionClass::for_descriptor(&registry["todo_write"]),
        ExecutionClass::Write
    );
    assert_eq!(
        ExecutionClass::for_descriptor(&registry["web_fetch"]),
        ExecutionClass::Read
    );
}

#[test]
fn write_scopes_are_tenant_bound_and_ignore_untrusted_target_hints() {
    let registry = crate::platform_catalog::worker_capability_catalog_with_writes(true).unwrap();
    let scope = CapabilityScopeV2 {
        tenant_id: "a".into(),
        user_id: "u".into(),
        session_id: "s".into(),
    };
    let other = CapabilityScopeV2 {
        tenant_id: "b".into(),
        ..scope.clone()
    };
    let descriptor = &registry["confluence_write"];
    let first = write_target(descriptor, &scope, &serde_json::json!({"page_id": "1"}));
    assert_eq!(
        first,
        write_target(descriptor, &scope, &serde_json::json!({"page_id": "2"}))
    );
    assert_ne!(first, write_target(descriptor, &other, &Value::Null));
    assert!(write_target(&registry["web_fetch"], &scope, &Value::Null).is_none());
}

#[test]
fn blocked_target_queue_is_bounded_even_with_idle_slots() {
    let limit = PoolLimit {
        concurrency: 8,
        queue_capacity: 1,
    };
    let pools = ExecutionPools::new(ExecutionPoolConfig {
        reads: limit,
        writes: limit,
        office: limit,
        sandbox: limit,
    })
    .unwrap();
    let _running = pools
        .admit(ExecutionClass::Write, "a", Some("same".into()))
        .unwrap();
    let _queued = pools
        .admit(ExecutionClass::Write, "a", Some("same".into()))
        .unwrap();
    assert!(
        pools
            .admit(ExecutionClass::Write, "a", Some("same".into()))
            .is_err()
    );
    let state = pools.0.lock().unwrap();
    assert_eq!(state.pools[&ExecutionClass::Write].running, 1);
    assert_eq!(state.pools[&ExecutionClass::Write].waiting, 1);
}
use crate::{ExecutionStore, MemoryStore, NewExecution, StoreError};
use ai_platform_capability_contract::{
    ApprovalPolicy, CAPABILITY_EXECUTION_SCHEMA_VERSION, CapabilityExecutionStatus,
    CapabilityExecutionV2, canonical_json_hash,
};
use serde_json::json;
fn new_execution(id: &str) -> NewExecution {
    NewExecution {
        execution: CapabilityExecutionV2 {
            schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.to_string(),
            execution_id: id.to_string(),
            lease_id: "00000000-0000-0000-0000-000000000001".to_string(),
            tenant_id: "tenant-a".to_string(),
            user_id: "user-a".to_string(),
            session_id: "session-a".to_string(),
            run_id: "00000000-0000-0000-0000-000000000002".to_string(),
            tool_call_id: "call-a".to_string(),
            attempt_id: "attempt-a".to_string(),
            capability_id: "platform.echo".to_string(),
            capability_revision: 1,
            arguments_hash: canonical_json_hash(&serde_json::json!({})).unwrap(),
            idempotency_key: "idem-a".to_string(),
            effect: CapabilityEffect::Read,
            status: CapabilityExecutionStatus::Published,
            events_url: format!("/internal/v2/capabilities/executions/{id}/events"),
            result: None,
            error: None,
        },
        arguments: serde_json::json!({}),
        resource_binding: serde_json::json!({
            "snapshot_id": "snapshot-a",
            "capability_revision": 1,
            "capability_id": "platform.echo",
            "capability_version": "1",
            "schema_hash": "fixture",
            "bound_dataset_ids": [],
        }),
        approval_policy: ApprovalPolicy::Never,
        approval_id: None,
        approval_status: "not_required".to_string(),
    }
}

fn saturated_test_pools() -> crate::http_service::pools::ExecutionPools {
    use crate::http_service::pools::{ExecutionPoolConfig, ExecutionPools, PoolLimit};
    let limit = PoolLimit {
        concurrency: 1,
        queue_capacity: 0,
    };
    ExecutionPools::new(ExecutionPoolConfig {
        reads: limit,
        writes: limit,
        office: limit,
        sandbox: limit,
    })
    .unwrap()
}

async fn admitted_reservation(
    store: &MemoryStore,
    pools: &crate::http_service::pools::ExecutionPools,
    requested: NewExecution,
) -> (
    Result<ReserveOutcome, StoreError>,
    Option<crate::http_service::pools::ExecutionTicket>,
) {
    use crate::http_service::pools::ExecutionClass;
    let mut ticket = None;
    let mut admission = || {
        ticket = Some(
            pools
                .admit(ExecutionClass::Read, "tenant-a", None)
                .map_err(|_| StoreError::CapacityExhausted)?,
        );
        Ok(())
    };
    let result = store
        .reserve_admitted_owned(requested, None, None, &mut admission)
        .await;
    drop(admission);
    (result, ticket)
}

#[tokio::test]
async fn full_queue_duplicate_replays_original_result_without_new_admission() {
    let store = MemoryStore::default();
    let pools = saturated_test_pools();
    let (first, first_ticket) = admitted_reservation(&store, &pools, new_execution("first")).await;
    let first = first.unwrap();
    assert!(first.created);
    let fence = "dispatch-fence";
    assert!(
        store
            .dispatch(&first.record.execution.scope(), "first", fence)
            .await
            .unwrap()
            .claimed
    );
    store
        .append_event(
            &first.record.execution.scope(),
            "first",
            "terminal-event",
            "terminal",
            CapabilityExecutionStatus::Succeeded,
            BTreeMap::from([("result".into(), json!({"ok": true}))]),
            Some(fence),
        )
        .await
        .unwrap();
    let expected = store
        .get(&first.record.execution.scope(), "first")
        .await
        .unwrap();
    let (replay, ticket) = admitted_reservation(&store, &pools, new_execution("duplicate")).await;
    let replay = replay.unwrap();
    assert!(!replay.created);
    assert_eq!(replay.record.execution, expected.execution);
    assert!(ticket.is_none());
    assert!(first_ticket.is_some());
}

#[tokio::test]
async fn full_queue_new_reservation_leaves_no_recoverable_record() {
    let store = MemoryStore::default();
    let pools = saturated_test_pools();
    let (_, _running) = admitted_reservation(&store, &pools, new_execution("first")).await;
    let mut fresh = new_execution("fresh");
    fresh.execution.tool_call_id = "new-call".into();
    fresh.execution.idempotency_key = "new-key".into();
    let scope = fresh.execution.scope();
    let (result, ticket) = admitted_reservation(&store, &pools, fresh.clone()).await;
    assert_eq!(result.unwrap_err(), StoreError::CapacityExhausted);
    assert!(ticket.is_none());
    assert_eq!(
        store.get(&scope, "fresh").await.unwrap_err(),
        StoreError::NotFound
    );
    assert!(
        store
            .recoverable()
            .await
            .unwrap()
            .iter()
            .all(|record| record.execution.execution_id != "fresh")
    );
    drop(_running);
    assert!(
        admitted_reservation(&store, &pools, fresh)
            .await
            .0
            .unwrap()
            .created
    );
}

#[tokio::test]
async fn full_queue_preserves_body_and_actor_conflicts() {
    let store = MemoryStore::default();
    let pools = saturated_test_pools();
    let (_, _running) = admitted_reservation(&store, &pools, new_execution("first")).await;
    for conflict in ["body", "actor", "tenant", "lease", "approval"] {
        let mut requested = new_execution("conflicting");
        match conflict {
            "body" => requested.arguments = json!({"changed": true}),
            "actor" => requested.execution.user_id = "other-user".into(),
            "tenant" => requested.execution.tenant_id = "other-tenant".into(),
            "lease" => requested.execution.lease_id = "other-lease".into(),
            _ => requested.approval_id = Some("other-approval".into()),
        }
        let (result, ticket) = admitted_reservation(&store, &pools, requested).await;
        assert_eq!(
            result.unwrap_err(),
            StoreError::IdempotencyConflict,
            "{conflict}"
        );
        assert!(ticket.is_none());
    }
}

#[tokio::test]
async fn concurrent_same_key_gets_one_identity_one_ticket_and_one_dispatch() {
    let store = MemoryStore::default();
    let pools = saturated_test_pools();
    let (left, right) = tokio::join!(
        admitted_reservation(&store, &pools, new_execution("left")),
        admitted_reservation(&store, &pools, new_execution("right")),
    );
    let (left, left_ticket) = left;
    let (right, right_ticket) = right;
    let left = left.unwrap();
    let right = right.unwrap();
    assert_ne!(left.created, right.created);
    assert_ne!(left_ticket.is_some(), right_ticket.is_some());
    assert_eq!(
        left.record.execution.execution_id,
        right.record.execution.execution_id
    );
    let id = &left.record.execution.execution_id;
    let scope = left.record.execution.scope();
    assert!(store.dispatch(&scope, id, "winner").await.unwrap().claimed);
    let second = store.dispatch(&scope, id, "loser").await;
    assert!(second.is_err() || !second.unwrap().claimed);
}
#[tokio::test]
async fn http_duplicate_returns_existing_result_when_pool_is_full() {
    use super::super::{
        WorkerState,
        execution::{create_execution, lease_for_record},
    };
    use ai_platform_capability_contract::CreateCapabilityExecutionRequestV2;
    use axum::{
        Json,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
    };
    let store = Arc::new(MemoryStore::default());
    let mut state = WorkerState::try_new(
        store.clone(),
        "test-token".into(),
        b"test-lease-secret-at-least-thirty-two-bytes".to_vec(),
        true,
    )
    .unwrap();
    state.execution_pools = saturated_test_pools();
    let descriptor = state.capability_registry["platform.echo"].clone();
    let seed = new_execution("00000000-0000-0000-0000-000000000003");
    let scope = seed.execution.scope();
    store
        .register_runtime_binding(
            &scope,
            &seed.execution.run_id,
            crate::RuntimeCapabilityBinding {
                capability_type: "tool".into(),
                name: descriptor.name.clone(),
                snapshot_id: "snapshot-a".into(),
                capability_revision: 1,
                capability_id: descriptor.id.clone(),
                capability_version: descriptor.version.clone(),
                schema_hash: descriptor.schema_hash.clone(),
                effect: descriptor.effect,
                approval_policy: descriptor.approval_policy,
                bound_dataset_ids: Default::default(),
                connector_binding: None,
                memory_policy: None,
            },
        )
        .await;
    let mut lease = lease_for_record(&seed.execution);
    lease.sign(&state.lease_secret).unwrap();
    let request = CreateCapabilityExecutionRequestV2 {
        schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.into(),
        lease,
        idempotency_key: "idem-a".into(),
        descriptor,
        connector_binding: None,
        arguments: json!({}),
    };
    let mut headers = HeaderMap::new();
    for (key, value) in [
        ("x-ai-platform-internal-token", "test-token"),
        ("x-ai-tenant-id", "tenant-a"),
        ("x-ai-user-id", "user-a"),
        ("x-ai-session-id", "session-a"),
    ] {
        headers.insert(key, value.parse().unwrap());
    }
    let response = create_execution(State(state.clone()), headers.clone(), Json(request.clone()))
        .await
        .unwrap_or_else(|(status, _)| panic!("initial request: {status}"))
        .into_response();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    let first: CapabilityExecutionV2 = serde_json::from_slice(&bytes).unwrap();
    let mut terminal = None;
    for _ in 0..100 {
        let current = store.get(&scope, &first.execution_id).await.unwrap();
        if current.execution.status.is_terminal() {
            terminal = Some(current.execution);
            break;
        }
        tokio::task::yield_now().await;
    }
    let expected = terminal.expect("fixture runner completes");
    assert_eq!(expected.status, CapabilityExecutionStatus::Succeeded);
    let _full = state
        .execution_pools
        .admit(ExecutionClass::Read, "other-tenant", None)
        .unwrap();
    let response = create_execution(State(state.clone()), headers.clone(), Json(request.clone()))
        .await
        .unwrap_or_else(|(status, _)| panic!("duplicate request: {status}"))
        .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<CapabilityExecutionV2>(&bytes).unwrap(),
        expected
    );
    let mut conflict = request.clone();
    conflict.arguments = json!({"changed": true});
    conflict.lease.arguments_hash = canonical_json_hash(&conflict.arguments).unwrap();
    conflict.lease.sign(&state.lease_secret).unwrap();
    let failure = create_execution(State(state.clone()), headers.clone(), Json(conflict)).await;
    assert!(matches!(failure, Err((StatusCode::CONFLICT, _))));
    let mut fresh = request;
    fresh.idempotency_key = "fresh-key".into();
    fresh.lease.tool_call_id = "fresh-call".into();
    fresh.lease.sign(&state.lease_secret).unwrap();
    let failure = create_execution(State(state), headers, Json(fresh)).await;
    match failure {
        Err((StatusCode::SERVICE_UNAVAILABLE, Json(body))) => {
            assert_eq!(body.error, "capacity_exhausted");
            assert_eq!(body.retryable, Some(true));
        }
        _ => panic!("new request must receive retryable capacity_exhausted"),
    }
    assert_eq!(store.recoverable().await.unwrap().len(), 0);
}
