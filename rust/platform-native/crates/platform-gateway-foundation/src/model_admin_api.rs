//! Opt-in administration handlers from `src/api/v1/models.py`.
//!
//! Nothing is mounted by default. Construction retains collaborators only: no
//! connection, environment/credential read, task, or provider request is started.
//! The host MUST supply a current, trusted `get_user_context` equivalent. Identity
//! and tenant fields in request bodies/queries are ignored, never authenticated.
//! All source handlers are implemented by `handle`; `router` is an optional HTTP
//! binding. It must not be merged with the existing read-only `/models` router.
//!
//! Writes are sequential: model service (including its pricing work), audit,
//! optional invalidation publication, then response validation. Audit failures
//! propagate after a possibly committed write; ordinary publish failures do not.
//! Dropping a future cancels the current awaited collaborator, never starts later
//! steps, and does not claim rollback of a mutation already accepted by storage.
//!
//! Boundaries: the host resolves trusted peer/request state and all authentication
//! modes. JSON integers are i64 and decimals use the existing 96-bit Decimal;
//! arbitrary Python numbers/objects and nonfinite JSON are unsupported. Native
//! ModelService preserves a safe UniqueConflict classification but collapses
//! other database and capability diagnostics; its bridge cannot recover Python
//! capability validation text.
//! Other injected services can supply typed DuplicateKey/Capability errors. Raw
//! database/upstream exception text is deliberately never stored/logged/returned:
//! create's Python raw 500 detail is replaced by the native sanitized envelope.
//! Response datetimes use ISO text supplied by the database adapter. Wire parser
//! limits and malformed JSON diagnostics belong to the explicit HTTP boundary.
//! HEAD and method failures precede collaborators; single-segment IDs reject a
//! decoded slash, while adapter IDs intentionally retain their source path capture.
//! Source main.py keeps HTTP/request-validation detail responses and adds code 2000
//! with trace to uncaught failures. Internal native failures use that safe envelope.
//! Trailing-slash redirects require host policy with a trusted origin; no origin
//! is inferred from Host or forwarded headers by this unmounted factory.

