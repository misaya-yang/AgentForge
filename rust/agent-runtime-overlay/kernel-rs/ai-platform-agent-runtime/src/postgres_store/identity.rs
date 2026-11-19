use codex_protocol::ThreadId;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use sqlx::PgConnection;
use sqlx::Row;
use sqlx::postgres::PgConnectOptions;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use super::MemberScope;
use super::PlatformThreadIdentity;
use super::PostgresThreadStore;
use super::RuntimeStoreReadinessStage;
use super::projection::store_error;
use super::projection::thread_uuid;

const REQUIRED_RUNTIME_STORE_READINESS_SQL: &str = r#"
SELECT CASE
    WHEN NOT COALESCE(
        has_schema_privilege(current_user, current_schema(), 'USAGE'), FALSE
    ) THEN 'runtime_schema_usage_missing'
    WHEN to_regclass('assistant_runtime_threads') IS NULL
      OR to_regclass('assistant_runtime_thread_members') IS NULL
      OR to_regclass('assistant_runtime_thread_projections') IS NULL
      OR to_regclass('assistant_runtime_items') IS NULL THEN 'runtime_thread_schema_missing'
    WHEN to_regprocedure(
          'append_assistant_runtime_item(uuid,uuid,character varying,character varying,character varying,uuid,character varying,character varying,character varying,character varying,character varying,character varying,jsonb,character)'
      ) IS NULL THEN 'runtime_item_append_function_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_threads'), 'SELECT'
    ), FALSE) THEN 'runtime_thread_root_select_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_threads'), 'INSERT'
    ), FALSE) THEN 'runtime_thread_root_insert_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_threads'), 'UPDATE'
    ), FALSE) THEN 'runtime_thread_root_update_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_thread_members'), 'SELECT'
    ), FALSE) THEN 'runtime_thread_member_select_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_thread_members'), 'INSERT'
    ), FALSE) THEN 'runtime_thread_member_insert_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_thread_projections'), 'SELECT'
    ), FALSE) THEN 'runtime_thread_projection_select_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_thread_projections'), 'INSERT'
    ), FALSE) THEN 'runtime_thread_projection_insert_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_thread_projections'), 'UPDATE'
    ), FALSE) THEN 'runtime_thread_projection_update_missing'
    WHEN NOT COALESCE(has_table_privilege(
        current_user, to_regclass('assistant_runtime_items'), 'SELECT'
    ), FALSE) THEN 'runtime_items_select_missing'
    WHEN NOT COALESCE(has_function_privilege(
        current_user,
        to_regprocedure(
            'append_assistant_runtime_item(uuid,uuid,character varying,character varying,character varying,uuid,character varying,character varying,character varying,character varying,character varying,character varying,jsonb,character)'
        ),
        'EXECUTE'
    ), FALSE) THEN 'runtime_item_append_execute_missing'
    WHEN EXISTS (SELECT 1 FROM pg_catalog.pg_class WHERE oid=to_regclass('assistant_runtime_threads') AND relrowsecurity)
      AND (NOT COALESCE(has_function_privilege(current_user,
          to_regprocedure('assistant.runtime_thread_scope_v1(uuid,boolean,boolean)'), 'EXECUTE'), FALSE)
        OR NOT COALESCE(has_function_privilege(current_user,
          to_regprocedure('assistant.runtime_run_scope_v1(uuid)'), 'EXECUTE'), FALSE))
      THEN 'runtime_thread_schema_missing'
    ELSE 'healthy'
