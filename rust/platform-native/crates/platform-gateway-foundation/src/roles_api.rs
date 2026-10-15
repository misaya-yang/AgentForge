//! Role catalog with tenant-scoped membership counts and member lists.
//!
//! Role definitions remain global, so their creation/update/deletion requires a
//! current platform-administrator account. Tenant administrators can assign
//! existing non-platform roles through the guarded users routes. No caller may
//! use role membership reads to enumerate accounts in another tenant.
//! Constructors are inert; cancellation and error redaction are unchanged.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    roles_store::{RolesDatabase, RolesStoreError},
    service_config_api::{
        ServiceConfigCancellation, ServiceConfigError, ServiceConfigIdentity,
        ServiceConfigIdentityResolver,
    },
    service_registry::python_truthy,
    native_contract::roles as contract,
    usage_parser::python_value_to_string,
};
use ai_platform_auth::rbac::Rbac;
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, RawQuery, Request, State},
    http::{HeaderMap, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::{collections::HashSet, sync::Arc};

pub use contract::ROLES_ROUTES;
pub type RolesApiError = ServiceConfigError;
impl From<RolesStoreError> for ServiceConfigError {
    fn from(error: RolesStoreError) -> Self {
        if error == RolesStoreError::Cancelled {
            Self::Cancelled
        } else {
            Self::Internal
        }
    }
}
pub trait RolesClock: Send + Sync {
    fn utc_naive_isoformat(&self) -> Result<String, RolesApiError>;
}
pub struct SystemRolesClock;
impl RolesClock for SystemRolesClock {
    fn utc_naive_isoformat(&self) -> Result<String, RolesApiError> {
        Ok(crate::protocol::ProtocolTimestamp::from(Utc::now().naive_utc()).to_isoformat())
    }
}
pub struct RolesDependencies {
    pub identity: Arc<dyn ServiceConfigIdentityResolver>,
    pub rbac: Rbac,
    pub database: Option<Arc<dyn RolesDatabase>>,
    pub clock: Arc<dyn RolesClock>,
}
pub struct RolesApi {
    dependencies: RolesDependencies,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RolesOperation {
    List,
    Create,
    Permissions { category: Option<String> },
    Get { role_name: String },
    Update { role_name: String },
    Delete { role_name: String },
    Users { role_name: String },
}
impl RolesOperation {
    fn permission(&self) -> &'static str {
        match self {
            Self::Create => "role:create",
            Self::Update { .. } => "role:edit",
            Self::Delete { .. } => "role:delete",
            _ => "role:list",
        }
    }
    fn path(&self) -> String {
        match self {
            Self::List | Self::Create => "/api/v1/roles".into(),
            Self::Permissions { .. } => "/api/v1/roles/permissions".into(),
            Self::Users { role_name } => format!("/api/v1/roles/{role_name}/users"),
            Self::Get { role_name } | Self::Update { role_name } | Self::Delete { role_name } => {
                format!("/api/v1/roles/{role_name}")
            }
        }
    }
    fn takes_body(&self) -> bool {
        matches!(self, Self::Create | Self::Update { .. })
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct RoleCreate {
    pub role_name: String,
    pub description: String,
    pub permissions: Vec<String>,
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct RoleUpdate {
    pub description: Option<String>,
    pub permissions: Option<Vec<String>>,
}
impl RolesApi {
    pub fn new(dependencies: RolesDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        operation: RolesOperation,
        headers: &HeaderMap,
        body: &Value,
        context: &ConfigAuditRequestContext,
    ) -> Result<Value, RolesApiError> {
        let identity = self
            .dependencies
            .identity
            .resolve(headers, &operation.path())
            .await?;
        self.execute(operation, &identity, body, context).await
    }
    pub async fn execute(
        &self,
        operation: RolesOperation,
        identity: &ServiceConfigIdentity,
        body: &Value,
        _context: &ConfigAuditRequestContext,
    ) -> Result<Value, RolesApiError> {
        let create = if operation == RolesOperation::Create {
            Some(validate_role_create(body)?)
        } else {
            None
        };
        let update = if matches!(operation, RolesOperation::Update { .. }) {
            Some(validate_role_update(body)?)
        } else {
            None
        };
        self.dependencies
            .rbac
            .require(&identity.roles, operation.permission())
            .map_err(|message| RolesApiError::Http {
                status: 403,
                body: json!({"error":message}),
            })?;
        if identity.user_id.trim().is_empty() || identity.tenant_id.trim().is_empty() {
            return Err(RolesApiError::detail(403, "Active tenant account required"));
        }
        let mutating = matches!(operation, RolesOperation::Create | RolesOperation::Update { .. } | RolesOperation::Delete { .. });
        if mutating && !identity.roles.iter().any(|role| role == "platform_admin") {
            return Err(RolesApiError::detail(403, "Only platform administrators can change shared roles"));
        }
        let database = self
            .dependencies
            .database
            .as_ref()
            .filter(|db| db.truthy() && db.enabled())
            .ok_or_else(|| RolesApiError::detail(503, "Database not available"))?;
        if mutating && !database.platform_administrator(&identity.user_id, &identity.tenant_id).await? {
            return Err(RolesApiError::detail(403, "Current platform-administrator authority required"));
        }
        match operation {
            RolesOperation::List => {
                let mut roles = vec![];
                for role in database.list_roles().await? {
                    let name = role
                        .get("role_name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let count = database.tenant_role_user_count(name, &identity.tenant_id).await?;
                    roles.push(role_response(&role, count)?);
                }
                Ok(contract::role_list(roles))
            }
            RolesOperation::Create => {
                let body = create.expect("validated create");
                if database
                    .get_role(&body.role_name)
                    .await?
                    .as_ref()
                    .is_some_and(python_truthy)
                {
                    return Err(RolesApiError::detail(400, "Role already exists"));
                }
                validate_permissions(&body.permissions, &database.list_permissions(None).await?)?;
                database
                    .create_role(&body.role_name, Some(&body.description), &body.permissions)
                    .await?;
                Ok(
                    contract::role_value(&body.role_name, Some(&body.description), &body.permissions.iter().map(|p|json!(p)).collect::<Vec<_>>(), false, 0, Some(&self.dependencies.clock.utc_naive_isoformat()?), None),
                )
            }
            RolesOperation::Permissions { category } => {
                let permissions = database
                    .list_permissions(category.as_deref())
                    .await?
                    .iter()
                    .map(permission_response)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(contract::permission_list(permissions))
            }
            RolesOperation::Get { role_name } => {
                let role = require_role(database.as_ref(), &role_name).await?;
                let count = database.tenant_role_user_count(&role_name, &identity.tenant_id).await?;
                role_response(&role, count)
            }
            RolesOperation::Update { role_name } => {
                let role = require_role(database.as_ref(), &role_name).await?;
                if role.get("is_system").is_some_and(python_truthy) {
                    return Err(RolesApiError::detail(400, "Cannot modify system role"));
                }
                let body = update.expect("validated update");
                if let Some(permissions) = &body.permissions {
                    validate_permissions(permissions, &database.list_permissions(None).await?)?;
                }
                database
                    .update_role(
                        &role_name,
                        body.description.as_deref(),
                        body.permissions.as_deref(),
                    )
                    .await?;
                let updated = database.get_role(&role_name).await?;
                let count = database.tenant_role_user_count(&role_name, &identity.tenant_id).await?;
                // A vanished readback is a source ordinary exception, after count.
                role_response(&updated.ok_or(RolesApiError::Internal)?, count)
            }
            RolesOperation::Delete { role_name } => {
                let role = require_role(database.as_ref(), &role_name).await?;
                if role.get("is_system").is_some_and(python_truthy) {
                    return Err(RolesApiError::detail(400, "Cannot delete system role"));
                }
                let count = database.get_role_user_count(&role_name).await?;
                if count > 0 {
                    return Err(RolesApiError::detail(
                        400,
                        format!(
                            "Cannot delete role with {count} assigned user(s). Remove users first."
                        ),
                    ));
                }
                database.delete_role(&role_name).await?;
                Ok(contract::deleted(&role_name))
            }
            RolesOperation::Users { role_name } => {
                require_role(database.as_ref(), &role_name).await?;
                let users = database.tenant_users_by_role(&role_name, &identity.tenant_id).await?;
                Ok(contract::role_users(role_name, users))
            }
        }
    }
}
async fn require_role(database: &dyn RolesDatabase, name: &str) -> Result<Value, RolesApiError> {
    database
        .get_role(name)
        .await?
        .filter(python_truthy)
        .ok_or_else(|| RolesApiError::detail(404, "Role not found"))
}
fn validate_permissions(permissions: &[String], catalog: &[Value]) -> Result<(), RolesApiError> {
    let valid: HashSet<_> = catalog
        .iter()
        .filter_map(|p| p.get("permission_code").and_then(Value::as_str))
        .collect();
    for permission in permissions {
        if permission != "admin:*" && !valid.contains(permission.as_str()) {
            return Err(RolesApiError::detail(
                400,
                format!("Invalid permission: {permission}"),
            ));
        }
    }
    Ok(())
}
fn role_response(role: &Value, count: i64) -> Result<Value, RolesApiError> {
    let mut response = json!({"role_name":"","description":null,"permissions":[],"is_system":false,"user_count":count,"created_at":null,"updated_at":null});
    for field in ["role_name", "description", "permissions", "is_system"] {
        if let Some(value) = role.get(field) {
            response[field] = value.clone();
        }
    }
    if !response["role_name"].is_string()
        || !(response["description"].is_null() || response["description"].is_string())
        || !response["permissions"]
            .as_array()
            .is_some_and(|v| v.iter().all(Value::is_string))
    {
        return Err(RolesApiError::Internal);
    }
    response["is_system"] = json!(
        crate::rate_config_api::boolean(&response["is_system"])
            .map_err(|_| RolesApiError::Internal)?
    );
    for field in ["created_at", "updated_at"] {
        if let Some(value) = role.get(field).filter(|v| python_truthy(v)) {
            response[field] = json!(python_value_to_string(value));
        }
    }
    Ok(contract::role_value(
        response["role_name"].as_str().expect("validated role name"),
        response["description"].as_str(), response["permissions"].as_array().expect("validated permissions"),
        response["is_system"].as_bool().expect("normalized boolean"), count,
        response["created_at"].as_str(),response["updated_at"].as_str(),
    ))
}
fn permission_response(permission: &Value) -> Result<Value, RolesApiError> {
    let mut response = json!({"description":null,"is_system":false});
    for field in ["permission_code", "name", "category", "resource", "action"] {
        let value = permission
            .get(field)
            .filter(|v| v.is_string())
            .ok_or(RolesApiError::Internal)?;
        response[field] = value.clone();
    }
    if let Some(description) = permission.get("description") {
        if !description.is_null() && !description.is_string() {
            return Err(RolesApiError::Internal);
        }
        response["description"] = description.clone();
    }
    if let Some(value) = permission.get("is_system") {
        response["is_system"] =
            json!(crate::rate_config_api::boolean(value).map_err(|_| RolesApiError::Internal)?);
    }
    Ok(contract::permission_value(
        response["permission_code"].as_str().expect("validated permission"), response["name"].as_str().expect("validated name"),
        response["category"].as_str().expect("validated category"), response["resource"].as_str().expect("validated resource"),
        response["action"].as_str().expect("validated action"),response["description"].as_str(),
        response["is_system"].as_bool().expect("normalized boolean"),
    ))
}

fn validation(kind: &str, loc: Vec<Value>, input: &Value, model: &str, bound: usize) -> Value {
    let message = match kind {
        "model_type" => format!("Input should be a valid dictionary or instance of {model}"),
        "model_attributes_type" => {
            "Input should be a valid dictionary or object to extract fields from".into()
        }
        "missing" => "Field required".into(),
        "string_type" => "Input should be a valid string".into(),
        "list_type" => "Input should be a valid list".into(),
        "string_too_short" => format!(
            "String should have at least {bound} character{}",
            if bound == 1 { "" } else { "s" }
        ),
        "string_too_long" => format!("String should have at most {bound} characters"),
        "string_pattern_mismatch" => "String should match pattern '^[a-z_]+$'".into(),
        _ => unreachable!("known role validation"),
    };
    let mut value = json!({"type":kind,"loc":loc,"msg":message,"input":input});
    match kind {
        "model_type" => value["ctx"] = json!({"class_name":model}),
        "string_too_short" => value["ctx"] = json!({"min_length":bound}),
        "string_too_long" => value["ctx"] = json!({"max_length":bound}),
        "string_pattern_mismatch" => value["ctx"] = json!({"pattern":"^[a-z_]+$"}),
        _ => (),
    }
    value
}
fn string_field(
    body: &Value,
    field: &str,
    required: bool,
    max: usize,
    errors: &mut Vec<Value>,
) -> Option<String> {
    let loc = vec![json!("body"), json!(field)];
    let Some(input) = body.get(field) else {
        if required {
            errors.push(validation("missing", loc, body, "", 0));
        }
        return None;
    };
    if !required && input.is_null() {
        return None;
    }
    let Some(value) = input.as_str() else {
        errors.push(validation("string_type", loc, input, "", 0));
        return None;
    };
    let length = value.chars().count();
    let failure = if length == 0 {
        Some(("string_too_short", 1))
    } else if length > max {
        Some(("string_too_long", max))
    } else if field == "role_name" && !value.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        Some(("string_pattern_mismatch", 0))
    } else {
        None
    };
    if let Some((kind, bound)) = failure {
        errors.push(validation(kind, loc, input, "", bound));
    }
    Some(value.to_owned())
}
fn permissions_field(body: &Value, nullable: bool, errors: &mut Vec<Value>) -> Option<Vec<String>> {
    let Some(value) = body.get("permissions") else {
        return if nullable { None } else { Some(vec![]) };
    };
    if nullable && value.is_null() {
        return None;
    }
    let Some(values) = value.as_array() else {
        errors.push(validation(
            "list_type",
            vec![json!("body"), json!("permissions")],
            value,
            "",
            0,
        ));
        return None;
    };
    let mut result = vec![];
    for (index, value) in values.iter().enumerate() {
        if let Some(value) = value.as_str() {
            result.push(value.to_owned());
        } else {
            errors.push(validation(
                "string_type",
                vec![json!("body"), json!("permissions"), json!(index)],
                value,
                "",
                0,
            ));
        }
    }
    Some(result)
}
pub fn validate_role_create(body: &Value) -> Result<RoleCreate, RolesApiError> {
    if !body.is_object() {
        return Err(RolesApiError::validation(vec![validation(
            "model_type",
            vec![json!("body")],
            body,
            "RoleCreate",
            0,
        )]));
    }
    let mut errors = vec![];
    let role_name = string_field(body, "role_name", true, contract::NAME_MAX, &mut errors);
    let description = string_field(body, "description", true, contract::DESCRIPTION_MAX, &mut errors);
    let permissions = permissions_field(body, false, &mut errors);
    if !errors.is_empty() {
        return Err(RolesApiError::validation(errors));
    }
    Ok(RoleCreate {
        role_name: role_name.expect("valid name"),
        description: description.expect("valid description"),
        permissions: permissions.expect("valid permissions"),
    })
}
pub fn validate_role_update(body: &Value) -> Result<RoleUpdate, RolesApiError> {
    if !body.is_object() {
        return Err(RolesApiError::validation(vec![validation(
            "model_type",
            vec![json!("body")],
            body,
            "RoleUpdate",
            0,
        )]));
    }
    let mut errors = vec![];
    let description = string_field(body, "description", false, contract::DESCRIPTION_MAX, &mut errors);
    let permissions = permissions_field(body, true, &mut errors);
    if !errors.is_empty() {
        return Err(RolesApiError::validation(errors));
    }
    Ok(RoleUpdate {
        description,
        permissions,
    })
}
#[derive(Clone)]
struct HttpState {
    api: Arc<RolesApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(api: Arc<RolesApi>, cancellation: Arc<dyn ServiceConfigCancellation>) -> Router {
    let mut router = Router::new();
    let paths: std::collections::BTreeSet<_> = ROLES_ROUTES.iter().map(|(_, path, _)| *path).collect();
    for path in paths {
        router = router.route(&format!("/api/v1{path}"), any(http_dispatch));
    }
    router
        .route_layer(middleware::from_fn(route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn route_guard(request: Request, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_default();
    let methods = contract::allowed_methods(route);
    if methods.contains(&request.method().as_str()) {
        next.run(request).await
    } else {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
async fn result(
    state: &HttpState,
    value: Result<Value, RolesApiError>,
    context: &ConfigAuditRequestContext,
) -> Response {
    match value {
        Ok(value) => Json(value).into_response(),
        Err(RolesApiError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let (status, body) = error.response(context).expect("ordinary error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn http_dispatch(
    State(state): State<HttpState>,
    trusted: Option<Extension<ConfigAuditRequestContext>>,
    matched: MatchedPath,
    path: Option<Path<String>>,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
    request: Request,
) -> Response {
    let mut context = trusted
        .map(|Extension(v)| v)
        .unwrap_or_else(|| state.api.dependencies.identity.context(&headers));
    context.headers = headers.clone();
    let route = matched.as_str();
    let name = path
        .map(|Path(v)| v)
        .unwrap_or_else(|| "permissions".into());
    let operation = if route == "/api/v1/roles" {
        if method == Method::POST {
            RolesOperation::Create
        } else {
            RolesOperation::List
        }
    } else if route.ends_with("/users") {
        RolesOperation::Users { role_name: name }
    } else if route.ends_with("/permissions") && method == Method::GET {
        let category = query
            .as_ref()
            .and_then(|q| reqwest::Url::parse(&format!("http://roles.invalid/?{q}")).ok())
            .and_then(|url| {
                url.query_pairs()
                    .filter(|(key, _)| key == "category")
                    .map(|(_, v)| v.into_owned())
                    .last()
            });
        RolesOperation::Permissions { category }
    } else if method == Method::PUT {
        RolesOperation::Update { role_name: name }
    } else if method == Method::DELETE {
        RolesOperation::Delete { role_name: name }
    } else {
        RolesOperation::Get { role_name: name }
    };
    let bytes = if operation.takes_body() {
        match Bytes::from_request(request, &state).await {
            Ok(bytes) => bytes,
            Err(error) => return error.into_response(),
        }
    } else {
        Bytes::new()
    };
    let body = if !operation.takes_body() || bytes.is_empty() {
        Value::Null
    } else if crate::http_content_type::is_json_content_type(&headers) {
        match serde_json::from_slice(&bytes) { Ok(value) => value, Err(_) => return result(&state,Err(RolesApiError::validation(vec![json!({"type":"json_invalid","loc":["body",0],"msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])),&context).await }
    } else {
        json!(String::from_utf8_lossy(&bytes))
    };
    let identity = match state
        .api
        .dependencies
        .identity
        .resolve(&headers, &operation.path())
        .await
    {
        Ok(value) => value,
        Err(error) => return result(&state, Err(error), &context).await,
    };
    if operation.takes_body() && !body.is_object() {
        return result(
            &state,
            Err(RolesApiError::validation(vec![validation(
                if body.is_null() {
                    "missing"
                } else {
                    "model_attributes_type"
                },
                vec![json!("body")],
                &body,
                "",
                0,
            )])),
            &context,
        )
        .await;
    }
    result(
        &state,
        state
            .api
            .execute(operation, &identity, &body, &context)
            .await,
        &context,
    )
    .await
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use contract::*;
    #[test]
    fn runtime_request_validation_matches_shared_bounds_and_nullability() {
        let valid=json!({"role_name":"editor","description":"d","ignored":true});
        assert_eq!(validate_role_create(&valid).unwrap().permissions,Vec::<String>::new());
        for field in ["role_name","description"] { let mut body=valid.clone(); body.as_object_mut().unwrap().remove(field); assert!(validate_role_create(&body).is_err()); }
        for value in [json!(null),json!(""),json!("A"),json!("a".repeat(NAME_MAX+1))] { let mut body=valid.clone(); body["role_name"]=value; assert!(validate_role_create(&body).is_err()); }
        for length in [1,DESCRIPTION_MAX] { let mut body=valid.clone(); body["description"]=json!("界".repeat(length)); assert!(validate_role_create(&body).is_ok()); }
        let mut body=valid.clone(); body["description"]=json!("界".repeat(DESCRIPTION_MAX+1)); assert!(validate_role_create(&body).is_err());
        let mut body=valid; body["permissions"]=Value::Null; assert!(validate_role_create(&body).is_err());
        assert!(validate_role_update(&json!({"description":null,"permissions":null,"ignored":true})).is_ok());
        assert!(validate_role_update(&json!({"description":""})).is_err());
        assert_eq!(create_schema()["properties"]["role_name"]["maxLength"],NAME_MAX);
        assert_eq!(update_schema()["properties"]["description"]["maxLength"],DESCRIPTION_MAX);
    }
    fn assert_fields(value: &Value,schema: &Value) {
        let fields: std::collections::BTreeSet<_>=value.as_object().unwrap().keys().collect();
        let declared: std::collections::BTreeSet<_>=schema["properties"].as_object().unwrap().keys().collect();
        assert_eq!(fields,declared);
    }    #[test]
    fn runtime_response_builders_match_owned_shapes() {
        let role=role_response(&json!({"role_name":"r","is_system":1}),3).unwrap();
        assert_fields(&role,&role_schema()); assert_eq!(role["is_system"],true);
        assert!(role_response(&json!({"permissions":[1]}),0).is_err());
        let permission=permission_response(&json!({"permission_code":"r:list","name":"Read","category":"r","resource":"r","action":"list"})).unwrap();
        assert_fields(&permission,&permission_schema()); assert!(permission_response(&json!({})).is_err());
        for (value,id) in [(role_list(vec![role]),"list_roles"),(permission_list(vec![permission]),"list_permissions"),(role_users("r".into(),vec![]),"get_role_users"),(deleted("r"),"delete_role")] { assert_fields(&value,&success_schema(id)); }
    }    struct TestIdentity;
    impl ServiceConfigIdentityResolver for TestIdentity {
        fn resolve<'a>(&'a self, _headers: &'a HeaderMap, path: &'a str) -> crate::service_config_api::ServiceConfigFuture<'a, ServiceConfigIdentity> {
            Box::pin(async move {
                assert!(path.starts_with("/api/v1/roles"));
                Ok(ServiceConfigIdentity { user_id:"user".into(), tenant_id:"tenant".into(), roles:vec!["platform_admin".into()], permissions:vec![] })
            })
        }
    }
    struct TestDatabase;
    impl RolesDatabase for TestDatabase {
        fn enabled(&self)->bool { true }
        fn list_roles(&self)->crate::roles_store::RolesStoreFuture<'_,Vec<Value>> { Box::pin(async { Ok(vec![json!({"role_name":"reader"})]) }) }
        fn get_role<'a>(&'a self, name:&'a str)->crate::roles_store::RolesStoreFuture<'a,Option<Value>> { Box::pin(async move { Ok((name!="new_role").then(||json!({"role_name":name,"description":"role"}))) }) }
        fn create_role<'a>(&'a self,_name:&'a str,_description:Option<&'a str>,_permissions:&'a [String])->crate::roles_store::RolesStoreFuture<'a,()> { Box::pin(async { Ok(()) }) }
        fn update_role<'a>(&'a self,_name:&'a str,_description:Option<&'a str>,_permissions:Option<&'a [String]>)->crate::roles_store::RolesStoreFuture<'a,()> { Box::pin(async { Ok(()) }) }
        fn delete_role<'a>(&'a self,_name:&'a str)->crate::roles_store::RolesStoreFuture<'a,bool> { Box::pin(async { Ok(true) }) }
        fn list_permissions<'a>(&'a self,_category:Option<&'a str>)->crate::roles_store::RolesStoreFuture<'a,Vec<Value>> { Box::pin(async { Ok(vec![json!({"permission_code":"role:list","name":"List","category":"role","resource":"role","action":"list"})]) }) }
        fn get_role_user_count<'a>(&'a self,_name:&'a str)->crate::roles_store::RolesStoreFuture<'a,i64> { Box::pin(async { Ok(0) }) }
        fn get_users_by_role<'a>(&'a self,_name:&'a str)->crate::roles_store::RolesStoreFuture<'a,Vec<Value>> { Box::pin(async { panic!("tenant-unscoped member read") }) }
        fn tenant_role_user_count<'a>(&'a self,_name:&'a str,tenant:&'a str)->crate::roles_store::RolesStoreFuture<'a,i64> { Box::pin(async move { assert_eq!(tenant,"tenant"); Ok(0) }) }
        fn tenant_users_by_role<'a>(&'a self,_name:&'a str,tenant:&'a str)->crate::roles_store::RolesStoreFuture<'a,Vec<Value>> { Box::pin(async move { assert_eq!(tenant,"tenant"); Ok(vec![json!({"user_id":"user"})]) }) }
        fn platform_administrator<'a>(&'a self,user:&'a str,tenant:&'a str)->crate::roles_store::RolesStoreFuture<'a,bool> { Box::pin(async move { assert_eq!((user,tenant),("user","tenant")); Ok(true) }) }
    }
    fn test_api(database: Option<Arc<dyn RolesDatabase>>) -> RolesApi {
        RolesApi::new(RolesDependencies { identity:Arc::new(TestIdentity), rbac:Rbac::new(std::collections::HashMap::from([("platform_admin".into(),vec!["admin:*".into()])])), database, clock:Arc::new(SystemRolesClock) })
    }
    #[tokio::test]
    async fn all_seven_production_operations_emit_schema_valid_success() {
        let api=test_api(Some(Arc::new(TestDatabase)));
        let cases = [
            (RolesOperation::List,"list_roles",json!({})),
            (RolesOperation::Create,"create_role",json!({"role_name":"new_role","description":"new"})),
            (RolesOperation::Permissions{category:None},"list_permissions",json!({})),
            (RolesOperation::Get{role_name:"reader".into()},"get_role",json!({})),
            (RolesOperation::Update{role_name:"reader".into()},"update_role",json!({"description":null})),
            (RolesOperation::Delete{role_name:"reader".into()},"delete_role",json!({})),
            (RolesOperation::Users{role_name:"reader".into()},"get_role_users",json!({})),
        ];
        for (operation,id,body) in cases {
            let value=api.handle(operation,&HeaderMap::new(),&body,&ConfigAuditRequestContext::default()).await.unwrap();
            let validator=jsonschema::validator_for(&success_schema(id)).unwrap();
            assert!(validator.is_valid(&value),"{id}: {value}");
        }
    }
    #[tokio::test]
    async fn production_failure_statuses_and_redaction_remain_bounded() {
        let api=test_api(None); let context=ConfigAuditRequestContext::default();
        let error=api.handle(RolesOperation::List,&HeaderMap::new(),&Value::Null,&context).await.unwrap_err();
        assert_eq!(error.response(&context).unwrap(),(503,json!({"detail":"Database not available"})));
        let error=api.handle(RolesOperation::Create,&HeaderMap::new(),&json!({}),&context).await.unwrap_err();
        assert_eq!(error.response(&context).unwrap().0,422);
        let error=api.execute(RolesOperation::List,&ServiceConfigIdentity::default(),&Value::Null,&context).await.unwrap_err();
        assert_eq!(error.response(&context).unwrap().0,403);
        assert_eq!(RolesApiError::Exception("private database detail".into()).response(&context).unwrap(),(500,json!({"error":{"code":2000,"message":"Internal server error"}})));
    }

    struct UnavailableIdentity;
    impl ServiceConfigIdentityResolver for UnavailableIdentity {
        fn resolve<'a>(&'a self, _headers:&'a HeaderMap, _path:&'a str)->crate::service_config_api::ServiceConfigFuture<'a,ServiceConfigIdentity> {
            Box::pin(async {
                // Same trusted error mapping as the production AdmissionService adapter.
                let error=ai_platform_auth::admission::AdmissionError::Unavailable;
                Err(RolesApiError::Http { status:error.status(),body:json!({"error":error.to_string()}) })
            })
        }
    }
    #[tokio::test]
    async fn authority_and_storage_outages_both_match_declared_503() {
        let mut api=test_api(None); let context=ConfigAuditRequestContext::default();
        let doc=crate::native_contract::document();
        let schema=&doc["paths"]["/api/v1/roles"]["get"]["responses"]["503"]["content"]["application/json"]["schema"];
        let validator=jsonschema::validator_for(schema).unwrap();
        let storage=api.handle(RolesOperation::List,&HeaderMap::new(),&Value::Null,&context).await.unwrap_err().response(&context).unwrap();
        assert_eq!(storage.0,503); assert!(validator.is_valid(&storage.1));
        api.dependencies.identity=Arc::new(UnavailableIdentity);
        let authority=api.handle(RolesOperation::List,&HeaderMap::new(),&Value::Null,&context).await.unwrap_err().response(&context).unwrap();
        assert_eq!(authority.0,503); assert!(validator.is_valid(&authority.1));
        assert!(authority.1.get("error").is_some());
    }

}
