//! User decisions are durable facts, independent of a Runtime process waiter.

use codex_protocol::ThreadId;
use codex_thread_store::{ThreadStoreError, ThreadStoreResult};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::{MemberScope, PostgresThreadStore, store_error};
use crate::{AssistantTurnEventV1, SequencedAssistantTurnEventV1};

pub(crate) struct CommittedApprovalDecision {
    pub root_thread_id: ThreadId,
    pub event: SequencedAssistantTurnEventV1,
    pub changed: bool,
}

const LOCK_RUN: &str = r#"
    SELECT r.run_id,r.harness_thread_id,r.status
    FROM assistant_runs r JOIN assistant_tool_approvals a ON a.run_id=r.run_id
    WHERE a.approval_id=$1 AND a.tenant_id=$2 AND a.user_id=$3 AND a.session_id=$4
      AND r.tenant_id=a.tenant_id AND r.user_id=a.user_id AND r.session_id=a.session_id
      AND r.engine='agent_runtime' AND r.harness_turn_id=r.run_id::text
    FOR UPDATE OF r
"#;

const LOCK_ROOT: &str = r#"
    SELECT root.runtime_thread_id FROM assistant_runtime_threads root
    JOIN assistant_runtime_thread_members member
      ON member.runtime_thread_id=root.runtime_thread_id
     AND member.kernel_thread_id=root.runtime_thread_id
     AND member.tenant_id=root.tenant_id AND member.user_id=root.user_id
     AND member.session_id=root.session_id
    JOIN assistant_runtime_thread_projections projection
      ON projection.kernel_thread_id=member.kernel_thread_id
     AND projection.runtime_thread_id=root.runtime_thread_id
    WHERE root.runtime_thread_id=$1 AND root.tenant_id=$2 AND root.user_id=$3
      AND root.session_id=$4 AND root.deleted_at IS NULL
      AND root.archived_at IS NULL AND projection.deleted_at IS NULL
    FOR UPDATE OF root
"#;

const LOCK_APPROVAL: &str = r#"
    SELECT tool_call_id,tool_name,arguments,status
    FROM assistant_tool_approvals
    WHERE approval_id=$1 AND run_id=$2 AND tenant_id=$3 AND user_id=$4 AND session_id=$5
    FOR UPDATE
"#;

// Approval is user authority, not execution admission. No local owner/fence is
// needed while takeover is pending. Worker still checks its fence at dispatch.
const LOCK_LIVE_AUTHORITY: &str = r#"
    SELECT l.run_id FROM assistant_runs r
    JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id
    JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id
    WHERE r.run_id=$1 AND s.snapshot_id=r.runtime_snapshot_id
      AND s.run_id=r.run_id AND s.runtime_thread_id=r.harness_thread_id
      AND s.tenant_id=r.tenant_id AND s.user_id=r.user_id AND s.session_id=r.session_id
      AND l.status='active' AND l.expires_at>clock_timestamp()
      AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp())
      AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x
                      WHERE x.snapshot_id=s.snapshot_id)
    FOR SHARE OF l,s
"#;

