use chrono::DateTime;
use chrono::Utc;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::SessionContextWindow;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadMemoryMode;
use codex_rollout::RolloutItem;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadMetadataPatch;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use sqlx::Row;
use uuid::Uuid;

use super::PostgresThreadStore;
use super::ThreadProjection;

impl PostgresThreadStore {
    /// Every mutating writer takes the root lock before a projection lock. This
    /// is also the deletion fence: no append can race past a committed tombstone.
    pub(crate) async fn lock_root(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        root_thread_id: Uuid,
    ) -> ThreadStoreResult<()> {
        let root: Option<Uuid> = sqlx::query_scalar(
            "SELECT runtime_thread_id FROM assistant_runtime_threads WHERE runtime_thread_id=$1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(root_thread_id)
        .fetch_optional(&mut **transaction)
        .await
        .map_err(store_error)?;
        if root.is_none() {
            return Err(ThreadStoreError::ThreadNotFound {
                thread_id: ThreadId::from_u128(root_thread_id.as_u128()),
            });
        }
        self.check_execution_write(transaction, root_thread_id)
            .await?;
        Ok(())
    }

    pub(crate) async fn load_projection(
        &self,
        thread_id: ThreadId,
        include_archived: bool,
    ) -> ThreadStoreResult<(
        ThreadProjection,
        Option<DateTime<Utc>>,
        DateTime<Utc>,
        DateTime<Utc>,
    )> {
        let row = sqlx::query(
            r#"
            SELECT projection.projection, projection.archived_at, projection.deleted_at,
                   projection.created_at, projection.updated_at
            FROM assistant_runtime_thread_projections AS projection
            JOIN assistant_runtime_threads AS root
              ON root.runtime_thread_id = projection.runtime_thread_id
             AND root.deleted_at IS NULL
            WHERE projection.kernel_thread_id = $1
            "#,
        )
        .bind(thread_uuid(thread_id)?)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?
        .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?;
        let archived_at: Option<DateTime<Utc>> = row.try_get("archived_at").map_err(store_error)?;
        let deleted_at: Option<DateTime<Utc>> = row.try_get("deleted_at").map_err(store_error)?;
        if deleted_at.is_some() || (archived_at.is_some() && !include_archived) {
            return Err(ThreadStoreError::ThreadNotFound { thread_id });
        }
        let value: Value = row.try_get("projection").map_err(store_error)?;
        Ok((
            serde_json::from_value(value).map_err(json_error)?,
            archived_at,
            row.try_get("created_at").map_err(store_error)?,
            row.try_get("updated_at").map_err(store_error)?,
        ))
    }

    pub(super) async fn ensure_visible(
        &self,
        thread_id: ThreadId,
        include_archived: bool,
    ) -> ThreadStoreResult<()> {
        self.load_projection(thread_id, include_archived).await?;
        Ok(())
    }

    pub(super) async fn set_archived(
        &self,
        thread_id: ThreadId,
        archived: bool,
    ) -> ThreadStoreResult<()> {
        let kernel_thread_id = thread_uuid(thread_id)?;
        let result = sqlx::query(
            r#"
            UPDATE assistant_runtime_thread_projections
            SET archived_at = CASE WHEN $2 THEN NOW() ELSE NULL END
            WHERE kernel_thread_id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(kernel_thread_id)
        .bind(archived)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        if result.rows_affected() == 0 {
            return Err(ThreadStoreError::ThreadNotFound { thread_id });
        }
        sqlx::query(
            r#"
            UPDATE assistant_runtime_threads
            SET archived_at = CASE WHEN $2 THEN NOW() ELSE NULL END
            WHERE runtime_thread_id = $1
            "#,
        )
        .bind(kernel_thread_id)
        .bind(archived)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;
        Ok(())
    }
}

pub(super) fn session_meta_item(params: &CreateThreadParams) -> RolloutItem {
    RolloutItem::SessionMeta(SessionMetaLine {
        meta: SessionMeta {
            creator_user_id: params.creator_user_id.clone(),
            creator_account_id: params.creator_account_id.clone(),
            runtime_workspace_roots: params.runtime_workspace_roots.as_ref().map(|roots| {
                roots
                    .iter()
                    .map(|root| root.as_path().to_path_buf())
                    .collect()
            }),
            session_id: params.session_id,
            id: params.thread_id,
            forked_from_id: params.forked_from_id,
            parent_thread_id: params.parent_thread_id,
            cwd: params.metadata.cwd.clone().unwrap_or_default(),
            agent_nickname: params.source.get_nickname(),
            agent_role: params.source.get_agent_role(),
            agent_path: params.source.get_agent_path().map(Into::into),
            originator: params.originator.clone(),
            source: params.source.clone(),
            thread_source: params.thread_source.clone(),
            model_provider: Some(params.metadata.model_provider.clone()),
            base_instructions: Some(params.base_instructions.clone()),
            dynamic_tools: (!params.dynamic_tools.is_empty()).then(|| params.dynamic_tools.clone()),
            selected_capability_roots: params.selected_capability_roots.clone(),
            memory_mode: matches!(params.metadata.memory_mode, ThreadMemoryMode::Disabled)
                .then_some("disabled".to_string()),
            history_mode: params.history_mode,
            history_base: params.history_base,
            subagent_history_start_ordinal: params.subagent_history_start_ordinal,
            multi_agent_version: params.multi_agent_version,
            context_window: Some(SessionContextWindow::new(params.initial_window_id.clone())),
            ..SessionMeta::default()
        },
        git: None,
    })
}

pub(super) fn stored_thread(
    thread_id: ThreadId,
    created: CreateThreadParams,
    metadata: ThreadMetadataPatch,
    archived_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    history: Option<StoredThreadHistory>,
) -> StoredThread {
    let resolved_git_info = git_info(&metadata);
    StoredThread {
        originator: Some(created.originator.clone()),
        daybreak_enabled: metadata.daybreak_enabled,
        thread_id,
        extra_config: created.extra_config,
        rollout_path: metadata.rollout_path,
        forked_from_id: created.forked_from_id,
        parent_thread_id: created.parent_thread_id,
        preview: metadata.preview.unwrap_or_default(),
        name: metadata.name.flatten(),
        model_provider: metadata
            .model_provider
            .unwrap_or(created.metadata.model_provider),
        model: metadata.model,
        reasoning_effort: metadata.reasoning_effort.flatten(),
        created_at: metadata.created_at.unwrap_or(created_at),
        updated_at: metadata.updated_at.unwrap_or(updated_at),
        recency_at: metadata
            .advance_recency_at
            .or(metadata.updated_at)
            .unwrap_or(updated_at),
        archived_at,
        section: None,
        section_position: None,
        section_entered_at: None,
        project_id: metadata.project_id.flatten(),
        cwd: metadata.cwd.or(created.metadata.cwd).unwrap_or_default(),
        cli_version: metadata
            .cli_version
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
        source: metadata.source.unwrap_or(created.source),
        history_mode: created.history_mode,
        thread_source: metadata.thread_source.flatten().or(created.thread_source),
        agent_nickname: metadata.agent_nickname.flatten(),
        agent_role: metadata.agent_role.flatten(),
        agent_path: metadata.agent_path.flatten(),
        git_info: resolved_git_info,
        approval_mode: metadata.approval_mode.unwrap_or(AskForApproval::Never),
        permission_profile: metadata
            .permission_profile
            .unwrap_or_else(PermissionProfile::read_only),
        token_usage: metadata.token_usage,
        first_user_message: metadata.first_user_message,
        history,
    }
}

fn git_info(metadata: &ThreadMetadataPatch) -> Option<codex_protocol::protocol::GitInfo> {
    let patch = metadata.git_info.as_ref()?;
    let commit_hash = patch
        .sha
        .clone()
        .flatten()
        .as_deref()
        .map(codex_git_utils::GitSha::new);
    let branch = patch.branch.clone().flatten();
    let repository_url = patch.origin_url.clone().flatten();
    if commit_hash.is_none() && branch.is_none() && repository_url.is_none() {
        return None;
    }
    Some(codex_protocol::protocol::GitInfo {
        commit_hash,
        branch,
        repository_url,
    })
}

pub(crate) fn thread_uuid(thread_id: ThreadId) -> ThreadStoreResult<Uuid> {
    Uuid::parse_str(&thread_id.to_string()).map_err(|error| ThreadStoreError::InvalidRequest {
        message: format!("invalid Agent thread id: {error}"),
    })
}

pub(crate) fn payload_hash(payload: &Value) -> ThreadStoreResult<String> {
    let encoded = serde_json::to_vec(payload).map_err(json_error)?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}

pub(crate) fn json_error(error: serde_json::Error) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: format!("failed to encode thread-store payload: {error}"),
    }
}

pub(crate) fn store_error(error: sqlx::Error) -> ThreadStoreError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.code().as_deref() == Some("23505")
    {
        return ThreadStoreError::Conflict {
            message: "PostgreSQL thread-store identity conflict".to_string(),
        };
    }
    ThreadStoreError::Internal {
        message: format!("PostgreSQL thread-store operation failed: {error}"),
    }
}
