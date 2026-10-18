use codex_protocol::ThreadId;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use sqlx::Row;
use uuid::Uuid;

use crate::AssistantTurnEventV1;
use crate::SequencedAssistantTurnEventV1;

use super::PostgresThreadStore;
use super::json_error;
use super::payload_hash;
use super::store_error;
use super::thread_uuid;

pub(crate) struct PlatformLifecycleEvent {
    pub(crate) kernel_thread_id: ThreadId,
    pub(crate) turn_id: String,
    pub(crate) item_id: Option<String>,
    pub(crate) event_key: String,
    pub(crate) item_type: String,
    pub(crate) status: String,
    pub(crate) payload: Value,
}

impl PostgresThreadStore {
    /// Appends a platform lifecycle receipt to the immutable Item Store.
    /// Event ids are derived from the idempotency key so a retry of the same
    /// callback is a no-op rather than a second tool result.
    pub(crate) async fn append_platform_lifecycle_event(
        &self,
        event: PlatformLifecycleEvent,
    ) -> ThreadStoreResult<i64> {
        let thread_id = event.kernel_thread_id;
        let result = self.append_platform_lifecycle_event_inner(event).await;
        self.record_write_result(thread_id, result)
    }

    async fn append_platform_lifecycle_event_inner(
        &self,
        event: PlatformLifecycleEvent,
    ) -> ThreadStoreResult<i64> {
        let scope = self
            .member_scope(thread_uuid(event.kernel_thread_id)?)
            .await?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        self.lock_root(&mut transaction, scope.root_thread_id)
            .await?;
        let sequence = self
            .append_platform_lifecycle_event_in_transaction(&mut transaction, &scope, event)
            .await?;
        transaction.commit().await.map_err(store_error)?;
        Ok(sequence)
    }