impl PostgresThreadStore {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn commit_dynamic_approval_decision(
        &self,
        approval_id: Uuid,
        tenant_id: &str,
        user_id: &str,
        session_id: &str,
        approved: bool,
        reason: Option<&str>,
        receipt_only_thread: Option<Uuid>,
    ) -> ThreadStoreResult<CommittedApprovalDecision> {
        let mut tx = self.tenant_transaction(tenant_id).await?;
        // Match claim/cancel/dispatch: run first. Locking approval first would
        // invert Worker's run -> execution -> approval order.
        let run = sqlx::query(LOCK_RUN)
            .bind(approval_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_error)?
            .ok_or_else(|| conflict("approval scope unavailable"))?;
        let run_id: Uuid = run.try_get("run_id").map_err(store_error)?;
        let root_id: Uuid = run.try_get("harness_thread_id").map_err(store_error)?;
        validate_receipt_thread(receipt_only_thread, root_id)?;
        let root_thread_id = ThreadId::from_u128(root_id.as_u128());
        let root: Option<Uuid> = sqlx::query_scalar(LOCK_ROOT)
            .bind(root_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_error)?;
        if root.is_none() {
            return Err(conflict("approval thread unavailable"));
        }
        let managed: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM assistant_runtime_execution_owners \
             WHERE run_id=$1 AND runtime_thread_id=$2)",
        )
        .bind(run_id)
        .bind(root_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(store_error)?;
        if !managed {
            return Err(conflict("approval has no durable execution context"));
        }
        let row = sqlx::query(LOCK_APPROVAL)
            .bind(approval_id)
            .bind(run_id)
            .bind(tenant_id)
            .bind(user_id)
            .bind(session_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(store_error)?;
        let call_id: String = row.try_get("tool_call_id").map_err(store_error)?;
        let tool_name: String = row.try_get("tool_name").map_err(store_error)?;
        let arguments: Value = row.try_get("arguments").map_err(store_error)?;
        // The original dynamic required receipt proves destination kind and
        // argument binding. Native requests lack its effect/hash pair.
        let required: Option<Value> = sqlx::query_scalar(
            "SELECT payload FROM assistant_runtime_items WHERE runtime_thread_id=$1 \
             AND kernel_thread_id=$1 AND event_key=$2 \
             AND event_type='compat/v1/approval_required'",
        )
        .bind(root_id)
        .bind(format!("compat/approval/{approval_id}/required"))
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_error)?;
        validate_required(
            required
                .as_ref()
                .ok_or_else(|| conflict("dynamic approval receipt missing"))?,
            approval_id,
            run_id,
            root_id,
            session_id,
            &call_id,
            &tool_name,
            &arguments,
        )?;
        let event_key = format!("compat/approval/{approval_id}/result");
        let event_id = decision_event_id(approval_id);
        let proposed = decision_event(
            approval_id,
            run_id,
            root_id,
            session_id,
            &call_id,
            approved,
            reason,
        );
        let saved = sqlx::query(
            "SELECT event_id,sequence,payload FROM assistant_runtime_items \
             WHERE runtime_thread_id=$1 AND kernel_thread_id=$1 AND event_key=$2 \
               AND event_type='compat/v1/approval_result'",
        )
        .bind(root_id)
        .bind(&event_key)
        .fetch_optional(&mut *tx)
        .await
        .map_err(store_error)?;
        let saved = saved
            .map(|saved| -> ThreadStoreResult<_> {
                let event: AssistantTurnEventV1 =
                    serde_json::from_value(saved.try_get("payload").map_err(store_error)?)
                        .map_err(|_| conflict("approval decision receipt invalid"))?;
                let saved_id = saved.try_get::<Uuid, _>("event_id").map_err(store_error)?;
                let sequence = saved.try_get("sequence").map_err(store_error)?;
                Ok((saved_id, SequencedAssistantTurnEventV1 { sequence, event }))
            })
            .transpose()?;
        if let Some(event) =
            resolve_decision_receipt(receipt_only_thread.is_some(), saved, event_id, &proposed)?
        {
            tx.commit().await.map_err(store_error)?;
            return Ok(CommittedApprovalDecision {
                root_thread_id,
                event,
                changed: false,
            });
        }
        let run_status: String = run.try_get("status").map_err(store_error)?;
        let status: String = row.try_get("status").map_err(store_error)?;
        if !matches!(run_status.as_str(), "running" | "awaiting_approval") || status != "pending" {
            return Err(conflict("approval is no longer pending on an active run"));
        }
        if approved {
            let live: Option<Uuid> = sqlx::query_scalar(LOCK_LIVE_AUTHORITY)
                .bind(run_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(store_error)?;
            if live.is_none() {
                return Err(conflict("approval original authority expired"));
            }
        }
        // Wall-clock deadline after blocking locks; preserve nullable deadlines
        // and the original expiry. This operation never renews authority.
        let changed = sqlx::query(
            "UPDATE assistant_tool_approvals SET status=$2,reason=$3,approved_by=$4, \
             approved_at=clock_timestamp() WHERE approval_id=$1 AND status='pending' \
             AND (expires_at IS NULL OR expires_at>clock_timestamp()) \
             AND (NOT $5 OR EXISTS (SELECT 1 FROM assistant_runs r \
                 JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id \
                 JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id \
                 WHERE r.run_id=$6 AND s.snapshot_id=r.runtime_snapshot_id \
                   AND s.run_id=r.run_id AND s.runtime_thread_id=r.harness_thread_id \
                   AND s.tenant_id=r.tenant_id AND s.user_id=r.user_id AND s.session_id=r.session_id \
                   AND l.status='active' AND l.expires_at>clock_timestamp() \
                   AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp()) \
                   AND NOT EXISTS (SELECT 1 FROM assistant_runtime_snapshot_revocations x \
                                   WHERE x.snapshot_id=s.snapshot_id)))",
        ).bind(approval_id).bind(if approved { "approved" } else { "rejected" })
            .bind(reason).bind(user_id).bind(approved).bind(run_id)
            .execute(&mut *tx).await.map_err(store_error)?;
        if changed.rows_affected() != 1 {
            return Err(conflict("approval expired or already decided"));
        }
        let scope = MemberScope {
            root_thread_id: root_id,
            tenant_id: tenant_id.to_string(),
            user_id: user_id.to_string(),
            session_id: session_id.to_string(),
        };
        let event = self
            .append_v1_event_in_transaction(
                &mut tx,
                &scope,
                root_thread_id,
                event_id,
                &event_key,
                &proposed,
            )
            .await?;
        // Failed append/commit rolls back the decision. Pollers see neither
        // the approved state nor its receipt before commit.
        tx.commit().await.map_err(store_error)?;
        Ok(CommittedApprovalDecision {
            root_thread_id,
            event,
            changed: true,
        })
    }
}

