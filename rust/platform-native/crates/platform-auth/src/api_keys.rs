//! Bound database keys only. No issuance, static fallback or fabricated service identity.
use crate::accounts::{PgAccounts, StoreError};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::FromRow;
use std::collections::BTreeSet;

/// Request-scoped authority: resolve again for every request to observe revocation/expiry.
/// Credential metadata has no Debug/Serialize implementation.
pub struct ApiKeyAuthority {
    pub id: i32,
    pub key_id: String,
    pub key_hash: String,
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub tier: String,
    pub effective_permissions: Vec<String>,
    /// Never includes permissions inherited from the owner.
    pub declared_scopes: BTreeSet<String>,
    pub allowed_services: Vec<String>,
    pub allowed_models: Vec<String>,
    pub rate_limit: Option<Value>,
}
#[derive(FromRow)]
struct KeyRow {
    id: i32,
    key_id: String,
    user_id: String,
    tenant_id: String,
    roles: Vec<String>,
    permissions: Vec<String>,
    tier: String,
    rate_limit: Option<Value>,
    allowed_services: Vec<String>,
    allowed_models: Vec<String>,
    legacy_scopes: Option<Value>,
}
fn normalized(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty() && seen.insert(s.clone()))
        .collect()
}
impl ApiKeyAuthority {
    pub fn allows_knowledge(&self, write: bool) -> bool {
        self.declared_scopes.contains(if write {
            "knowledge:write"
        } else {
            "knowledge:read"
        })
    }
    /// Key restriction only: tenant/user constraints must also independently allow access.
    pub fn allows_service(&self, aliases: &[&str]) -> bool {
        let patterns: Vec<String> = self
            .allowed_services
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if patterns.is_empty() {
            return true;
        }
        let aliases: Vec<String> = aliases
            .iter()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if aliases.is_empty() {
            return false;
        }
        patterns.iter().any(|pattern| {
            aliases.iter().any(|alias| {
                pattern == "*"
                    || if let Some(prefix) = pattern.strip_suffix('*') {
                        alias.starts_with(prefix)
                    } else {
                        pattern == alias
                    }
            })
        })
    }
}
impl PgAccounts {
    pub async fn bound_api_key(&self, raw_key: &str) -> Result<ApiKeyAuthority, StoreError> {
        if raw_key.is_empty() || raw_key.len() > 8192 {
            return Err(StoreError::InvalidInput);
        }
        let hash = format!("{:x}", Sha256::digest(raw_key.as_bytes()));
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| StoreError::Unavailable)?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|_| StoreError::Unavailable)?;
        let row = sqlx::query_as::<_, KeyRow>("SELECT k.id,COALESCE(k.key_id,'ak_legacy_'||k.id::text) AS key_id,k.user_id,k.tenant_id,k.roles,k.permissions,k.tier,k.rate_limit,k.allowed_services,k.allowed_models,to_jsonb(k)->'scopes' AS legacy_scopes FROM gateway.api_keys k JOIN gateway.users u ON u.user_id=k.user_id AND u.tenant_id=k.tenant_id JOIN gateway.tenants t ON t.tenant_id=k.tenant_id WHERE k.key_hash=$1 AND k.enabled AND (k.expires_at IS NULL OR k.expires_at>NOW()) AND u.status='active' AND t.status='active' AND k.user_id<>'' AND k.tenant_id<>''")
            .bind(&hash).fetch_optional(&mut *tx).await.map_err(|_| StoreError::Unavailable)?.ok_or(StoreError::Changed)?;
        let owner = Self::current_authority_in(&mut tx, &row.user_id, &row.tenant_id).await?;
        let key_permissions = normalized(row.permissions);
        let mut declared_scopes: BTreeSet<String> = key_permissions.iter().cloned().collect();
        if let Some(scopes) = row.legacy_scopes.filter(|v| !v.is_null()) {
            let values: Vec<Option<String>> =
                serde_json::from_value(scopes).map_err(|_| StoreError::InvalidInput)?;
            declared_scopes.extend(normalized(values.into_iter().flatten()));
        }
        let effective_permissions =
            normalized(key_permissions.into_iter().chain(owner.permissions));
        // Owner roles/tier MUST NOT replace the key's restricted roles/tier.
        let mut roles = normalized(row.roles);
        if roles.is_empty() {
            roles.push("user".into());
        }
        roles = normalized(
            roles
                .into_iter()
                .chain(effective_permissions.iter().cloned()),
        );
        tx.commit().await.map_err(|_| StoreError::Unavailable)?;
        Ok(ApiKeyAuthority {
            id: row.id,
            key_id: row.key_id,
            key_hash: hash,
            user_id: row.user_id,
            tenant_id: row.tenant_id,
            roles,
            tier: if row.tier.is_empty() {
                "normal".into()
            } else {
                row.tier
            },
            effective_permissions,
            declared_scopes,
            allowed_services: row.allowed_services,
            allowed_models: row.allowed_models,
            rate_limit: row.rate_limit,
        })
    }
}

/// Selection is not authentication; callers must verify the selected credential.
pub enum SelectedCredential<'a> {
    Jwt(&'a str),
    ApiKey(&'a str),
}
pub fn select_credential<'a>(
    headers: &'a axum::http::HeaderMap,
    key_header: &axum::http::HeaderName,
) -> Result<SelectedCredential<'a>, StoreError> {
    if key_header == axum::http::header::AUTHORIZATION {
        return Err(StoreError::InvalidInput);
    }
    let authorization = headers.get_all(axum::http::header::AUTHORIZATION);
    if authorization.iter().count() > 1 || headers.get_all(key_header).iter().count() > 1 {
        return Err(StoreError::InvalidInput);
    }
    if let Some(value) = authorization.iter().next() {
        let value = value.to_str().map_err(|_| StoreError::InvalidInput)?;
        let mut parts = value.split_whitespace();
        if parts
            .next()
            .is_some_and(|s| s.eq_ignore_ascii_case("bearer"))
        {
            let token = parts.next().ok_or(StoreError::InvalidInput)?;
            if parts.next().is_some() {
                return Err(StoreError::InvalidInput);
            }
            // Verification failure must not trigger fallback to an accompanying key.
            return Ok(SelectedCredential::Jwt(token));
        }
    }
    let key = headers
        .get(key_header)
        .ok_or(StoreError::Changed)?
        .to_str()
        .map_err(|_| StoreError::InvalidInput)?;
    if key.is_empty() || key.len() > 8192 {
        return Err(StoreError::InvalidInput);
    }
    Ok(SelectedCredential::ApiKey(key))
}
