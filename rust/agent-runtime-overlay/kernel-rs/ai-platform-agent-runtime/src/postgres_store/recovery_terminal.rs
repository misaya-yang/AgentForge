//! Recovery terminal facts are committed with their durable receipts, never
//! before them. This is a managed-run repair, not a second execution admission.

use codex_thread_store::{ThreadStoreError, ThreadStoreResult};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::{AssistantTurnEventV1, SequencedAssistantTurnEventV1};

use super::execution::ExecutionClaim;
use super::{MemberScope, PostgresThreadStore, store_error, thread_uuid};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveryTerminalMode {
    Owned,
    ExpiredAuthority,
}

const LOCK_RUN: &str = r#"
    SELECT tenant_id,user_id,session_id,status,error
    FROM assistant_runs
    WHERE run_id=$1 AND engine='agent_runtime' AND harness_thread_id=$2
      AND harness_turn_id=$1::text
    FOR UPDATE
"#;

const LOCK_ROOT: &str = r#"
    SELECT root.runtime_thread_id
    FROM assistant_runtime_threads root
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

const LOCK_OWNER: &str = r#"
    SELECT runtime_thread_id,owner_id,fence,context,
           lease_until<=clock_timestamp() AS expired
    FROM assistant_runtime_execution_owners WHERE run_id=$1 FOR UPDATE
"#;

// Re-evaluated after run/root/owner locks, never trusted from the scan page.
// Match the original scoped snapshot and include its own expiry, not only the
// model lease. This cannot renew authority or dispatch a capability.
const AUTHORITY_IS_LIVE: &str = r#"
    SELECT EXISTS (
        SELECT 1 FROM assistant_runs r
        JOIN assistant_runtime_model_leases l ON l.run_id=r.run_id
        JOIN assistant_runtime_snapshots s ON s.snapshot_id=l.snapshot_id
        WHERE r.run_id=$1 AND s.snapshot_id=r.runtime_snapshot_id
          AND s.run_id=r.run_id AND s.runtime_thread_id=r.harness_thread_id
          AND s.tenant_id=r.tenant_id AND s.user_id=r.user_id AND s.session_id=r.session_id
          AND l.status='active' AND l.expires_at>clock_timestamp()
          AND (s.expires_at IS NULL OR s.expires_at>clock_timestamp())
          AND NOT EXISTS (
              SELECT 1 FROM assistant_runtime_snapshot_revocations x
              WHERE x.snapshot_id=s.snapshot_id
          )
    )
"#;

const UPDATE_TERMINAL: &str = r#"
    UPDATE assistant_runs r
    SET status=$2,error=$3,finished_at=COALESCE(finished_at,clock_timestamp()),
        updated_at=clock_timestamp()
    WHERE r.run_id=$1 AND r.engine='agent_runtime'
      AND r.status IN ('running','awaiting_approval')
      AND EXISTS (
          SELECT 1 FROM assistant_runtime_execution_owners o
          WHERE o.run_id=r.run_id AND o.runtime_thread_id=r.harness_thread_id
            AND o.owner_id=$4 AND o.fence=$5
            AND CASE WHEN $6 THEN o.lease_until<=clock_timestamp()
                     ELSE o.lease_until>clock_timestamp() END
      )
"#;

