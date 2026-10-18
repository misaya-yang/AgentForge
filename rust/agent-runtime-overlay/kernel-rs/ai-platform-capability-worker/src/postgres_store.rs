//! PostgreSQL authority for Capability Contract V2 executions.

use std::collections::BTreeMap;

use ai_platform_capability_contract::{
    ApprovalPolicy, CAPABILITY_EVENT_SCHEMA_VERSION, CAPABILITY_EXECUTION_SCHEMA_VERSION,
    CapabilityEffect, CapabilityEventPageV2, CapabilityEventV2, CapabilityExecutionStatus,
    CapabilityExecutionV2, CapabilityScopeV2,
};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    CapabilityIdentity, DispatchOutcome, EVENT_PAGE_SIZE, ExecutionRecord, ExecutionStore,
    MAX_EVENT_PAYLOAD_BYTES, NewExecution, ReserveOutcome, RuntimeCapabilityBinding,
    RuntimeConnectorBinding, StoreError,
};

const AUTHORIZE_RUNTIME_BINDING_SQL: &str = r#"
SELECT s.snapshot_id,
       s.capability_revision,
       COALESCE(
           ARRAY(
               SELECT DISTINCT item->'payload'->>'dataset_id'
               FROM jsonb_array_elements(
                   CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,items}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,items}'
                       ELSE '[]'::jsonb
                   END
               ) AS item
               WHERE item->>'kind' = 'knowledge'
                 AND item->>'tenant_id' = s.tenant_id
                 AND item->>'capability_revision' = s.capability_revision::text
                 AND NULLIF(item->'payload'->>'dataset_id', '') IS NOT NULL
           ),
           ARRAY[]::text[]
       ) AS bound_dataset_ids,
       (
           SELECT capability->>'type'
           FROM jsonb_array_elements(
               s.snapshot #> '{readonly_capabilities,capability_allowlist}'
           ) AS capability
           WHERE capability->>'name' = $6
             AND capability->>'id' = $7
             AND COALESCE(capability->>'version', 'null') = $8
             AND capability->>'schema_hash' = $9
       ) AS matched_capability_type,
       COALESCE(
           (
               SELECT capability->'connector_binding'
               FROM jsonb_array_elements(
                   s.snapshot #> '{readonly_capabilities,capability_allowlist}'
               ) AS capability
               WHERE capability->>'name' = $6
                 AND capability->>'id' = $7
                 AND COALESCE(capability->>'version', 'null') = $8
                 AND capability->>'schema_hash' = $9
           ),
           'null'::jsonb
       ) AS connector_binding,
       COALESCE(
           (
               SELECT CASE
                   WHEN capability->>'read_only' = 'true' THEN 'read'
                   WHEN capability->>'effect' IN ('read', 'write', 'unknown')
                       THEN capability->>'effect'
                   WHEN capability->>'approval_required' = 'false' THEN 'read'
                   ELSE 'write'
               END
               FROM jsonb_array_elements(
                   (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,tools}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,tools}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,mcp}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,mcp}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,deferred}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,deferred}'
                       ELSE '[]'::jsonb
                   END)
               ) AS capability
               WHERE capability->>'name' = $6
                 AND capability->>'id' = $7
                 AND COALESCE(capability->>'version', 'null') = $8
                 AND capability->>'schema_hash' = $9
               LIMIT 1
           ),
           'unknown'
       ) AS matched_effect,
       COALESCE(
           (
               SELECT CASE
                   WHEN capability->>'approval_policy' IN ('never', 'on_request', 'always')
                       THEN capability->>'approval_policy'
                   WHEN capability->>'requires_confirmation' = 'true'
                     OR capability->>'approval_required' = 'true'
                       THEN 'always'
                   ELSE 'never'
               END
               FROM jsonb_array_elements(
                   (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,tools}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,tools}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,mcp}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,mcp}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,deferred}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,deferred}'
                       ELSE '[]'::jsonb
                   END)
               ) AS capability
               WHERE capability->>'name' = $6
                 AND capability->>'id' = $7
                 AND COALESCE(capability->>'version', 'null') = $8
                 AND capability->>'schema_hash' = $9
               LIMIT 1
           ),
           'never'
       ) AS matched_approval_policy,
       COALESCE(s.snapshot #> '{memory,policy}', 'null'::jsonb) AS memory_policy
FROM assistant_runtime_snapshots AS s
JOIN assistant_runtime_model_leases AS l
  ON l.snapshot_id = s.snapshot_id
 AND l.run_id = s.run_id
 AND l.tenant_id = s.tenant_id
 AND l.user_id = s.user_id
 AND l.session_id = s.session_id
 AND l.capability_revision = s.capability_revision
JOIN assistant_runs AS r
  ON r.run_id = s.run_id
 AND r.tenant_id = s.tenant_id
 AND r.user_id = s.user_id
 AND r.session_id = s.session_id
LEFT JOIN assistant_runtime_snapshot_revocations AS rev
  ON rev.snapshot_id = s.snapshot_id
 AND rev.tenant_id = s.tenant_id
 AND rev.user_id = s.user_id
 AND rev.session_id = s.session_id
WHERE s.run_id = $1
  AND s.tenant_id = $2
  AND s.user_id = $3
  AND s.session_id = $4
  AND s.capability_revision = $5
  AND r.status = 'running'
  AND r.engine = 'agent_runtime'
  AND l.status = 'active'
  AND l.expires_at > NOW()
  AND (s.expires_at IS NULL OR s.expires_at > NOW())
  AND rev.snapshot_id IS NULL
  AND jsonb_typeof(s.snapshot #> '{readonly_capabilities,capability_allowlist}') = 'array'
  AND (
      SELECT capability->>'type'
      FROM jsonb_array_elements(
          s.snapshot #> '{readonly_capabilities,capability_allowlist}'
      ) AS capability
      WHERE capability->>'name' = $6
        AND capability->>'id' = $7
        AND COALESCE(capability->>'version', 'null') = $8
        AND capability->>'schema_hash' = $9
  ) IS NOT NULL
FOR UPDATE OF s
"#;

// SDK authority is a separate source. The agent_runtime SQL above is unchanged.
const AUTHORIZE_SDK_BINDING_SQL: &str = r#"
SELECT s.snapshot_id,
       s.capability_revision,
       COALESCE(
           ARRAY(
               SELECT DISTINCT item->'payload'->>'dataset_id'
               FROM jsonb_array_elements(
                   CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,items}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,items}'
                       ELSE '[]'::jsonb
                   END
               ) AS item
               WHERE item->>'kind' = 'knowledge'
                 AND item->>'tenant_id' = s.tenant_id
                 AND item->>'capability_revision' = s.capability_revision::text
                 AND NULLIF(item->'payload'->>'dataset_id', '') IS NOT NULL
           ),
           ARRAY[]::text[]
       ) AS bound_dataset_ids,
       (
           SELECT capability->>'type'
           FROM jsonb_array_elements(
               s.snapshot #> '{readonly_capabilities,capability_allowlist}'
           ) AS capability
           WHERE capability->>'name' = $6
             AND capability->>'id' = $7
             AND COALESCE(capability->>'version', 'null') = $8
             AND capability->>'schema_hash' = $9
       ) AS matched_capability_type,
       COALESCE(
           (
               SELECT capability->'connector_binding'
               FROM jsonb_array_elements(
                   s.snapshot #> '{readonly_capabilities,capability_allowlist}'
               ) AS capability
               WHERE capability->>'name' = $6
                 AND capability->>'id' = $7
                 AND COALESCE(capability->>'version', 'null') = $8
                 AND capability->>'schema_hash' = $9
           ),
           'null'::jsonb
       ) AS connector_binding,
       COALESCE(
           (
               SELECT CASE
                   WHEN COALESCE(capability->>'effect', capability->>'operation_kind') IN ('write', 'unknown')
                       THEN COALESCE(capability->>'effect', capability->>'operation_kind')
                   WHEN capability->>'read_only' = 'true' THEN 'read'
                   ELSE 'write'
               END
               FROM jsonb_array_elements(
                   (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,tools}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,tools}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,mcp}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,mcp}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,deferred}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,deferred}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,attachment_tools}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,attachment_tools}'
                       ELSE '[]'::jsonb
                   END)
               ) AS capability
               WHERE capability->>'name' = $6
                 AND capability->>'id' = $7
                 AND COALESCE(capability->>'version', 'null') = $8
                 AND capability->>'schema_hash' = $9
               LIMIT 1
           ),
           'unknown'
       ) AS matched_effect,
       COALESCE(
           (
               SELECT CASE
                   WHEN COALESCE(capability->>'approval_policy', capability->>'approval') IN ('on_request', 'always')
                       THEN COALESCE(capability->>'approval_policy', capability->>'approval')
                   WHEN capability->>'read_only' = 'true'
                     AND COALESCE(capability->>'effect', capability->>'operation_kind', 'read') NOT IN ('write', 'unknown') THEN 'never'
                   ELSE 'always'
               END
               FROM jsonb_array_elements(
                   (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,tools}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,tools}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,mcp}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,mcp}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,deferred}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,deferred}'
                       ELSE '[]'::jsonb
                   END)
                   || (CASE
                       WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,attachment_tools}') = 'array'
                           THEN s.snapshot #> '{readonly_capabilities,attachment_tools}'
                       ELSE '[]'::jsonb
                   END)
               ) AS capability
               WHERE capability->>'name' = $6
                 AND capability->>'id' = $7
                 AND COALESCE(capability->>'version', 'null') = $8
                 AND capability->>'schema_hash' = $9
               LIMIT 1
           ),
           'never'
       ) AS matched_approval_policy,
       COALESCE(s.snapshot #> '{memory,policy}', 'null'::jsonb) AS memory_policy
FROM assistant.assistant_sdk_runs AS s
JOIN assistant_runs AS r
  ON r.run_id = s.run_id AND r.tenant_id = s.tenant_id
 AND r.user_id = s.user_id AND r.session_id = s.session_id
WHERE s.run_id = $1 AND s.tenant_id = $2 AND s.user_id = $3
  AND s.session_id = $4 AND s.capability_revision = $5
  AND s.engine IN ('openai_agents', 'claude_agent_sdk')
  AND r.engine = s.engine AND r.status = 'running'
  AND r.harness_thread_id IS NULL AND r.harness_turn_id IS NULL
  AND s.revoked_at IS NULL AND s.expires_at > clock_timestamp()
  AND s.lease_until > clock_timestamp() AND s.fence > 0
  AND s.snapshot #>> '{readonly_capabilities,tenant_id}' = s.tenant_id
  AND s.snapshot #>> '{readonly_capabilities,capability_revision}' = s.capability_revision::text
  AND jsonb_typeof(s.snapshot #> '{readonly_capabilities,capability_allowlist}') = 'array'
  AND (
      SELECT COUNT(*) FROM jsonb_array_elements(
          s.snapshot #> '{readonly_capabilities,capability_allowlist}'
      ) AS capability
      WHERE capability->>'name' = $6 AND capability->>'id' = $7
        AND COALESCE(capability->>'version', 'null') = $8
        AND capability->>'schema_hash' = $9
  ) = 1
  AND (
      SELECT COUNT(*) FROM jsonb_array_elements(
          COALESCE(s.snapshot #> '{readonly_capabilities,tools}', '[]'::jsonb)
          || COALESCE(s.snapshot #> '{readonly_capabilities,mcp}', '[]'::jsonb)
          || COALESCE(s.snapshot #> '{readonly_capabilities,deferred}', '[]'::jsonb)
          || COALESCE(s.snapshot #> '{readonly_capabilities,attachment_tools}', '[]'::jsonb)
      ) AS capability
      WHERE capability->>'name' = $6 AND capability->>'id' = $7
        AND COALESCE(capability->>'version', 'null') = $8
        AND capability->>'schema_hash' = $9
        AND capability->>'tenant_id' = s.tenant_id
        AND capability->>'capability_revision' = s.capability_revision::text
  ) = 1
"#;

const RECOVERABLE_EXECUTIONS_SQL: &str = r#"
SELECT e.*
FROM assistant_capability_executions AS e
WHERE (
    (
        (
            (e.effect = 'read' AND e.status IN ('published', 'dispatched', 'running'))
            OR (
                e.effect IN ('write', 'unknown')
                AND e.status = 'published'
                AND e.dispatch_fence IS NULL
            )
        )
        AND (EXISTS (
            SELECT 1
            FROM assistant_runs AS r
            JOIN assistant_runtime_model_leases AS l
              ON l.run_id = r.run_id
             AND l.tenant_id = r.tenant_id
             AND l.user_id = r.user_id
             AND l.session_id = r.session_id
            JOIN assistant_runtime_snapshots AS s
              ON s.snapshot_id = l.snapshot_id
             AND s.run_id = r.run_id
             AND s.tenant_id = r.tenant_id
             AND s.user_id = r.user_id
             AND s.session_id = r.session_id
            LEFT JOIN assistant_runtime_snapshot_revocations AS x
              ON x.snapshot_id = s.snapshot_id
             AND x.tenant_id = s.tenant_id
             AND x.user_id = s.user_id
             AND x.session_id = s.session_id
            WHERE r.run_id = e.run_id
              AND r.tenant_id = e.tenant_id
              AND r.user_id = e.user_id
              AND r.session_id = e.session_id
              AND r.status = 'running'
              AND r.engine = 'agent_runtime'
              AND l.status = 'active'
              AND l.expires_at > NOW()
              AND (s.expires_at IS NULL OR s.expires_at > NOW())
              AND x.snapshot_id IS NULL
        ) OR EXISTS (
            SELECT 1 FROM assistant.assistant_sdk_runs AS s
            JOIN assistant_runs AS r ON r.run_id=s.run_id
              AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.session_id=s.session_id
            WHERE s.run_id=e.run_id AND s.tenant_id=e.tenant_id
              AND s.user_id=e.user_id AND s.session_id=e.session_id
              AND s.engine IN ('openai_agents','claude_agent_sdk')
              AND r.engine=s.engine AND r.status='running'
              AND r.harness_thread_id IS NULL AND r.harness_turn_id IS NULL
              AND s.revoked_at IS NULL AND s.expires_at > clock_timestamp()
              AND s.lease_until > clock_timestamp() AND s.fence > 0
              AND s.capability_revision=e.capability_revision
              AND s.snapshot_id::text=e.resource_binding->>'snapshot_id'
        ))
    )
    OR (
        e.effect IN ('write', 'unknown')
        AND e.status IN ('dispatched', 'running')
        AND e.dispatch_fence IS NOT NULL
        AND (e.worker_lease_until IS NULL OR e.worker_lease_until <= NOW())
    )
)
ORDER BY e.created_at
LIMIT 256
"#;

#[derive(Clone)]
pub struct PostgresExecutionStore {
    pool: PgPool,
}

impl PostgresExecutionStore {
    async fn tenant_controls_enabled(&self) -> Result<bool, StoreError> {
        ai_platform_capability_contract::database_connection::tenant_controls_enabled(
            &self.pool,
            "assistant.assistant_capability_executions",
            "assistant.recoverable_capability_executions_v1()",
        )
        .await
        .map_err(database_error)
    }

    async fn begin_tenant(
        &self,
        tenant: &str,
    ) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, StoreError> {
        let mut transaction = self.pool.begin().await.map_err(database_error)?;
        ai_platform_capability_contract::database_connection::bind_tenant(
            &mut transaction, tenant,
            ai_platform_capability_contract::database_connection::policy::DatabaseRole::CapabilityWorker,
        ).await.map_err(database_error)?;
        Ok(transaction)
    }

    // The workspace lint protects SQLite callers; this store is explicitly PostgreSQL.
    #[allow(clippy::disallowed_methods)]
    pub async fn connect_with_options(
        options: PgConnectOptions,
        max_connections: u32,
    ) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(max_connections.clamp(1, 20))
            .connect_with(options)
            .await?;
        Ok(Self::from_pool(pool))
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Clone the worker's single pool handle for a read adapter.  `PgPool`
    /// clones share the same pool and do not create another pool.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }
}

fn status_name(status: CapabilityExecutionStatus) -> &'static str {
    match status {
        CapabilityExecutionStatus::Published => "published",
        CapabilityExecutionStatus::AwaitingApproval => "awaiting_approval",
        CapabilityExecutionStatus::Dispatched => "dispatched",
        CapabilityExecutionStatus::Running => "running",
        CapabilityExecutionStatus::Succeeded => "succeeded",
        CapabilityExecutionStatus::Failed => "failed",
        CapabilityExecutionStatus::Cancelled => "cancelled",
        CapabilityExecutionStatus::Timeout => "timeout",
        CapabilityExecutionStatus::SideEffectUnknown => "side_effect_unknown",
    }
}

fn parse_status(value: &str) -> Result<CapabilityExecutionStatus, StoreError> {
    match value {
        "published" => Ok(CapabilityExecutionStatus::Published),
        "awaiting_approval" => Ok(CapabilityExecutionStatus::AwaitingApproval),
        "dispatched" => Ok(CapabilityExecutionStatus::Dispatched),
        "running" => Ok(CapabilityExecutionStatus::Running),
        "succeeded" => Ok(CapabilityExecutionStatus::Succeeded),
        "failed" => Ok(CapabilityExecutionStatus::Failed),
        "cancelled" => Ok(CapabilityExecutionStatus::Cancelled),
        "timeout" => Ok(CapabilityExecutionStatus::Timeout),
        "side_effect_unknown" => Ok(CapabilityExecutionStatus::SideEffectUnknown),
        _ => Err(StoreError::Internal),
    }
}

fn parse_effect(value: &str) -> Result<CapabilityEffect, StoreError> {
    match value {
        "read" => Ok(CapabilityEffect::Read),
        "write" => Ok(CapabilityEffect::Write),
        "unknown" => Ok(CapabilityEffect::Unknown),
        _ => Err(StoreError::Internal),
    }
}

fn row_to_event(row: &PgRow) -> Result<CapabilityEventV2, StoreError> {
    let payload = row
        .try_get::<Value, _>("payload")
        .map_err(|_| StoreError::Internal)?;
    Ok(CapabilityEventV2 {
        schema_version: CAPABILITY_EVENT_SCHEMA_VERSION.to_string(),
        execution_id: row
            .try_get::<Uuid, _>("execution_id")
            .map_err(|_| StoreError::Internal)?
            .to_string(),
        tool_call_id: row
            .try_get("tool_call_id")
            .map_err(|_| StoreError::Internal)?,
        sequence: row
            .try_get::<i64, _>("sequence")
            .map_err(|_| StoreError::Internal)?
            .try_into()
            .map_err(|_| StoreError::Internal)?,
        event: row.try_get("event").map_err(|_| StoreError::Internal)?,
        status: parse_status(
            row.try_get::<String, _>("status")
                .map_err(|_| StoreError::Internal)?
                .as_str(),
        )?,
        payload: serde_json::from_value(payload).map_err(|_| StoreError::Internal)?,
        created_at_epoch_ms: row
            .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .map_err(|_| StoreError::Internal)?
            .timestamp_millis()
            .max(0) as u64,
    })
}

fn parse_approval_policy(value: &str) -> Result<ApprovalPolicy, StoreError> {
    match value {
        "never" => Ok(ApprovalPolicy::Never),
        "on_request" => Ok(ApprovalPolicy::OnRequest),
        "always" => Ok(ApprovalPolicy::Always),
        _ => Err(StoreError::Internal),
    }
}

fn approval_policy_name(value: ApprovalPolicy) -> &'static str {
    match value {
        ApprovalPolicy::Never => "never",
        ApprovalPolicy::OnRequest => "on_request",
        ApprovalPolicy::Always => "always",
    }
}

fn effect_name(value: CapabilityEffect) -> &'static str {
    match value {
        CapabilityEffect::Read => "read",
        CapabilityEffect::Write => "write",
        CapabilityEffect::Unknown => "unknown",
    }
}

fn uuid(value: &str) -> Result<Uuid, StoreError> {
    Uuid::parse_str(value).map_err(|_| StoreError::Internal)
}

fn row_to_record(row: &PgRow) -> Result<ExecutionRecord, StoreError> {
    let status = parse_status(
        row.try_get::<String, _>("status")
            .map_err(|_| StoreError::Internal)?
            .as_str(),
    )?;
    let result = row
        .try_get::<Option<Value>, _>("result_summary")
        .map_err(|_| StoreError::Internal)?;
    let error = row
        .try_get::<Option<String>, _>("error_code")
        .map_err(|_| StoreError::Internal)?;
    let execution = CapabilityExecutionV2 {
        schema_version: CAPABILITY_EXECUTION_SCHEMA_VERSION.to_string(),
        execution_id: row
            .try_get::<Uuid, _>("execution_id")
            .map_err(|_| StoreError::Internal)?
            .to_string(),
        lease_id: row
            .try_get::<Uuid, _>("lease_id")
            .map_err(|_| StoreError::Internal)?
            .to_string(),
        tenant_id: row.try_get("tenant_id").map_err(|_| StoreError::Internal)?,
        user_id: row.try_get("user_id").map_err(|_| StoreError::Internal)?,
        session_id: row
            .try_get("session_id")
            .map_err(|_| StoreError::Internal)?,
        run_id: row
            .try_get::<Uuid, _>("run_id")
            .map_err(|_| StoreError::Internal)?
            .to_string(),
        tool_call_id: row
            .try_get("tool_call_id")
            .map_err(|_| StoreError::Internal)?,
        attempt_id: row
            .try_get("attempt_id")
            .map_err(|_| StoreError::Internal)?,
        capability_id: row
            .try_get("capability_id")
            .map_err(|_| StoreError::Internal)?,
        capability_revision: row
            .try_get::<i64, _>("capability_revision")
            .map_err(|_| StoreError::Internal)?
            .try_into()
            .map_err(|_| StoreError::Internal)?,
        arguments_hash: format!(
            "sha256:{}",
            row.try_get::<String, _>("arguments_sha256")
                .map_err(|_| StoreError::Internal)?
                .trim_end()
        ),
        idempotency_key: row
            .try_get("idempotency_key")
            .map_err(|_| StoreError::Internal)?,
        effect: parse_effect(
            row.try_get::<String, _>("effect")
                .map_err(|_| StoreError::Internal)?
                .as_str(),
        )?,
        status,
        events_url: row
            .try_get("events_url")
            .map_err(|_| StoreError::Internal)?,
        result: if status.is_terminal() {
            result.clone()
        } else {
            None
        },
        error: if status.is_terminal() { error } else { None },
    };
    execution.validate().map_err(|_| StoreError::Internal)?;
    Ok(ExecutionRecord {
        execution,
        arguments: row.try_get("arguments").map_err(|_| StoreError::Internal)?,
        resource_binding: row
            .try_get("resource_binding")
            .map_err(|_| StoreError::Internal)?,
        result_summary: result,
        approval_policy: parse_approval_policy(
            row.try_get::<String, _>("approval_policy")
                .map_err(|_| StoreError::Internal)?
                .as_str(),
        )?,
        approval_id: row
            .try_get::<Option<Uuid>, _>("approval_id")
            .map_err(|_| StoreError::Internal)?
            .map(|value| value.to_string()),
        approval_status: row
            .try_get("approval_status")
            .map_err(|_| StoreError::Internal)?,
        dispatch_fence: row
            .try_get::<Option<Uuid>, _>("dispatch_fence")
            .map_err(|_| StoreError::Internal)?
            .map(|value| value.to_string()),
        worker_lease_until_epoch_ms: row
            .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("worker_lease_until")
            .map_err(|_| StoreError::Internal)?
            .map(|value| value.timestamp_millis().max(0) as u64),
        last_sequence: row
            .try_get::<i64, _>("last_event_sequence")
            .map_err(|_| StoreError::Internal)?
            .try_into()
            .map_err(|_| StoreError::Internal)?,
    })
}

fn database_error(error: sqlx::Error) -> StoreError {
    let Some(database) = error.as_database_error() else {
        return StoreError::Internal;
    };
    match database.code().as_deref() {
        Some("23505") => StoreError::IdempotencyConflict,
        Some("42501") if database.message().contains("APPROVAL") => StoreError::ApprovalRequired,
        Some("42501") if database.message().contains("FENCE_MISMATCH") => {
            StoreError::DispatchFenceMismatch
        }
        Some("42501") => StoreError::ScopeMismatch,
        Some("22023") if database.message().contains("EVENT_INVALID") => StoreError::InvalidEvent,
        Some("55000") => StoreError::TerminalImmutable,
        _ => StoreError::Internal,
    }
}

#[async_trait]
impl ExecutionStore for PostgresExecutionStore {
    async fn ready(&self) -> Result<(), StoreError> {
        self.tenant_controls_enabled().await.map(|_| ())
    }

    async fn authorize_runtime_binding(
        &self,
        scope: &crate::CapabilityScopeV2,
        run_id: &str,
        capability_revision: u64,
        identity: &CapabilityIdentity,
    ) -> Result<RuntimeCapabilityBinding, StoreError> {
        let mut transaction = self.begin_tenant(&scope.tenant_id).await?;
        let row = sqlx::query(AUTHORIZE_RUNTIME_BINDING_SQL)
            .bind(uuid(run_id)?)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .bind(i64::try_from(capability_revision).map_err(|_| StoreError::Internal)?)
            .bind(&identity.name)
            .bind(&identity.capability_id)
            .bind(&identity.version)
            .bind(&identity.schema_hash)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
        let (row, sdk_authority) = if let Some(row) = row {
            (row, false)
        } else {
            let row = sqlx::query(AUTHORIZE_SDK_BINDING_SQL)
                .bind(uuid(run_id)?)
                .bind(&scope.tenant_id)
                .bind(&scope.user_id)
                .bind(&scope.session_id)
                .bind(i64::try_from(capability_revision).map_err(|_| StoreError::Internal)?)
                .bind(&identity.name)
                .bind(&identity.capability_id)
                .bind(&identity.version)
                .bind(&identity.schema_hash)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(database_error)?
                .ok_or(StoreError::RuntimeBindingUnauthorized)?;
            (row, true)
        };
        transaction.commit().await.map_err(database_error)?;
        let revision: i64 = row
            .try_get("capability_revision")
            .map_err(|_| StoreError::Internal)?;
        let connector_value: Value = row
            .try_get("connector_binding")
            .map_err(|_| StoreError::Internal)?;
        let connector_binding = if connector_value.is_null() {
            None
        } else {
            Some(
                serde_json::from_value::<RuntimeConnectorBinding>(connector_value)
                    .map_err(|_| StoreError::RuntimeBindingUnauthorized)?,
            )
        };
        let memory_policy_value: Value = row
            .try_get("memory_policy")
            .map_err(|_| StoreError::Internal)?;
        let memory_policy = if memory_policy_value.is_null() {
            None
        } else {
            let policy = serde_json::from_value::<crate::write_capabilities::MemoryPolicyBinding>(
                memory_policy_value,
            )
            .map_err(|_| StoreError::RuntimeBindingUnauthorized)?;
            policy
                .validate()
                .map_err(|_| StoreError::RuntimeBindingUnauthorized)?;
            Some(policy)
        };
        let matched_capability_type: String = row
            .try_get("matched_capability_type")
            .map_err(|_| StoreError::Internal)?;
        if matched_capability_type != identity.capability_type
            && !(matched_capability_type == "connector" && connector_binding.is_some())
        {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        if matches!(matched_capability_type.as_str(), "connector" | "mcp") {
            let connector = connector_binding
                .as_ref()
                .ok_or(StoreError::RuntimeBindingUnauthorized)?;
            connector.validate(&identity.name)?;
            if matched_capability_type == "mcp" && connector.provider != "mcp" {
                return Err(StoreError::RuntimeBindingUnauthorized);
            }
        } else if connector_binding.is_some() {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        let matched_effect = row
            .try_get::<String, _>("matched_effect")
            .map_err(|_| StoreError::Internal)?;
        let effect = if matched_effect == "unknown" && !sdk_authority {
            if identity.capability_type == "mcp" {
                return Err(StoreError::RuntimeBindingUnauthorized);
            }
            identity.effect
        } else {
            parse_effect(&matched_effect)?
        };
        let matched_approval_policy = row
            .try_get::<String, _>("matched_approval_policy")
            .map_err(|_| StoreError::Internal)?;
        let approval_policy = if matched_effect == "unknown" && !sdk_authority {
            identity.approval_policy
        } else {
            parse_approval_policy(&matched_approval_policy)?
        };
        if effect != identity.effect
            || approval_policy != identity.approval_policy
            || (sdk_authority
                && effect == CapabilityEffect::Read
                && approval_policy != ApprovalPolicy::Never)
        {
            return Err(StoreError::RuntimeBindingUnauthorized);
        }
        Ok(RuntimeCapabilityBinding {
            snapshot_id: row
                .try_get::<Uuid, _>("snapshot_id")
                .map_err(|_| StoreError::Internal)?
                .to_string(),
            capability_revision: revision.try_into().map_err(|_| StoreError::Internal)?,
            capability_type: matched_capability_type,
            name: identity.name.clone(),
            capability_id: identity.capability_id.clone(),
            capability_version: identity.version.clone(),
            schema_hash: identity.schema_hash.clone(),
            effect,
            approval_policy,
            bound_dataset_ids: row
                .try_get::<Vec<String>, _>("bound_dataset_ids")
                .map_err(|_| StoreError::Internal)?
                .into_iter()
                .collect(),
            connector_binding,
            memory_policy,
        })
    }

    async fn reserve(&self, requested: NewExecution) -> Result<ReserveOutcome, StoreError> {
        self.reserve_owned(requested, None, None).await
    }

    async fn reserve_owned(
        &self,
        requested: NewExecution,
        owner: Option<&str>,
        fence: Option<i64>,
    ) -> Result<ReserveOutcome, StoreError> {
        self.reserve_admitted_owned(requested, owner, fence, &mut || Ok(()))
            .await
    }

    async fn reserve_admitted_owned(
        &self,
        requested: NewExecution,
        owner: Option<&str>,
        fence: Option<i64>,
        admit: &mut (dyn FnMut() -> Result<(), StoreError> + Send),
    ) -> Result<ReserveOutcome, StoreError> {
        let mut transaction = self.begin_tenant(&requested.execution.tenant_id).await?;
        sqlx::query("SELECT assistant.assert_worker_execution_owner($1,$2,$3,$4,$5,$6,TRUE)")
            .bind(uuid(&requested.execution.run_id)?)
            .bind(&requested.execution.tenant_id)
            .bind(&requested.execution.user_id)
            .bind(&requested.execution.session_id)
            .bind(owner.map(uuid).transpose()?)
            .bind(fence)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let proposed_id = uuid(&requested.execution.execution_id)?;
        let row = sqlx::query(
            "SELECT * FROM reserve_assistant_capability_execution(\
             $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
        )
        .bind(proposed_id)
        .bind(uuid(&requested.execution.lease_id)?)
        .bind(&requested.execution.tenant_id)
        .bind(&requested.execution.user_id)
        .bind(&requested.execution.session_id)
        .bind(uuid(&requested.execution.run_id)?)
        .bind(&requested.execution.tool_call_id)
        .bind(&requested.execution.attempt_id)
        .bind(&requested.execution.capability_id)
        .bind(
            i64::try_from(requested.execution.capability_revision)
                .map_err(|_| StoreError::Internal)?,
        )
        .bind(&requested.arguments)
        .bind(
            requested
                .execution
                .arguments_hash
                .strip_prefix("sha256:")
                .ok_or(StoreError::Internal)?,
        )
        .bind(&requested.execution.idempotency_key)
        .bind(effect_name(requested.execution.effect))
        .bind(approval_policy_name(requested.approval_policy))
        .bind(requested.approval_id.as_deref().map(uuid).transpose()?)
        .bind(&requested.approval_status)
        .bind(&requested.execution.events_url)
        .bind(&requested.resource_binding)
        .fetch_one(&mut *transaction)
        .await
        .map_err(database_error)?;
        let record = row_to_record(&row)?;
        // The authority function already serializes identity conflicts and the
        // transaction retains the owner fence lock. A new row is uncommitted:
        // recovery/dispatch cannot see it until admission succeeds and commits.
        if !crate::same_reservation(&record, &requested) {
            transaction.rollback().await.map_err(database_error)?;
            return Err(StoreError::IdempotencyConflict);
        }
        if record.execution.execution_id == proposed_id.to_string()
            && let Err(error) = admit()
        {
            transaction.rollback().await.map_err(database_error)?;
            return Err(error);
        }
        transaction.commit().await.map_err(database_error)?;
        Ok(ReserveOutcome {
            created: record.execution.execution_id == proposed_id.to_string(),
            record,
        })
    }

    async fn get(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<ExecutionRecord, StoreError> {
        let mut transaction = self.begin_tenant(&scope.tenant_id).await?;
        let row = sqlx::query(
            "SELECT * FROM assistant_capability_executions \
             WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4",
        )
        .bind(uuid(execution_id)?)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?
        .ok_or(StoreError::NotFound)?;
        transaction.commit().await.map_err(database_error)?;
        row_to_record(&row)
    }

    async fn dispatch(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        dispatch_fence: &str,
    ) -> Result<DispatchOutcome, StoreError> {
        let mut transaction = self.begin_tenant(&scope.tenant_id).await?;
        let row =
            sqlx::query("SELECT * FROM dispatch_assistant_capability_execution($1,$2,$3,$4,$5,$6)")
                .bind(uuid(execution_id)?)
                .bind(&scope.tenant_id)
                .bind(&scope.user_id)
                .bind(&scope.session_id)
                .bind(uuid(dispatch_fence)?)
                .bind(30_000_i64)
                .fetch_one(&mut *transaction)
                .await
                .map_err(database_error)?;
        let claimed: bool = row.try_get("claimed").map_err(|_| StoreError::Internal)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(DispatchOutcome {
            record: self.get(scope, execution_id).await?,
            claimed,
        })
    }

    async fn recoverable(&self) -> Result<Vec<ExecutionRecord>, StoreError> {
        let sql = if self.tenant_controls_enabled().await? {
            "SELECT * FROM assistant.recoverable_capability_executions_v1()"
        } else {
            RECOVERABLE_EXECUTIONS_SQL
        };
        let rows = sqlx::query(sql)
            .fetch_all(&self.pool)
            .await
            .map_err(database_error)?;
        rows.iter().map(row_to_record).collect()
    }

    async fn renew(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        dispatch_fence: &str,
        lease_ms: u64,
    ) -> Result<bool, StoreError> {
        let mut transaction = self.begin_tenant(&scope.tenant_id).await?;
        let lease_ms =
            i64::try_from(lease_ms.clamp(1_000, 120_000)).map_err(|_| StoreError::Internal)?;
        let changed: Option<i32> = sqlx::query_scalar(
            "UPDATE assistant_capability_executions \
                SET worker_lease_until = NOW() + make_interval(secs => ($5::double precision / 1000.0)), \
                    updated_at=NOW() \
              WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 \
                AND dispatch_fence=$6 AND status IN ('dispatched','running') \
                AND worker_lease_until > NOW() \
                RETURNING 1",
        )
        .bind(uuid(execution_id)?)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(lease_ms)
        .bind(uuid(dispatch_fence)?)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        Ok(changed.is_some())
    }

    async fn append_event(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        event_id: &str,
        event: &str,
        status: CapabilityExecutionStatus,
        payload: BTreeMap<String, Value>,
        dispatch_fence: Option<&str>,
    ) -> Result<CapabilityEventV2, StoreError> {
        match serde_json::to_vec(&payload) {
            Ok(encoded) if encoded.len() <= MAX_EVENT_PAYLOAD_BYTES => {}
            _ => return Err(StoreError::InvalidEvent),
        }
        let mut append_transaction = self.begin_tenant(&scope.tenant_id).await?;
        append_family_fence(
            &mut append_transaction,
            "assistant.assistant_capability_events",
        )
        .await
        .map_err(database_error)?;
        let _sequence: i64 = sqlx::query_scalar(
            "SELECT append_assistant_capability_event($1,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(uuid(execution_id)?)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(uuid(event_id)?)
        .bind(event)
        .bind(status_name(status))
        .bind(serde_json::to_value(&payload).map_err(|_| StoreError::InvalidEvent)?)
        .bind(dispatch_fence.map(uuid).transpose()?)
        .fetch_one(&mut *append_transaction)
        .await
        .map_err(database_error)?;
        let row = sqlx::query(
            "SELECT execution_id, tool_call_id, sequence, event, status, payload, created_at \
             FROM assistant_capability_events \
             WHERE execution_id=$1 AND event_id=$2 \
               AND tenant_id=$3 AND user_id=$4 AND session_id=$5",
        )
        .bind(uuid(execution_id)?)
        .bind(uuid(event_id)?)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .fetch_optional(&mut *append_transaction)
        .await
        .map_err(database_error)?
        .ok_or(StoreError::Internal)?;
        let event = row_to_event(&row)?;
        append_transaction.commit().await.map_err(database_error)?;
        Ok(event)
    }

    async fn events(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        after_sequence: u64,
    ) -> Result<CapabilityEventPageV2, StoreError> {
        let record = self.get(scope, execution_id).await?;
        let mut transaction = self.begin_tenant(&scope.tenant_id).await?;
        let rows = sqlx::query(
            "SELECT sequence,event,status,payload,created_at \
             FROM assistant_capability_events \
             WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 \
               AND session_id=$4 AND sequence>$5 \
             ORDER BY sequence LIMIT $6",
        )
        .bind(uuid(execution_id)?)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(i64::try_from(after_sequence).map_err(|_| StoreError::InvalidEvent)?)
        .bind(i64::try_from(EVENT_PAGE_SIZE + 1).map_err(|_| StoreError::Internal)?)
        .fetch_all(&mut *transaction)
        .await
        .map_err(database_error)?;
        transaction.commit().await.map_err(database_error)?;
        let has_more = rows.len() > EVENT_PAGE_SIZE;
        let events = rows
            .into_iter()
            .take(EVENT_PAGE_SIZE)
            .map(|row| {
                let sequence: i64 = row.try_get("sequence").map_err(|_| StoreError::Internal)?;
                Ok(CapabilityEventV2 {
                    schema_version: CAPABILITY_EVENT_SCHEMA_VERSION.to_string(),
                    execution_id: execution_id.to_string(),
                    tool_call_id: record.execution.tool_call_id.clone(),
                    sequence: sequence.try_into().map_err(|_| StoreError::Internal)?,
                    event: row.try_get("event").map_err(|_| StoreError::Internal)?,
                    status: parse_status(
                        row.try_get::<String, _>("status")
                            .map_err(|_| StoreError::Internal)?
                            .as_str(),
                    )?,
                    payload: serde_json::from_value(
                        row.try_get("payload").map_err(|_| StoreError::Internal)?,
                    )
                    .map_err(|_| StoreError::Internal)?,
                    created_at_epoch_ms: row
                        .try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")
                        .map_err(|_| StoreError::Internal)?
                        .timestamp_millis()
                        .try_into()
                        .map_err(|_| StoreError::Internal)?,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        let next_sequence = events.last().map_or(after_sequence, |event| event.sequence);
        Ok(CapabilityEventPageV2 {
            schema_version: CAPABILITY_EVENT_SCHEMA_VERSION.to_string(),
            execution_id: execution_id.to_string(),
            after_sequence,
            next_sequence,
            has_more,
            events,
        })
    }

    async fn cancel(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
    ) -> Result<ExecutionRecord, StoreError> {
        self.cancel_owned(scope, execution_id, None, None).await
    }

    async fn cancel_owned(
        &self,
        scope: &CapabilityScopeV2,
        execution_id: &str,
        owner: Option<&str>,
        fence: Option<i64>,
    ) -> Result<ExecutionRecord, StoreError> {
        let current = self.get(scope, execution_id).await?;
        let mut transaction = self.begin_tenant(&scope.tenant_id).await?;
        sqlx::query("SELECT assistant.assert_worker_execution_owner($1,$2,$3,$4,$5,$6,FALSE)")
            .bind(uuid(&current.execution.run_id)?)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .bind(owner.map(uuid).transpose()?)
            .bind(fence)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        let row = sqlx::query("SELECT * FROM assistant_capability_executions WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 FOR UPDATE")
            .bind(uuid(execution_id)?).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id)
            .fetch_one(&mut *transaction).await.map_err(database_error)?;
        let current = row_to_record(&row)?;
        if !current.execution.status.is_terminal() {
            append_family_fence(&mut transaction, "assistant.assistant_capability_events")
                .await
                .map_err(database_error)?;
            let status = if current.dispatch_fence.is_some()
                && !matches!(current.execution.effect, CapabilityEffect::Read)
            {
                CapabilityExecutionStatus::SideEffectUnknown
            } else {
                CapabilityExecutionStatus::Cancelled
            };
            sqlx::query(
                "SELECT append_assistant_capability_event($1,$2,$3,$4,$5,'terminal',$6,$7,$8)",
            )
            .bind(uuid(execution_id)?)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .bind(Uuid::now_v7())
            .bind(status_name(status))
            .bind(serde_json::json!({"error_code":status_name(status)}))
            .bind(current.dispatch_fence.as_deref().map(uuid).transpose()?)
            .execute(&mut *transaction)
            .await
            .map_err(database_error)?;
        }
        transaction.commit().await.map_err(database_error)?;
        self.get(scope, execution_id).await
    }
}

// Authority-only append activation; errors are propagated, never a legacy retry.
async fn append_family_fence(
    connection: &mut sqlx::PgConnection,
    family: &str,
) -> Result<(), sqlx::Error> {
    let installed: bool = sqlx::query_scalar(
        "SELECT to_regprocedure('partition_ops.family_enabled(text)') IS NOT NULL",
    )
    .fetch_one(&mut *connection)
    .await?;
    if installed {
        let _: bool = sqlx::query_scalar("SELECT partition_ops.family_enabled($1)")
            .bind(family)
            .fetch_one(connection)
            .await?;
    }
    Ok(())
}