use crate::{
    config_audit_writer::{
        ConfigAuditError, ConfigAuditIdentity, ConfigAuditRequestContext, ConfigAuditValue,
        ConfigAuditWriter, ConfigChange,
    },
    model_service::{
        CreateModel, ModelClock, ModelDatabase, ModelFields, ModelObserver, ModelPricingCache,
        ModelRow, ModelService, ModelServiceError, UpdateModel,
    },
};
use ai_platform_auth::rbac::{Capability, Rbac, check_capability, permission_denied_detail};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{MatchedPath, Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{DateTime, Datelike, NaiveDateTime, SecondsFormat, Timelike};
use rust_decimal::Decimal;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub const MODEL_CONFIG_CHANNEL: &str = "gateway:model-config:changed:v1";
pub const MODEL_ADMIN_ROUTES: &[(&str, &str, &str)] = &[
    (
        "GET",
        "/model-capability-adapters",
        "list_capability_adapters",
    ),
    (
        "GET",
        "/model-capability-adapters/{adapter_id:path}/schema",
        "get_capability_adapter_schema",
    ),
    ("GET", "/models", "list_models"),
    ("POST", "/models", "create_model"),
    ("GET", "/models/{model_id}", "get_model"),
    ("PUT", "/models/{model_id}", "update_model"),
    (
        "POST",
        "/models/{model_id}/capabilities/reset",
        "reset_model_capabilities",
    ),
    ("DELETE", "/models/{model_id}", "delete_model"),
    ("PATCH", "/models/{model_id}/toggle", "toggle_model"),
];
pub type AdminFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ModelAdminError>> + Send + 'a>>;
pub type AdminServiceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AdminServiceError>> + Send + 'a>>;

/// Values returned only by trusted authentication, never deserialized from input.
#[derive(Clone, Default)]
pub struct ResolvedModelUser {
    pub user_id: String,
    pub tenant_id: String,
    pub roles: Vec<String>,
    pub is_authenticated: bool,
}
impl ConfigAuditIdentity for ResolvedModelUser {
    fn user_id(&self) -> &str {
        &self.user_id
    }
    fn tenant_id(&self) -> &str {
        &self.tenant_id
    }
}
impl ResolvedModelUser {
    fn tenant(&self) -> &str {
        if self.tenant_id.is_empty() {
            "default"
        } else {
            &self.tenant_id
        }
    }
    fn admin(&self) -> bool {
        self.roles.iter().any(|r| r == "admin")
    }
    fn access(&self) -> &'static str {
        if self.admin() {
            "admin"
        } else if self.roles.iter().any(|r| r == "premium" || r == "vip") {
            "premium"
        } else {
            "public"
        }
    }
}
/// Resolve credentials against current authority. Public headers are evidence for
/// this collaborator to verify, never a direct source of tenant/role authority.
pub trait ModelAdminIdentityResolver: Send + Sync {
    fn resolve<'a>(&'a self, headers: &'a HeaderMap) -> AdminFuture<'a, ResolvedModelUser>;
    /// Host-derived peer, request_id and trace_id. Do not infer trusted peer state
    /// from forwarded headers here; the audit writer applies the configured policy.
    fn audit_request(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
}

/// Only capability messages validated safe by the trusted domain adapter belong
/// here. No arbitrary SQL/provider exception text may enter this type.
#[derive(Clone)]
pub enum AdminServiceError {
    Capability(String),
    RevisionConflict,
    DuplicateKey,
    Unavailable,
    Cancelled,
}
impl std::fmt::Debug for AdminServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Capability(_) => "Capability",
            Self::RevisionConflict => "RevisionConflict",
            Self::DuplicateKey => "DuplicateKey",
            Self::Unavailable => "Unavailable",
            Self::Cancelled => "Cancelled",
        })
    }
}
impl From<ModelServiceError> for AdminServiceError {
    fn from(error: ModelServiceError) -> Self {
        match error {
            ModelServiceError::CapabilityRevisionConflict => Self::RevisionConflict,
            ModelServiceError::UniqueConflict => Self::DuplicateKey,
            ModelServiceError::Capability => {
                Self::Capability("invalid_model_capability_profile".into())
            }
            _ => Self::Unavailable,
        }
    }
}
pub trait ModelAdminService: Send + Sync {
    fn list<'a>(
        &'a self,
        tenant: &'a str,
        provider: Option<&'a str>,
        disabled: bool,
        access: Option<&'a str>,
    ) -> AdminServiceFuture<'a, Vec<ModelRow>>;
    fn get<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
    ) -> AdminServiceFuture<'a, Option<ModelRow>>;
    fn create<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: &'a str,
        data: CreateModel,
    ) -> AdminServiceFuture<'a, ModelRow>;
    fn update<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
        data: UpdateModel,
    ) -> AdminServiceFuture<'a, Option<ModelRow>>;
    fn reset<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
        revision: Option<i64>,
    ) -> AdminServiceFuture<'a, Option<ModelRow>>;
    fn delete<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        provider: Option<&'a str>,
    ) -> AdminServiceFuture<'a, bool>;
    fn toggle<'a>(
        &'a self,
        tenant: &'a str,
        model: &'a str,
        enabled: bool,
        provider: Option<&'a str>,
    ) -> AdminServiceFuture<'a, Option<ModelRow>>;
}
impl<D: ModelDatabase, P: ModelPricingCache, C: ModelClock, O: ModelObserver> ModelAdminService
    for ModelService<D, P, C, O>
{
    fn list<'a>(
        &'a self,
        t: &'a str,
        p: Option<&'a str>,
        d: bool,
        a: Option<&'a str>,
    ) -> AdminServiceFuture<'a, Vec<ModelRow>> {
        Box::pin(async move { self.list_models(t, p, d, a).await.map_err(Into::into) })
    }
    fn get<'a>(&'a self, t: &'a str, m: &'a str) -> AdminServiceFuture<'a, Option<ModelRow>> {
        Box::pin(async move { self.get_model(t, m, None).await.map_err(Into::into) })
    }
    fn create<'a>(
        &'a self,
        t: &'a str,
        m: &'a str,
        p: &'a str,
        d: CreateModel,
    ) -> AdminServiceFuture<'a, ModelRow> {
        Box::pin(async move { self.create_model(t, m, p, d).await.map_err(Into::into) })
    }
    fn update<'a>(
        &'a self,
        t: &'a str,
        m: &'a str,
        p: Option<&'a str>,
        d: UpdateModel,
    ) -> AdminServiceFuture<'a, Option<ModelRow>> {
        Box::pin(async move { self.update_model(t, m, p, d).await.map_err(Into::into) })
    }
    fn reset<'a>(
        &'a self,
        t: &'a str,
        m: &'a str,
        p: Option<&'a str>,
        r: Option<i64>,
    ) -> AdminServiceFuture<'a, Option<ModelRow>> {
        Box::pin(async move {
            self.reset_model_capabilities(t, m, p, r)
                .await
                .map_err(Into::into)
        })
    }
    fn delete<'a>(
        &'a self,
        t: &'a str,
        m: &'a str,
        p: Option<&'a str>,
    ) -> AdminServiceFuture<'a, bool> {
        Box::pin(async move { self.delete_model(t, m, p).await.map_err(Into::into) })
    }
    fn toggle<'a>(
        &'a self,
        t: &'a str,
        m: &'a str,
        e: bool,
        p: Option<&'a str>,
    ) -> AdminServiceFuture<'a, Option<ModelRow>> {
        Box::pin(async move { self.toggle_model(t, m, e, p).await.map_err(Into::into) })
    }
}
/// Adapts the shared writer while permitting a host to retain source exception
/// classes. The native writer uses deliberately sanitized transport errors.
pub trait ModelAdminAudit: Send + Sync {
    fn record<'a>(
        &'a self,
        request: &'a ConfigAuditRequestContext,
        user: &'a ResolvedModelUser,
        change: ConfigChange<'a>,
    ) -> AdminServiceFuture<'a, ()>;
}
impl ModelAdminAudit for ConfigAuditWriter {
    fn record<'a>(
        &'a self,
        request: &'a ConfigAuditRequestContext,
        user: &'a ResolvedModelUser,
        change: ConfigChange<'a>,
    ) -> AdminServiceFuture<'a, ()> {
        Box::pin(async move {
            self.record_config_change(request, user, change)
                .await
                .map_err(|error| {
                    if error == ConfigAuditError::Cancelled {
                        AdminServiceError::Cancelled
                    } else {
                        AdminServiceError::Unavailable
                    }
                })
        })
    }
}
pub trait ModelConfigPublisher: Send + Sync {
    /// An ordinary error is ignored. Cancellation is not an error and must drop
    /// this future, rather than detach publication or continue response work.
    fn prepare(&self) -> Result<bool, AdminServiceError> {
        Ok(true)
    }
    fn publish<'a>(&'a self, channel: &'a str, payload: &'a str) -> AdminServiceFuture<'a, ()>;
}
#[derive(Clone)]
pub enum ModelAdminError {
    Http { status: u16, body: Value },
    Cancelled,
}
impl std::fmt::Debug for ModelAdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Http { status, .. } => write!(f, "ModelAdminError({status})"),
            Self::Cancelled => f.write_str("ModelAdminError::Cancelled"),
        }
    }
}
impl ModelAdminError {
    pub fn detail(status: u16, detail: impl Into<Value>) -> Self {
        Self::Http {
            status,
            body: json!({"detail":detail.into()}),
        }
    }
    fn internal() -> Self {
        Self::Http {
            status: 500,
            body: json!({"error":{"code":2000,"message":"Internal server error"}}),
        }
    }
    fn validation(errors: Vec<Value>) -> Self {
        Self::detail(422, Value::Array(errors))
    }
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. } => Some(*status),
            Self::Cancelled => None,
        }
    }
    pub fn body(&self) -> Option<&Value> {
        match self {
            Self::Http { body, .. } => Some(body),
            Self::Cancelled => None,
        }
    }
}
#[derive(Clone, Debug)]
pub enum ModelAdminOperation {
    ListAdapters,
    AdapterSchema(String),
    List,
    Create,
    Get(String),
    Update(String),
    Reset(String),
    Delete(String),
    Toggle(String),
}
#[derive(Clone)]
pub struct ModelAdminRequest {
    pub operation: ModelAdminOperation,
    pub headers: HeaderMap,
    /// Last value wins, matching Starlette QueryParams. Only declared keys count.
    pub query: HashMap<String, String>,
    pub body: Value,
    /// None reproduces direct Python invocation with request=None. HTTP binding
    /// always supplies host-derived request context, including the reset endpoint.
    pub audit_request: Option<ConfigAuditRequestContext>,
}
#[derive(Clone)]
pub struct ModelAdminApi {
    identity: Arc<dyn ModelAdminIdentityResolver>,
    service: Option<Arc<dyn ModelAdminService>>,
    audit: Arc<dyn ModelAdminAudit>,
    publisher: Option<Arc<dyn ModelConfigPublisher>>,
}
impl ModelAdminApi {
    pub fn new(
        identity: Arc<dyn ModelAdminIdentityResolver>,
        service: Option<Arc<dyn ModelAdminService>>,
        audit: Arc<dyn ModelAdminAudit>,
        publisher: Option<Arc<dyn ModelConfigPublisher>>,
    ) -> Self {
        Self {
            identity,
            service,
            audit,
            publisher,
        }
    }
    fn service(&self) -> Result<&dyn ModelAdminService, ModelAdminError> {
        self.service
            .as_deref()
            .ok_or_else(|| ModelAdminError::detail(503, "Model service is not initialized."))
    }
    fn require_write(user: &ResolvedModelUser) -> Result<(), ModelAdminError> {
        let rbac = Rbac::new(HashMap::from([("admin".into(), vec!["admin:*".into()])]));
        let capability = Capability::GatewayModelConfigWrite;
        {
            use ai_platform_auth::cedar::resources::{
                self, ObjectAction, ObjectProfile, ObjectResource, ObjectSubject, ResourceKind,
            };
            let subject = ObjectSubject {
                tenant_id: user.tenant_id.clone(),
                user_id: user.user_id.clone(),
                roles: user.roles.clone(),
                anonymous: !user.is_authenticated,
                ..Default::default()
            };
            let mut resource =
                ObjectResource::new(ResourceKind::Model, "model-catalog", &user.tenant_id);
            resource.profile = ObjectProfile::ModelAdministration;
            resource.required_permissions = capability
                .accepted_permissions()
                .iter()
                .map(|v| (*v).into())
                .collect();
            let legacy = check_capability(&rbac, &user.roles, &[], capability).allowed();
            if legacy
                && !resources::compare_current(&subject, &resource, ObjectAction::Admin, legacy)
            {
                return Err(ModelAdminError::detail(
                    403,
                    "Model resource authorization denied",
                ));
            }
            if !legacy {
                let _ = resources::compare_current(&subject, &resource, ObjectAction::Admin, false);
            }
        }
        if check_capability(&rbac, &user.roles, &[], capability).allowed() {
            Ok(())
        } else {
            Err(ModelAdminError::detail(
                403,
                permission_denied_detail(capability, "", None),
            ))
        }
    }
    #[allow(clippy::too_many_arguments)] // Mirrors source audit and invalidation arguments.
    async fn changed(
        &self,
        request: Option<&ConfigAuditRequestContext>,
        user: &ResolvedModelUser,
        model: &str,
        provider: Option<&str>,
        action: &str,
        before: Option<ConfigAuditValue>,
        after: Option<ConfigAuditValue>,
        event_model: Option<&str>,
        event_row: Option<&ModelRow>,
    ) -> Result<(), ModelAdminError> {
        let Some(request) = request else {
            return Ok(());
        };
        self.audit
            .record(
                request,
                user,
                ConfigChange {
                    resource_type: "model",
                    resource_id: model,
                    action,
                    before: before.as_ref(),
                    after: after.as_ref(),
                },
            )
            .await
            .map_err(|error| effect_error(action, error))?;
        // Source evaluates event arguments after audit, even with no Redis client.
        let event_model = match event_model {
            Some(model) => model.to_owned(),
            None => event_row
                .and_then(|row| row.get("model_id"))
                .map(crate::usage_parser::python_value_to_string)
                .ok_or_else(ModelAdminError::internal)?,
        };
        if let Some(publisher) = &self.publisher {
            if !publisher
                .prepare()
                .map_err(|error| effect_error(action, error))?
            {
                return Ok(());
            }
            let payload = model_event_payload(user.tenant(), &event_model, provider);
            if matches!(
                publisher.publish(MODEL_CONFIG_CHANNEL, &payload).await,
                Err(AdminServiceError::Cancelled)
            ) {
                return Err(ModelAdminError::Cancelled);
            }
        }
        Ok(())
    }
    pub async fn handle(&self, request: ModelAdminRequest) -> Result<Value, ModelAdminError> {
        // FastAPI solves get_model_service before get_user_context, then validates
        // decoded body/query values. Adapter catalog routes have no model service
        // dependency. JSON syntax errors are handled before either dependency.
        let service = if matches!(
            &request.operation,
            ModelAdminOperation::ListAdapters | ModelAdminOperation::AdapterSchema(_)
        ) {
            None
        } else {
            Some(self.service()?)
        };
        let user = self.identity.resolve(&request.headers).await?;
        let provider = request.query.get("provider_id").map(String::as_str);
        match &request.operation {
            ModelAdminOperation::ListAdapters => return Ok(adapter_catalog()),
            ModelAdminOperation::AdapterSchema(id) => {
                return adapter_catalog()
                    .as_array()
                    .and_then(|values| {
                        values
                            .iter()
                            .find(|value| value["id"].as_str() == Some(id))
                            .cloned()
                    })
                    .ok_or_else(|| ModelAdminError::detail(404, "Capability adapter not found"));
            }
            _ => (),
        }
        let service = service.expect("model route dependency resolved before identity");
        match &request.operation {
            ModelAdminOperation::List => {
                let include =
                    query_bool(&request.query, "include_disabled", Some(false))? && user.admin();
                let rows = service
                    .list(
                        user.tenant(),
                        provider,
                        include,
                        if include { None } else { Some(user.access()) },
                    )
                    .await
                    .map_err(internal_service_error)?;
                rows.into_iter()
                    .map(|row| model_response(row, !user.admin()))
                    .collect::<Result<Vec<_>, _>>()
                    .map(Value::Array)
            }
            ModelAdminOperation::Get(model) => {
                let row = service
                    .get(user.tenant(), model)
                    .await
                    .map_err(internal_service_error)?
                    .filter(|row| !row.is_empty())
                    .ok_or_else(not_found)?;
                model_response(row, !user.admin())
            }
            ModelAdminOperation::Create => {
                let body = validate_create(&request.body)?;
                Self::require_write(&user)?;
                let row = service
                    .create(user.tenant(), &body.model_id, &body.provider_id, body.data)
                    .await
                    .map_err(create_error)?;
                self.changed(
                    request.audit_request.as_ref(),
                    &user,
                    &body.model_id,
                    Some(&body.provider_id),
                    "create",
                    None,
                    Some(body.audit),
                    Some(&body.model_id),
                    None,
                )
                .await?;
                model_response(row, false)
            }
            ModelAdminOperation::Update(model) => {
                let mut body = validate_update(&request.body)?;
                Self::require_write(&user)?;
                if body.data.new_model_id.as_deref() == Some(model) {
                    body.data.new_model_id = None;
                }
                let row = service
                    .update(user.tenant(), model, provider, body.data)
                    .await
                    .map_err(update_error)?
                    .filter(|row| !row.is_empty())
                    .ok_or_else(not_found)?;
                let event_provider = event_provider(provider, &row);
                self.changed(
                    request.audit_request.as_ref(),
                    &user,
                    model,
                    event_provider.as_deref(),
                    "update",
                    Some(json!({"model_id":model,"provider_id":provider}).into()),
                    Some(body.audit),
                    None,
                    Some(&row),
                )
                .await?;
                model_response(row, false)
            }
            ModelAdminOperation::Reset(model) => {
                let revision = query_revision(&request.query)?;
                Self::require_write(&user)?;
                let row = service
                    .reset(user.tenant(), model, provider, revision)
                    .await
                    .map_err(reset_error)?
                    .ok_or_else(not_found)?;
                let revision_after = row
                    .get("capability_revision")
                    .ok_or_else(ModelAdminError::internal)?;
                let event_provider = event_provider(provider, &row);
                let context = request
                    .audit_request
                    .as_ref()
                    .ok_or_else(ModelAdminError::internal)?;
                self.changed(
                    Some(context),
                    &user,
                    model,
                    event_provider.as_deref(),
                    "capabilities_reset",
                    Some(json!({"capability_revision":revision}).into()),
                    Some(json!({"capability_revision":revision_after}).into()),
                    Some(model),
                    None,
                )
                .await?;
                model_response(row, false)
            }
            ModelAdminOperation::Delete(model) => {
                Self::require_write(&user)?;
                if !service
                    .delete(user.tenant(), model, provider)
                    .await
                    .map_err(internal_service_error)?
                {
                    return Err(not_found());
                }
                self.changed(
                    request.audit_request.as_ref(),
                    &user,
                    model,
                    provider,
                    "delete",
                    Some(json!({"model_id":model,"provider_id":provider}).into()),
                    None,
                    Some(model),
                    None,
                )
                .await?;
                Ok(json!({"model_id":model,"status":"deleted"}))
            }
            ModelAdminOperation::Toggle(model) => {
                let enabled = query_bool(&request.query, "is_enabled", None)?;
                Self::require_write(&user)?;
                let row = service
                    .toggle(user.tenant(), model, enabled, provider)
                    .await
                    .map_err(internal_service_error)?
                    .filter(|row| !row.is_empty())
                    .ok_or_else(not_found)?;
                let event_provider = event_provider(provider, &row);
                self.changed(
                    request.audit_request.as_ref(),
                    &user,
                    model,
                    event_provider.as_deref(),
                    "toggle",
                    Some(json!({"model_id":model,"provider_id":provider}).into()),
                    Some(json!({"is_enabled":enabled}).into()),
                    Some(model),
                    None,
                )
                .await?;
                // Source has no response_model here: keep pricing_snapshot and
                // service additions, unlike create/get/list/update/reset.
                Ok(Value::Object(row))
            }
            ModelAdminOperation::ListAdapters | ModelAdminOperation::AdapterSchema(_) => {
                unreachable!()
            }
        }
    }
}
fn not_found() -> ModelAdminError {
    ModelAdminError::detail(404, "Model not found")
}
fn internal_service_error(error: AdminServiceError) -> ModelAdminError {
    match error {
        AdminServiceError::Cancelled => ModelAdminError::Cancelled,
        _ => ModelAdminError::internal(),
    }
}
fn effect_error(action: &str, error: AdminServiceError) -> ModelAdminError {
    if action == "create" {
        create_error(error)
    } else {
        internal_service_error(error)
    }
}
fn create_error(error: AdminServiceError) -> ModelAdminError {
    match error {
        AdminServiceError::Capability(detail) => ModelAdminError::detail(422, detail),
        AdminServiceError::DuplicateKey => ModelAdminError::detail(409, "Model already exists"),
        error => internal_service_error(error),
    }
}
fn update_error(error: AdminServiceError) -> ModelAdminError {
    match error {
        AdminServiceError::RevisionConflict => {
            ModelAdminError::detail(409, "Model capability profile was updated")
        }
        AdminServiceError::Capability(detail) => ModelAdminError::detail(422, detail),
        error => internal_service_error(error),
    }
}
fn reset_error(error: AdminServiceError) -> ModelAdminError {
    match error {
        AdminServiceError::RevisionConflict => {
            ModelAdminError::detail(409, "Model capability profile was updated")
        }
        error => internal_service_error(error),
    }
}
fn adapter_catalog() -> Value {
    serde_json::from_str(include_str!("../data/capability-adapters.json"))
        .expect("bundled adapter catalog")
}
fn event_provider(provider: Option<&str>, row: &ModelRow) -> Option<String> {
    provider
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            row.get("provider_id")
                .filter(|value| crate::service_registry::python_truthy(value))
                .map(crate::usage_parser::python_value_to_string)
                .filter(|value| !value.is_empty())
        })
}
fn ascii_json_string(value: &str) -> String {
    let mut result = String::from("\"");
    for character in value.chars() {
        if character.is_ascii() && character != '\u{7f}' {
            let encoded = serde_json::to_string(&character.to_string()).expect("JSON string");
            result.push_str(&encoded[1..encoded.len() - 1]);
        } else {
            let mut units = [0u16; 2];
            for unit in character.encode_utf16(&mut units) {
                result.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    result.push('"');
    result
}
pub fn model_event_payload(tenant: &str, model: &str, provider: Option<&str>) -> String {
    format!(
        "{{\"tenant_id\":{},\"model_id\":{},\"provider_id\":{}}}",
        ascii_json_string(tenant),
        ascii_json_string(model),
        provider
            .map(ascii_json_string)
            .unwrap_or_else(|| "null".into())
    )
}

// Pydantic's non-strict JSON validation within the native numeric boundary.
#[derive(Clone, Copy)]
enum FieldKind {
    Text(Option<usize>),
    Integer(Option<i64>),
    Bool,
    Price,
    Object,
}
const FIELDS: &[(&str, FieldKind)] = &[
    ("model_id", FieldKind::Text(Some(100))),
    ("provider_id", FieldKind::Text(Some(50))),
    ("display_name", FieldKind::Text(Some(100))),
    ("context_window", FieldKind::Integer(Some(1))),
    ("max_output_tokens", FieldKind::Integer(Some(1))),
    ("supports_vision", FieldKind::Bool),
    ("supports_tools", FieldKind::Bool),
    ("input_price_per_1k", FieldKind::Price),
    ("output_price_per_1k", FieldKind::Price),
    ("access_level", FieldKind::Text(None)),
    ("is_enabled", FieldKind::Bool),
    ("sort_order", FieldKind::Integer(None)),
    ("capability_overrides", FieldKind::Object),
];
fn issue(
    location: &str,
    field: Option<&str>,
    input: Value,
    kind: &str,
    message: &str,
    context: Option<Value>,
) -> Value {
    let mut error = json!({"type":kind,"loc":if let Some(field)=field {json!([location,field])} else {json!([location])},"msg":message,"input":input});
    if let Some(context) = context {
        error["ctx"] = context;
    }
    error
}
fn parse_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(value) => Some(i64::from(*value)),
        Value::Number(value) => value.as_i64().or_else(|| {
            value
                .as_f64()
                .filter(|value| {
                    value.fract() == 0.0 && *value >= i64::MIN as f64 && *value < -(i64::MIN as f64)
                })
                .map(|value| value as i64)
        }),
        Value::String(value) => {
            let raw = value.trim();
            let (negative, raw) = if let Some(v) = raw.strip_prefix('-') {
                (true, v)
            } else {
                (false, raw.strip_prefix('+').unwrap_or(raw))
            };
            let (digits, fraction) = raw
                .split_once('.')
                .map_or((raw, None), |(a, b)| (a, Some(b)));
            if digits.is_empty()
                || fraction.is_some_and(|v| v.is_empty() || !v.bytes().all(|b| b == b'0'))
            {
                return None;
            }
            let bytes = digits.as_bytes();
            let mut normalized = String::new();
            if negative {
                normalized.push('-');
            }
            for (i, b) in bytes.iter().copied().enumerate() {
                if b == b'_' {
                    if i == 0
                        || !bytes[i - 1].is_ascii_digit()
                        || !bytes.get(i + 1).is_some_and(u8::is_ascii_digit)
                    {
                        return None;
                    }
                } else if b.is_ascii_digit() {
                    normalized.push(char::from(b));
                } else {
                    return None;
                }
            }
            normalized.parse().ok()
        }
        _ => None,
    }
}
fn parse_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => match value.as_f64() {
            Some(0.0) => Some(false),
            Some(1.0) => Some(true),
            _ => None,
        },
        Value::String(value) => match value.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    }
}
fn decimal_input_text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(value) => value
            .trim_matches(crate::usage_parser::python_whitespace)
            .to_owned(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    Some(
        text.chars()
            .filter(|character| *character != '_')
            .map(|character| {
                crate::usage_parser::decimal_digit(character)
                    .map(|digit| char::from(b'0' + digit))
                    .unwrap_or(character)
            })
            .collect(),
    )
}
fn parse_decimal(value: &Value) -> Option<Decimal> {
    let text = decimal_input_text(value)?;
    // Expand exponents before exact parsing: Decimal::from_str may round, which
    // would silently change a price outside the existing native precision limit.
    let Some(index) = text.find(['e', 'E']) else {
        return Decimal::from_str_exact(&text).ok();
    };
    let exponent = text[index + 1..].parse::<i64>().ok()?;
    if !(-100..=100).contains(&exponent) {
        return None;
    }
    let base = &text[..index];
    let (sign, base) = if let Some(v) = base.strip_prefix('-') {
        ("-", v)
    } else {
        ("", base.strip_prefix('+').unwrap_or(base))
    };
    let (whole, fraction) = base.split_once('.').unwrap_or((base, ""));
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|value| value.is_ascii_digit()) {
        return None;
    }
    let point = whole.len() as i64 + exponent;
    let expanded = if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point >= digits.len() as i64 {
        format!(
            "{sign}{digits}{}",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let point = point as usize;
        format!("{sign}{}.{}", &digits[..point], &digits[point..])
    };
    Decimal::from_str_exact(&expanded).ok()
}

