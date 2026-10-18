use std::sync::Arc;

use codex_protocol::ThreadId;
use codex_rollout::RolloutItem;
use codex_thread_store::AppendThreadItemsParams;
use codex_thread_store::ArchiveThreadParams;
use codex_thread_store::CreateThreadParams;
use codex_thread_store::DeleteThreadParams;
use codex_thread_store::ListThreadsParams;
use codex_thread_store::LoadThreadHistoryParams;
use codex_thread_store::PersistContext;
use codex_thread_store::ReadThreadByRolloutPathParams;
use codex_thread_store::ReadThreadParams;
use codex_thread_store::ResumeThreadParams;
use codex_thread_store::StoredModelContext;
use codex_thread_store::StoredThread;
use codex_thread_store::StoredThreadHistory;
use codex_thread_store::ThreadMetadataPatch;
use codex_thread_store::ThreadPage;
use codex_thread_store::ThreadStore;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreFuture;
use codex_thread_store::UpdateThreadMetadataParams;

use super::PostgresThreadStore;
use super::store_error;
use super::thread_uuid;

impl ThreadStore for PostgresThreadStore {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn create_thread(&self, params: CreateThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let thread_id = params.thread_id;
            let result = PostgresThreadStore::create_thread(self, params).await;
            self.record_write_result(thread_id, result)
        })
    }

    fn stage_pending_thread_metadata(
        &self,
        thread_id: ThreadId,
        patch: ThreadMetadataPatch,
    ) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.pending_metadata
                .lock()
                .await
                .entry(thread_id)
                .or_default()
                .merge(patch);
            Ok(())
        })
    }

    fn read_pending_thread_metadata(
        &self,
        thread_id: ThreadId,
    ) -> ThreadStoreFuture<'_, Option<ThreadMetadataPatch>> {
        Box::pin(async move { Ok(self.pending_metadata.lock().await.get(&thread_id).cloned()) })
    }

    fn remove_pending_thread_metadata(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.pending_metadata.lock().await.remove(&thread_id);
            Ok(())
        })
    }

    fn resume_thread(
        &self,
        params: ResumeThreadParams,
    ) -> ThreadStoreFuture<'_, Arc<Vec<RolloutItem>>> {
        Box::pin(async move {
            self.check_write_health(params.thread_id)?;
            self.ensure_visible(params.thread_id, params.include_archived)
                .await?;
            if let Some(history) = params.history
                && !matches!(
                    history.first(),
                    Some(RolloutItem::SessionMeta(meta)) if meta.meta.id == params.thread_id
                )
            {
                // Upstream import/override histories are intentionally caller-owned.
                // Visibility and latched write failures still apply before returning one.
                return Ok(history);
            }
            // This store uses the platform execution fence rather than a local file
            // writer. Reload canonical snapshots after admission; neither an absent
            // nor an unrecognized revision proves that durable history is unchanged.
            let history = PostgresThreadStore::load_history(
                self,
                LoadThreadHistoryParams {
                    thread_id: params.thread_id,
                    include_archived: params.include_archived,
                },
            )
            .await?;
            Ok(Arc::new(history.items))
        })
    }

    fn append_items(&self, params: AppendThreadItemsParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let thread_id = params.thread_id;
            self.check_write_health(thread_id)?;
            let result = PostgresThreadStore::append_items(self, params).await;
            self.record_write_result(thread_id, result)
        })
    }

    fn persist_thread(
        &self,
        thread_id: ThreadId,
        _context: PersistContext,
    ) -> ThreadStoreFuture<'_, ()> {
        // create, append and metadata updates await PostgreSQL COMMIT. This is
        // deliberately synchronous for preparation, turn-start and steered input
        // alike: there is no background queue whose failures could be lost.
        Box::pin(async move {
            self.check_write_health(thread_id)?;
            self.ensure_visible(thread_id, true).await
        })
    }

    fn flush_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        // A read barrier also surfaces a disconnected store instead of reporting
        // a successful flush from a no-op after PostgreSQL has become unavailable.
        Box::pin(async move {
            self.check_write_health(thread_id)?;
            self.ensure_visible(thread_id, true).await
        })
    }

    fn shutdown_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            self.check_write_health(thread_id)?;
            self.ensure_visible(thread_id, true).await?;
            self.pending_metadata.lock().await.remove(&thread_id);
            Ok(())
        })
    }

    fn discard_thread(&self, thread_id: ThreadId) -> ThreadStoreFuture<'_, ()> {
        // The store owns no per-thread file writer. Initialization failure drops
        // only staged metadata; already committed history stays recoverable.
        Box::pin(async move {
            self.clear_host_startup(thread_id).await;
            self.remove_pending_thread_metadata(thread_id).await
        })
    }

    fn load_history(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreFuture<'_, StoredThreadHistory> {
        Box::pin(PostgresThreadStore::load_history(self, params))
    }

    fn load_latest_model_context(
        &self,
        params: LoadThreadHistoryParams,
    ) -> ThreadStoreFuture<'_, StoredModelContext> {
        Box::pin(async move {
            let history = self.load_history(params).await?;
            Ok(StoredModelContext {
                thread_id: history.thread_id,
                items: history.items,
                revision: history.revision,
            })
        })
    }

    fn read_thread(&self, params: ReadThreadParams) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(PostgresThreadStore::read_thread(self, params))
    }

    fn read_thread_by_rollout_path(
        &self,
        _params: ReadThreadByRolloutPathParams,
    ) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(async {
            Err(ThreadStoreError::Unsupported {
                operation: "read_thread_by_rollout_path",
            })
        })
    }

    fn list_threads(&self, _params: ListThreadsParams) -> ThreadStoreFuture<'_, ThreadPage> {
        Box::pin(async {
            Err(ThreadStoreError::Unsupported {
                operation: "thread/list_requires_platform_scope",
            })
        })
    }

    fn update_thread_metadata(
        &self,
        params: UpdateThreadMetadataParams,
    ) -> ThreadStoreFuture<'_, Option<StoredThread>> {
        Box::pin(async move {
            let thread_id = params.thread_id;
            self.check_write_health(thread_id)?;
            let result = PostgresThreadStore::update_thread_metadata(self, params).await;
            self.record_write_result(thread_id, result)
        })
    }

    fn archive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(self.set_archived(params.thread_id, true))
    }

    fn unarchive_thread(&self, params: ArchiveThreadParams) -> ThreadStoreFuture<'_, StoredThread> {
        Box::pin(async move {
            self.set_archived(params.thread_id, false).await?;
            self.read_thread(ReadThreadParams {
                thread_id: params.thread_id,
                include_archived: true,
                include_history: false,
            })
            .await
        })
    }

    fn delete_thread(&self, params: DeleteThreadParams) -> ThreadStoreFuture<'_, ()> {
        Box::pin(async move {
            let kernel_thread_id = thread_uuid(params.thread_id)?;
            let scope = match self.member_scope_with_deleted(kernel_thread_id, true).await {
                Ok(scope) => scope,
                Err(ThreadStoreError::ThreadNotFound { .. }) => return Ok(()),
                Err(error) => return Err(error),
            };
            let mut transaction = self.tenant_transaction(&scope.tenant_id).await?;
            let root_id: Option<uuid::Uuid> = sqlx::query_scalar(
                "SELECT runtime_thread_id FROM assistant_runtime_thread_members WHERE kernel_thread_id=$1",
            )
            .bind(kernel_thread_id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(store_error)?;
            let Some(root_id) = root_id else {
                return Ok(());
            };
            // Lock even an already tombstoned root so concurrent repeat deletes
            // remain idempotent. Append takes this same lock before writing.
            sqlx::query("SELECT runtime_thread_id FROM assistant_runtime_threads WHERE runtime_thread_id=$1 FOR UPDATE")
                .bind(root_id)
                .execute(&mut *transaction)
                .await
                .map_err(store_error)?;
            sqlx::query(
                r#"
                WITH RECURSIVE descendants AS (
                    SELECT kernel_thread_id FROM assistant_runtime_thread_members
                    WHERE kernel_thread_id=$1
                    UNION
                    SELECT member.kernel_thread_id
                    FROM assistant_runtime_thread_members AS member
                    JOIN descendants ON member.parent_kernel_thread_id=descendants.kernel_thread_id
                )
                UPDATE assistant_runtime_thread_projections
                SET deleted_at=COALESCE(deleted_at, NOW())
                WHERE kernel_thread_id IN (SELECT kernel_thread_id FROM descendants)
                   OR ($1=$2 AND runtime_thread_id=$2)
                "#,
            )
            .bind(kernel_thread_id)
            .bind(root_id)
            .execute(&mut *transaction)
            .await
            .map_err(store_error)?;
            sqlx::query(
                "UPDATE assistant_runtime_threads SET deleted_at=COALESCE(deleted_at, NOW()) WHERE runtime_thread_id=$1",
            )
            .bind(kernel_thread_id)
            .execute(&mut *transaction)
            .await
            .map_err(store_error)?;
            transaction.commit().await.map_err(store_error)?;
            self.pending_metadata.lock().await.remove(&params.thread_id);
            // Append-only audit items, artifacts and platform attachment ACLs
            // remain retained. Native thread attachments are explicitly unsupported.
            Ok(())
        })
    }
}

impl PostgresThreadStore {
    pub(crate) fn check_write_health(&self, thread_id: ThreadId) -> super::ThreadStoreResult<()> {
        if self
            .write_failures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&thread_id)
        {
            return Err(ThreadStoreError::Internal {
                message: "thread persistence failed; reload from durable history before continuing"
                    .to_string(),
            });
        }
        Ok(())
    }

    pub(crate) fn record_write_result<T>(
        &self,
        thread_id: ThreadId,
        result: super::ThreadStoreResult<T>,
    ) -> super::ThreadStoreResult<T> {
        if result.is_err() {
            // Core may log an append error and continue. Keep that error sticky
            // so a later successful database read cannot fabricate a flush fence.
            self.write_failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(thread_id);
        }
        result
    }
}