    // Caller holds the root lock and its operation-specific execution authority.
    pub(super) async fn append_platform_lifecycle_event_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &super::MemberScope,
        event: PlatformLifecycleEvent,
    ) -> ThreadStoreResult<i64> {
        if event.event_key.is_empty() || event.event_key.len() > 255 || event.turn_id.is_empty() {
            return Err(ThreadStoreError::InvalidRequest {
                message: "platform lifecycle identifiers are invalid".to_string(),
            });
        }
        append_family_fence(&mut **transaction, "assistant.assistant_runtime_items")
            .await
            .map_err(store_error)?;
        let kernel_thread_uuid = thread_uuid(event.kernel_thread_id)?;
        let payload_hash = payload_hash(&event.payload)?;
        let digest = Sha256::digest(event.event_key.as_bytes());
        let mut event_id_bytes = [0u8; 16];
        event_id_bytes.copy_from_slice(&digest[..16]);
        let event_id = Uuid::from_bytes(event_id_bytes);
        let visible: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM assistant_runtime_thread_projections WHERE kernel_thread_id=$1 AND deleted_at IS NULL)",
        )
        .bind(kernel_thread_uuid)
        .fetch_one(&mut **transaction)
        .await
        .map_err(store_error)?;
        if !visible {
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: ThreadId::from_u128(kernel_thread_uuid.as_u128()),
            });
        }
        let sequence = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT append_assistant_runtime_item(
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                'agent-runtime/tool-lifecycle', $10, $11, $12, $13
            )
            "#,
        )
        .bind(scope.root_thread_id)
        .bind(kernel_thread_uuid)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(event_id)
        .bind(event.event_key)
        .bind(event.turn_id)
        .bind(event.item_id)
        .bind(event.item_type)
        .bind(event.status)
        .bind(event.payload)
        .bind(payload_hash)
        .fetch_one(&mut **transaction)
        .await
        .map_err(store_error)?;
        Ok(sequence)
    }

    /// Reads the durable lifecycle receipts needed to reconstruct a turn
    /// after a Runtime process restart.
    pub async fn read_platform_lifecycle_events(
        &self,
        kernel_thread_id: ThreadId,
        turn_id: &str,
    ) -> ThreadStoreResult<Vec<Value>> {
        let kernel_thread_uuid = thread_uuid(kernel_thread_id)?;
        let scope = self.member_scope(kernel_thread_uuid).await?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        let rows = sqlx::query(
            r#"
            SELECT payload
              FROM assistant_runtime_items
             WHERE kernel_thread_id = $1
               AND turn_id = $2
               AND event_type = 'agent-runtime/tool-lifecycle'
             ORDER BY sequence
            "#,
        )
        .bind(kernel_thread_uuid)
        .bind(turn_id)
        .fetch_all(&mut *transaction)
        .await
        .map_err(store_error)?;
        transaction.commit().await.map_err(store_error)?;
        rows.into_iter()
            .map(|row| row.try_get("payload").map_err(store_error))
            .collect()
    }

    pub(crate) async fn read_platform_lifecycle_turn_ids(
        &self,
        kernel_thread_id: ThreadId,
    ) -> ThreadStoreResult<Vec<String>> {
        let kernel_thread_uuid = thread_uuid(kernel_thread_id)?;
        let scope = self.member_scope(kernel_thread_uuid).await?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        let rows = sqlx::query(
            r#"
            SELECT DISTINCT turn_id
              FROM assistant_runtime_items
             WHERE kernel_thread_id = $1
               AND event_type = 'agent-runtime/tool-lifecycle'
               AND turn_id IS NOT NULL
             ORDER BY turn_id
            "#,
        )
        .bind(kernel_thread_uuid)
        .fetch_all(&mut *transaction)
        .await
        .map_err(store_error)?;
        transaction.commit().await.map_err(store_error)?;
        rows.into_iter()
            .map(|row| row.try_get("turn_id").map_err(store_error))
            .collect()
    }

    /// Admission check for a turn terminal. Every published call must have a
    /// durable, phase/effect-specific result before the caller may project the
    /// terminal envelope.
    pub(crate) async fn admit_turn_terminal(
        &self,
        kernel_thread_id: ThreadId,
        turn_id: &str,
    ) -> ThreadStoreResult<Vec<crate::platform_lifecycle::RecoveredToolCall>> {
        self.check_write_health(kernel_thread_id)?;
        let events = self
            .read_platform_lifecycle_events(kernel_thread_id, turn_id)
            .await?;
        let unclosed = crate::platform_lifecycle::unclosed_tool_calls(&events);
        for call in &unclosed {
            self.append_platform_lifecycle_event(terminal_lifecycle_event(
                kernel_thread_id,
                turn_id,
                call,
            ))
            .await?;
        }
        Ok(unclosed)
    }

    /// Recovery holds run/root/owner locks across admission and the terminal.
    pub(super) async fn admit_turn_terminal_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &super::MemberScope,
        kernel_thread_id: ThreadId,
        turn_id: &str,
    ) -> ThreadStoreResult<()> {
        let events: Vec<Value> = sqlx::query_scalar(
            "SELECT payload FROM assistant_runtime_items WHERE runtime_thread_id=$1 \
             AND kernel_thread_id=$2 AND turn_id=$3 \
             AND event_type='agent-runtime/tool-lifecycle' ORDER BY sequence",
        )
        .bind(scope.root_thread_id)
        .bind(thread_uuid(kernel_thread_id)?)
        .bind(turn_id)
        .fetch_all(&mut **transaction)
        .await
        .map_err(store_error)?;
        for call in crate::platform_lifecycle::unclosed_tool_calls(&events) {
            self.append_platform_lifecycle_event_in_transaction(
                transaction,
                scope,
                terminal_lifecycle_event(kernel_thread_id, turn_id, &call),
            )
            .await?;
        }
        Ok(())
    }

    /// Resolves one Agent member to its platform root ownership.
    pub async fn identity_for_kernel_thread(
        &self,
        kernel_thread_id: ThreadId,
    ) -> ThreadStoreResult<super::PlatformThreadIdentity> {
        let scope = self.member_scope(thread_uuid(kernel_thread_id)?).await?;
        let identity = super::PlatformThreadIdentity::new(
            ThreadId::from_u128(scope.root_thread_id.as_u128()),
            scope.tenant_id,
            scope.user_id,
            scope.session_id,
        );
        self.cache_terminal_identity(kernel_thread_id, identity.clone());
        Ok(identity)
    }

    /// A bounded last-known identity is only a failure-notification address.
    /// It must never authorize a model request, tool, read or mutation.
    pub(crate) fn cached_terminal_identity(
        &self,
        thread_id: ThreadId,
    ) -> Option<super::PlatformThreadIdentity> {
        self.terminal_identity_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&thread_id)
            .cloned()
    }

    pub(super) fn cache_terminal_identity(
        &self,
        thread_id: ThreadId,
        identity: super::PlatformThreadIdentity,
    ) {
        let mut identities = self
            .terminal_identity_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if identities.len() >= 16_384
            && !identities.contains_key(&thread_id)
            && let Some(oldest) = identities.keys().next().copied()
        {
            identities.remove(&oldest);
        }
        identities.insert(thread_id, identity);
    }

    /// Resolve a child's lease owner from the exact persisted turn context.
    /// Never infer it from the newest root run: concurrent turns and resume can
    /// otherwise bind a child to a different capability snapshot.
    pub(crate) async fn root_turn_id(
        &self,
        kernel_thread_id: ThreadId,
        turn_id: &str,
    ) -> ThreadStoreResult<String> {
        self.check_write_health(kernel_thread_id)?;
        self.ensure_visible(kernel_thread_id, true).await?;
        let scope = self.member_scope(thread_uuid(kernel_thread_id)?).await?;
        self.check_write_health(ThreadId::from_u128(scope.root_thread_id.as_u128()))?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        let root_turn_id = if thread_uuid(kernel_thread_id)? == scope.root_thread_id {
            turn_id.to_string()
        } else {
            let roots: Vec<String> = sqlx::query_scalar(
                r#"
                SELECT DISTINCT payload->'payload'->>'root_turn_id'
                FROM assistant_runtime_items
                WHERE kernel_thread_id=$1 AND runtime_thread_id=$2
                  AND event_type='rollout/item'
                  AND payload->>'type'='turn_context'
                  AND payload->'payload'->>'turn_id'=$3
                  AND payload->'payload'->>'root_turn_id' IS NOT NULL
                "#,
            )
            .bind(thread_uuid(kernel_thread_id)?)
            .bind(scope.root_thread_id)
            .bind(turn_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(store_error)?;
            if roots.len() != 1 {
                return Err(ThreadStoreError::Conflict {
                    message: "child turn has no unambiguous durable root attribution".to_string(),
                });
            }
            roots[0].clone()
        };
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM assistant_runtime_snapshots AS s JOIN assistant_runs AS r ON r.run_id=s.run_id WHERE s.runtime_thread_id=$1 AND s.run_id::text=$2 AND s.tenant_id=$3 AND s.user_id=$4 AND s.session_id=$5 AND r.status='running' AND r.engine='agent_runtime' AND NOT EXISTS (SELECT 1 FROM assistant_runtime_items AS item WHERE item.kernel_thread_id=$1 AND item.turn_id=$2 AND item.event_type IN ('compat/v1/run_finished', 'compat/v1/run_error')))",
        )
        .bind(scope.root_thread_id)
        .bind(&root_turn_id)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(store_error)?;
        if !valid {
            return Err(ThreadStoreError::Conflict {
                message: "turn attribution does not match the authenticated root snapshot"
                    .to_string(),
            });
        }
        transaction.commit().await.map_err(store_error)?;
        Ok(root_turn_id)
    }

    /// Verifies a caller-provided root identity without creating or changing it.
    pub async fn verify_root_identity(
        &self,
        identity: &super::PlatformThreadIdentity,
    ) -> ThreadStoreResult<()> {
        let root_id = thread_uuid(identity.runtime_thread_id)?;
        let scope = self.root_scope(root_id).await?;
        if scope.tenant_id != identity.tenant_id
            || scope.user_id != identity.user_id
            || scope.session_id != identity.session_id
        {
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: identity.runtime_thread_id,
            });
        }
        Ok(())
    }

    /// Appends one projected V1 event to the root-monotonic durable event log.
    pub async fn append_v1_event(
        &self,
        kernel_thread_id: ThreadId,
        event_id: Uuid,
        event_key: &str,
        event: &AssistantTurnEventV1,
    ) -> ThreadStoreResult<i64> {
        if event.event_type == "run_finished" {
            self.check_write_health(kernel_thread_id)?;
        }
        let result = self
            .append_v1_event_inner(kernel_thread_id, event_id, event_key, event)
            .await;
        self.record_write_result(kernel_thread_id, result)
    }

    async fn append_v1_event_inner(
        &self,
        kernel_thread_id: ThreadId,
        event_id: Uuid,
        event_key: &str,
        event: &AssistantTurnEventV1,
    ) -> ThreadStoreResult<i64> {
        let scope = self.member_scope(thread_uuid(kernel_thread_id)?).await?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        self.lock_root(&mut transaction, scope.root_thread_id)
            .await?;
        let saved = self
            .append_v1_event_in_transaction(
                &mut transaction,
                &scope,
                kernel_thread_id,
                event_id,
                event_key,
                event,
            )
            .await?;
        transaction.commit().await.map_err(store_error)?;
        Ok(saved.sequence)
    }

    // Caller holds the root lock. Return the saved envelope on replay so a
    // post-commit broadcast preserves its original timestamp as well as cursor.
    pub(super) async fn append_v1_event_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &super::MemberScope,
        kernel_thread_id: ThreadId,
        event_id: Uuid,
        event_key: &str,
        event: &AssistantTurnEventV1,
    ) -> ThreadStoreResult<SequencedAssistantTurnEventV1> {
        if event_key.is_empty() || event_key.len() > 255 {
            return Err(ThreadStoreError::InvalidRequest {
                message: "runtime event key must contain 1 to 255 characters".to_string(),
            });
        }
        append_family_fence(&mut **transaction, "assistant.assistant_runtime_items")
            .await
            .map_err(store_error)?;
        let kernel_thread_uuid = thread_uuid(kernel_thread_id)?;
        let payload = serde_json::to_value(event).map_err(json_error)?;
        let hash = payload_hash(&payload)?;
        let turn_id = event.data.get("run_id").and_then(Value::as_str);
        let item_id = event
            .data
            .get("tool_call_id")
            .or_else(|| event.data.get("item_id"))
            .and_then(Value::as_str);
        let status = event.data.get("status").and_then(Value::as_str);
        let visible: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM assistant_runtime_thread_projections WHERE kernel_thread_id=$1 AND deleted_at IS NULL)",
        )
        .bind(kernel_thread_uuid)
        .fetch_one(&mut **transaction)
        .await
        .map_err(store_error)?;
        if !visible {
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: ThreadId::from_u128(kernel_thread_uuid.as_u128()),
            });
        }
        if let Some(row) = sqlx::query("SELECT event_id,sequence,payload FROM assistant_runtime_items WHERE kernel_thread_id=$1 AND event_key=$2")
            .bind(kernel_thread_uuid).bind(event_key).fetch_optional(&mut **transaction).await.map_err(store_error)? {
            let original: AssistantTurnEventV1 = serde_json::from_value(row.try_get("payload").map_err(store_error)?).map_err(json_error)?;
            if row.try_get::<Uuid,_>("event_id").map_err(store_error)? != event_id || !same_event_fact(&original, event) {
                return Err(ThreadStoreError::Conflict { message: "original runtime event facts changed".to_string() });
            }
            let sequence = row.try_get("sequence").map_err(store_error)?;
            return Ok(SequencedAssistantTurnEventV1 { sequence, event: original });
        }
        let sequence = sqlx::query_scalar::<_, i64>(
            r#"
            SELECT append_assistant_runtime_item(
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                $10, 'assistant_turn_event_v1', $11, $12, $13
            )
            "#,
        )
        .bind(scope.root_thread_id)
        .bind(kernel_thread_uuid)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(event_id)
        .bind(event_key)
        .bind(turn_id)
        .bind(item_id)
        .bind(format!("compat/v1/{}", event.event_type))
        .bind(status)
        .bind(payload)
        .bind(hash)
        .fetch_one(&mut **transaction)
        .await
        .map_err(store_error)?;
        Self::notify_v1_event_in_transaction(transaction, scope.root_thread_id, sequence).await?;
        Ok(SequencedAssistantTurnEventV1 {
            sequence,
            event: event.clone(),
        })
    }

    /// Reads an ordered, bounded V1 cursor page for one platform root Thread.
    pub async fn read_v1_events_after(
        &self,
        root_thread_id: ThreadId,
        after_sequence: i64,
        limit: i64,
    ) -> ThreadStoreResult<Vec<SequencedAssistantTurnEventV1>> {
        if after_sequence < 0 || !(1..=1_000).contains(&limit) {
            return Err(ThreadStoreError::InvalidRequest {
                message: "event cursor must be non-negative and limit must be 1 to 1000"
                    .to_string(),
            });
        }
        let scope = self
            .root_scope_with_deleted(thread_uuid(root_thread_id)?, true)
            .await?;
        let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
        let rows = sqlx::query(
            r#"
            SELECT sequence, payload
            FROM assistant_runtime_items
            WHERE runtime_thread_id = $1
              AND sequence > $2
              AND event_type LIKE 'compat/v1/%'
            ORDER BY sequence
            LIMIT $3
            "#,
        )
        .bind(thread_uuid(root_thread_id)?)
        .bind(after_sequence)
        .bind(limit)
        .fetch_all(&mut *transaction)
        .await
        .map_err(store_error)?;
        transaction.commit().await.map_err(store_error)?;
        rows.into_iter()
            .map(|row| {
                let sequence = row.try_get("sequence").map_err(store_error)?;
                let payload: Value = row.try_get("payload").map_err(store_error)?;
                let event = serde_json::from_value(payload).map_err(json_error)?;
                Ok(SequencedAssistantTurnEventV1 { sequence, event })
            })
            .collect()
    }
}