// Preserve Python Decimal.__str__ exponent, trailing-zero and signed-zero shape.
fn decimal_text(value: &Value) -> Option<String> {
    let raw = decimal_input_text(value)?;
    let (negative, raw) = if let Some(v) = raw.strip_prefix('-') {
        (true, v)
    } else {
        (false, raw.strip_prefix('+').unwrap_or(&raw))
    };
    let (coefficient, exponent) = raw.find(['e', 'E']).map_or(Some((raw, 0i64)), |index| {
        raw[index + 1..]
            .parse::<i64>()
            .ok()
            .map(|exp| (&raw[..index], exp))
    })?;
    let (whole, fraction) = coefficient.split_once('.').unwrap_or((coefficient, ""));
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let exponent = exponent.checked_sub(fraction.len() as i64)?;
    let adjusted = exponent.checked_add(digits.len() as i64 - 1)?;
    let sign = if negative { "-" } else { "" };
    if exponent > 0 || adjusted < -6 {
        let coefficient = if digits.len() > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits.into()
        };
        Some(format!(
            "{sign}{coefficient}E{}{adjusted}",
            if adjusted >= 0 { "+" } else { "" }
        ))
    } else if exponent == 0 {
        Some(format!("{sign}{digits}"))
    } else {
        let point = digits.len() as i64 + exponent;
        if point > 0 {
            let point = point as usize;
            Some(format!("{sign}{}.{}", &digits[..point], &digits[point..]))
        } else {
            Some(format!("{sign}0.{}{digits}", "0".repeat((-point) as usize)))
        }
    }
}