impl PostgresThreadStore {
    /// None means an expired-authority scan candidate changed before locking.
    /// A live recovery claim losing its fence is an error, never a fake terminal.
    pub(crate) async fn commit_recovery_terminal(
        &self,
        claim: &ExecutionClaim,
        event: &AssistantTurnEventV1,
        mode: RecoveryTerminalMode,
    ) -> ThreadStoreResult<Option<SequencedAssistantTurnEventV1>> {
        self.check_write_health(claim.thread_id)?;
        let (status, error) = validate_terminal(claim, event, mode)?;
        if mode == RecoveryTerminalMode::Owned && claim.owner_id != self.instance_id {
            return Err(conflict("recovery terminal owner is not this process"));
        }
        let root_id = thread_uuid(claim.thread_id)?;
        let identity = self.root_scope_with_deleted(root_id, true).await?;
        crate::http_service::recovery_fault::terminal_pause(
            "terminal_before_transaction",
            claim.run_id,
        )
        .await;
        let mut tx = self.tenant_transaction(&identity.tenant_id).await?;
        let row = sqlx::query(LOCK_RUN)
            .bind(claim.run_id)
            .bind(root_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_error)?
            .ok_or_else(|| conflict("recovery terminal run/thread identity mismatch"))?;
        let scope = MemberScope {
            root_thread_id: root_id,
            tenant_id: row.try_get("tenant_id").map_err(store_error)?,
            user_id: row.try_get("user_id").map_err(store_error)?,
            session_id: row.try_get("session_id").map_err(store_error)?,
        };
        if event.data.get("session_id").and_then(Value::as_str) != Some(scope.session_id.as_str()) {
            return Err(conflict("recovery terminal session identity mismatch"));
        }
        // Match claim/cancel order: run -> root -> owner. Do not call lock_root:
        // its process-local live-claim check cannot authorize expired closure.
        let root: Option<Uuid> = sqlx::query_scalar(LOCK_ROOT)
            .bind(root_id)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_error)?;
        if root.is_none() {
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: claim.thread_id,
            });
        }
        let owner = sqlx::query(LOCK_OWNER)
            .bind(claim.run_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(store_error)?
            .ok_or_else(|| conflict("recovery terminal owner missing"))?;
        let same_owner = owner
            .try_get::<Uuid, _>("runtime_thread_id")
            .map_err(store_error)?
            == root_id
            && owner.try_get::<Uuid, _>("owner_id").map_err(store_error)? == claim.owner_id
            && owner.try_get::<i64, _>("fence").map_err(store_error)? == claim.fence
            && owner.try_get::<Value, _>("context").map_err(store_error)? == claim.context;
        if !same_owner {
            return if mode == RecoveryTerminalMode::ExpiredAuthority {
                Ok(None)
            } else {
                Err(conflict("recovery terminal execution fence lost"))
            };
        }
        let event_key = match mode {
            RecoveryTerminalMode::Owned => format!("compat/recovery/{}/terminal", claim.run_id),
            RecoveryTerminalMode::ExpiredAuthority => {
                format!("compat/recovery/{}/authority-expired", claim.run_id)
            }
        };
        let saved_status: String = row.try_get("status").map_err(store_error)?;
        if !matches!(saved_status.as_str(), "running" | "awaiting_approval") {
            // Retry after an uncertain commit only returns the original fact.
            // Missing receipts on legacy terminal rows need explicit evidence-
            // backed reconciliation; they must not be fabricated by a replay.
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM assistant_runtime_items \
                 WHERE runtime_thread_id=$1 AND kernel_thread_id=$1 AND event_key=$2)",
            )
            .bind(root_id)
            .bind(&event_key)
            .fetch_one(&mut *tx)
            .await
            .map_err(store_error)?;
            if saved_status != status
                || row
                    .try_get::<Option<String>, _>("error")
                    .map_err(store_error)?
                    .as_deref()
                    != error
                || !exists
            {
                return if mode == RecoveryTerminalMode::ExpiredAuthority {
                    Ok(None)
                } else {
                    Err(conflict(
                        "recovery terminal already changed or receipt missing",
                    ))
                };
            }
            let saved = self
                .append_v1_event_in_transaction(
                    &mut tx,
                    &scope,
                    claim.thread_id,
                    claim.run_id,
                    &event_key,
                    event,
                )
                .await?;
            tx.commit().await.map_err(store_error)?;
            return Ok(Some(saved));
        }
        let expired: bool = owner.try_get("expired").map_err(store_error)?;
        if mode == RecoveryTerminalMode::ExpiredAuthority {
            if !expired
                || sqlx::query_scalar::<_, bool>(AUTHORITY_IS_LIVE)
                    .bind(claim.run_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(store_error)?
            {
                return Ok(None);
            }
        } else {
            sqlx::query("SELECT assert_runtime_execution($1,$2,$3)")
                .bind(claim.run_id)
                .bind(claim.owner_id)
                .bind(claim.fence)
                .execute(&mut *tx)
                .await
                .map_err(store_error)?;
        }
        // The held root lock prevents new lifecycle facts racing admission.
        // Dispatched writes/unknown effects keep the existing immutable unknown
        // result classifier. No Worker reservation, dispatch or replay occurs.
        self.admit_turn_terminal_in_transaction(
            &mut tx,
            &scope,
            claim.thread_id,
            &claim.run_id.to_string(),
        )
        .await?;
        if mode == RecoveryTerminalMode::ExpiredAuthority {
            sqlx::query(
                "UPDATE assistant_tool_approvals SET status='cancelled', \
                 reason='recovery_authority_expired',approved_at=clock_timestamp() \
                 WHERE run_id=$1 AND tenant_id=$2 AND user_id=$3 AND session_id=$4 \
                 AND status IN ('pending','approved')",
            )
            .bind(claim.run_id)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        }
        let changed = sqlx::query(UPDATE_TERMINAL)
            .bind(claim.run_id)
            .bind(status)
            .bind(error)
            .bind(claim.owner_id)
            .bind(claim.fence)
            .bind(mode == RecoveryTerminalMode::ExpiredAuthority)
            .execute(&mut *tx)
            .await
            .map_err(store_error)?;
        if changed.rows_affected() != 1 {
            return Err(conflict("recovery terminal transition lost its fence"));
        }
        crate::http_service::recovery_fault::terminal_pause(
            "terminal_after_row_update",
            claim.run_id,
        )
        .await;
        let saved = self
            .append_v1_event_in_transaction(
                &mut tx,
                &scope,
                claim.thread_id,
                claim.run_id,
                &event_key,
                event,
            )
            .await?;
        crate::http_service::recovery_fault::terminal_pause("terminal_after_event", claim.run_id)
            .await;
        if mode == RecoveryTerminalMode::Owned {
            // Do not commit after an append blocked past the owner's lease.
            sqlx::query("SELECT assert_runtime_execution_fence($1,$2,$3)")
                .bind(claim.run_id)
                .bind(claim.owner_id)
                .bind(claim.fence)
                .execute(&mut *tx)
                .await
                .map_err(store_error)?;
        }
        tx.commit().await.map_err(store_error)?;
        // Durable SSE replay remains authoritative if the process dies here.
        crate::http_service::recovery_fault::terminal_pause("terminal_after_commit", claim.run_id)
            .await;
        Ok(Some(saved))
    }
}