fn terminal_lifecycle_event(
    kernel_thread_id: ThreadId,
    turn_id: &str,
    call: &crate::platform_lifecycle::RecoveredToolCall,
) -> PlatformLifecycleEvent {
    PlatformLifecycleEvent {
        kernel_thread_id,
        turn_id: turn_id.to_string(),
        item_id: Some(call.call_id.clone()),
        event_key: format!("tool-result/{turn_id}/{}", call.call_id),
        item_type: "tool_result".to_string(),
        status: call.result_status.to_string(),
        payload: serde_json::json!({
            "schema_version": "agent-runtime-tool-lifecycle/v1",
            "turn_id": turn_id,
            "tool_call_id": call.call_id,
            "lifecycle": "terminal",
            "result_status": call.result_status,
            "detail": call.detail,
            "recovery": "terminal_admission",
        }),
    }
}

// Restarting projection may allocate another local timestamp. Preserve the
// saved timestamp; event identity, schema, parameters and facts remain exact.
fn same_event_fact(original: &AssistantTurnEventV1, replay: &AssistantTurnEventV1) -> bool {
    original.schema_version == replay.schema_version
        && original.event_type == replay.event_type
        && original.data == replay.data
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    #[test]
    fn recovery_event_reuses_original_timestamp_but_never_changes_facts() {
        let original = AssistantTurnEventV1::new(
            "approval_required",
            serde_json::json!({"approval_id":"original","arguments_hash":"original-hash"}),
        );
        let mut replay = original.clone();
        replay.timestamp += 10.0;
        assert!(same_event_fact(&original, &replay));
        replay.data["arguments_hash"] = serde_json::json!("changed-hash");
        assert!(!same_event_fact(&original, &replay));
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
