//! Tenant-scoped account administration using the existing users routes.
//!
//! Nonempty authenticated tenant identity is mandatory. Account administration
//! requires an exact admin/platform_admin role in addition to endpoint RBAC;
//! the PostgreSQL write guard rechecks actor and target authority transactionally.
//! Platform operators and the caller's own authority are protected. Self-service
//! profile edits remain limited to the caller's display name. Creation and role
//! assignments commit together; no route uses an account upsert or separate grant.
//! Public response shapes and cancellation/error rendering remain compatible.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    rate_config_api::{boolean, integer},
    service_access_resolver::clear_service_access_constraint_cache,
    service_config_api::{
        ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture, ServiceConfigIdentity,
        ServiceConfigIdentityResolver,
    },
    service_registry::python_truthy,
    usage_parser::{python_value_to_string, python_whitespace},
    users_store::{NewUser, UserListFilter, UserMutation, UsersDatabase},
};
use ai_platform_auth::{PasswordEngine, admission::AdmissionService, rbac::Rbac};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, sync::Arc};

use crate::native_contract::users as contract;
pub use contract::USERS_ROUTES;
pub trait UsersProfileIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity>;
}
impl UsersProfileIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ServiceConfigIdentity> {
        ServiceConfigIdentityResolver::resolve(self, headers, path)
    }
}
pub trait UsersPasswordHasher: Send + Sync {
    fn hash<'a>(&'a self, password: &'a str) -> ServiceConfigFuture<'a, String>;
}
impl UsersPasswordHasher for PasswordEngine {
    fn hash<'a>(&'a self, password: &'a str) -> ServiceConfigFuture<'a, String> {
        Box::pin(async move {
            PasswordEngine::hash(self, password.to_owned())
                .await
                .map_err(|_| ServiceConfigError::Internal)
        })
    }
}
/// No Debug/Serialize. Explicit private configuration, no env reads or fallback.
pub struct UsersConfiguration {
    default_password: String,
    allowed_email_domain: String,
}
impl UsersConfiguration {
    pub fn new(default_password: String, allowed_email_domain: String) -> Self {
        Self {
            default_password,
            allowed_email_domain: allowed_email_domain
                .trim_matches(python_whitespace)
                .to_lowercase(),
        }
    }
}
pub trait UsersRuntimeValues: Send + Sync {
    fn collision_suffix(&self) -> String;
    fn created_at(&self) -> String;
}
pub struct SystemUsersRuntimeValues;
impl UsersRuntimeValues for SystemUsersRuntimeValues {
    fn collision_suffix(&self) -> String {
        uuid::Uuid::new_v4().simple().to_string()[..6].to_owned()
    }
    fn created_at(&self) -> String {
        let now = Utc::now();
        now.format(if now.timestamp_subsec_micros() == 0 {
            "%Y-%m-%dT%H:%M:%S"
        } else {
            "%Y-%m-%dT%H:%M:%S%.6f"
        })
        .to_string()
    }
}
pub trait UsersServiceAccessInvalidation: Send + Sync {
    fn clear(&self) -> Result<(), ServiceConfigError>;
}
pub struct GlobalUsersServiceAccessInvalidation;
impl UsersServiceAccessInvalidation for GlobalUsersServiceAccessInvalidation {
    fn clear(&self) -> Result<(), ServiceConfigError> {
        clear_service_access_constraint_cache();
        Ok(())
    }
}
pub struct UsersDependencies {
    pub identity: Arc<dyn ServiceConfigIdentityResolver>,
    pub profile_identity: Arc<dyn UsersProfileIdentityResolver>,
    pub rbac: Rbac,
    pub database: Option<Arc<dyn UsersDatabase>>,
    pub passwords: Arc<dyn UsersPasswordHasher>,
    pub configuration: UsersConfiguration,
    pub runtime: Arc<dyn UsersRuntimeValues>,
    pub service_access: Arc<dyn UsersServiceAccessInvalidation>,
}
pub struct UsersApi {
    dependencies: UsersDependencies,
}
#[derive(Clone, Debug)]
pub enum UsersOperation {
    List {
        page: Value,
        page_size: Value,
        status: Option<String>,
        search: Option<String>,
    },
    Create,
    Get {
        user_id: String,
    },
    Update {
        user_id: String,
    },
    Delete {
        user_id: String,
    },
    ResetPassword {
        user_id: String,
    },
    Enable {
        user_id: String,
    },
    Disable {
        user_id: String,
    },
    Profile {
        user_id: String,
    },
}
impl UsersOperation {
    fn path(&self) -> String {
        match self {
            Self::List { .. } | Self::Create => "/api/v1/users".into(),
            Self::Get { user_id } | Self::Update { user_id } | Self::Delete { user_id } => {
                format!("/api/v1/users/{user_id}")
            }
            Self::ResetPassword { user_id } => format!("/api/v1/users/{user_id}/reset-password"),
            Self::Enable { user_id } => format!("/api/v1/users/{user_id}/enable"),
            Self::Disable { user_id } => format!("/api/v1/users/{user_id}/disable"),
            Self::Profile { user_id } => format!("/api/v1/users/{user_id}/profile"),
        }
    }
    fn model(&self) -> Option<&'static str> {
        match self {
            Self::Create => Some("UserCreate"),
            Self::Update { .. } => Some("UserUpdate"),
            Self::Profile { .. } => Some("ProfileUpdate"),
            _ => None,
        }
    }
    fn permission(&self) -> Option<&'static str> {
        match self {
            Self::List { .. } | Self::Get { .. } => Some("user:list"),
            Self::Create => Some("user:create"),
            Self::Delete { .. } => Some("user:delete"),
            Self::Profile { .. } => None,
            _ => Some("user:edit"),
        }
    }
}
fn missing() -> ServiceConfigError {
    ServiceConfigError::detail(404, "User not found")
}
impl UsersApi {
    pub fn new(dependencies: UsersDependencies) -> Self {
        Self { dependencies }
    }
    fn database(&self) -> Result<&dyn UsersDatabase, ServiceConfigError> {
        self.dependencies
            .database
            .as_deref()
            .filter(|db| db.enabled())
            .ok_or_else(|| ServiceConfigError::detail(503, "Database not available"))
    }
    async fn hash_default(&self) -> Result<String, ServiceConfigError> {
        let password = &self.dependencies.configuration.default_password;
        if password.is_empty() {
            return Err(ServiceConfigError::detail(
                503,
                "DEFAULT_USER_PASSWORD is not configured",
            ));
        }
        self.dependencies.passwords.hash(password).await
    }
    pub async fn handle(
        &self,
        operation: UsersOperation,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        let path = operation.path();
        let identity = if matches!(operation, UsersOperation::Profile { .. }) {
            self.dependencies
                .profile_identity
                .resolve(headers, &path)
                .await?
        } else {
            self.dependencies.identity.resolve(headers, &path).await?
        };
        self.execute(operation, &identity, body).await
    }
    pub async fn execute(
        &self,
        operation: UsersOperation,
        identity: &ServiceConfigIdentity,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        let body = if let Some(model) = operation.model() {
            validate_model(
                model,
                body,
                &self.dependencies.configuration.allowed_email_domain,
            )?
        } else {
            Value::Null
        };
        let pagination = if let UsersOperation::List {
            page, page_size, ..
        } = &operation
        {
            Some(validate_pagination(page, page_size)?)
        } else {
            None
        };
        if let Some(permission) = operation.permission() {
            self.dependencies
                .rbac
                .require(&identity.roles, permission)
                .map_err(|e| ServiceConfigError::Http {
                    status: 403,
                    body: json!({"error":e}),
                })?;
        }
        if identity.user_id.trim().is_empty() || identity.tenant_id.trim().is_empty() {
            return Err(ServiceConfigError::detail(403, "Active tenant account required"));
        }
        let administering = !matches!(operation,
            UsersOperation::List { .. } | UsersOperation::Get { .. } | UsersOperation::Profile { .. });
        if administering && !identity.roles.iter().any(|role| matches!(role.as_str(), "admin" | "platform_admin")) {
            return Err(ServiceConfigError::detail(403, "Tenant administrator role required"));
        }
        let tenant = &identity.tenant_id;
        match operation {
            UsersOperation::List { status, search, .. } => {
                let (page, page_size) = pagination.expect("validated pagination");
                let offset = (page - 1)
                    .checked_mul(page_size)
                    .ok_or(ServiceConfigError::Internal)?;
                let filter = UserListFilter {
                    status,
                    search,
                    tenant_id: Some(tenant.into()),
                    limit: page_size,
                    offset,
                };
                let (rows, total) = self.database()?.list_users_paginated(&filter).await?;
                let users = rows
                    .iter()
                    .map(|row| project_user(row, &[], true))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(contract::listed(users, total, page, page_size))
            }
            UsersOperation::Create => {
                let db = self.database()?;
                let email = body["email"].as_str().expect("validated email");
                if db
                    .get_user_by_email(email)
                    .await?
                    .as_ref()
                    .is_some_and(python_truthy)
                {
                    return Err(ServiceConfigError::detail(400, "Email already registered"));
                }
                let mut user_id = email
                    .split('@')
                    .next()
                    .unwrap_or("")
                    .replace('.', "_")
                    .to_lowercase();
                if db
                    .get_user(&user_id)
                    .await?
                    .as_ref()
                    .is_some_and(python_truthy)
                {
                    user_id = format!("{user_id}_{}", self.dependencies.runtime.collision_suffix());
                }
                let hash = self.hash_default().await?;
                let user = json!({"user_id":user_id,"email":email,"display_name":body["display_name"],"department":body["department"],"username":user_id,"roles":body["roles"],"force_password_change":true,"status":"active","tier":"normal","tenant_id":tenant,"created_by":identity.user_id});
                db.save_user_with_password(&NewUser::new(user.clone(), hash))
                    .await?;
                let mut user = user;
                user["created_at"] = json!(self.dependencies.runtime.created_at());
                project_user(&user, &[], true)
            }
            UsersOperation::Get { user_id } => {
                let db = self.database()?;
                let user = scoped(db, &user_id, tenant).await?;
                let extra = extra(db, &user_id).await?;
                project_user(&user, &extra, true)
            }
            UsersOperation::Update { user_id } => {
                let db = self.database()?;
                let user = scoped(db, &user_id, tenant).await?;
                let admin = identity.roles.iter().any(|r| matches!(r.as_str(), "admin" | "platform_admin"));
                let privileged: Vec<_> = ["roles", "status", "tier"]
                    .into_iter()
                    .filter(|key| !body[*key].is_null())
                    .collect();
                if !privileged.is_empty() && !admin {
                    return Err(ServiceConfigError::detail(
                        403,
                        format!("Only admins can modify: {}", privileged.join(", ")),
                    ));
                }
                if !body["extra_permissions"].is_null() && !admin {
                    return Err(ServiceConfigError::detail(
                        403,
                        "Only admins can modify extra permissions",
                    ));
                }
                let service_updated =
                    ["service_access_mode", "allowed_services", "denied_services"]
                        .iter()
                        .any(|key| !body[*key].is_null());
                if service_updated && !admin {
                    return Err(ServiceConfigError::detail(
                        403,
                        "Only admins can modify service access policy",
                    ));
                }
                let mut fields = body.as_object().expect("validated body").clone();
                for name in [
                    "roles",
                    "extra_permissions",
                    "service_access_mode",
                    "allowed_services",
                    "denied_services",
                ] {
                    fields.remove(name);
                }
                if service_updated {
                    let mut metadata = user["metadata"].as_object().cloned().unwrap_or_default();
                    let mut service = metadata
                        .get("service_access")
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    if !body["service_access_mode"].is_null() {
                        service.insert("mode".into(), body["service_access_mode"].clone());
                    }
                    for name in ["allowed_services", "denied_services"] {
                        if !body[name].is_null() {
                            service
                                .insert(name.into(), json!(normalize_service_list(&body[name])?));
                        }
                    }
                    if !service.contains_key("mode") {
                        let allow = service.get("allowed_services").is_some_and(python_truthy);
                        service.insert(
                            "mode".into(),
                            json!(if allow { "allowlist" } else { "all" }),
                        );
                    }
                    metadata.insert("service_access".into(), json!(service));
                    fields.insert("metadata".into(), json!(metadata));
                }
                let mutation = UserMutation {
                    fields,
                    roles: optional_strings(&body["roles"]),
                    extra_permissions: optional_strings(&body["extra_permissions"]),
                    granted_by: Some(identity.user_id.clone()),
                    profile_only: false,
                };
                if !mutation.fields.is_empty()
                    || mutation.roles.is_some()
                    || mutation.extra_permissions.is_some()
                {
                    if !db
                        .update_user_for_tenant(&user_id, tenant, &mutation)
                        .await?
                    {
                        return Err(missing());
                    }
                    if service_updated {
                        self.dependencies.service_access.clear()?;
                    }
                }
                let user = scoped(db, &user_id, tenant).await?;
                let extra = extra(db, &user_id).await?;
                project_user(&user, &extra, true)
            }
            UsersOperation::Delete { user_id } => {
                if user_id == identity.user_id {
                    return Err(ServiceConfigError::detail(
                        400,
                        "Cannot delete your own account",
                    ));
                }
                let db = self.database()?;
                scoped(db, &user_id, tenant).await?;
                if user_id == "admin" {
                    return Err(ServiceConfigError::detail(
                        400,
                        "Cannot delete system admin",
                    ));
                }
                if !db.delete_user_for_tenant(&user_id, tenant, &identity.user_id).await? {
                    return Err(missing());
                }
                self.dependencies.service_access.clear()?;
                Ok(contract::deleted(&user_id))
            }
            UsersOperation::ResetPassword { user_id } => {
                let db = self.database()?;
                scoped(db, &user_id, tenant).await?;
                let hash = self.hash_default().await?;
                if !db
                    .reset_user_password_for_tenant(&user_id, tenant, &hash, &identity.user_id)
                    .await?
                {
                    return Err(missing());
                }
                Ok(contract::password_reset(&user_id))
            }
            UsersOperation::Enable { user_id } => {
                let db = self.database()?;
                scoped(db, &user_id, tenant).await?;
                let mutation = UserMutation {
                    fields: json!({"status":"active"})
                        .as_object()
                        .expect("object")
                        .clone(),
                    granted_by: Some(identity.user_id.clone()),
                    ..Default::default()
                };
                if !db
                    .update_user_for_tenant(&user_id, tenant, &mutation)
                    .await?
                {
                    return Err(missing());
                }
                Ok(contract::enabled(&user_id))
            }
            UsersOperation::Disable { user_id } => {
                if user_id == identity.user_id {
                    return Err(ServiceConfigError::detail(
                        400,
                        "Cannot disable your own account",
                    ));
                }
                let db = self.database()?;
                scoped(db, &user_id, tenant).await?;
                if user_id == "admin" {
                    return Err(ServiceConfigError::detail(
                        400,
                        "Cannot disable system admin",
                    ));
                }
                let mutation = UserMutation {
                    fields: json!({"status":"disabled"})
                        .as_object()
                        .expect("object")
                        .clone(),
                    granted_by: Some(identity.user_id.clone()),
                    ..Default::default()
                };
                if !db
                    .update_user_for_tenant(&user_id, tenant, &mutation)
                    .await?
                {
                    return Err(missing());
                }
                Ok(contract::disabled(&user_id))
            }
            UsersOperation::Profile { user_id } => {
                if user_id != identity.user_id {
                    return Err(ServiceConfigError::detail(
                        403,
                        "Can only update your own profile",
                    ));
                }
                let db = self.database()?;
                scoped(db, &user_id, tenant).await?;
                let fields = body.as_object().expect("validated profile").clone();
                if !fields.is_empty()
                    && !db
                        .update_user_for_tenant(
                            &user_id,
                            tenant,
                            &UserMutation {
                                fields,
                                granted_by: Some(identity.user_id.clone()),
                                profile_only: true,
                                ..Default::default()
                            },
                        )
                        .await?
                {
                    return Err(missing());
                }
                let user = scoped(db, &user_id, tenant).await?;
                let extra = extra(db, &user_id).await?;
                project_user(&user, &extra, false)
            }
        }
    }
}
async fn scoped(
    db: &dyn UsersDatabase,
    user: &str,
    tenant: &str,
) -> Result<Value, ServiceConfigError> {
    db.get_user_for_tenant(user, tenant)
        .await?
        .filter(python_truthy)
        .ok_or_else(missing)
}
async fn extra(db: &dyn UsersDatabase, user: &str) -> Result<Vec<Value>, ServiceConfigError> {
    Ok(db
        .get_user_extra_permissions(user)
        .await?
        .iter()
        .map(|row| row.get("permission_code").cloned().unwrap_or(Value::Null))
        .collect())
}
fn optional_strings(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|a| {
        a.iter()
            .map(|v| v.as_str().expect("validated string").to_owned())
            .collect()
    })
}
pub fn normalize_service_list(value: &Value) -> Result<Vec<String>, ServiceConfigError> {
    let values = match value {
        Value::Null => vec![],
        Value::Array(v) => v.clone(),
        Value::String(s) => s.chars().map(|c| json!(c.to_string())).collect(),
        Value::Object(m) => m.keys().map(|s| json!(s)).collect(),
        _ => return Err(ServiceConfigError::Internal),
    };
    let mut out = vec![];
    for value in values {
        let text = if python_truthy(&value) {
            python_value_to_string(&value)
        } else {
            String::new()
        };
        let token = text.trim_matches(python_whitespace).to_owned();
        if !token.is_empty() && !out.contains(&token) {
            out.push(token);
        }
    }
    Ok(out)
}
pub fn extract_service_access(metadata: &Value) -> Result<Value, ServiceConfigError> {
    let service = metadata
        .as_object()
        .and_then(|m| m.get("service_access"))
        .filter(|v| v.is_object())
        .unwrap_or(&Value::Null);
    let raw = &service["mode"];
    let mode = if python_truthy(raw) {
        python_value_to_string(raw)
            .trim_matches(python_whitespace)
            .to_lowercase()
    } else {
        String::new()
    };
    let mode = if contract::SERVICE_ACCESS_MODES.contains(&mode.as_str()) {
        mode
    } else if python_truthy(&service["allowed_services"]) {
        "allowlist".into()
    } else {
        "all".into()
    };
    Ok(
        json!({"service_access_mode":mode,"allowed_services":normalize_service_list(&service["allowed_services"])?,"denied_services":normalize_service_list(&service["denied_services"])?}),
    )
}
/// Declared response fields only. Invalid source row types become internal errors.
pub fn project_user(
    user: &Value,
    extra: &[Value],
    metadata: bool,
) -> Result<Value, ServiceConfigError> {
    let source = user.as_object().ok_or(ServiceConfigError::Internal)?;
    let mut result = contract::user_defaults();
    for (field, value) in result.as_object_mut().expect("object") {
        if let Some(v) = source.get(field) {
            *value = v.clone();
        }
    }
    result["extra_permissions"] = json!(extra);
    if metadata {
        for (key, value) in extract_service_access(&user["metadata"])?
            .as_object()
            .expect("object")
        {
            result[key] = value.clone();
        }
    }
    for (field, value) in result.as_object_mut().expect("object") {
        match field.as_str() {
            "roles" | "extra_permissions" | "allowed_services" | "denied_services" => {
                if !value
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string))
                {
                    return Err(ServiceConfigError::Internal);
                }
            }
            "force_password_change" => {
                *value = json!(boolean(value).map_err(|_| ServiceConfigError::Internal)?)
            }
            "user_id" | "status" | "tier" | "service_access_mode" => {
                if !value.is_string() {
                    return Err(ServiceConfigError::Internal);
                }
            }
            _ => {
                if !value.is_null() && !value.is_string() {
                    return Err(ServiceConfigError::Internal);
                }
            }
        }
    }
    Ok(result)
}
fn validation(
    kind: &str,
    loc: Vec<Value>,
    input: &Value,
    message: impl Into<String>,
    ctx: Option<Value>,
) -> Value {
    let mut error = json!({"type":kind,"loc":loc,"msg":message.into(),"input":input});
    if let Some(ctx) = ctx {
        error["ctx"] = ctx;
    }
    error
}
fn field_error(
    kind: &str,
    field: &str,
    input: &Value,
    message: impl Into<String>,
    ctx: Option<Value>,
) -> Value {
    validation(kind, vec![json!("body"), json!(field)], input, message, ctx)
}
fn value_error(field: &str, input: &Value, message: &str) -> Value {
    field_error(
        "value_error",
        field,
        input,
        format!("Value error, {message}"),
        Some(json!({"error":{}})),
    )
}
fn email(value: &str, domain: &str) -> Result<String, &'static str> {
    let raw = value.trim_matches(python_whitespace);
    if raw.is_empty() {
        return Err("Email is required");
    }
    let raw = if raw.contains('@') {
        raw.to_owned()
    } else {
        format!("{raw}@{domain}")
    };
    let Some((local, host)) = raw.split_once('@') else {
        return Err("Invalid email format");
    };
    let valid_local = !local.is_empty()
        && local
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._%+-".contains(&c));
    let valid_host = host.rsplit_once('.').is_some_and(|(prefix, suffix)| {
        !prefix.is_empty()
            && prefix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-".contains(&c))
            && suffix.len() >= 2
            && suffix.bytes().all(|c| c.is_ascii_alphabetic())
    });
    if !valid_local || !valid_host {
        return Err("Invalid email format");
    }
    Ok(raw.to_lowercase())
}
pub fn validate_model(
    model: &str,
    input: &Value,
    domain: &str,
) -> Result<Value, ServiceConfigError> {
    let Some(object) = input.as_object() else {
        let kind = if input.is_null() {
            "missing"
        } else {
            "model_attributes_type"
        };
        return Err(ServiceConfigError::validation(vec![validation(
            kind,
            vec![json!("body")],
            input,
            if input.is_null() {
                "Field required"
            } else {
                "Input should be a valid dictionary or object to extract fields from"
            },
            None,
        )]));
    };
    let fields = match model {
        "UserCreate" => contract::CREATE_FIELDS,
        "ProfileUpdate" => contract::PROFILE_FIELDS,
        _ => contract::UPDATE_FIELDS,
    };
    let mut out = Map::new();
    let mut errors = vec![];
    for field in fields {
        let required = model == "UserCreate" && ["email", "display_name"].contains(field);
        let value = match object.get(*field) {
            Some(value) => value,
            None if required => {
                errors.push(field_error("missing", field, input, "Field required", None));
                continue;
            }
            None => {
                if model == "UserCreate" {
                    out.insert(
                        (*field).into(),
                        if *field == "roles" {
                            json!(["user"])
                        } else {
                            Value::Null
                        },
                    );
                }
                continue;
            }
        };
        let nullable = !required && !(model == "UserCreate" && *field == "roles");
        if value.is_null() && nullable {
            out.insert((*field).into(), Value::Null);
            continue;
        }
        if [
            "roles",
            "extra_permissions",
            "allowed_services",
            "denied_services",
        ]
        .contains(field)
        {
            match value.as_array() {
                None => errors.push(field_error(
                    "list_type",
                    field,
                    value,
                    "Input should be a valid list",
                    None,
                )),
                Some(array) => {
                    for (i, v) in array.iter().enumerate() {
                        if !v.is_string() {
                            errors.push(validation(
                                "string_type",
                                vec![json!("body"), json!(field), json!(i)],
                                v,
                                "Input should be a valid string",
                                None,
                            ));
                        }
                    }
                    out.insert((*field).into(), value.clone());
                }
            }
            continue;
        }
        let Some(text) = value.as_str() else {
            errors.push(field_error(
                "string_type",
                field,
                value,
                "Input should be a valid string",
                None,
            ));
            continue;
        };
        let length = text.chars().count();
        if *field == "display_name" && length < 1 {
            errors.push(field_error(
                "string_too_short",
                field,
                value,
                "String should have at least 1 character",
                Some(json!({"min_length":1})),
            ));
            continue;
        }
        let max = match *field {
            "display_name" => Some(contract::DISPLAY_NAME_MAX),
            "department" => Some(contract::DEPARTMENT_MAX),
            _ => None,
        };
        if let Some(max) = max.filter(|max| length > *max) {
            errors.push(field_error(
                "string_too_long",
                field,
                value,
                format!("String should have at most {max} characters"),
                Some(json!({"max_length":max})),
            ));
            continue;
        }
        let mut parsed = text.to_owned();
        if *field == "department" && !contract::DEPARTMENTS.contains(&text) {
            errors.push(value_error(
                field,
                value,
                "Department must be one of: cs, sales, tech, admin",
            ));
            continue;
        }
        if *field == "service_access_mode" {
            parsed = text.trim_matches(python_whitespace).to_lowercase();
            if !contract::SERVICE_ACCESS_MODES.contains(&parsed.as_str()) {
                errors.push(value_error(
                    field,
                    value,
                    "service_access_mode must be one of: all, allowlist",
                ));
                continue;
            }
        }
        if *field == "email" {
            match email(text, domain) {
                Err(message) => {
                    errors.push(value_error(field, value, message));
                    continue;
                }
                Ok(email) => {
                    if !email.ends_with(&format!("@{domain}")) {
                        errors.push(value_error(
                            field,
                            value,
                            &format!("Only @{domain} email addresses are allowed"),
                        ));
                        continue;
                    }
                    parsed = email;
                }
            }
        }
        out.insert((*field).into(), json!(parsed));
    }
    if errors.is_empty() {
        Ok(json!(out))
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}
fn validate_pagination(page: &Value, page_size: &Value) -> Result<(i64, i64), ServiceConfigError> {
    let mut values = vec![];
    let mut errors = vec![];
    for (field, input, default, max) in [
        ("page", page, contract::PAGE_DEFAULT, None),
        ("page_size", page_size, contract::PAGE_SIZE_DEFAULT, Some(contract::PAGE_SIZE_MAX)),
    ] {
        if input.is_null() {
            values.push(default);
            continue;
        }
        match integer(input) {
            Err(kind) => {
                let msg = if kind == "int_parsing" {
                    "Input should be a valid integer, unable to parse string as an integer"
                } else if kind == "int_from_float" {
                    "Input should be a valid integer, got a number with a fractional part"
                } else {
                    "Input should be a valid integer"
                };
                errors.push(validation(
                    kind,
                    vec![json!("query"), json!(field)],
                    input,
                    msg,
                    None,
                ));
            }
            Ok(value) => {
                if value < 1 {
                    errors.push(validation(
                        "greater_than_equal",
                        vec![json!("query"), json!(field)],
                        input,
                        "Input should be greater than or equal to 1",
                        Some(json!({"ge":1})),
                    ));
                } else if max.is_some_and(|max| value > max) {
                    errors.push(validation(
                        "less_than_equal",
                        vec![json!("query"), json!(field)],
                        input,
                        "Input should be less than or equal to 100",
                        Some(json!({"le":100})),
                    ));
                }
                values.push(value);
            }
        }
    }
    if errors.is_empty() {
        Ok((values[0], values[1]))
    } else {
        Err(ServiceConfigError::validation(errors))
    }
}
#[derive(Clone)]
struct HttpState {
    api: Arc<UsersApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(api: Arc<UsersApi>, cancellation: Arc<dyn ServiceConfigCancellation>) -> Router {
    let mut routes = vec![];
    let mut router = Router::new();
    for (_, route, _) in USERS_ROUTES {
        if !routes.contains(route) {
            routes.push(*route);
            router = router.route(&format!("/api/v1{route}"), any(http));
        }
    }
    router
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn response(
    state: &HttpState,
    value: Result<Value, ServiceConfigError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let (status, body) = error.response(context).expect("noncancelled");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
fn routed_operation(
    route: &str, method: &str, user_id: String, uri: &axum::http::Uri,
) -> Result<UsersOperation, Response> {
    Ok(match (route, method) {
        ("/api/v1/users", "GET") => {
            let pairs = match Query::<Vec<(String, String)>>::try_from_uri(uri) {
                Ok(Query(v)) => v,
                Err(e) => return Err(e.into_response()),
            };
            let get = |name: &str| {
                pairs
                    .iter()
                    .rev()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            };
            UsersOperation::List {
                page: get("page").map_or(Value::Null, |v| json!(v)),
                page_size: get("page_size").map_or(Value::Null, |v| json!(v)),
                status: get("status"),
                search: get("search"),
            }
        }
        ("/api/v1/users", _) => UsersOperation::Create,
        ("/api/v1/users/{user_id}", "GET") => UsersOperation::Get { user_id },
        ("/api/v1/users/{user_id}", "PUT") => UsersOperation::Update { user_id },
        ("/api/v1/users/{user_id}", _) => UsersOperation::Delete { user_id },
        ("/api/v1/users/{user_id}/reset-password", _) => UsersOperation::ResetPassword { user_id },
        ("/api/v1/users/{user_id}/enable", _) => UsersOperation::Enable { user_id },
        ("/api/v1/users/{user_id}/disable", _) => UsersOperation::Disable { user_id },
        _ => UsersOperation::Profile { user_id },
    })
}

async fn http(
    State(state): State<HttpState>,
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Response {
    let params = match params {
        Ok(Path(v)) => v,
        Err(e) => return e.into_response(),
    };
    if params.values().any(|v| v.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let route = matched.as_str();
    let method = request.method().as_str();
    let allowed = contract::allowed_methods(route);
    if !allowed.contains(&method) {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let user_id = params.get("user_id").cloned().unwrap_or_default();
    let operation = match routed_operation(route, method, user_id, request.uri()) {
        Ok(operation) => operation,
        Err(response) => return response,
    };
    let headers = request.headers().clone();
    let mut context = request
        .extensions()
        .get::<ConfigAuditRequestContext>()
        .cloned()
        .or_else(|| {
            request
                .extensions()
                .get::<Extension<ConfigAuditRequestContext>>()
                .map(|Extension(v)| v.clone())
        })
        .unwrap_or_else(|| state.api.dependencies.identity.context(&headers));
    context.headers = headers.clone();
    let body = if operation.model().is_some() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(e) => return e.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else if crate::http_content_type::is_json_content_type(&headers) {
            match serde_json::from_slice(&bytes) { Ok(v)=>v,Err(_)=>return response(&state,Err(ServiceConfigError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await }
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    } else {
        Value::Null
    };
    response(
        &state,
        state.api.handle(operation, &headers, &body).await,
        &context,
    )
    .await
}

#[cfg(test)]
#[path = "users_contract_tests.rs"]
mod contract_tests;
