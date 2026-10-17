use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;

use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_rollout::RolloutItem;
use codex_rollout::persisted_rollout_items;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadMetadataPatch;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use codex_thread_store::UpdateThreadMetadataParams;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use sqlx::Row;
use uuid::Uuid;

mod approval_decision;
mod events;
pub(crate) mod event_notifications;
pub(crate) mod execution;
mod identity;
pub(crate) mod history_codec;
pub(crate) mod projection;
pub(crate) mod recovery_terminal;
mod thread_store;

pub(crate) use self::events::PlatformLifecycleEvent;
use self::projection::json_error;
pub(crate) use self::projection::payload_hash;
use self::projection::session_meta_item;
use self::projection::store_error;
use self::projection::stored_thread;
use self::projection::thread_uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlatformThreadIdentity {
    pub runtime_thread_id: ThreadId,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
}

impl PlatformThreadIdentity {
    pub fn new(
        runtime_thread_id: ThreadId,
        tenant_id: impl Into<String>,
        user_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            runtime_thread_id,
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            session_id: session_id.into(),
        }
    }
}

#[derive(Clone)]
pub struct PostgresThreadStore {
    pub(crate) pool: PgPool,
    pub(crate) instance_id: Uuid,
    pub(crate) execution_claims:
        Arc<std::sync::Mutex<HashMap<ThreadId, execution::ExecutionClaim>>>,
    pending_metadata: Arc<tokio::sync::Mutex<HashMap<ThreadId, ThreadMetadataPatch>>>,
    write_failures: Arc<std::sync::Mutex<HashSet<ThreadId>>>,
    terminal_identity_cache: Arc<std::sync::Mutex<HashMap<ThreadId, PlatformThreadIdentity>>>,
    pub(crate) staged_startup:
        Arc<tokio::sync::Mutex<HashMap<ThreadId, crate::tool_policy::PlatformStartupData>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeStoreReadinessStage {
    Healthy,
    DatabaseUnavailable,
    SchemaUsageMissing,
    ThreadSchemaMissing,
    ThreadRootSelectMissing,
    ThreadRootInsertMissing,
    ThreadRootUpdateMissing,
    ThreadMemberSelectMissing,
    ThreadMemberInsertMissing,
    ThreadProjectionSelectMissing,
    ThreadProjectionInsertMissing,
    ThreadProjectionUpdateMissing,
    RuntimeItemsSelectMissing,
    RuntimeItemAppendFunctionMissing,
    RuntimeItemAppendExecuteMissing,
}

impl RuntimeStoreReadinessStage {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::DatabaseUnavailable => "database_unavailable",
            Self::SchemaUsageMissing => "runtime_schema_usage_missing",
            Self::ThreadSchemaMissing => "runtime_thread_schema_missing",
            Self::ThreadRootSelectMissing => "runtime_thread_root_select_missing",
            Self::ThreadRootInsertMissing => "runtime_thread_root_insert_missing",
            Self::ThreadRootUpdateMissing => "runtime_thread_root_update_missing",
            Self::ThreadMemberSelectMissing => "runtime_thread_member_select_missing",
            Self::ThreadMemberInsertMissing => "runtime_thread_member_insert_missing",
            Self::ThreadProjectionSelectMissing => "runtime_thread_projection_select_missing",
            Self::ThreadProjectionInsertMissing => "runtime_thread_projection_insert_missing",
            Self::ThreadProjectionUpdateMissing => "runtime_thread_projection_update_missing",
            Self::RuntimeItemsSelectMissing => "runtime_items_select_missing",
            Self::RuntimeItemAppendFunctionMissing => "runtime_item_append_function_missing",
            Self::RuntimeItemAppendExecuteMissing => "runtime_item_append_execute_missing",
        }
    }

    pub(crate) fn is_healthy(self) -> bool {
        self == Self::Healthy
    }
}

