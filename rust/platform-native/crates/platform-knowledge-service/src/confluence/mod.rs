//! Knowledge-owned Confluence source connections and admission. Never uses Gateway SQL.
mod acl;
mod bindings;
pub mod catalog;
mod client;
mod connections;
pub mod credentials;
mod detach;
pub mod discovery;
mod http;
mod images;
mod legacy_clients;
mod legacy_detach;
pub mod legacy_image_segments;
mod legacy_images;
pub mod legacy_management;
mod legacy_publication;
pub mod legacy_sync;
pub mod parser;
mod publication;
pub mod scheduler;
mod sql;
mod sync;
use crate::{migration_knowledge_ingest as ingest, migration_knowledge_query as query};
use axum::{
    Json, Router,
    http::StatusCode,
    response::{IntoResponse, Response},
};
pub use http::routes;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::sync::Arc;
pub type Result<T> = std::result::Result<T, Error>;
pub struct Error {
    pub status: StatusCode,
    pub detail: Value,
}
impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfluenceError")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}
impl Error {
    pub fn new(status: StatusCode, message: &str) -> Self {
        Self {
            status,
            detail: json!(message),
        }
    }
    pub fn unavailable(message: &str) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, message)
    }
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "Current dataset access required")
    }
    pub fn conflict(message: &str) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    pub fn missing(message: &str) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"detail":self.detail}))).into_response()
    }
}
impl From<sqlx::Error> for Error {
    fn from(_: sqlx::Error) -> Self {
        Self::unavailable("Knowledge persistence unavailable")
    }
}
impl From<query::types::QueryError> for Error {
    fn from(e: query::types::QueryError) -> Self {
        match e {
            query::types::QueryError::Forbidden => Self::forbidden(),
            query::types::QueryError::NotFound => Self::missing("Dataset is unavailable"),
            _ => Self::unavailable("Current source actor is unavailable"),
        }
    }
}
pub fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
pub fn owner(v: &Value) -> &str {
    v["owner_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| text(v, "created_by"))
}
pub fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut out = json!({});
    for k in keys {
        out[*k] = v[*k].clone();
    }
    out
}
#[derive(Clone)]
pub struct Sources {
    pub ingestion: Arc<ingest::IngestionService>,
    pub runtime: Arc<sync::Runtime>,
    pub enabled: bool,
    pub refresh_enabled: bool,
    pub max_retries: u32,
    pub pool: PgPool,
    pub store: query::store::QueryStore,
    pub actors: query::actor::RuntimeActorClient,
    pub encryption_key: Arc<String>,
    pub client: reqwest::Client,
    pub image_vlm: Option<Arc<ingest::vlm::VlmService>>,
}
impl Sources {
    pub fn new(
        pool: PgPool,
        actors: query::actor::RuntimeActorClient,
        key: String,
        ingestion: Arc<ingest::IngestionService>,
        enabled: bool,
        timeout: std::time::Duration,
        max_retries: u32,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| Error::unavailable("Confluence client unavailable"))?;
        let object_policy = ingestion.store.object_policy.clone();
        Ok(Self {
            ingestion,
            runtime: Arc::new(sync::Runtime::default()),
            enabled,
            refresh_enabled: false,
            max_retries,
            store: query::store::QueryStore::new(pool.clone()).with_object_policy(object_policy),
            pool,
            actors,
            encryption_key: Arc::new(key),
            client,
            image_vlm: None,
        })
    }
    pub fn with_acl_refresh(mut self, enabled: bool) -> Self {
        self.refresh_enabled = enabled;
        self
    }
    /// Optional source image-processor extension. The authoritative manual-sync
    /// entrypoint does not supply this, so normal composition leaves it off.
    pub fn with_image_vlm(mut self, service: Arc<ingest::vlm::VlmService>) -> Self {
        self.image_vlm = Some(service);
        self
    }
    pub async fn actor(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        editor: bool,
        writable: bool,
    ) -> Result<query::types::UserContext> {
        if !self.enabled {
            return Err(Error::unavailable(
                "Confluence manual synchronization is unavailable",
            ));
        }
        if user.anonymous() || user.user_id.is_empty() || user.tenant_id.is_empty() {
            return Err(Error::new(
                StatusCode::UNAUTHORIZED,
                "Authentication required",
            ));
        }
        let actor = self.actors.resolve(&user.tenant_id, &user.user_id).await?;
        if actor.anonymous() || actor.user_id != user.user_id || actor.tenant_id != user.tenant_id {
            return Err(Error::forbidden());
        }
        let ds = self
            .store
            .require_dataset_access(&actor, dataset, if editor { "editor" } else { "viewer" })
            .await?;
        if ds.tenant_id != actor.tenant_id {
            return Err(Error::missing("Dataset is unavailable"));
        }
        if editor && writable {
            let value = serde_json::to_value(ds).map_err(|_| Error::forbidden())?;
            writable_dataset(&value)?;
        }
        Ok(actor)
    }
    pub async fn connection(&self, id: &str) -> Result<Option<Value>> {
        Ok(sqlx::query_scalar(
            "SELECT to_jsonb(c) FROM confluence_connections c WHERE connection_id=$1",
        )
        .bind(id)
        .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
        .await?)
    }
    pub async fn binding(&self, id: &str) -> Result<Option<Value>> {
        Ok(sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE binding_id=$1",
        )
        .bind(id)
        .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
        .await?)
    }
}
pub fn writable_dataset(ds: &Value) -> Result<()> {
    ingest::require_index_writable(ds).map_err(|_| Error::conflict("Dataset index is not writable"))
}
pub fn owned(row: &Value, actor: &query::types::UserContext) -> bool {
    text(row, "tenant_id") == actor.tenant_id && owner(row) == actor.user_id
}
/// Canonical permission policy on the transaction connection; no nested pool acquisition.
async fn locked_dataset(
    conn: &mut PgConnection,
    actor: &query::types::UserContext,
    id: &str,
    writable: bool,
) -> Result<Value> {
    let ds: Value = sqlx::query_scalar(
        "SELECT to_jsonb(d) FROM datasets d WHERE dataset_id=$1 AND is_deleted=FALSE FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(ai_platform_knowledge_domain::database::connection(
        &mut *conn,
    ))
    .await?
    .ok_or_else(|| Error::forbidden())?;
    if text(&ds, "tenant_id") != actor.tenant_id || ds["is_archived"] == true {
        return Err(Error::forbidden());
    }
    let owner_access = text(&ds, "created_by") == actor.user_id
        || actor.user_tier == "admin"
        || actor.roles.iter().any(|r| r == "admin");
    if !owner_access {
        let grants:Vec<String>=sqlx::query_scalar("SELECT permission FROM dataset_permissions WHERE dataset_id=$1 AND ((subject_type='user' AND subject_id=$2) OR (subject_type='tenant_role' AND subject_id=ANY($3::text[])))").bind(id).bind(&actor.user_id).bind(&actor.roles).fetch_all(ai_platform_knowledge_domain::database::connection(&mut *conn)).await?;
        if !grants.iter().any(|p| p == "editor" || p == "owner") {
            return Err(Error::forbidden());
        }
    }
    if writable {
        writable_dataset(&ds)?;
    }
    Ok(ds)
}

fn deletion_writable(dataset: &Value, target: &str) -> Result<()> {
    let mut ds = dataset.clone();
    let marker =
        json!({"operation":"document_delete","target_id":target,"status":"pending","version":1});
    if ds["index_config"]["retrieval"]["_index_deletion_fence"] == marker {
        if let Some(obj) = ds["index_config"]["retrieval"].as_object_mut() {
            obj.remove("_index_deletion_fence");
        }
    }
    writable_dataset(&ds)
}
