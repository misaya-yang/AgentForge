//! Tenant-scoped SCIM2 lifecycle over existing users and managed role grants.
// SPDX-License-Identifier: Apache-2.0
use super::*;
use serde::{Deserialize, Serialize};
use sqlx::Row;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRequest {
    pub scopes: Vec<String>,
    pub ttl_seconds: u64,
}
pub(super) struct ScimActor {
    tenant: String,
    credential: String,
    scopes: Vec<String>,
}
impl ScimActor {
    pub(super) fn require(&self, scope: &str) -> Result<()> {
        if self.scopes.iter().any(|s| s == scope) {
            Ok(())
        } else {
            Err(EnterpriseError::Forbidden)
        }
    }
}
const USER_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:User";
const GROUP_SCHEMA: &str = "urn:ietf:params:scim:schemas:core:2.0:Group";
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Email {
    pub value: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct UserInput {
    pub schemas: Vec<String>,
    #[serde(rename = "externalId")]
    pub external_id: String,
    #[serde(rename = "userName")]
    pub user_name: String,
    #[serde(rename = "displayName", default)]
    pub display_name: Option<String>,
    #[serde(default = "active")]
    pub active: bool,
    #[serde(default)]
    pub emails: Vec<Email>,
}
fn active() -> bool {
    true
}
#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub value: String,
    #[serde(default)]
    pub display: Option<String>,
}
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct GroupInput {
    pub schemas: Vec<String>,
    #[serde(rename = "externalId")]
    pub external_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default)]
    pub members: Vec<Member>,
}
impl EnterpriseService {
    pub async fn issue_scim_credential(
        &self,
        p: &Principal,
        input: CredentialRequest,
    ) -> Result<Value> {
        if !p.authority.roles.iter().any(|r| r == "admin")
            && !p
                .authority
                .permissions
                .iter()
                .any(|r| r == "identity:provision")
        {
            return Err(EnterpriseError::Forbidden);
        }
        self.require_recent_mfa(p).await?;
        if input.ttl_seconds == 0
            || input.ttl_seconds > 90 * 86400
            || input.scopes.is_empty()
            || input.scopes.len() > 4
            || input.scopes.iter().any(|s| {
                !matches!(
                    s.as_str(),
                    "scim.users.read"
                        | "scim.users.write"
                        | "scim.groups.read"
                        | "scim.groups.write"
                )
            })
        {
            return Err(EnterpriseError::Invalid);
        }
        let token = crate::sessions::browser::opaque();
        let id = uuid::Uuid::new_v4().to_string();
        let expires = self
            .now()?
            .checked_add(input.ttl_seconds)
            .ok_or(EnterpriseError::Invalid)?;
        sqlx::query("INSERT INTO gateway.enterprise_scim_credentials(credential_id,tenant_id,token_hash,scopes,created_by,expires_at) VALUES($1::text::uuid,$2,$3,$4,$5,to_timestamp($6::bigint::double precision))").bind(&id).bind(&p.account.tenant_id).bind(digest(token.as_bytes())).bind(input.scopes).bind(&p.account.user_id).bind(i64::try_from(expires).map_err(|_|EnterpriseError::Invalid)?).execute(&self.pool).await?;
        Ok(json!({"id":id,"token":token,"tenant_id":p.account.tenant_id,"expires_at":expires}))
    }
    pub(super) async fn scim_actor(&self, tenant: &str, headers: &HeaderMap) -> Result<ScimActor> {
        if headers.contains_key("cookie") || headers.get_all("authorization").iter().count() != 1 {
            return Err(EnterpriseError::Proof);
        }
        let raw = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .ok_or(EnterpriseError::Proof)?;
        let mut parts = raw.split_whitespace();
        if !parts
            .next()
            .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
        {
            return Err(EnterpriseError::Proof);
        }
        let token = parts.next().ok_or(EnterpriseError::Proof)?;
        if parts.next().is_some() || token.len() != 64 {
            return Err(EnterpriseError::Proof);
        }
        self.transient
            .rate("scim-credential", token, 600, 60)
            .await?;
        let row=sqlx::query("SELECT credential_id::text AS id,tenant_id,scopes FROM gateway.enterprise_scim_credentials WHERE token_hash=$1 AND tenant_id=$2 AND revoked_at IS NULL AND expires_at>CURRENT_TIMESTAMP AND EXISTS(SELECT 1 FROM gateway.tenants t WHERE t.tenant_id=$2 AND t.status='active')").bind(digest(token.as_bytes())).bind(tenant).fetch_optional(&self.pool).await?.ok_or(EnterpriseError::Proof)?;
        Ok(ScimActor {
            tenant: row.try_get("tenant_id")?,
            credential: row.try_get("id")?,
            scopes: row.try_get("scopes")?,
        })
    }
    pub(super) async fn scim_list_users(
        &self,
        a: &ScimActor,
        start: u64,
        count: u64,
        filter: Option<&str>,
    ) -> Result<Value> {
        a.require("scim.users.read")?;
        let count = count.min(100);
        let start = start.max(1);
        if start > i64::MAX as u64 {
            return Err(EnterpriseError::Invalid);
        }
        let user_filter = filter.map(parse_filter).transpose()?;
        let total:i64=sqlx::query_scalar("SELECT COUNT(*) FROM gateway.users u JOIN gateway.enterprise_scim_users s USING(tenant_id,user_id) WHERE u.tenant_id=$1 AND NOT s.deleted AND ($2::text IS NULL OR LOWER(u.username)=LOWER($2))").bind(&a.tenant).bind(&user_filter).fetch_one(&self.pool).await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT u.user_id FROM gateway.users u JOIN gateway.enterprise_scim_users s USING(tenant_id,user_id) WHERE u.tenant_id=$1 AND NOT s.deleted AND ($2::text IS NULL OR LOWER(u.username)=LOWER($2)) ORDER BY u.user_id OFFSET $3 LIMIT $4").bind(&a.tenant).bind(user_filter).bind(start as i64-1).bind(count as i64).fetch_all(&self.pool).await?;
        let mut resources = Vec::new();
        for id in ids {
            resources.push(self.scim_user(a, &id).await?);
        }
        Ok(
            json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:ListResponse"],"totalResults":total,"startIndex":start,"itemsPerPage":resources.len(),"Resources":resources}),
        )
    }
    pub(super) async fn scim_user(&self, a: &ScimActor, id: &str) -> Result<Value> {
        // A write-scoped provisioner needs to receive its mutation's resource.
        if !a
            .scopes
            .iter()
            .any(|s| s == "scim.users.read" || s == "scim.users.write")
        {
            return Err(EnterpriseError::Forbidden);
        }
        let r=sqlx::query("SELECT u.user_id,u.username,u.email,u.display_name,u.status,u.metadata,s.external_id,s.version FROM gateway.users u JOIN gateway.enterprise_scim_users s USING(tenant_id,user_id) WHERE u.tenant_id=$1 AND u.user_id=$2 AND NOT s.deleted").bind(&a.tenant).bind(id).fetch_optional(&self.pool).await?.ok_or(EnterpriseError::NotFound)?;
        let email: Option<String> = r.try_get("email")?;
        let metadata: Value = r.try_get("metadata")?;
        let emails = metadata["enterprise_scim"]["emails"]
            .as_array()
            .cloned()
            .unwrap_or_else(|| {
                email
                    .map(|value| vec![json!({"value":value,"primary":true})])
                    .unwrap_or_default()
            });
        Ok(
            json!({"schemas":[USER_SCHEMA],"id":id,"externalId":r.try_get::<String,_>("external_id")?,"userName":r.try_get::<String,_>("username")?,"displayName":r.try_get::<Option<String>,_>("display_name")?,"active":r.try_get::<String,_>("status")?=="active","emails":emails,"meta":{"resourceType":"User","version":format!("W/\"{}\"",r.try_get::<i64,_>("version")?)}}),
        )
    }
    pub(super) async fn scim_create_user(&self, a: &ScimActor, input: UserInput) -> Result<Value> {
        a.require("scim.users.write")?;
        validate_user(&input)?;
        let id = format!("scim_{}", uuid::Uuid::new_v4());
        let email = primary_email(&input)?;
        let emails = serde_json::to_value(&input.emails).map_err(|_| EnterpriseError::Invalid)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO gateway.users(user_id,tenant_id,username,email,display_name,status,roles,permissions,tier,password_hash,force_password_change,created_by,metadata) VALUES($1,$2,$3,$4,$5,$6,ARRAY[]::varchar[],ARRAY[]::varchar[],'normal',NULL,false,$7,jsonb_build_object('enterprise_scim',jsonb_build_object('emails',$8::jsonb)))").bind(&id).bind(&a.tenant).bind(input.user_name).bind(email).bind(input.display_name).bind(if input.active{"active"}else{"inactive"}).bind(format!("scim:{}",a.credential)).bind(emails).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO gateway.enterprise_scim_users(tenant_id,user_id,external_id) VALUES($1,$2,$3)").bind(&a.tenant).bind(&id).bind(input.external_id).execute(&mut *tx).await?;
        tx.commit().await?;
        self.scim_user(a, &id).await
    }
    pub(super) async fn scim_replace_user(
        &self,
        a: &ScimActor,
        id: &str,
        input: UserInput,
        version: &str,
    ) -> Result<Value> {
        a.require("scim.users.write")?;
        validate_user(&input)?;
        let expected = parse_version(version)?;
        let email = primary_email(&input)?;
        let emails = serde_json::to_value(&input.emails).map_err(|_| EnterpriseError::Invalid)?;
        let mut tx = self.pool.begin().await?;
        admin::lock_mutable_target(&mut tx, &a.tenant, id).await?;
        let updated=sqlx::query("UPDATE gateway.enterprise_scim_users SET external_id=$3,version=version+1 WHERE tenant_id=$1 AND user_id=$2 AND version=$4 AND NOT deleted").bind(&a.tenant).bind(id).bind(input.external_id).bind(expected).execute(&mut *tx).await?.rows_affected();
        if updated != 1 {
            return Err(EnterpriseError::Precondition);
        }
        // Durable exact revision invalidates BFF, native bearer and queued actor
        // authority on the next check, including disable/re-enable cycles.
        sqlx::query("UPDATE gateway.users SET username=$3,email=$4,display_name=$5,status=$6,auth_epoch=auth_epoch+1,metadata=COALESCE(metadata,'{}'::jsonb)||jsonb_build_object('enterprise_scim',COALESCE(metadata->'enterprise_scim','{}'::jsonb)||jsonb_build_object('emails',$7::jsonb)),updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2").bind(&a.tenant).bind(id).bind(input.user_name).bind(email).bind(input.display_name).bind(if input.active{"active"}else{"inactive"}).bind(emails).execute(&mut *tx).await?;
        tx.commit().await?;
        self.scim_user(a, id).await
    }
    pub(super) async fn scim_delete_user(
        &self,
        a: &ScimActor,
        id: &str,
        version: &str,
    ) -> Result<()> {
        a.require("scim.users.write")?;
        let mut tx = self.pool.begin().await?;
        admin::lock_mutable_target(&mut tx, &a.tenant, id).await?;
        if sqlx::query("UPDATE gateway.enterprise_scim_users SET deleted=true,version=version+1 WHERE tenant_id=$1 AND user_id=$2 AND version=$3 AND NOT deleted").bind(&a.tenant).bind(id).bind(parse_version(version)?).execute(&mut *tx).await?.rows_affected()!=1{return Err(EnterpriseError::Conflict);}
        sqlx::query("UPDATE gateway.users SET status='inactive',auth_epoch=auth_epoch+1,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND user_id=$2").bind(&a.tenant).bind(id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}
fn validate_user(i: &UserInput) -> Result<()> {
    if i.schemas != [USER_SCHEMA] || i.emails.len() > 8 {
        return Err(EnterpriseError::Invalid);
    }
    for email in &i.emails {
        bounded(&email.value, 255)?;
        if !email.value.contains('@') {
            return Err(EnterpriseError::Invalid);
        }
        if let Some(kind) = &email.kind {
            bounded(kind, 64)?;
        }
    }
    bounded(&i.external_id, 255)?;
    bounded(&i.user_name, 255)?;
    if let Some(v) = &i.display_name {
        bounded(v, 255)?;
    }
    Ok(())
}
fn primary_email(i: &UserInput) -> Result<Option<String>> {
    if i.emails.iter().filter(|e| e.primary).count() > 1 {
        return Err(EnterpriseError::Invalid);
    }
    let email = i
        .emails
        .iter()
        .find(|e| e.primary)
        .or_else(|| i.emails.first())
        .map(|e| e.value.to_ascii_lowercase());
    if let Some(e) = &email {
        bounded(e, 255)?;
        if !e.contains('@') {
            return Err(EnterpriseError::Invalid);
        }
    }
    Ok(email)
}
fn parse_filter(filter: &str) -> Result<String> {
    let v = filter
        .strip_prefix("userName eq \"")
        .and_then(|s| s.strip_suffix('"'))
        .ok_or(EnterpriseError::Invalid)?;
    bounded(v, 255)?;
    if v.contains(['"', '\\']) {
        return Err(EnterpriseError::Invalid);
    }
    Ok(v.into())
}
fn parse_version(v: &str) -> Result<i64> {
    v.strip_prefix("W/\"")
        .and_then(|s| s.strip_suffix('"'))
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or(EnterpriseError::Precondition)
}
#[path = "scim_groups.rs"]
mod groups;
#[path = "scim_patch.rs"]
mod patch;
pub use patch::Patch;

#[path = "scim_admin.rs"]
mod admin;