#[derive(Clone, Debug)]
pub(crate) struct MemberScope {
    pub(crate) root_thread_id: Uuid,
    tenant_id: String,
    user_id: String,
    session_id: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct ThreadProjection {
    #[serde(default, deserialize_with = "deserialize_creation_metadata")]
    pub(crate) created: Option<CreateThreadParams>,
    metadata: ThreadMetadataPatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_policy: Option<crate::tool_policy::PlatformToolPolicy>,
}

impl PostgresThreadStore {
    /// Tombstone the root Runtime thread for one platform session while
    /// retaining its append-only item log.  Session deletion is deliberately
    /// idempotent: a missing or already-deleted thread is reported as false.
    pub async fn cleanup_session(
        &self,
        tenant_id: &str,
        user_id: &str,
        session_id: &str,
    ) -> ThreadStoreResult<bool> {
        let mut transaction = self.pool.begin().await.map_err(store_error)?;
        let runtime_thread_id: Option<Uuid> = sqlx::query_scalar(
            "SELECT runtime_thread_id FROM assistant_runtime_threads \
             WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 \
               AND deleted_at IS NULL ORDER BY created_at LIMIT 1 FOR UPDATE",
        )
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store_error)?;
        let Some(runtime_thread_id) = runtime_thread_id else {
            transaction.commit().await.map_err(store_error)?;
            return Ok(false);
        };

        sqlx::query(
            "UPDATE assistant_runtime_thread_projections \
                SET deleted_at=NOW(), updated_at=NOW() \
              WHERE runtime_thread_id=$1 AND deleted_at IS NULL",
        )
        .bind(runtime_thread_id)
        .execute(&mut *transaction)
        .await
        .map_err(store_error)?;
        let result = sqlx::query(
            "UPDATE assistant_runtime_threads \
                SET deleted_at=NOW(), updated_at=NOW() \
              WHERE runtime_thread_id=$1 AND tenant_id=$2 AND user_id=$3 \
                AND session_id=$4 AND deleted_at IS NULL",
        )
        .bind(runtime_thread_id)
        .bind(tenant_id)
        .bind(user_id)
        .bind(session_id)
        .execute(&mut *transaction)
        .await
        .map_err(store_error)?;
        transaction.commit().await.map_err(store_error)?;
        Ok(result.rows_affected() > 0)
    }