fn parse_field(
    location: &str,
    field: &str,
    value: &Value,
    kind: FieldKind,
) -> Result<Value, Value> {
    let err = |kind, msg, ctx| issue(location, Some(field), value.clone(), kind, msg, ctx);
    match kind {
        FieldKind::Text(maximum) => {
            let Some(text) = value.as_str() else {
                return Err(err("string_type", "Input should be a valid string", None));
            };
            if let Some(maximum) = maximum {
                let length = text.chars().count();
                if length < 1 {
                    return Err(err(
                        "string_too_short",
                        "String should have at least 1 character",
                        Some(json!({"min_length":1})),
                    ));
                }
                if length > maximum {
                    return Err(err(
                        "string_too_long",
                        &format!("String should have at most {maximum} characters"),
                        Some(json!({"max_length":maximum})),
                    ));
                }
            }
            Ok(value.clone())
        }
        FieldKind::Integer(minimum) => {
            let Some(number) = parse_integer(value) else {
                return Err(if value.is_string() {
                    err(
                        "int_parsing",
                        "Input should be a valid integer, unable to parse string as an integer",
                        None,
                    )
                } else if value.as_f64().is_some_and(|number| number.fract() != 0.0) {
                    err(
                        "int_from_float",
                        "Input should be a valid integer, got a number with a fractional part",
                        None,
                    )
                } else {
                    err("int_type", "Input should be a valid integer", None)
                });
            };
            if let Some(minimum) = minimum
                && number < minimum
            {
                return Err(err(
                    "greater_than_equal",
                    &format!("Input should be greater than or equal to {minimum}"),
                    Some(json!({"ge":minimum})),
                ));
            }
            Ok(json!(number))
        }
        FieldKind::Bool => parse_bool(value).map(Value::Bool).ok_or_else(|| {
            if value.is_string() || value.is_number() {
                err(
                    "bool_parsing",
                    "Input should be a valid boolean, unable to interpret input",
                    None,
                )
            } else {
                err("bool_type", "Input should be a valid boolean", None)
            }
        }),
        FieldKind::Price => {
            if !value.is_string() && !value.is_number() {
                return Err(err(
                    "decimal_type",
                    "Decimal input should be an integer, float, string or Decimal object",
                    None,
                ));
            }
            if value.as_str().is_some_and(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "nan"
                        | "snan"
                        | "inf"
                        | "+inf"
                        | "-inf"
                        | "infinity"
                        | "+infinity"
                        | "-infinity"
                )
            }) {
                return Err(err(
                    "finite_number",
                    "Input should be a finite number",
                    None,
                ));
            }
            let decimal = parse_decimal(value)
                .ok_or_else(|| err("decimal_parsing", "Input should be a valid decimal", None))?;
            if decimal < Decimal::ZERO {
                return Err(err(
                    "greater_than_equal",
                    "Input should be greater than or equal to 0",
                    Some(json!({"ge":0})),
                ));
            }
            Ok(json!(
                decimal_text(value).unwrap_or_else(|| decimal.to_string())
            ))
        }
        FieldKind::Object => {
            if value.is_object() {
                Ok(value.clone())
            } else {
                Err(err("dict_type", "Input should be a valid dictionary", None))
            }
        }
    }
}
fn default_value(field: &str) -> Option<Value> {
    Some(match field {
        "context_window" => json!(128000),
        "max_output_tokens" => json!(4096),
        "supports_vision" => json!(false),
        "supports_tools" | "is_enabled" => json!(true),
        "input_price_per_1k" | "output_price_per_1k" => json!("0"),
        "access_level" => json!("public"),
        "sort_order" => json!(0),
        "capability_overrides" => json!({}),
        _ => return None,
    })
}
/// Normalized source schema JSON (prices are Decimal text). Unknown fields are
/// ignored. Update nulls stay null here and are excluded only from its audit dump.
pub fn validate_model_body(body: &Value, update: bool) -> Result<ModelRow, ModelAdminError> {
    let Some(input) = body.as_object() else {
        return Err(ModelAdminError::validation(vec![if body.is_null() {
            issue("body", None, Value::Null, "missing", "Field required", None)
        } else {
            issue(
                "body",
                None,
                body.clone(),
                "model_attributes_type",
                "Input should be a valid dictionary or object to extract fields from",
                None,
            )
        }]));
    };
    let mut result = Map::new();
    let mut errors = Vec::new();
    let fields = FIELDS
        .iter()
        .copied()
        .filter(|(field, _)| !update || *field != "provider_id")
        .chain(update.then_some(("expected_capability_revision", FieldKind::Integer(Some(1)))));
    for (field, kind) in fields {
        match input.get(field) {
            None if update => {
                result.insert(field.into(), Value::Null);
            }
            None => {
                if let Some(value) = default_value(field) {
                    result.insert(field.into(), value);
                } else {
                    errors.push(issue(
                        "body",
                        Some(field),
                        body.clone(),
                        "missing",
                        "Field required",
                        None,
                    ));
                }
            }
            Some(value) if update && value.is_null() => {
                result.insert(field.into(), Value::Null);
            }
            Some(value) => match parse_field("body", field, value, kind) {
                Ok(value) => {
                    result.insert(field.into(), value);
                }
                Err(error) => errors.push(error),
            },
        }
    }
    if errors.is_empty() {
        Ok(result)
    } else {
        Err(ModelAdminError::validation(errors))
    }
}
fn audit_body(values: &ModelRow, update: bool) -> ConfigAuditValue {
    // Preserve source schema field order, rather than serde_json map ordering.
    ConfigAuditValue::Object(
        FIELDS
            .iter()
            .map(|(field, _)| *field)
            .chain(std::iter::once("expected_capability_revision"))
            .filter_map(|field| {
                values
                    .get(field)
                    .filter(|value| !update || !value.is_null())
                    .map(|value| {
                        (
                            field.to_owned(),
                            if matches!(field, "input_price_per_1k" | "output_price_per_1k")
                                && !value.is_null()
                            {
                                ConfigAuditValue::Decimal(
                                    parse_decimal(value).expect("validated Decimal"),
                                )
                            } else {
                                ConfigAuditValue::from(value.clone())
                            },
                        )
                    })
            })
            .collect(),
    )
}
pub struct ModelCreateBody {
    pub model_id: String,
    pub provider_id: String,
    pub data: CreateModel,
    pub audit: ConfigAuditValue,
}
pub fn validate_create(body: &Value) -> Result<ModelCreateBody, ModelAdminError> {
    let values = validate_model_body(body, false)?;
    let text = |field| {
        values
            .get(field)
            .and_then(Value::as_str)
            .expect("validated text")
            .to_owned()
    };
    let int = |field| {
        values
            .get(field)
            .and_then(Value::as_i64)
            .expect("validated integer")
    };
    let boolean = |field| {
        values
            .get(field)
            .and_then(Value::as_bool)
            .expect("validated bool")
    };
    Ok(ModelCreateBody {
        model_id: text("model_id"),
        provider_id: text("provider_id"),
        audit: audit_body(&values, false),
        data: CreateModel {
            fields: ModelFields {
                display_name: text("display_name"),
                context_window: int("context_window"),
                max_output_tokens: int("max_output_tokens"),
                supports_vision: boolean("supports_vision"),
                supports_tools: boolean("supports_tools"),
                input_price_per_1k: parse_decimal(&values["input_price_per_1k"])
                    .expect("validated Decimal"),
                output_price_per_1k: parse_decimal(&values["output_price_per_1k"])
                    .expect("validated Decimal"),
                access_level: text("access_level"),
                sort_order: int("sort_order"),
                catalog_capabilities: None,
            },
            is_enabled: boolean("is_enabled"),
            capability_overrides: values["capability_overrides"]
                .as_object()
                .expect("validated object")
                .clone(),
        },
    })
}
pub struct ModelUpdateBody {
    pub data: UpdateModel,
    pub audit: ConfigAuditValue,
}
pub fn validate_update(body: &Value) -> Result<ModelUpdateBody, ModelAdminError> {
    let values = validate_model_body(body, true)?;
    let text = |field| values.get(field).and_then(Value::as_str).map(str::to_owned);
    let int = |field| values.get(field).and_then(Value::as_i64);
    let boolean = |field| values.get(field).and_then(Value::as_bool);
    Ok(ModelUpdateBody {
        audit: audit_body(&values, true),
        data: UpdateModel {
            new_model_id: text("model_id"),
            display_name: text("display_name"),
            context_window: int("context_window"),
            max_output_tokens: int("max_output_tokens"),
            supports_vision: boolean("supports_vision"),
            supports_tools: boolean("supports_tools"),
            input_price_per_1k: parse_decimal(&values["input_price_per_1k"]),
            output_price_per_1k: parse_decimal(&values["output_price_per_1k"]),
            access_level: text("access_level"),
            is_enabled: boolean("is_enabled"),
            sort_order: int("sort_order"),
            catalog_capabilities: None,
            capability_overrides: values["capability_overrides"].as_object().cloned(),
            expected_capability_revision: int("expected_capability_revision"),
        },
    })
}
/// Validate a decoded query value, including source TypeAdapter null cases.
/// HTTP QueryParams themselves contain strings; absence is handled by handlers.
pub fn validate_model_query_value(field: &str, value: &Value) -> Result<Value, ModelAdminError> {
    if value.is_null() && matches!(field, "provider_id" | "expected_capability_revision") {
        return Ok(Value::Null);
    }
    let kind = match field {
        "is_enabled" | "include_disabled" => FieldKind::Bool,
        "expected_capability_revision" => FieldKind::Integer(Some(1)),
        "provider_id" => FieldKind::Text(None),
        _ => return Err(ModelAdminError::internal()),
    };
    parse_field("query", field, value, kind)
        .map_err(|error| ModelAdminError::validation(vec![error]))
}
fn query_bool(
    query: &HashMap<String, String>,
    field: &str,
    default: Option<bool>,
) -> Result<bool, ModelAdminError> {
    match query.get(field) {
        Some(value) => parse_field("query", field, &json!(value), FieldKind::Bool)
            .map(|value| value.as_bool().expect("bool"))
            .map_err(|error| ModelAdminError::validation(vec![error])),
        None => default.ok_or_else(|| {
            ModelAdminError::validation(vec![issue(
                "query",
                Some(field),
                Value::Null,
                "missing",
                "Field required",
                None,
            )])
        }),
    }
}
fn query_revision(query: &HashMap<String, String>) -> Result<Option<i64>, ModelAdminError> {
    query
        .get("expected_capability_revision")
        .map(|value| {
            parse_field(
                "query",
                "expected_capability_revision",
                &json!(value),
                FieldKind::Integer(Some(1)),
            )
            .map(|value| value.as_i64().expect("integer"))
            .map_err(|error| ModelAdminError::validation(vec![error]))
        })
        .transpose()
}
fn response_datetime(value: &Value) -> Option<Value> {
    let text = value.as_str()?;
    if let Ok(value) = DateTime::parse_from_rfc3339(text) {
        if !(1..=9999).contains(&value.year())
            || value.nanosecond() >= 1_000_000_000
            || !value.nanosecond().is_multiple_of(1000)
        {
            return None;
        }
        return Some(json!(value.to_rfc3339_opts(
            if value.nanosecond() == 0 {
                SecondsFormat::Secs
            } else {
                SecondsFormat::Micros
            },
            true
        )));
    }
    let value = NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f").ok()?;
    if !(1..=9999).contains(&value.year())
        || value.nanosecond() >= 1_000_000_000
        || !value.nanosecond().is_multiple_of(1000)
    {
        return None;
    }
    Some(json!(if value.nanosecond() == 0 {
        value.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        value.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
    }))
}
/// Source response_model projection. Redacted admin layers are restored as empty
/// defaults by ModelResponse; unknown service data (including pricing_snapshot)
/// is dropped. Toggle intentionally does not call this function.
pub fn model_response(mut row: ModelRow, redact: bool) -> Result<Value, ModelAdminError> {
    if redact {
        row.remove("catalog_capabilities");
        row.remove("capability_overrides");
    }
    let mut output = validate_model_body(&Value::Object(row.clone()), false)
        .map_err(|_| ModelAdminError::internal())?;
    let tenant = row
        .get("tenant_id")
        .and_then(Value::as_str)
        .ok_or_else(ModelAdminError::internal)?;
    output.insert("tenant_id".into(), json!(tenant));
    for field in ["catalog_capabilities", "effective_capabilities"] {
        let value = row.get(field).cloned().unwrap_or_else(|| json!({}));
        if !value.is_object() {
            return Err(ModelAdminError::internal());
        }
        output.insert(field.into(), value);
    }
    let revision = row.get("capability_revision").cloned().unwrap_or(json!(1));
    output.insert(
        "capability_revision".into(),
        parse_field(
            "response",
            "capability_revision",
            &revision,
            FieldKind::Integer(Some(1)),
        )
        .map_err(|_| ModelAdminError::internal())?,
    );
    for field in ["created_at", "updated_at"] {
        output.insert(
            field.into(),
            row.get(field)
                .and_then(response_datetime)
                .ok_or_else(ModelAdminError::internal)?,
        );
    }
    Ok(Value::Object(output))
}