END
"#;

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
        Self::from_pool_and_check_readiness(pool).await
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
        Self::from_pool_and_check_readiness(pool).await
    }

    pub async fn from_pool_and_check_readiness(pool: sqlx::PgPool) -> ThreadStoreResult<Self> {
        let store = Self::from_pool(pool);
        let stage = store.readiness_stage().await;
        if !stage.is_healthy() {
            return Err(ThreadStoreError::Internal {
                message: stage.code().to_string(),
            });
        }
        Ok(store)
    }

    pub(crate) async fn readiness_stage(&self) -> RuntimeStoreReadinessStage {
        let Ok(mut connection) = self.pool.acquire().await else {
            return RuntimeStoreReadinessStage::DatabaseUnavailable;
        };
        self.readiness_stage_on(&mut *connection).await
    }

    pub(crate) async fn readiness_stage_on(
        &self,
        connection: &mut PgConnection,
    ) -> RuntimeStoreReadinessStage {
        let code = match sqlx::query_scalar::<_, String>(REQUIRED_RUNTIME_STORE_READINESS_SQL)
            .fetch_one(&mut *connection)
            .await
        {
            Ok(code) => code,
            Err(_) => return RuntimeStoreReadinessStage::DatabaseUnavailable,
        };
        match code.as_str() {
            "healthy" => RuntimeStoreReadinessStage::Healthy,
            "runtime_schema_usage_missing" => RuntimeStoreReadinessStage::SchemaUsageMissing,
            "runtime_thread_schema_missing" => RuntimeStoreReadinessStage::ThreadSchemaMissing,
            "runtime_thread_root_select_missing" => {
                RuntimeStoreReadinessStage::ThreadRootSelectMissing
            }
            "runtime_thread_root_insert_missing" => {
                RuntimeStoreReadinessStage::ThreadRootInsertMissing
            }
            "runtime_thread_root_update_missing" => {
                RuntimeStoreReadinessStage::ThreadRootUpdateMissing
            }
            "runtime_thread_member_select_missing" => {
                RuntimeStoreReadinessStage::ThreadMemberSelectMissing
            }
            "runtime_thread_member_insert_missing" => {
                RuntimeStoreReadinessStage::ThreadMemberInsertMissing
            }
            "runtime_thread_projection_select_missing" => {
                RuntimeStoreReadinessStage::ThreadProjectionSelectMissing
            }
            "runtime_thread_projection_insert_missing" => {
                RuntimeStoreReadinessStage::ThreadProjectionInsertMissing
            }
            "runtime_thread_projection_update_missing" => {
                RuntimeStoreReadinessStage::ThreadProjectionUpdateMissing
            }
            "runtime_items_select_missing" => RuntimeStoreReadinessStage::RuntimeItemsSelectMissing,
            "runtime_item_append_function_missing" => {
                RuntimeStoreReadinessStage::RuntimeItemAppendFunctionMissing
            }
            "runtime_item_append_execute_missing" => {
                RuntimeStoreReadinessStage::RuntimeItemAppendExecuteMissing
            }
            _ => RuntimeStoreReadinessStage::ThreadSchemaMissing,
        }
    }

    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self {
            pool,
            instance_id: Uuid::now_v7(),
            owner_routing: Default::default(),
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
        let mut transaction = self.tenant_transaction(&identity.tenant_id).await?;
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
        .execute(&mut *transaction)
        .await
        .map_err(store_error)?;
        transaction.commit().await.map_err(store_error)?;

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
        self.root_scope_with_deleted(root_thread_id, false).await
    }

    pub(crate) async fn root_scope_with_deleted(
        &self,
        root_thread_id: Uuid,
        include_deleted: bool,
    ) -> ThreadStoreResult<MemberScope> {
        let sql = if self.tenant_controls_enabled().await? {
            "SELECT tenant_id,user_id,session_id FROM assistant.runtime_thread_scope_v1($1,true,$2)"
        } else {
            r#"
            SELECT tenant_id, user_id, session_id
            FROM assistant_runtime_threads
            WHERE runtime_thread_id = $1 AND ($2 OR deleted_at IS NULL)
            "#
        };
        let row = sqlx::query(sql)
            .bind(root_thread_id)
            .bind(include_deleted)
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
        self.member_scope_with_deleted(kernel_thread_id, false)
            .await
    }

    pub(crate) async fn member_scope_with_deleted(
        &self,
        kernel_thread_id: Uuid,
        include_deleted: bool,
    ) -> ThreadStoreResult<MemberScope> {
        let sql = if self.tenant_controls_enabled().await? {
            "SELECT runtime_thread_id,tenant_id,user_id,session_id FROM assistant.runtime_thread_scope_v1($1,false,$2)"
        } else {
            r#"
            SELECT member.runtime_thread_id, member.tenant_id, member.user_id,
                   member.session_id
            FROM assistant_runtime_thread_members AS member
            JOIN assistant_runtime_threads AS root
              ON root.runtime_thread_id = member.runtime_thread_id
             AND ($2 OR root.deleted_at IS NULL)
            WHERE member.kernel_thread_id = $1
            "#
        };
        let row = sqlx::query(sql)
            .bind(kernel_thread_id)
            .bind(include_deleted)
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
