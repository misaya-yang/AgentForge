//! Current-reader and audience guards for the unmounted quiz route family.
//! Native provenance stays in the established artifact-share scope adapter.
use crate::{
    artifact_share_scope::{ArtifactShareScope, NativeArtifactShareScope, freeze_source_scope},
    artifact_shares_api::ArtifactSharesIdentityResolver,
    quiz_share_store::QuizShareDatabase,
    quiz_types::{QuizError, QuizFuture, QuizIdentity},
    session_history::SourceReferences,
};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuizAudience {
    pub audience: String,
    pub user_id: Option<String>,
}

pub trait QuizScope: Send + Sync {
    fn authenticated<'a>(&'a self, user: &'a QuizIdentity, id: &'a str) -> QuizFuture<'a, ()>;
    fn shared<'a>(&'a self, headers: &'a HeaderMap, code: &'a str) -> QuizFuture<'a, QuizAudience>;
}

/// Construction is inert: no account, database, clock or source lookup occurs.
pub struct NativeQuizScope {
    pub sources: Arc<NativeArtifactShareScope>,
    pub identities: Arc<dyn ArtifactSharesIdentityResolver>,
    pub database: Option<Arc<dyn QuizShareDatabase>>,
    pub settings_available: bool,
}

fn unavailable() -> QuizError {
    QuizError::detail(
        404,
        json!({"code":"share_unavailable","message":"Quiz not found or expired"}),
    )
}
fn invalid_scope() -> QuizError {
    QuizError::detail(410, "Share source identities cannot be verified")
}

/// Decode the private frozen scope and reject duplicate, unversioned or malformed
/// identities. This is intentionally separate from the create-only scope encoder.
pub fn parse_source_scope(value: &Value) -> Result<SourceReferences, QuizError> {
    let decoded;
    let scope = if let Some(text) = value.as_str() {
        decoded = serde_json::from_str::<Value>(text).map_err(|_| invalid_scope())?;
        &decoded
    } else {
        value
    };
    let object = scope.as_object().ok_or_else(invalid_scope)?;
    // Python's scope.get("version") != 1 also accepts True and 1.0. Actual
    // source-version numbers below are stricter: type(version) is int.
    let version = object.get("version").ok_or_else(invalid_scope)?;
    if !(version.as_i64() == Some(1)
        || version.as_f64() == Some(1.0)
        || version.as_bool() == Some(true))
    {
        return Err(invalid_scope());
    }
    let datasets = object
        .get("dataset_ids")
        .and_then(Value::as_array)
        .ok_or_else(invalid_scope)?;
    let documents = object
        .get("document_ids")
        .and_then(Value::as_array)
        .ok_or_else(invalid_scope)?;
    let versions = object
        .get("source_versions")
        .and_then(Value::as_array)
        .ok_or_else(invalid_scope)?;
    let mut refs = SourceReferences::default();
    for value in datasets {
        let id = value.as_str().ok_or_else(invalid_scope)?;
        if !refs.datasets.insert(id.to_owned()) {
            return Err(invalid_scope());
        }
    }
    for value in documents {
        let parts = value
            .as_array()
            .filter(|v| v.len() == 2)
            .ok_or_else(invalid_scope)?;
        let dataset = parts[0].as_str().ok_or_else(invalid_scope)?;
        let document = parts[1].as_str().ok_or_else(invalid_scope)?;
        if !refs.documents.insert((dataset.into(), document.into())) {
            return Err(invalid_scope());
        }
    }
    for value in versions {
        let parts = value
            .as_array()
            .filter(|v| v.len() == 4)
            .ok_or_else(invalid_scope)?;
        let dataset = parts[0].as_str().ok_or_else(invalid_scope)?;
        let document = parts[1].as_str().ok_or_else(invalid_scope)?;
        let version = parts[2].as_i64().ok_or_else(invalid_scope)?;
        let hash = parts[3].as_str().ok_or_else(invalid_scope)?;
        if !refs
            .versions
            .insert((dataset.into(), document.into(), version, hash.into()))
        {
            return Err(invalid_scope());
        }
    }
    freeze_source_scope(&refs).map_err(|_| invalid_scope())?;
    Ok(refs)
}