    async fn create_thread(&self, mut params: CreateThreadParams) -> ThreadStoreResult<()> {
        let kernel_thread_id = thread_uuid(params.thread_id)?;
        let (scope, relation_kind) = if let Some(parent_thread_id) = params.parent_thread_id {
            (
                self.member_scope(thread_uuid(parent_thread_id)?).await?,
                "subagent",
            )
        } else if let Some(forked_from_id) = params.forked_from_id {
            (
                self.member_scope(thread_uuid(forked_from_id)?).await?,
                "fork",
            )
        } else {
            (self.root_scope(kernel_thread_id).await?, "root")
        };
        if relation_kind == "root" && scope.root_thread_id != kernel_thread_id {
            return Err(ThreadStoreError::Conflict {
                message: "reserved root thread does not match platform ownership".to_string(),
            });
        }
        let kernel_session_id =
            Uuid::parse_str(&params.session_id.to_string()).map_err(|error| {
                ThreadStoreError::InvalidRequest {
                    message: format!("invalid Agent session id: {error}"),
                }
            })?;
        // Managed creation identity comes from the authenticated platform scope.
        // A tenant is not a ChatGPT account and must never populate that field.
        params.creator_user_id = Some(scope.user_id.clone());
        params.creator_account_id = None;
        let projection = ThreadProjection {
            created: Some(params.clone()),
            metadata: ThreadMetadataPatch::default(),
            tool_policy: self
                .staged_startup
                .lock()
                .await
                .get(&params.thread_id)
                .map(|startup| startup.policy.clone()),
        };
        let creation_metadata = serde_json::to_value(&params).map_err(json_error)?;
        let projection_json = serde_json::to_value(&projection).map_err(json_error)?;
        let mut transaction = self.pool.begin().await.map_err(store_error)?;
        self.lock_root(&mut transaction, scope.root_thread_id)
            .await?;
        sqlx::query(
            r#"
            INSERT INTO assistant_runtime_thread_members (
                kernel_thread_id, runtime_thread_id, kernel_session_id,
                parent_kernel_thread_id, forked_from_kernel_thread_id,
                relation_kind, tenant_id, user_id, session_id, metadata
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ON CONFLICT (kernel_thread_id) DO NOTHING
            "#,
        )
        .bind(kernel_thread_id)
        .bind(scope.root_thread_id)
        .bind(kernel_session_id)
        .bind(params.parent_thread_id.map(thread_uuid).transpose()?)
        .bind(params.forked_from_id.map(thread_uuid).transpose()?)
        .bind(relation_kind)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(&creation_metadata)
        .execute(&mut *transaction)
        .await
        .map_err(store_error)?;
        let member = sqlx::query(
            r#"
            SELECT runtime_thread_id, tenant_id, user_id, session_id, metadata
            FROM assistant_runtime_thread_members
            WHERE kernel_thread_id = $1
            FOR SHARE
            "#,
        )
        .bind(kernel_thread_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(store_error)?;
        let stored_root_id: Uuid = member.try_get("runtime_thread_id").map_err(store_error)?;
        let stored_tenant_id: String = member.try_get("tenant_id").map_err(store_error)?;
        let stored_user_id: String = member.try_get("user_id").map_err(store_error)?;
        let stored_session_id: String = member.try_get("session_id").map_err(store_error)?;
        let stored_metadata: Value = member.try_get("metadata").map_err(store_error)?;
        if stored_root_id != scope.root_thread_id
            || stored_tenant_id != scope.tenant_id
            || stored_user_id != scope.user_id
            || stored_session_id != scope.session_id
            || stored_metadata != creation_metadata
        {
            return Err(ThreadStoreError::Conflict {
                message: "Agent thread identity is already bound to another platform member"
                    .to_string(),
            });
        }
        sqlx::query(
            r#"
            INSERT INTO assistant_runtime_thread_projections (
                kernel_thread_id, runtime_thread_id, tenant_id, user_id,
                session_id, projection
            ) VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (kernel_thread_id) DO NOTHING
            "#,
        )
        .bind(kernel_thread_id)
        .bind(scope.root_thread_id)
        .bind(&scope.tenant_id)
        .bind(&scope.user_id)
        .bind(&scope.session_id)
        .bind(projection_json)
        .execute(&mut *transaction)
        .await
        .map_err(store_error)?;
        if relation_kind == "root" {
            sqlx::query(
                "UPDATE assistant_runtime_threads SET import_status = 'not_required' WHERE runtime_thread_id = $1",
            )
            .bind(scope.root_thread_id)
            .execute(&mut *transaction)
            .await
            .map_err(store_error)?;
        }
        // The member, projection and canonical first history item become visible
        // together. A crash cannot leave a resumable thread with no session meta.
        let session_meta = session_meta_item(&params);
        self.append_items_in_transaction(
            &mut transaction,
            &scope,
            params.thread_id,
            vec![(
                format!("rollout/session-meta/{}", params.thread_id),
                kernel_thread_id,
                session_meta,
            )],
        )
        .await?;
        transaction.commit().await.map_err(store_error)?;
        self.staged_startup.lock().await.remove(&params.thread_id);
        self.cache_terminal_identity(
            params.thread_id,
            PlatformThreadIdentity::new(
                ThreadId::from_u128(scope.root_thread_id.as_u128()),
                scope.tenant_id,
                scope.user_id,
                scope.session_id,
            ),
        );
        Ok(())
    }

    async fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreResult<()> {
        if params.items.is_empty() {
            return Ok(());
        }
        let projection = self.load_projection(params.thread_id, true).await?.0;
        let history_mode = projection
            .created
            .as_ref()
            .map(|created| created.history_mode)
            .unwrap_or_default();
        let items = persisted_rollout_items(&params.items, history_mode)
            .into_iter()
            .map(|item| {
                let event_id = Uuid::now_v7();
                (
                    format!("rollout/{}/{event_id}", params.thread_id),
                    event_id,
                    item,
                )
            })
            .collect();
        self.append_items_with_keys(params.thread_id, items).await
    }

    async fn append_items_with_keys(
        &self,
        thread_id: ThreadId,
        items: Vec<(String, Uuid, RolloutItem)>,
    ) -> ThreadStoreResult<()> {
        if items.is_empty() {
            return Ok(());
        }
        let kernel_thread_id = thread_uuid(thread_id)?;
        let scope = self.member_scope(kernel_thread_id).await?;
        let mut transaction = self.pool.begin().await.map_err(store_error)?;
        self.lock_root(&mut transaction, scope.root_thread_id)
            .await?;
        let visible: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM assistant_runtime_thread_projections WHERE kernel_thread_id=$1 AND deleted_at IS NULL)",
        )
        .bind(kernel_thread_id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(store_error)?;
        if !visible {
            return Err(ThreadStoreError::ThreadNotFound { thread_id });
        }
        let output_calls: Vec<_> = items
            .iter()
            .filter_map(|(_, _, item)| match item {
                RolloutItem::ResponseItem(envelope) => match &envelope.item {
                    codex_protocol::models::ResponseItem::FunctionCallOutput {
                        call_id: Some(call_id),
                        ..
                    } => Some(call_id.clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        self.append_items_in_transaction(&mut transaction, &scope, thread_id, items)
            .await?;
        transaction.commit().await.map_err(store_error)?;
        if crate::http_service::recovery_fault::armed("core_output_saved").await {
            for call_id in output_calls {
                let row = sqlx::query("SELECT params FROM assistant_runtime_invocations WHERE kernel_thread_id=$1 AND call_id=$2 ORDER BY created_at DESC LIMIT 1")
                    .bind(kernel_thread_id).bind(call_id).fetch_optional(&self.pool).await.map_err(store_error)?;
                if let Some(row) = row {
                    let params: codex_app_server_protocol::DynamicToolCallParams =
                        serde_json::from_value(row.try_get("params").map_err(store_error)?)
                            .map_err(json_error)?;
                    crate::http_service::recovery_fault::pause(
                        "core_output_saved",
                        &params,
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await;
                }
            }
        }
        Ok(())
    }

    async fn append_items_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        scope: &MemberScope,
        thread_id: ThreadId,
        items: Vec<(String, Uuid, RolloutItem)>,
    ) -> ThreadStoreResult<()> {
        let kernel_thread_id = thread_uuid(thread_id)?;
        for (event_key, event_id, item) in items {
            let payload = history_codec::encode_rollout(&item).map_err(json_error)?;
            let payload_hash = payload_hash(&payload)?;
            let item_type = payload.get("type").and_then(Value::as_str);
            sqlx::query(
                r#"
                SELECT append_assistant_runtime_item(
                    $1, $2, $3, $4, $5, $6, $7,
                    NULL, NULL, 'rollout/item', $8, NULL, $9, $10
                )
                "#,
            )
            .bind(scope.root_thread_id)
            .bind(kernel_thread_id)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .bind(event_id)
            .bind(event_key)
            .bind(item_type)
            .bind(payload)
            .bind(payload_hash)
            .execute(&mut **transaction)
            .await
            .map_err(store_error)?;
        }
        Ok(())
    }

    async fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreResult<StoredThreadHistory> {
        self.ensure_visible(params.thread_id, params.include_archived)
            .await?;
        let rows = sqlx::query(
            r#"
            SELECT payload
            FROM assistant_runtime_items
            WHERE kernel_thread_id = $1
              AND event_type = 'rollout/item'
            ORDER BY sequence
            "#,
        )
        .bind(thread_uuid(params.thread_id)?)
        .fetch_all(&self.pool)
        .await
        .map_err(store_error)?;
        let items = rows
            .into_iter()
            .map(|row| {
                let payload: Value = row.try_get("payload").map_err(store_error)?;
                history_codec::decode_rollout(payload).map_err(json_error)
            })
            .collect::<ThreadStoreResult<Vec<_>>>()?;
        Ok(StoredThreadHistory {
            thread_id: params.thread_id,
            items,
            // No snapshot revision is issued until a fenced validation protocol exists.
            revision: None,
        })
    }

    async fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreResult<StoredThread> {
        let (projection, archived_at, created_at, updated_at) = self
            .load_projection(params.thread_id, params.include_archived)
            .await?;
        let created = projection
            .created
            .ok_or_else(|| ThreadStoreError::Internal {
                message: "thread projection is missing creation metadata".to_string(),
            })?;
        let history = if params.include_history {
            Some(
                self.load_history(LoadThreadHistoryParams {
                    thread_id: params.thread_id,
                    include_archived: params.include_archived,
                })
                .await?,
            )
        } else {
            None
        };
        Ok(stored_thread(
            params.thread_id,
            created,
            projection.metadata,
            archived_at,
            created_at,
            updated_at,
            history,
        ))
    }

    async fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreResult<Option<StoredThread>> {
        let kernel_thread_id = thread_uuid(params.thread_id)?;
        let scope = self.member_scope(kernel_thread_id).await?;
        let mut pending = self.pending_metadata.lock().await;
        let mut transaction = self.pool.begin().await.map_err(store_error)?;
        self.lock_root(&mut transaction, scope.root_thread_id)
            .await?;
        let row = sqlx::query(
            r#"
            SELECT projection, archived_at, deleted_at
            FROM assistant_runtime_thread_projections
            WHERE kernel_thread_id = $1
            FOR UPDATE
            "#,
        )
        .bind(kernel_thread_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store_error)?
        .ok_or(ThreadStoreError::ThreadNotFound {
            thread_id: params.thread_id,
        })?;
        let archived_at: Option<DateTime<Utc>> = row.try_get("archived_at").map_err(store_error)?;
        let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at").map_err(store_error)?;
        if deleted_at.is_some() || (archived_at.is_some() && !params.include_archived) {
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: params.thread_id,
            });
        }
        let value: Value = row.try_get("projection").map_err(store_error)?;
        let mut projection: ThreadProjection = serde_json::from_value(value).map_err(json_error)?;
        let mut patch = pending.get(&params.thread_id).cloned().unwrap_or_default();
        patch.merge(params.patch);
        // Creator and originator fields are fill-only; ordinary metadata patches
        // cannot change the authenticated creator or the creation-time originator.
        patch.creator_user_id = projection
            .metadata
            .creator_user_id
            .is_none()
            .then(|| scope.user_id.clone());
        patch.creator_account_id = None;
        patch.originator = if projection.metadata.originator.is_none() {
            projection
                .created
                .as_ref()
                .map(|created| created.originator.clone())
        } else {
            None
        };
        projection.metadata.merge(patch);
        sqlx::query(
            "UPDATE assistant_runtime_thread_projections SET projection = $2 WHERE kernel_thread_id = $1",
        )
        .bind(kernel_thread_id)
        .bind(serde_json::to_value(projection).map_err(json_error)?)
        .execute(&mut *transaction)
        .await
        .map_err(store_error)?;
        transaction.commit().await.map_err(store_error)?;
        pending.remove(&params.thread_id);
        drop(pending);
        self.read_thread(ReadThreadParams {
            thread_id: params.thread_id,
            include_archived: params.include_archived,
            include_history: false,
        })
        .await
        .map(Some)
    }
}

/// Target dynamic tools use tagged namespaces. Normalize old flat creation
/// metadata on read while retaining the original durable JSON and history.
fn deserialize_creation_metadata<'de, D>(
    deserializer: D,
) -> Result<Option<CreateThreadParams>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    let Some(mut value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    if let Some(tools) = value.get_mut("dynamic_tools") {
        let raw: Vec<Value> = serde_json::from_value(tools.clone()).map_err(D::Error::custom)?;
        let normalized = codex_protocol::dynamic_tools::normalize_dynamic_tool_specs(raw)
            .map_err(D::Error::custom)?;
        *tools = serde_json::to_value(normalized).map_err(D::Error::custom)?;
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(D::Error::custom)
}