fn validate_terminal<'a>(
    claim: &ExecutionClaim,
    event: &'a AssistantTurnEventV1,
    mode: RecoveryTerminalMode,
) -> ThreadStoreResult<(&'a str, Option<&'a str>)> {
    let status = event
        .data
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| conflict("recovery terminal status missing"))?;
    let expected_type = match status {
        "succeeded" => "run_finished",
        "failed" | "cancelled" => "run_error",
        _ => return Err(conflict("recovery terminal status is not canonical")),
    };
    if event.schema_version != "assistant-turn-contract/v1"
        || event.event_type != expected_type
        || event.data.get("run_id").and_then(Value::as_str)
            != Some(claim.run_id.to_string().as_str())
        || event.data.get("thread_id").and_then(Value::as_str)
            != Some(claim.thread_id.to_string().as_str())
    {
        return Err(conflict("recovery terminal envelope identity mismatch"));
    }
    let error = match event.data.get("error_code") {
        None | Some(Value::Null) => None,
        Some(Value::String(error)) => Some(error.as_str()),
        _ => return Err(conflict("recovery terminal error code invalid")),
    };
    if mode == RecoveryTerminalMode::ExpiredAuthority
        && (status != "failed" || error != Some("RUNTIME_RECOVERY_AUTHORITY_EXPIRED"))
    {
        return Err(conflict(
            "expired authority cannot produce another terminal outcome",
        ));
    }
    Ok((status, error))
}

fn conflict(message: &str) -> ThreadStoreError {
    ThreadStoreError::Conflict {
        message: message.to_string(),
    }
}

#[cfg(test)]
#[path = "recovery_terminal_tests.rs"]
mod tests;