fn validate_receipt_thread(expected: Option<Uuid>, actual: Uuid) -> ThreadStoreResult<()> {
    if expected.is_some_and(|expected| expected != actual) {
        return Err(conflict("approval receipt thread mismatch"));
    }
    Ok(())
}

// Returning None is permission to consider a *new* decision. Receipt-only reads
// must instead fail when absent and must never reach the mutation path below.
fn resolve_decision_receipt(
    receipt_only: bool,
    saved: Option<(Uuid, SequencedAssistantTurnEventV1)>,
    event_id: Uuid,
    proposed: &AssistantTurnEventV1,
) -> ThreadStoreResult<Option<SequencedAssistantTurnEventV1>> {
    match saved {
        Some((saved_id, saved)) => {
            // Old random IDs, changed scope/decision/reason and invalid contract
            // identities must not be interpreted as a fresh approval.
            if saved_id != event_id || !same_decision(&saved.event, proposed) {
                return Err(conflict("approval decision differs from original"));
            }
            Ok(Some(saved))
        }
        None if receipt_only => Err(conflict("approval decision receipt missing")),
        None => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_required(
    receipt: &Value,
    approval_id: Uuid,
    run_id: Uuid,
    root_id: Uuid,
    session_id: &str,
    call_id: &str,
    tool_name: &str,
    arguments: &Value,
) -> ThreadStoreResult<()> {
    let data = &receipt["data"];
    let hash = ai_platform_capability_contract::canonical_json_hash(arguments)
        .map_err(|_| conflict("approval arguments invalid"))?;
    if receipt["event_type"] != "approval_required"
        || data["approval_id"] != approval_id.to_string()
        || data["run_id"] != run_id.to_string()
        || data["thread_id"] != root_id.to_string()
        || data["session_id"] != session_id
        || data["tool_call_id"] != call_id
        || data["arguments_hash"] != hash
        || !matches!(data["effect"].as_str(), Some("write" | "unknown"))
        || call_id.is_empty()
        || tool_name.is_empty()
        || !arguments.is_object()
        || matches!(tool_name, "command_execution" | "file_change")
    {
        return Err(conflict(
            "approval is not bound to a durable dynamic request",
        ));
    }
    Ok(())
}

fn decision_event(
    approval_id: Uuid,
    run_id: Uuid,
    root_id: Uuid,
    session_id: &str,
    call_id: &str,
    approved: bool,
    reason: Option<&str>,
) -> AssistantTurnEventV1 {
    AssistantTurnEventV1::new(
        "approval_result",
        json!({
            "run_id":run_id, "session_id":session_id, "thread_id":root_id,
            "approval_id":approval_id, "tool_call_id":call_id,
            "status":if approved { "approved" } else { "rejected" },
            "approved":approved, "reason":reason,
        }),
    )
}

// Event IDs are unique across the entire root log. The required receipt already
// owns approval_id; derive a distinct stable result ID from its full event key,
// matching the existing lifecycle/recovery-output UUID convention.
fn decision_event_id(approval_id: Uuid) -> Uuid {
    let digest = Sha256::digest(format!("compat/approval/{approval_id}/result").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

fn same_decision(saved: &AssistantTurnEventV1, proposed: &AssistantTurnEventV1) -> bool {
    saved.schema_version == proposed.schema_version
        && saved.event_type == proposed.event_type
        && saved.data == proposed.data
}

fn conflict(message: &str) -> ThreadStoreError {
    ThreadStoreError::Conflict {
        message: message.to_string(),
    }
}

#[cfg(test)]
#[path = "approval_decision_tests.rs"]
mod tests;