impl QuizScope for NativeQuizScope {
    fn authenticated<'a>(&'a self, user: &'a QuizIdentity, id: &'a str) -> QuizFuture<'a, ()> {
        Box::pin(async move {
            if self.database.is_none() {
                return Err(QuizError::detail(503, "Database not available"));
            }
            // get_user_context permits guests; do not impose an account gate here.
            let refs = self
                .sources
                .quiz_sources_with_origin(user, id, false)
                .await?;
            if !self.sources.allowed(user, &refs).await? {
                return Err(QuizError::detail(
                    403,
                    json!({"code":"ASSISTANT_SOURCE_ACCESS_REVOKED"}),
                ));
            }
            Ok(())
        })
    }

    fn shared<'a>(&'a self, headers: &'a HeaderMap, code: &'a str) -> QuizFuture<'a, QuizAudience> {
        Box::pin(async move {
            let db = self
                .database
                .as_ref()
                .ok_or_else(|| QuizError::detail(503, "Database not available"))?;
            // guard_row is restricted to kind='quiz' and must retain payload privately.
            let row = db.guard_row(code).await?.ok_or_else(unavailable)?;
            if !crate::service_registry::python_truthy(&row["is_active"]) {
                return Err(unavailable());
            }
            if crate::service_registry::python_truthy(&row["expires_at"]) {
                let expiry = row["expires_at"].as_str().ok_or(QuizError::Internal)?;
                let expiry =
                    DateTime::parse_from_rfc3339(expiry).map_err(|_| QuizError::Internal)?;
                if expiry <= Utc::now() {
                    return Err(unavailable());
                }
            }
            let audience = if crate::service_registry::python_truthy(&row["audience"]) {
                row["audience"].as_str().ok_or_else(unavailable)?
            } else {
                "public"
            };
            match audience {
                "internal" => {
                    if !self.settings_available {
                        return Err(QuizError::detail(503, "Authentication is unavailable"));
                    }
                    let user = self.identities.resolve(headers).await?;
                    if !user.is_authenticated
                        || user.user_id.is_empty()
                        || user.tenant_id.as_deref().unwrap_or("").is_empty()
                    {
                        return Err(QuizError::detail(
                            401,
                            "Sign in to open this internal share",
                        ));
                    }
                    // Tenant denial comes before any live account query or scope decode.
                    if user.tenant_id.as_deref() != row["tenant_id"].as_str() {
                        return Err(QuizError::detail(404, "Share not found"));
                    }
                    self.sources.require_active(&user).await?;
                    let refs = parse_source_scope(&row["source_scope"])?;
                    if !self.sources.allowed(&user, &refs).await? {
                        return Err(QuizError::detail(
                            403,
                            "Share source access has been revoked",
                        ));
                    }
                    Ok(QuizAudience {
                        audience: audience.into(),
                        user_id: Some(user.user_id),
                    })
                }
                "public" => {
                    let decoded;
                    let payload = if let Some(text) = row["payload"].as_str() {
                        decoded =
                            serde_json::from_str::<Value>(text).map_err(|_| QuizError::Internal)?;
                        &decoded
                    } else {
                        &row["payload"]
                    };
                    let id = payload
                        .get("quiz_id")
                        .and_then(Value::as_str)
                        .ok_or_else(unavailable)?;
                    let id = uuid::Uuid::parse_str(id)
                        .map_err(|_| unavailable())?
                        .to_string();
                    let origin_user = QuizIdentity {
                        tenant_id: row["tenant_id"].as_str().map(str::to_owned),
                        ..QuizIdentity::default()
                    };
                    let refs = self
                        .sources
                        .quiz_sources_with_origin(&origin_user, &id, true)
                        .await
                        .map_err(|error| match error {
                            QuizError::Http(404 | 409, _) => unavailable(),
                            other => other,
                        })?;
                    if !refs.datasets.is_empty()
                        || !refs.documents.is_empty()
                        || !refs.versions.is_empty()
                    {
                        return Err(unavailable());
                    }
                    Ok(QuizAudience {
                        audience: audience.into(),
                        user_id: None,
                    })
                }
                _ => Err(unavailable()),
            }
        })
    }
}
