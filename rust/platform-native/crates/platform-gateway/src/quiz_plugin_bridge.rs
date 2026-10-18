//! Host-only glue for ai-quiz. Identity, source authorization, pools and deployment
//! activation remain Gateway authorities; quiz grading/persistence live in plugin.
use crate::{
    artifact_share_scope::ArtifactShareScope,
    artifact_shares_api::{
        ArtifactSharesError, ArtifactSharesIdentity, ArtifactSharesIdentityResolver,
    },
    session_history,
};
use ai_platform_plugin_quiz_gateway::{
    ports,
    quiz_api::{QuizApi, quiz_router},
    quiz_scope::NativeQuizScope,
    quiz_share_service::QuizShareService,
    quiz_share_store::PgQuizShareDatabase,
    quiz_store::{PgQuizDatabase, QuizService},
};
use serde_json::Value;
use std::sync::Arc;
pub fn host_error(error: ports::QuizError) -> ArtifactSharesError {
    match error {
        ports::QuizError::Http(s, v) => ArtifactSharesError::Http(s, v),
        ports::QuizError::Internal => ArtifactSharesError::Internal,
        ports::QuizError::Cancelled => ArtifactSharesError::Cancelled,
    }
}
fn plugin_error(error: ArtifactSharesError) -> ports::QuizError {
    match error {
        ArtifactSharesError::Http(s, v) => ports::QuizError::Http(s, v),
        ArtifactSharesError::Internal => ports::QuizError::Internal,
        ArtifactSharesError::Cancelled => ports::QuizError::Cancelled,
    }
}
impl From<ports::QuizError> for ArtifactSharesError {
    fn from(e: ports::QuizError) -> Self {
        host_error(e)
    }
}
fn host_identity(u: &ports::QuizIdentity) -> ArtifactSharesIdentity {
    ArtifactSharesIdentity {
        user_id: u.user_id.clone(),
        tenant_id: u.tenant_id.clone(),
        is_authenticated: u.is_authenticated,
        roles: u.roles.clone(),
        permissions: u.permissions.clone(),
        tier: u.tier.clone(),
    }
}
fn plugin_identity(u: ArtifactSharesIdentity) -> ports::QuizIdentity {
    ports::QuizIdentity {
        user_id: u.user_id,
        tenant_id: u.tenant_id,
        is_authenticated: u.is_authenticated,
        roles: u.roles,
        permissions: u.permissions,
        tier: u.tier,
    }
}
pub fn host_sources(v: ports::SourceReferences) -> session_history::SourceReferences {
    session_history::SourceReferences {
        datasets: v.datasets,
        documents: v.documents,
        versions: v.versions,
    }
}
fn plugin_sources(v: session_history::SourceReferences) -> ports::SourceReferences {
    ports::SourceReferences {
        datasets: v.datasets,
        documents: v.documents,
        versions: v.versions,
    }
}
pub struct HostQuizIdentity(pub Arc<dyn ArtifactSharesIdentityResolver>, pub Value);
impl ports::QuizIdentityResolver for HostQuizIdentity {
    fn resolve<'a>(
        &'a self,
        headers: &'a axum::http::HeaderMap,
    ) -> ports::QuizFuture<'a, ports::QuizIdentity> {
        Box::pin(async move {
            let user = self.0.resolve(headers).await.map_err(plugin_error)?;
            if !crate::assistant_catalog::publisher_admitted(
                &self.1,
                user.tenant_id.as_deref().unwrap_or(""),
            ) {
                return Err(ports::QuizError::detail(
                    403,
                    "Plugin publisher is not admitted for this tenant",
                ));
            }
            Ok(plugin_identity(user))
        })
    }
}
pub struct HostQuizSources(pub Arc<dyn ArtifactShareScope>, pub Value);
impl HostQuizSources {
    fn admit(&self, user: &ports::QuizIdentity) -> Result<(), ports::QuizError> {
        if crate::assistant_catalog::publisher_admitted(
            &self.1,
            user.tenant_id.as_deref().unwrap_or(""),
        ) {
            Ok(())
        } else {
            Err(ports::QuizError::detail(
                403,
                "Plugin publisher is not admitted for this tenant",
            ))
        }
    }
}
impl ports::QuizSourceAuthority for HostQuizSources {
    fn quiz_sources_with_origin<'a>(
        &'a self,
        user: &'a ports::QuizIdentity,
        id: &'a str,
        public: bool,
    ) -> ports::QuizFuture<'a, ports::SourceReferences> {
        Box::pin(async move {
            self.admit(user)?;
            self.0
                .quiz_sources_with_origin(&host_identity(user), id, public)
                .await
                .map(plugin_sources)
                .map_err(plugin_error)
        })
    }
    fn require_active<'a>(&'a self, user: &'a ports::QuizIdentity) -> ports::QuizFuture<'a, ()> {
        Box::pin(async move {
            self.admit(user)?;
            self.0
                .require_active(&host_identity(user))
                .await
                .map_err(plugin_error)
        })
    }
    fn allowed<'a>(
        &'a self,
        user: &'a ports::QuizIdentity,
        sources: &'a ports::SourceReferences,
    ) -> ports::QuizFuture<'a, bool> {
        Box::pin(async move {
            self.admit(user)?;
            self.0
                .allowed(
                    &host_identity(user),
                    &session_history::SourceReferences {
                        datasets: sources.datasets.clone(),
                        documents: sources.documents.clone(),
                        versions: sources.versions.clone(),
                    },
                )
                .await
                .map_err(plugin_error)
        })
    }
}
/// Must be called with the already verified deployment catalog, never request JSON.
/// A retired/disabled plugin cannot mount a fresh quiz generation surface by
/// merely leaving its old descriptor compiled into the Worker.
pub fn active_quiz_record(catalog: &Value) -> bool {
    catalog["capabilities"].as_array().is_some_and(|records| {
        records.iter().any(|r| {
            r["id"] == "generate_quiz"
                && r["plugin_id"] == "ai-quiz"
                && r["plugin_metadata"]["artifact_sha256"] == r["content_hash"]
                && r["plugin_metadata"]["publisher_tenants"]
                    .as_str()
                    .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())
                    .is_some_and(|tenants| {
                        tenants
                            .iter()
                            .any(|tenant| crate::assistant_catalog::publisher_admitted(r, tenant))
                    })
                && r["plugin_metadata"]["worker_active"] == true
                && r["plugin_metadata"]["publisher_id"]
                    .as_str()
                    .is_some_and(|id| {
                        !id.is_empty()
                            && id.len() <= 64
                            && id
                                .bytes()
                                .all(|v| v.is_ascii_alphanumeric() || b"._-".contains(&v))
                    })
                && r["plugin_metadata"]["activation_id"]
                    .as_str()
                    .is_some_and(|v| uuid::Uuid::parse_str(v).is_ok())
                && r["content_hash"].as_str().is_some_and(|v| {
                    v.strip_prefix("sha256:").is_some_and(|h| {
                        h.len() == 64
                            && h.bytes()
                                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    })
                })
        })
    })
}
/// Real route graph; no environment lookup, listener, provider call or DB startup.
pub fn router(
    catalog: &Value,
    pool: sqlx::PgPool,
    control_pool: sqlx::PgPool,
    sources: Arc<dyn ArtifactShareScope>,
    identity: Arc<dyn ArtifactSharesIdentityResolver>,
    trusted_proxies: crate::client_ip::TrustedProxies,
    rate: Option<Arc<ai_platform_plugin_quiz_gateway::quiz_rate::QuizRateAdmission>>,
    settings_available: bool,
) -> Result<axum::Router, ports::QuizError> {
    if !active_quiz_record(catalog) {
        return Err(ports::QuizError::detail(
            503,
            "ai-quiz plugin is not active",
        ));
    }
    let db = Arc::new(PgQuizDatabase::new(pool.clone(), control_pool.clone())?);
    let share_db = Arc::new(PgQuizShareDatabase::new(pool, control_pool)?);
    let record = catalog["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == ai_platform_plugin_quiz::CAPABILITY_ID)
        .unwrap()
        .clone();
    let identity: Arc<dyn ports::QuizIdentityResolver> =
        Arc::new(HostQuizIdentity(identity, record.clone()));
    let scope = Arc::new(NativeQuizScope {
        sources: Arc::new(HostQuizSources(sources, record)),
        identities: identity.clone(),
        database: Some(share_db.clone()),
        settings_available,
    });
    Ok(quiz_router(QuizApi {
        service: Some(Arc::new(QuizService::new(db))),
        shares: Some(Arc::new(QuizShareService::new(
            share_db,
            Arc::new(ai_platform_plugin_quiz_gateway::quiz_entropy::OsQuizEntropy),
        ))),
        scope,
        identity,
        rate,
        trusted_proxies,
    }))
}

/// Production graph seam: use the exact deployment catalog already selected for
/// Foundation and Runtime. The default baseline has no plugin and mounts nothing.
pub fn deployment_router(
    catalog: &Result<
        Arc<crate::assistant_catalog::CapabilityCatalog>,
        crate::assistant_catalog::CatalogError,
    >,
    pool: sqlx::PgPool,
    control_pool: sqlx::PgPool,
    sources: Arc<dyn ArtifactShareScope>,
    identity: Arc<dyn ArtifactSharesIdentityResolver>,
    access: Arc<crate::migration_content::access::NativeContentAccess>,
) -> Result<axum::Router, ports::QuizError> {
    let catalog = catalog.as_ref().map_err(|_| ports::QuizError::Internal)?;
    let value = serde_json::json!({"capabilities":catalog.records()});
    if !active_quiz_record(&value) {
        return Ok(axum::Router::new());
    }
    router(
        &value,
        pool,
        control_pool,
        sources,
        identity,
        access.trusted_proxies.clone(),
        access.limiter.as_ref().map(|limiter| {
            Arc::new(
                ai_platform_plugin_quiz_gateway::quiz_rate::QuizRateAdmission {
                    limiter: limiter.clone(),
                    config: access.rate_config.clone(),
                },
            )
        }),
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_current_signed_pack_projection_mounts_quiz_routes() {
        let mut catalog = serde_json::json!({"capabilities":[{"id":"generate_quiz","plugin_id":"ai-quiz","content_hash":format!("sha256:{}","a".repeat(64)),"plugin_metadata":{"artifact_sha256":format!("sha256:{}","a".repeat(64)),"publisher_tenants":"[\"tenant-a\"]","worker_active":true,"publisher_id":"agentforge-maintainers","activation_id":uuid::Uuid::new_v4().to_string()}}]});
        assert!(active_quiz_record(&catalog));
        for field in [
            "worker_active",
            "publisher_id",
            "activation_id",
            "artifact_sha256",
            "publisher_tenants",
        ] {
            let mut changed = catalog.clone();
            changed["capabilities"][0]["plugin_metadata"]
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(!active_quiz_record(&changed));
        }
        catalog["capabilities"][0]["plugin_metadata"]["worker_active"] = serde_json::json!(false);
        assert!(!active_quiz_record(&catalog));
        assert!(!active_quiz_record(
            &serde_json::json!({"capabilities":[{"id":"generate_quiz"}]})
        ));
    }
}

#[cfg(test)]
#[path = "quiz_plugin_bridge_tests.rs"]
mod bridge_tests;
