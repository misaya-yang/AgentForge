use codex_protocol::ThreadId;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use sqlx::Row;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::MemberScope;
use super::PlatformThreadIdentity;
use super::PostgresThreadStore;
use super::projection::store_error;
use super::projection::thread_uuid;

impl PostgresThreadStore {
    // The workspace lint protects SQLite callers; this store is explicitly PostgreSQL.
    #[allow(clippy::disallowed_methods)]
    pub async fn connect(database_url: &str, max_connections: u32) -> ThreadStoreResult<Self> {
        validate_pool_size(max_connections)?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(database_url)
            .await
            .map_err(connection_error)?;
        Ok(Self::from_pool(pool))
    }

    // The workspace lint protects SQLite callers; this store is explicitly PostgreSQL.
    #[allow(clippy::disallowed_methods)]
    pub async fn connect_with_options(
        options: PgConnectOptions,
        max_connections: u32,
    ) -> ThreadStoreResult<Self> {
        validate_pool_size(max_connections)?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect_with(options)
            .await
            .map_err(connection_error)?;
        Ok(Self::from_pool(pool))
    }

    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            instance_id: Uuid::now_v7(),
            execution_claims: Default::default(),
            pending_metadata: Default::default(),
            write_failures: Default::default(),
            terminal_identity_cache: Default::default(),
            staged_startup: Default::default(),
        }
    }

    /// Persists tenant ownership before the matching host-reserved `thread/start`.
    pub async fn authorize_root(&self, identity: &PlatformThreadIdentity) -> ThreadStoreResult<()> {
        validate_identity(identity)?;
        let root_id = thread_uuid(identity.runtime_thread_id)?;
        sqlx::query(
            r#"
            INSERT INTO assistant_runtime_threads (
                runtime_thread_id, tenant_id, user_id, session_id,
                source_kind, import_status
            ) VALUES ($1, $2, $3, $4, 'native', 'pending')
            ON CONFLICT (runtime_thread_id) DO NOTHING
            "#,
        )
        .bind(root_id)
        .bind(&identity.tenant_id)
        .bind(&identity.user_id)
        .bind(&identity.session_id)
        .execute(&self.pool)
        .await
        .map_err(store_error)?;

        let scope = self.root_scope(root_id).await?;
        if scope.tenant_id != identity.tenant_id
            || scope.user_id != identity.user_id
            || scope.session_id != identity.session_id
        {
            return Err(ThreadStoreError::Conflict {
                message: "runtime thread identity is already owned by another scope".to_string(),
            });
        }
        Ok(())
    }

    pub(crate) async fn root_scope(&self, root_thread_id: Uuid) -> ThreadStoreResult<MemberScope> {
        let row = sqlx::query(
            r#"
            SELECT tenant_id, user_id, session_id
            FROM assistant_runtime_threads
            WHERE runtime_thread_id = $1 AND deleted_at IS NULL
            "#,
        )
        .bind(root_thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?
        .ok_or_else(|| ThreadStoreError::Conflict {
            message: "root thread was not authorized by the platform".to_string(),
        })?;
        Ok(MemberScope {
            root_thread_id,
            tenant_id: row.try_get("tenant_id").map_err(store_error)?,
            user_id: row.try_get("user_id").map_err(store_error)?,
            session_id: row.try_get("session_id").map_err(store_error)?,
        })
    }

    pub(crate) async fn member_scope(
        &self,
        kernel_thread_id: Uuid,
    ) -> ThreadStoreResult<MemberScope> {
        let row = sqlx::query(
            r#"
            SELECT member.runtime_thread_id, member.tenant_id, member.user_id,
                   member.session_id
            FROM assistant_runtime_thread_members AS member
            JOIN assistant_runtime_threads AS root
              ON root.runtime_thread_id = member.runtime_thread_id
             AND root.deleted_at IS NULL
            WHERE member.kernel_thread_id = $1
            "#,
        )
        .bind(kernel_thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(store_error)?
        .ok_or_else(|| ThreadStoreError::ThreadNotFound {
            thread_id: ThreadId::from_u128(kernel_thread_id.as_u128()),
        })?;
        Ok(MemberScope {
            root_thread_id: row.try_get("runtime_thread_id").map_err(store_error)?,
            tenant_id: row.try_get("tenant_id").map_err(store_error)?,
            user_id: row.try_get("user_id").map_err(store_error)?,
            session_id: row.try_get("session_id").map_err(store_error)?,
        })
    }
}

fn validate_pool_size(max_connections: u32) -> ThreadStoreResult<()> {
    if (1..=200).contains(&max_connections) {
        Ok(())
    } else {
        Err(ThreadStoreError::InvalidRequest {
            message: "PostgreSQL pool size must be between 1 and 200".to_string(),
        })
    }
}

fn validate_identity(identity: &PlatformThreadIdentity) -> ThreadStoreResult<()> {
    if identity.tenant_id.trim().is_empty()
        || identity.user_id.trim().is_empty()
        || identity.session_id.trim().is_empty()
        || identity.tenant_id.len() > 255
        || identity.user_id.len() > 255
        || identity.session_id.len() > 255
    {
        return Err(ThreadStoreError::InvalidRequest {
            message: "tenant, user, and session identity must contain 1 to 255 bytes".to_string(),
        });
    }
    Ok(())
}

fn connection_error(_: sqlx::Error) -> ThreadStoreError {
    ThreadStoreError::Internal {
        message: "failed to connect to PostgreSQL thread store".to_string(),
    }
}
