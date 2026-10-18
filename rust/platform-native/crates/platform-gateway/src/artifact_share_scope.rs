//! Quiz origin + internal audience prerequisites. This reuses the existing
//! admitted-history provenance algorithm and Knowledge authorization adapter.
//! It never trusts a JWT alone or reads Knowledge tables / private runtime code.
use crate::{
    artifact_shares_api::{ArtifactSharesError, ArtifactSharesFuture, ArtifactSharesIdentity},
    artifact_shares_store::{PgArtifactSharesDatabase, pg_error},
    service_config_api::ServiceConfigError,
    session_history::{
        HistorySourceAccess, SharingSourceContext, SourceReferences, sharing_source_map,
    },
    users_store::UsersDatabase,
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::Row;
use std::{collections::HashSet, sync::Arc};
pub trait ArtifactShareScope: Send + Sync {
    fn quiz_sources<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
    ) -> ArtifactSharesFuture<'a, SourceReferences>;
    /// Generic plugin bridge preserves the distinction between legacy reads and
    /// public shares requiring a verifiable generation origin.
    fn quiz_sources_with_origin<'a>(
        &'a self, user: &'a ArtifactSharesIdentity, id: &'a str, _require_origin: bool,
    ) -> ArtifactSharesFuture<'a, SourceReferences> {
        self.quiz_sources(user, id)
    }
    fn require_active<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
    ) -> ArtifactSharesFuture<'a, ()>;
    fn allowed<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        sources: &'a SourceReferences,
    ) -> ArtifactSharesFuture<'a, bool>;
}
pub fn freeze_source_scope(s: &SourceReferences) -> Result<Value, ArtifactSharesError> {
    let documents = s
        .versions
        .iter()
        .map(|(d, id, _, _)| (d.clone(), id.clone()))
        .collect::<HashSet<_>>();
    let datasets = s
        .versions
        .iter()
        .map(|(d, _, _, _)| d.clone())
        .collect::<HashSet<_>>();
    if s.datasets.iter().any(String::is_empty)
        || s.documents
            .iter()
            .any(|(d, id)| d.is_empty() || id.is_empty())
        || s.versions.iter().any(|(d, id, v, h)| {
            d.is_empty()
                || id.is_empty()
                || *v <= 0
                || h.len() != 64
                || !h
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || s.documents != documents
        || !s.documents.iter().all(|(d, _)| s.datasets.contains(d))
        || (!s.datasets.is_empty() && s.datasets != datasets)
    {
        return Err(ArtifactSharesError::detail(
            409,
            "Share sources lack a verifiable immutable version",
        ));
    }
    let mut datasets = s.datasets.iter().collect::<Vec<_>>();
    datasets.sort();
    let mut documents = s.documents.iter().collect::<Vec<_>>();
    documents.sort();
    let mut versions = s.versions.iter().collect::<Vec<_>>();
    versions.sort();
    Ok(
        json!({"version":1,"dataset_ids":datasets,"document_ids":documents,"source_versions":versions}),
    )
}
pub struct NativeArtifactShareScope {
    pub database: Arc<PgArtifactSharesDatabase>,
    pub accounts: Option<Arc<dyn UsersDatabase>>,
    pub source_access: Option<Arc<dyn HistorySourceAccess>>,
}
fn source_unavailable() -> ArtifactSharesError {
    ArtifactSharesError::detail(503, json!({"code":"ASSISTANT_SOURCE_CHECK_UNAVAILABLE"}))
}
fn decoded(value: Value) -> Result<Value, ArtifactSharesError> {
    match value {
        Value::String(s) => serde_json::from_str(&s).map_err(|_| ArtifactSharesError::Internal),
        v => Ok(v),
    }
}
impl NativeArtifactShareScope {
    /// Current quiz provenance; legacy authenticated reads need not have origin.
    pub async fn quiz_sources_with_origin(
        &self,
        user: &ArtifactSharesIdentity,
        id: &str,
        require_origin: bool,
    ) -> Result<SourceReferences, ArtifactSharesError> {
        let (id, tenant) = (id.to_owned(), user.tenant_id.clone());
        let row:Option<Value>=self.database.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{
   sqlx::query_scalar("SELECT jsonb_build_object('dataset_ids',q.dataset_ids::text,'created_by',q.created_by,'session_id',e.session_id,'run_id',e.run_id::text) FROM assistant.quizzes q LEFT JOIN assistant_capability_executions e ON e.tenant_id=q.tenant_id AND e.user_id=q.created_by AND e.capability_id='generate_quiz' AND COALESCE(e.result_summary ->> 'quiz_id',e.result_summary #>> '{result,quiz_id}')=q.id::text WHERE q.id=$1::text::uuid AND q.tenant_id=$2 ORDER BY e.created_at LIMIT 1").bind(id).bind(tenant).fetch_optional(connection).await
  })).await.map_err(pg_error)?;
        let row = row.ok_or_else(|| ArtifactSharesError::detail(404, "Quiz not found"))?;
        let raw = decoded(
            if !crate::service_registry::python_truthy(&row["dataset_ids"]) {
                json!([])
            } else {
                row["dataset_ids"].clone()
            },
        )?;
        let ids = raw.as_array().ok_or_else(source_unavailable)?;
        let mut references = SourceReferences::default();
        for id in ids {
            if id.is_array() || id.is_object() {
                return Err(ArtifactSharesError::Internal);
            }
            // Python allows hashable non-string set members here: public is denied;
            // internal immutable freezing rejects them. Empty identity preserves both
            // failures without ever turning malformed input into an authorized dataset.
            references
                .datasets
                .insert(id.as_str().unwrap_or("").to_owned());
        }
        let mut verified = false;
        if let Some(session) = row["session_id"].as_str().filter(|s| !s.is_empty()) {
            let owner = row["created_by"].as_str().unwrap_or("None").to_owned();
            let session = session.to_owned();
            let tenant = user.tenant_id.clone();
            let (t, u, s) = (tenant.clone(), owner.clone(), session.clone());
            let snapshots:Vec<(String,Value,Option<DateTime<Utc>>)>=self.database.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{
    let rows=sqlx::query("SELECT run_id::text AS run_id, COALESCE(to_jsonb(snapshot::text),'null'::jsonb) AS snapshot, created_at FROM assistant_runtime_snapshots WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 ORDER BY created_at,snapshot_id").bind(s).bind(t).bind(u).fetch_all(connection).await?;
    rows.into_iter().map(|r|Ok((r.try_get::<Option<String>,_>("run_id")?.unwrap_or_default(),r.try_get("snapshot")?,r.try_get("created_at")?))).collect::<Result<Vec<_>,sqlx::Error>>()
   })).await.map_err(pg_error)?;
            let (t, u, s) = (tenant.clone(), owner.clone(), session.clone());
            let contexts:Vec<SharingSourceContext>=self.database.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{
    let rows=sqlx::query("SELECT turn_id::text AS run_id,to_jsonb((payload #> '{data,chunks}')::text) AS chunks,created_at FROM assistant_runtime_items WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND event_type='compat/v1/context_retrieved' ORDER BY sequence").bind(s).bind(t).bind(u).fetch_all(connection).await?;
    rows.into_iter().map(|r|Ok((r.try_get("run_id")?,r.try_get("chunks")?,r.try_get("created_at")?))).collect::<Result<Vec<_>,sqlx::Error>>()
   })).await.map_err(pg_error)?;
            let history:Option<Value>=self.database.operations.acquire().await.map_err(pg_error)?.execute(move|connection|Box::pin(async move{sqlx::query_scalar("SELECT COALESCE(to_jsonb(history::text),'null'::jsonb) FROM assistant.sessions WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3").bind(session).bind(tenant).bind(owner).fetch_optional(connection).await})).await.map_err(pg_error)?;
            let history = decoded(history.unwrap_or(json!([])))?;
            let snapshots = snapshots
                .into_iter()
                .map(|(run, snapshot, date)| Ok((run, decoded(snapshot)?, date)))
                .collect::<Result<Vec<_>, ArtifactSharesError>>()?;
            let contexts = contexts
                .into_iter()
                .map(|(run, chunks, date)| Ok((run, chunks.map(decoded).transpose()?, date)))
                .collect::<Result<Vec<_>, ArtifactSharesError>>()?;
            let (all, by_run) = sharing_source_map(&history, snapshots, contexts)
                .map_err(|_| source_unavailable())?;
            let run = row["run_id"].as_str().unwrap_or("None");
            verified = by_run.contains_key(run);
            let inherited = by_run.get(run).unwrap_or(&all);
            references
                .datasets
                .extend(inherited.datasets.iter().cloned());
            references
                .documents
                .extend(inherited.documents.iter().cloned());
            references
                .versions
                .extend(inherited.versions.iter().cloned());
        }
        if require_origin && references.datasets.is_empty() && !verified {
            return Err(ArtifactSharesError::detail(
                409,
                json!({"code":"ASSISTANT_SOURCE_ORIGIN_UNVERIFIED"}),
            ));
        }
        Ok(references)
    }
}
impl ArtifactShareScope for NativeArtifactShareScope {
    fn quiz_sources<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        id: &'a str,
    ) -> ArtifactSharesFuture<'a, SourceReferences> {
        Box::pin(self.quiz_sources_with_origin(user, id, true))
    }
    fn quiz_sources_with_origin<'a>(
        &'a self, user: &'a ArtifactSharesIdentity, id: &'a str, require_origin: bool,
    ) -> ArtifactSharesFuture<'a, SourceReferences> {
        Box::pin(NativeArtifactShareScope::quiz_sources_with_origin(self, user, id, require_origin))
    }
    fn require_active<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
    ) -> ArtifactSharesFuture<'a, ()> {
        Box::pin(async move {
            if !user.is_authenticated
                || user.user_id.is_empty()
                || user.tenant_id.as_deref().unwrap_or("").is_empty()
            {
                return Err(ArtifactSharesError::detail(
                    401,
                    "Sign in to open this internal share",
                ));
            }
            let accounts = self.accounts.as_ref().ok_or_else(|| {
                ArtifactSharesError::detail(503, "Account verification is unavailable")
            })?;
            let account = accounts.get_user(&user.user_id).await.map_err(|e| {
                if matches!(e, ServiceConfigError::Cancelled) {
                    ArtifactSharesError::Cancelled
                } else {
                    ArtifactSharesError::detail(503, "Account verification is unavailable")
                }
            })?;
            if account.as_ref().is_none_or(|v| {
                v["status"] != "active"
                    || v["tenant_id"].as_str().unwrap_or("")
                        != user.tenant_id.as_deref().unwrap_or("")
            }) {
                return Err(ArtifactSharesError::detail(
                    403,
                    "Account is not active in this tenant",
                ));
            }
            Ok(())
        })
    }
    fn allowed<'a>(
        &'a self,
        user: &'a ArtifactSharesIdentity,
        sources: &'a SourceReferences,
    ) -> ArtifactSharesFuture<'a, bool> {
        Box::pin(async move {
            let Some(access) = &self.source_access else {
                return Ok(sources.datasets.is_empty()
                    && sources.documents.is_empty()
                    && sources.versions.is_empty());
            };
            let principal = crate::migration_content::ContentIdentity {
                user_id: user.user_id.clone(),
                tenant_id: user.tenant_id.clone().unwrap_or_default(),
                tier: user.tier.clone(),
                roles: user.roles.clone(),
                permissions: user.permissions.clone(),
                is_authenticated: user.is_authenticated,
            };
            let visible = access.visible_sources(&principal, sources).await;
            Ok(sources
                .datasets
                .iter()
                .all(|id| visible.datasets.contains_key(id))
                && sources.documents.is_subset(&visible.documents)
                && sources.versions.is_subset(&visible.versions))
        })
    }
}