// Explicit opt-in HTTP binding. Authentication and trusted audit state are
// injected, not recreated with a second JWT/key verifier. Extractors accept only
// decoded JSON/queries; the host's body limit/middleware controls transport size.
// URL percent-decoding, invalid percent sequences and the exact syntax-location
// message for malformed JSON are host/Axum boundaries, not claimed Python parity.
pub type ModelAdminCancellationFuture<'a> = Pin<Box<dyn Future<Output = Response> + Send + 'a>>;
/// Required host transport policy for an explicitly cancelled collaborator. No
/// invented HTTP status, panic, detached work, or implicit indefinitely pending
/// response is supplied. Dropping a request future remains natural cancellation.
pub trait ModelAdminCancellation: Send + Sync {
    fn cancelled(&self) -> ModelAdminCancellationFuture<'_>;
}
#[derive(Clone)]
struct ModelAdminHttpState {
    api: Arc<ModelAdminApi>,
    cancellation: Arc<dyn ModelAdminCancellation>,
}
pub fn router(api: Arc<ModelAdminApi>, cancellation: Arc<dyn ModelAdminCancellation>) -> Router {
    Router::new()
        .route(
            "/api/v1/model-capability-adapters",
            any(http_method_fallback).get(http_adapters),
        )
        .route(
            "/api/v1/model-capability-adapters/{*adapter_schema}",
            any(http_method_fallback).get(http_adapter_schema),
        )
        .route(
            "/api/v1/models",
            any(http_method_fallback).get(http_list).post(http_create),
        )
        .route(
            "/api/v1/models/{model_id}",
            any(http_method_fallback)
                .get(http_get)
                .put(http_update)
                .delete(http_delete),
        )
        .route(
            "/api/v1/models/{model_id}/capabilities/reset",
            any(http_method_fallback).post(http_reset),
        )
        .route(
            "/api/v1/models/{model_id}/toggle",
            any(http_method_fallback).patch(http_toggle),
        )
        .route_layer(middleware::from_fn(http_route_guard))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(ModelAdminHttpState { api, cancellation })
}
// FastAPI rejects methods before resolving dependencies and reports the first
// partial route match in Allow, rather than the union of methods on the path.
// `any` suppresses Axum's automatic Allow union after a fallback response.
// The route guard supplies source Allow only for true 405s, never a route 404.
async fn http_method_fallback() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}
async fn http_route_guard(
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
    next: Next,
) -> Response {
    let params = match params {
        Ok(Path(params)) => params,
        Err(error) => return error.into_response(),
    };
    let invalid = params.get("model_id").is_some_and(|id| id.contains('/'))
        || params
            .get("adapter_schema")
            .is_some_and(|path| !path.ends_with("/schema"));
    let allowed: &[&str] = match matched.as_str() {
        "/api/v1/models" => &["GET", "POST"],
        "/api/v1/models/{model_id}" => &["GET", "PUT", "DELETE"],
        "/api/v1/models/{model_id}/capabilities/reset" => &["POST"],
        "/api/v1/models/{model_id}/toggle" => &["PATCH"],
        _ => &["GET"],
    };
    if !invalid && allowed.contains(&request.method().as_str()) {
        return next.run(request).await;
    }
    // Axum sets Content-Length before suppressing the HEAD representation.
    if invalid {
        (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
    } else {
        (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", allowed[0])],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response()
    }
}
async fn http_result(
    state: &ModelAdminHttpState,
    result: Result<Value, ModelAdminError>,
) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(ModelAdminError::Http { status, body }) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(body),
        )
            .into_response(),
        Err(ModelAdminError::Cancelled) => state.cancellation.cancelled().await,
    }
}
async fn http_call(
    state: ModelAdminHttpState,
    operation: ModelAdminOperation,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: Result<Value, ModelAdminError>,
) -> Response {
    let body = match body {
        Ok(body) => body,
        Err(error) => return http_result(&state, Err(error)).await,
    };
    let audit_request = Some(state.api.identity.audit_request(&headers));
    let trace = match audit_request
        .as_ref()
        .and_then(|context| context.trace_id.as_ref())
    {
        Some(trace) => trace.as_str().map(str::to_owned),
        None => headers
            .get("x-trace-id")
            .filter(|value| !value.is_empty())
            .or_else(|| headers.get("x-request-id"))
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    };
    let mut result = state
        .api
        .handle(ModelAdminRequest {
            operation,
            headers,
            query,
            body,
            audit_request,
        })
        .await;
    if let Err(ModelAdminError::Http { status: 500, body }) = &mut result
        && body["error"]["code"] == 2000
        && let Some(trace) = trace.filter(|value| !value.is_empty())
    {
        body["error"]["trace_id"] = trace.into();
    }
    http_result(&state, result).await
}
pub(crate) fn http_body(headers: &HeaderMap, bytes: Bytes) -> Result<Value, ModelAdminError> {
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let json_content = crate::http_content_type::is_json_content_type(headers);
    if !json_content {
        // FastAPI leaves non-JSON bytes for semantic body validation, after
        // dependency resolution. UTF-8 JSON-shaped transport boundary only.
        return Ok(json!(String::from_utf8_lossy(&bytes)));
    }
    serde_json::from_slice(&bytes).map_err(|_| {
        ModelAdminError::validation(vec![json!({"type":"json_invalid","loc":["body",0],
        "msg":"JSON decode error","input":{},"ctx":{"error":"Invalid JSON"}})])
    })
}
async fn http_adapters(State(state): State<ModelAdminHttpState>, headers: HeaderMap) -> Response {
    http_call(
        state,
        ModelAdminOperation::ListAdapters,
        headers,
        HashMap::new(),
        Ok(Value::Null),
    )
    .await
}
async fn http_adapter_schema(
    State(state): State<ModelAdminHttpState>,
    Path(path): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(id) = path.strip_suffix("/schema") else {
        return http_result(&state, Err(ModelAdminError::detail(404, "Not Found"))).await;
    };
    http_call(
        state,
        ModelAdminOperation::AdapterSchema(id.into()),
        headers,
        HashMap::new(),
        Ok(Value::Null),
    )
    .await
}
async fn http_list(
    State(state): State<ModelAdminHttpState>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    http_call(
        state,
        ModelAdminOperation::List,
        headers,
        query,
        Ok(Value::Null),
    )
    .await
}
async fn http_create(
    State(state): State<ModelAdminHttpState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let body = http_body(&headers, body);
    http_call(
        state,
        ModelAdminOperation::Create,
        headers,
        HashMap::new(),
        body,
    )
    .await
}
async fn http_get(
    State(state): State<ModelAdminHttpState>,
    Path(model): Path<String>,
    headers: HeaderMap,
) -> Response {
    http_call(
        state,
        ModelAdminOperation::Get(model),
        headers,
        HashMap::new(),
        Ok(Value::Null),
    )
    .await
}
async fn http_update(
    State(state): State<ModelAdminHttpState>,
    Path(model): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let body = http_body(&headers, body);
    http_call(
        state,
        ModelAdminOperation::Update(model),
        headers,
        query,
        body,
    )
    .await
}
async fn http_reset(
    State(state): State<ModelAdminHttpState>,
    Path(model): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    http_call(
        state,
        ModelAdminOperation::Reset(model),
        headers,
        query,
        Ok(Value::Null),
    )
    .await
}
async fn http_delete(
    State(state): State<ModelAdminHttpState>,
    Path(model): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    http_call(
        state,
        ModelAdminOperation::Delete(model),
        headers,
        query,
        Ok(Value::Null),
    )
    .await
}
async fn http_toggle(
    State(state): State<ModelAdminHttpState>,
    Path(model): Path<String>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    http_call(
        state,
        ModelAdminOperation::Toggle(model),
        headers,
        query,
        Ok(Value::Null),
    )
    .await
}

#[cfg(test)]
#[path = "model_admin_cedar_tests.rs"]
mod cedar_differential_tests;
