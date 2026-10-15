//! Content and artifact migration. Constructors are inert; the host binds canonical
//! identity, source authorization, provider and runtime clients before mounting.
pub mod access;
pub mod artifacts;
pub mod attachments;
pub mod capabilities;
pub mod conversations;
pub mod files;
pub mod image_adapters;
pub mod image_native;
pub mod image_provider;
pub mod images;
pub mod images_state;
pub mod read_broker;
pub mod shares;
pub mod skills;
pub use ai_platform_gateway_integrations::migration_content::storage;

use crate::postgres_operation::{PgOperationStorage, PgQueryFuture};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use ai_platform_gateway_integrations::migration_content::{ContentError, ContentFuture};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ContentIdentity {
    pub tenant_id: String,
    pub user_id: String,
    #[serde(default)]
    pub is_authenticated: bool,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub tier: String,
}
pub trait ContentAccess: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> ContentFuture<'a, ContentIdentity>;
    fn require_capability<'a>(
        &'a self,
        user: &'a ContentIdentity,
        capability: &'a str,
    ) -> ContentFuture<'a, ()>;
    fn rate_limit<'a>(
        &'a self,
        user: &'a ContentIdentity,
        operation: &'a str,
        service: Option<&'a str>,
    ) -> ContentFuture<'a, ()>;
    fn authorize_session<'a>(
        &'a self,
        user: &'a ContentIdentity,
        session_id: &'a str,
    ) -> ContentFuture<'a, Value>;
    /// Must re-evaluate inherited source ACLs and source versions at artifact creation.
    fn authorize_artifact_source<'a>(
        &'a self,
        user: &'a ContentIdentity,
        artifact: &'a artifacts::Artifact,
    ) -> ContentFuture<'a, ()>;
    fn require_vision_model<'a>(
        &'a self,
        user: &'a ContentIdentity,
        model_id: &'a str,
        provider_id: Option<&'a str>,
    ) -> ContentFuture<'a, ()>;
}
#[derive(Clone)]
pub struct ContentDatabase {
    pub(crate) operations: PgOperationStorage,
}
impl ContentDatabase {
    pub fn new(pool: sqlx::PgPool, control_pool: sqlx::PgPool) -> Result<Self, ContentError> {
        Ok(Self {
            operations: PgOperationStorage::new(pool, control_pool)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), ContentError> {
        Ok(self.operations.drain_cancelled().await?)
    }
    pub(crate) async fn atomic<T, F>(&self, f: F) -> Result<T, ContentError>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, T> + Send + 'static,
    {
        self.run(move |c| {
            Box::pin(async move {
                sqlx::query("BEGIN").execute(&mut *c).await?;
                match f(c).await {
                    Ok(value) => {
                        sqlx::query("COMMIT").execute(c).await?;
                        Ok(value)
                    }
                    Err(error) => {
                        let _ = sqlx::query("ROLLBACK").execute(c).await;
                        Err(error)
                    }
                }
            })
        })
        .await
    }
    pub(crate) async fn transaction<T, F>(&self, f: F) -> Result<T, ContentError>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, Result<T, ContentError>>
            + Send
            + 'static,
    {
        self.run(move |c| {
            Box::pin(async move {
                sqlx::query("BEGIN").execute(&mut *c).await?;
                match f(c).await {
                    Ok(Ok(value)) => {
                        sqlx::query("COMMIT").execute(c).await?;
                        Ok(Ok(value))
                    }
                    Ok(Err(error)) => {
                        sqlx::query("ROLLBACK").execute(c).await?;
                        Ok(Err(error))
                    }
                    Err(error) => {
                        let _ = sqlx::query("ROLLBACK").execute(c).await;
                        Err(error)
                    }
                }
            })
        })
        .await?
    }
    pub(crate) async fn run<T, F>(&self, f: F) -> Result<T, ContentError>
    where
        T: Send + 'static,
        F: for<'c> FnOnce(&'c mut sqlx::PgConnection) -> PgQueryFuture<'c, T> + Send + 'static,
    {
        let mut connection = self.operations.acquire().await?;
        Ok(connection.execute(f).await?)
    }
}
pub use ai_platform_gateway_integrations::migration_content::sha256;
pub fn hex_id(value: &str, prefix: &str, len: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|s| {
        s.len() == len
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn required(v: &Value, key: &str) -> Result<String, ContentError> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            ContentError::new(
                422,
                json!([{"type":"missing","loc":["body",key],"msg":"Field required","input":v}]),
            )
        })
}
pub use ai_platform_gateway_integrations::migration_content::percent_encode;
pub fn attachment_content_disposition(filename: &str) -> String {
    let encoded = percent_encode(filename);
    if encoded == filename {
        format!("attachment; filename=\"{filename}\"")
    } else {
        format!("attachment; filename*=UTF-8''{encoded}")
    }
}
pub use ai_platform_gateway_foundation::python_json::python_json;

/// The public router is nested under /api/v1 by composition. The private router
/// already contains its full /internal/v2 paths and is mounted separately.
pub struct ContentServices {
    pub access: std::sync::Arc<access::NativeContentAccess>,
    pub artifacts: std::sync::Arc<artifacts::ArtifactApi>,
    pub files: std::sync::Arc<files::FilesApi>,
    pub conversations: std::sync::Arc<conversations::ConversationApi>,
    pub shares: std::sync::Arc<shares::ConversationShares>,
    pub skills: std::sync::Arc<skills::SkillsApi>,
    pub images: std::sync::Arc<images::ImageService>,
}
pub fn router(services: ContentServices) -> axum::Router {
    axum::Router::new()
        .merge(artifacts::router(services.artifacts))
        .merge(files::router(services.files))
        .merge(conversations::router(services.conversations))
        .merge(shares::router(services.shares))
        .merge(skills::router(services.skills))
        .merge(images::router(services.images))
        .layer(axum::middleware::from_fn_with_state(
            services.access,
            access::request_context,
        ))
}
