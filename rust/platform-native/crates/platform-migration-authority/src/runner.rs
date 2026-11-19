use crate::{
    AuthorityError, LOCK_ID, LOCK_NAMESPACE, Result,
    credentials::validate_role_prefix,
    ledger,
    manifest::{ChangeSpec, EpochManifest, TransactionMode},
    sql::reject_embedded_transactions,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row, postgres::PgConnectOptions};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    str::FromStr,
    time::Instant,
};

#[derive(Clone)]
pub struct AuthorityPaths {
    pub database_dir: PathBuf,
}
impl AuthorityPaths {
    pub fn migrations_root(&self) -> PathBuf {
        self.database_dir.join("migrations")
    }
    pub fn bootstrap_dir(&self) -> PathBuf {
        self.database_dir.join("bootstrap")
    }
    pub fn baseline_dir(&self, baseline: &str) -> PathBuf {
        self.database_dir.join("baselines").join(baseline)
    }
    pub fn epoch_dir(&self, baseline: &str) -> PathBuf {
        self.migrations_root().join(baseline)
    }
}
pub struct MigrationAuthority {
    dsn: String,
    configured_options: Option<PgConnectOptions>,
    pub paths: AuthorityPaths,
    pub role_prefix: String,
    reconciliation_receipt: std::sync::Mutex<Option<Value>>,
    pub(crate) recovery_handlers:
        BTreeMap<String, std::sync::Arc<dyn crate::recovery::RecoveryHandler>>,
}
impl MigrationAuthority {
    pub fn new(dsn: String, paths: AuthorityPaths, role_prefix: String) -> Result<Self> {
        validate_role_prefix(&role_prefix)?;
        Ok(Self {
            dsn,
            configured_options: None,
            paths,
            role_prefix,
            reconciliation_receipt: std::sync::Mutex::new(None),
            recovery_handlers: BTreeMap::new(),
        })
    }
    pub fn from_principal(
        principal: &str,
        paths: AuthorityPaths,
        role_prefix: String,
    ) -> Result<Self> {
        let connection =
            crate::credential_files::resolve(&std::env::vars().collect(), principal, &role_prefix)?;
        let mut authority = Self::new(connection.dsn, paths, role_prefix)?;
        authority.configured_options = Some(connection.options);
        Ok(authority)
    }
    pub(crate) fn with_connect_options(
        dsn: String,
        options: PgConnectOptions,
        paths: AuthorityPaths,
        role_prefix: String,
    ) -> Result<Self> {
        let mut authority = Self::new(dsn, paths, role_prefix)?;
        authority.configured_options = Some(options);
        Ok(authority)
    }
    pub fn register_recovery_handler(
        &mut self,
        name: String,
        handler: std::sync::Arc<dyn crate::recovery::RecoveryHandler>,
    ) {
        self.recovery_handlers.insert(name, handler);
    }
    pub fn set_reconciliation_receipt(&self, value: Value) {
        *self
            .reconciliation_receipt
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(value);
    }
    pub fn reconciliation_receipt(&self) -> Option<Value> {
        self.reconciliation_receipt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn connect(&self, read_only: bool) -> Result<PgConnection> {
        let options = self
            .configured_options
            .clone()
            .map(Ok)
            .unwrap_or_else(|| PgConnectOptions::from_str(&self.dsn))?
            .application_name(&format!(
                "ai_gateway_authority_{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ));
        let mut conn = PgConnection::connect_with(&options).await.map_err(|_| {
            AuthorityError::Refused("database authority connection failed; values withheld".into())
        })?;
        if read_only {
            sqlx::query("SET default_transaction_read_only=on")
                .execute(&mut conn)
                .await?;
        }
        Ok(conn)
    }
    pub async fn acquire_lock(&self, conn: &mut PgConnection) -> Result<()> {
        sqlx::query("SELECT pg_advisory_lock($1::integer,$2::integer)")
            .bind(LOCK_NAMESPACE)
            .bind(LOCK_ID)
            .execute(conn)
            .await?;
        Ok(())
    }
    pub async fn release_lock(&self, mut conn: PgConnection) -> Result<()> {
        let result = sqlx::query("SELECT pg_advisory_unlock($1::integer,$2::integer)")
            .bind(LOCK_NAMESPACE)
            .bind(LOCK_ID)
            .execute(&mut conn)
            .await;
        conn.close().await?;
        result?;
        Ok(())
    }
    pub async fn ensure_ledger(&self, conn: &mut PgConnection) -> Result<()> {
        let mut tx = conn.begin().await?;
        let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
            .fetch_one(&mut *tx)
            .await?;
        set_role(&mut tx, &format!("{}owner", self.role_prefix)).await?;
        sqlx::raw_sql(ledger::LEDGER_DDL).execute(&mut *tx).await?;
        set_role(&mut tx, &previous).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn adopted_baseline(&self, conn: &mut PgConnection) -> Result<Option<Value>> {
        if !table_exists(conn, ledger::BASELINES_TABLE).await? {
            return Ok(None);
        }
        let rows: Vec<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM public.platform_schema_baselines b ORDER BY baseline_id",
        )
        .fetch_all(&mut *conn)
        .await?;
        if rows.len() > 1 {
            return Err(AuthorityError::Refused(
                "exactly one baseline can be authoritative".into(),
            ));
        }
        Ok(rows.into_iter().next())
    }
    pub async fn applied_changes(
        &self,
        conn: &mut PgConnection,
        baseline_id: &str,
    ) -> Result<BTreeMap<i32, String>> {
        if !table_exists(conn, ledger::CHANGES_TABLE).await? {
            return Ok(BTreeMap::new());
        }
        let rows = sqlx::query(ledger::SELECT_APPLIED_CHANGES)
            .bind(baseline_id)
            .fetch_all(conn)
            .await?;
        rows.into_iter()
            .map(|r| Ok((r.try_get("sequence")?, r.try_get("checksum_sha256")?)))
            .collect()
    }
    pub async fn apply_change_transactional(
        &self,
        conn: &mut PgConnection,
        baseline_id: &str,
        spec: &ChangeSpec,
        epoch_dir: &Path,
    ) -> Result<i32> {
        let sql = std::fs::read_to_string(epoch_dir.join(&spec.file))?;
        if format!("{:x}", Sha256::digest(sql.as_bytes())) != spec.sha256 {
            return Err(AuthorityError::Refused(format!(
                "change {} checksum mismatch; immutable migration refused",
                spec.sequence
            )));
        }
        reject_embedded_transactions(&sql, &spec.name)?;
        let started = Instant::now();
        let mut tx = conn.begin().await?;
        sqlx::query(
            "SELECT set_config('statement_timeout',$1,true),set_config('lock_timeout',$2,true)",
        )
        .bind((spec.timeout_seconds * 1000).to_string())
        .bind((spec.lock_budget_seconds * 1000).to_string())
        .execute(&mut *tx)
        .await?;
        evaluate_conditions(
            &mut tx,
            &self.role_prefix,
            &spec.preconditions,
            "pre",
            &spec.name,
        )
        .await?;
        let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
            .fetch_one(&mut *tx)
            .await?;
        set_role(&mut tx, &format!("{}{}", self.role_prefix, spec.owner)).await?;
        sqlx::raw_sql(&sql).execute(&mut *tx).await?;
        set_role(&mut tx, &previous).await?;
        evaluate_conditions(
            &mut tx,
            &self.role_prefix,
            &spec.postconditions,
            "post",
            &spec.name,
        )
        .await?;
        let duration = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
        sqlx::query(ledger::INSERT_CHANGE_SUCCESS)
            .bind(baseline_id)
            .bind(spec.sequence)
            .bind(&spec.name)
            .bind(&spec.sha256)
            .bind(duration)
            .bind(runner_digest())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(duration)
    }
    pub async fn apply_epoch(
        &self,
        conn: &mut PgConnection,
        manifest: &EpochManifest,
        epoch_dir: &Path,
    ) -> Result<Vec<String>> {
        if !crate::ledger_acl::inspect(conn, &self.role_prefix).await? {
            return Err(AuthorityError::Refused(
                "versioned ledger ACL bootstrap required before epoch discovery".into(),
            ));
        }
        let applied = self.applied_changes(conn, &manifest.baseline_id).await?;
        validate_applied_prefix(manifest, &applied)?;
        let mut next = applied.len() as i32 + 1;
        let mut lines = Vec::new();
        for spec in &manifest.changes {
            if applied.contains_key(&spec.sequence) {
                continue;
            }
            if spec.sequence != next {
                return Err(AuthorityError::Refused(
                    "out-of-order epoch application rejected".into(),
                ));
            }
            self.ensure_ledger(conn).await?;
            let duration = match spec.transaction_mode {
                TransactionMode::Transactional => {
                    self.apply_change_transactional(conn, &manifest.baseline_id, spec, epoch_dir)
                        .await?
                }
                TransactionMode::NonTransactional => {
                    self.apply_change_non_transactional(
                        conn,
                        &manifest.baseline_id,
                        spec,
                        epoch_dir,
                    )
                    .await?
                }
            };
            lines.push(format!(
                "applied {}:{:03} {} ({}ms)",
                manifest.baseline_id, spec.sequence, spec.name, duration
            ));
            next += 1;
        }
        Ok(lines)
    }
}
pub fn runner_digest() -> String {
    {
        let mut digest = Sha256::new();
        digest.update(include_bytes!("runner.rs"));
        digest.update(include_bytes!("recovery.rs"));
        digest.update(include_bytes!("recovery_sql.rs"));
        format!("{:x}", digest.finalize())
    }
}
pub async fn table_exists(conn: &mut PgConnection, table: &str) -> Result<bool> {
    Ok(sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname=$1)").bind(table).fetch_one(conn).await?)
}
pub async fn set_role(conn: &mut PgConnection, role: &str) -> Result<()> {
    sqlx::query("SELECT set_config('role',$1,true)")
        .bind(role)
        .execute(conn)
        .await?;
    Ok(())
}
pub async fn evaluate_conditions(
    conn: &mut PgConnection,
    prefix: &str,
    conditions: &[String],
    phase: &str,
    name: &str,
) -> Result<()> {
    let mut tx = conn.begin().await?;
    let previous: String = sqlx::query_scalar("SELECT current_setting('role')")
        .fetch_one(&mut *tx)
        .await?;
    set_role(&mut tx, &format!("{prefix}owner")).await?;
    for (index, condition) in conditions.iter().enumerate() {
        let row = sqlx::query(condition).fetch_one(&mut *tx).await?;
        if row.try_get::<bool, _>(0).ok() != Some(true) {
            return Err(AuthorityError::Refused(format!(
                "{phase}condition {} of change {name} failed",
                index + 1
            )));
        }
    }
    set_role(&mut tx, &previous).await?;
    tx.commit().await?;
    Ok(())
}
pub fn validate_applied_prefix(
    manifest: &EpochManifest,
    applied: &BTreeMap<i32, String>,
) -> Result<()> {
    for (index, (&sequence, sha)) in applied.iter().enumerate() {
        let declared = manifest
            .changes
            .iter()
            .find(|c| c.sequence == sequence)
            .ok_or_else(|| {
                AuthorityError::Refused(
                    "ledger contains sequences outside its immutable manifest".into(),
                )
            })?;
        if &declared.sha256 != sha {
            return Err(AuthorityError::Refused(
                "ledger checksum differs from immutable manifest".into(),
            ));
        }
        if sequence != index as i32 + 1 {
            return Err(AuthorityError::Refused(
                "epoch ledger is not a contiguous prefix".into(),
            ));
        }
    }
    Ok(())
}
