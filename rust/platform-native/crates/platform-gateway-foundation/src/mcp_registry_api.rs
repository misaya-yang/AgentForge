//! All fourteen opt-in tenant MCP registry and Assistant compatibility routes.
//! Construction is inert: identity, persistence, discovery and DNS are injected.
//! JSON syntax precedes dependency resolution; model/path validation follows
//! identity and precedes tenant/capability/admin checks. DNS precedes repository
//! availability on create/patch. Discovery availability precedes repository access.
//! Ordinary audit failures do not roll back an already-successful mutation.
//! Hosts own mounting, request/body limits, denial-event lifetime and trusted
//! forwarded-origin preprocessing. No credentials, environment values or upstream errors
//! are logged. Repository errors must carry stable codes rather than source text.
use crate::{
    config_audit_writer::ConfigAuditRequestContext,
    mcp_gateway_broker::McpDiscovery,
    mcp_registry_store::{McpContext, McpError, McpRepository},
    mcp_registry_validation::{self as validation, McpBodyKind, field_error},
    service_config_api::{ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture},
    service_registry::python_truthy,
    usage_parser::python_value_to_string,
};
use ai_platform_auth::{
    admission::AdmissionService,
    rbac::{Capability, Rbac, check_capability, permission_denied_detail},
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{FromRequest, MatchedPath, Path, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, future::Future, net::ToSocketAddrs, pin::Pin, sync::Arc};

pub const MCP_REGISTRY_ROUTES: &[(&str, &str, &str)] = &[
    ("POST", "/mcp/servers", "create_mcp_server"),
    ("GET", "/mcp/servers", "list_mcp_servers"),
    ("GET", "/mcp/servers/{server_id}", "get_mcp_server"),
    ("PATCH", "/mcp/servers/{server_id}", "update_mcp_server"),
    ("DELETE", "/mcp/servers/{server_id}", "delete_mcp_server"),
    (
        "POST",
        "/mcp/servers/{server_id}/connections",
        "create_mcp_connection",
    ),
    (
        "GET",
        "/mcp/servers/{server_id}/connections",
        "list_mcp_connections",
    ),
    (
        "DELETE",
        "/mcp/connections/{connection_id}",
        "revoke_mcp_connection",
    ),
    (
        "PUT",
        "/mcp/connections/{connection_id}/channel-grants",
        "grant_mcp_channel",
    ),
    ("GET", "/mcp/servers/{server_id}/tools", "list_mcp_tools"),
    (
        "POST",
        "/mcp/servers/{server_id}/discover",
        "discover_mcp_server",
    ),
    ("GET", "/assistant/mcp/servers", "list_legacy_mcp_servers"),
    ("GET", "/assistant/mcp/tools", "list_legacy_mcp_tools"),
    (
        "POST",
        "/assistant/mcp/servers/{server_name}/refresh",
        "refresh_legacy_mcp_server",
    ),
];
#[derive(Clone, Debug, Default)]
pub struct McpRegistryIdentity {
    pub user_id: String,
    pub tenant_id: String,
    pub authenticated: bool,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}
/// A host resolves canonical UserContext and AuthContext. Tenant/user/authenticated
/// come from UserContext; roles/permissions come from AuthContext. Source does not
/// add a second auth-tenant agreement check, and this adapter does not invent one.
pub trait McpRegistryIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, McpRegistryIdentity>;
    fn context(&self, headers: &HeaderMap) -> ConfigAuditRequestContext {
        ConfigAuditRequestContext {
            headers: headers.clone(),
            ..Default::default()
        }
    }
    fn capability_check_started(&self, _capability: Capability) {}
    fn permission_denied(
        &self,
        _identity: &McpRegistryIdentity,
        _capability: Capability,
        _context: &ConfigAuditRequestContext,
    ) {
    }
}
impl McpRegistryIdentityResolver for AdmissionService {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, McpRegistryIdentity> {
        Box::pin(async move {
            let request = self.request(headers, path);
            let user = request
                .resolve()
                .await
                .map_err(|e| ServiceConfigError::detail(e.status(), e.to_string()))?;
            Ok(McpRegistryIdentity {
                user_id: user.user_id().into(),
                tenant_id: user.tenant_id().into(),
                authenticated: true,
                roles: user.roles().to_vec(),
                permissions: user.permissions().to_vec(),
            })
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpDestinationError {
    Unresolved,
    Disallowed,
    Cancelled,
}
pub type McpDestinationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), McpDestinationError>> + Send + 'a>>;
/// Success requires all records to pass destination policy, never merely the first.
pub trait McpDestinationValidator: Send + Sync {
    fn validate<'a>(&'a self, host: &'a str, port: u16) -> McpDestinationFuture<'a>;
}
/// No work occurs until validate(). Resolution uses the system DNS implementation;
/// runtime discovery must separately resolve and pin its transport generation.
#[derive(Default)]
pub struct NativeMcpDestinationValidator;
impl McpDestinationValidator for NativeMcpDestinationValidator {
    fn validate<'a>(&'a self, host: &'a str, port: u16) -> McpDestinationFuture<'a> {
        Box::pin(async move {
            let host = host.to_owned();
            tokio::task::spawn_blocking(move || {
                let addresses = (host.as_str(), port)
                    .to_socket_addrs()
                    .map_err(|_| McpDestinationError::Unresolved)?
                    .collect::<Vec<_>>();
                if addresses.is_empty() {
                    return Err(McpDestinationError::Unresolved);
                }
                if addresses
                    .iter()
                    .all(|a| validation::safe_destination_ip(a.ip()))
                {
                    Ok(())
                } else {
                    Err(McpDestinationError::Disallowed)
                }
            })
            .await
            .map_err(|_| McpDestinationError::Unresolved)?
        })
    }
}
pub struct McpRegistryDependencies {
    pub identity: Arc<dyn McpRegistryIdentityResolver>,
    pub rbac: Rbac,
    pub enabled: bool,
    pub repository: Option<Arc<dyn McpRepository>>,
    /// Separate app.database presence matters: an injected repository alone must
    /// not cause an audit write when the canonical app has no database.
    pub audit: Option<Arc<dyn McpRepository>>,
    pub discovery: Option<Arc<dyn McpDiscovery>>,
    pub destination: Arc<dyn McpDestinationValidator>,
}
pub struct McpRegistryApi {
    dependencies: McpRegistryDependencies,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpRegistryOperation {
    CreateServer,
    ListServers,
    GetServer {
        server_id: String,
    },
    UpdateServer {
        server_id: String,
    },
    DeleteServer {
        server_id: String,
    },
    CreateConnection {
        server_id: String,
    },
    ListConnections {
        server_id: String,
    },
    RevokeConnection {
        connection_id: String,
    },
    GrantChannel {
        connection_id: String,
    },
    ListTools {
        server_id: String,
    },
    Discover {
        server_id: String,
    },
    LegacyListServers,
    LegacyListTools,
    LegacyRefresh {
        server_name: String,
        connection_id: Option<String>,
        principal_type: String,
    },
}
impl McpRegistryOperation {
    pub fn path(&self) -> String {
        match self {
            Self::CreateServer | Self::ListServers => "/api/v1/mcp/servers".into(),
            Self::GetServer { server_id }
            | Self::UpdateServer { server_id }
            | Self::DeleteServer { server_id } => format!("/api/v1/mcp/servers/{server_id}"),
            Self::CreateConnection { server_id } | Self::ListConnections { server_id } => {
                format!("/api/v1/mcp/servers/{server_id}/connections")
            }
            Self::RevokeConnection { connection_id } => {
                format!("/api/v1/mcp/connections/{connection_id}")
            }
            Self::GrantChannel { connection_id } => {
                format!("/api/v1/mcp/connections/{connection_id}/channel-grants")
            }
            Self::ListTools { server_id } => format!("/api/v1/mcp/servers/{server_id}/tools"),
            Self::Discover { server_id } => format!("/api/v1/mcp/servers/{server_id}/discover"),
            Self::LegacyListServers => "/api/v1/assistant/mcp/servers".into(),
            Self::LegacyListTools => "/api/v1/assistant/mcp/tools".into(),
            Self::LegacyRefresh { server_name, .. } => {
                format!("/api/v1/assistant/mcp/servers/{server_name}/refresh")
            }
        }
    }
    fn body_kind(&self) -> Option<McpBodyKind> {
        match self {
            Self::CreateServer => Some(McpBodyKind::ServerCreate),
            Self::UpdateServer { .. } => Some(McpBodyKind::ServerUpdate),
            Self::CreateConnection { .. } => Some(McpBodyKind::ConnectionCreate),
            Self::GrantChannel { .. } => Some(McpBodyKind::ChannelGrant),
            Self::Discover { .. } => Some(McpBodyKind::Discovery),
            _ => None,
        }
    }
    fn write(&self) -> bool {
        !matches!(
            self,
            Self::ListServers
                | Self::GetServer { .. }
                | Self::ListConnections { .. }
                | Self::ListTools { .. }
                | Self::LegacyListServers
                | Self::LegacyListTools
        )
    }
    fn status(&self) -> u16 {
        if matches!(self, Self::CreateServer | Self::CreateConnection { .. }) {
            201
        } else {
            200
        }
    }
}
fn request_id(context: &mut ConfigAuditRequestContext) -> String {
    let id = context
        .request_id
        .as_ref()
        .filter(|v| python_truthy(v))
        .or_else(|| context.trace_id.as_ref().filter(|v| python_truthy(v)))
        .map(python_value_to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    context.request_id = Some(json!(id));
    id
}
fn mcp_error(
    context: &mut ConfigAuditRequestContext,
    status: u16,
    code: &str,
    message: &str,
) -> ServiceConfigError {
    ServiceConfigError::detail(
        status,
        json!({"code":code,"message":message,"request_id":request_id(context)}),
    )
}
fn repository_error(
    context: &mut ConfigAuditRequestContext,
    error: McpError,
) -> ServiceConfigError {
    match error {
        McpError::NotFound(code) => mcp_error(context, 404, &code, "MCP resource not found"),
        McpError::Conflict(code) => mcp_error(
            context,
            409,
            &code,
            if code == "MCP_RESOURCE_CONFLICT" {
                "MCP resource conflict"
            } else {
                "MCP resource conflicts with existing state"
            },
        ),
        McpError::Validation(code) => {
            mcp_error(context, 422, &code, "MCP configuration is invalid")
        }
        McpError::Authorization(code) => {
            mcp_error(context, 403, &code, "MCP operation is not authorized")
        }
        McpError::Broker(code) => mcp_error(context, 502, &code, "MCP server unavailable"),
        McpError::Storage => mcp_error(
            context,
            503,
            "MCP_STORAGE_UNAVAILABLE",
            "MCP registry storage unavailable",
        ),
        McpError::Cancelled => ServiceConfigError::Cancelled,
        McpError::Unhandled => ServiceConfigError::Internal,
    }
}
fn redacted_validation(
    context: &mut ConfigAuditRequestContext,
    fields: Vec<Value>,
) -> ServiceConfigError {
    ServiceConfigError::detail(
        422,
        json!({"code":"REQUEST_VALIDATION_FAILED","message":"Request validation failed","request_id":request_id(context),"fields":fields}),
    )
}
impl McpRegistryApi {
    pub fn new(dependencies: McpRegistryDependencies) -> Self {
        Self { dependencies }
    }
    pub async fn handle(
        &self,
        op: McpRegistryOperation,
        headers: &HeaderMap,
        body: &Value,
    ) -> Result<Value, ServiceConfigError> {
        let mut context = self.dependencies.identity.context(headers);
        let id = self
            .dependencies
            .identity
            .resolve(headers, &op.path())
            .await?;
        self.execute(op, &id, body, &mut context).await
    }
    fn repository(
        &self,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<&dyn McpRepository, ServiceConfigError> {
        if !self.dependencies.enabled {
            return Err(mcp_error(
                context,
                503,
                "MCP_DISABLED",
                "MCP registry is disabled",
            ));
        }
        self.dependencies.repository.as_deref().ok_or_else(|| {
            mcp_error(
                context,
                503,
                "MCP_STORAGE_UNAVAILABLE",
                "MCP registry storage unavailable",
            )
        })
    }
    fn authorize(
        &self,
        id: &McpRegistryIdentity,
        write: bool,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<(), ServiceConfigError> {
        {
            use ai_platform_auth::cedar::resources::{
                self, ObjectAction, ObjectResource, ObjectSubject, ResourceKind,
            };
            let capability = if write {
                Capability::GatewayMcpWrite
            } else {
                Capability::GatewayMcpRead
            };
            let subject = ObjectSubject {
                tenant_id: id.tenant_id.clone(),
                user_id: id.user_id.clone(),
                roles: id.roles.clone(),
                permissions: id.permissions.clone(),
                anonymous: !id.authenticated,
                ..Default::default()
            };
            let mut resource =
                ObjectResource::new(ResourceKind::Tool, "mcp-registry", &id.tenant_id);
            resource.required_permissions = capability
                .accepted_permissions()
                .iter()
                .map(|v| (*v).into())
                .collect();
            let legacy = id.authenticated
                && !id.user_id.is_empty()
                && !id.tenant_id.is_empty()
                && id.tenant_id != "public"
                && {
                    let subjects = id
                        .roles
                        .iter()
                        .chain(&id.permissions)
                        .map(|v| v.trim())
                        .filter(|v| !v.is_empty())
                        .collect::<std::collections::BTreeSet<_>>();
                    capability.accepted_permissions().iter().any(|permission| {
                        self.dependencies.rbac.has_permission(
                            &subjects.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                            permission,
                        )
                    })
                }
                && (!write
                    || id.roles.iter().any(|v| {
                        ["admin", "tenant_admin", "superadmin", "super_admin"]
                            .contains(&v.to_lowercase().as_str())
                    }));
            let decision = resources::compare_current(
                &subject,
                &resource,
                if write {
                    ObjectAction::Write
                } else {
                    ObjectAction::Read
                },
                legacy,
            );
            if legacy && !decision {
                return Err(mcp_error(
                    context,
                    403,
                    "MCP_RESOURCE_DENIED",
                    "MCP resource authorization denied",
                ));
            }
        }
        if !id.authenticated || id.user_id.is_empty() {
            return Err(mcp_error(
                context,
                401,
                "AUTHENTICATION_REQUIRED",
                "Authentication required",
            ));
        }
        if id.tenant_id.is_empty() || id.tenant_id == "public" {
            return Err(mcp_error(
                context,
                403,
                "TENANT_REQUIRED",
                "Tenant identity required",
            ));
        }
        let capability = if write {
            Capability::GatewayMcpWrite
        } else {
            Capability::GatewayMcpRead
        };
        self.dependencies
            .identity
            .capability_check_started(capability);
        if !check_capability(
            &self.dependencies.rbac,
            &id.roles,
            &id.permissions,
            capability,
        )
        .allowed()
        {
            self.dependencies
                .identity
                .permission_denied(id, capability, context);
            let trace = context
                .request_id
                .as_ref()
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    context
                        .trace_id
                        .as_ref()
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or("");
            return Err(ServiceConfigError::detail(
                403,
                permission_denied_detail(capability, trace, None),
            ));
        }
        if write
            && !id.roles.iter().any(|role| {
                matches!(
                    role.to_lowercase().as_str(),
                    "admin" | "tenant_admin" | "superadmin" | "super_admin"
                )
            })
        {
            return Err(mcp_error(
                context,
                403,
                "MCP_ADMIN_REQUIRED",
                "Tenant administrator access is required",
            ));
        }
        Ok(())
    }
    async fn destinations(
        &self,
        body: &Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<(), ServiceConfigError> {
        for field in ["base_url", "oauth_metadata_url"] {
            if let Some(url) = body.get(field).and_then(Value::as_str) {
                let (host, port) = validation::persisted_destination(url).map_err(|_| {
                    mcp_error(
                        context,
                        422,
                        "MCP_URL_INVALID",
                        "MCP destination is invalid",
                    )
                })?;
                self.dependencies
                    .destination
                    .validate(&host, port)
                    .await
                    .map_err(|error| match error {
                        McpDestinationError::Unresolved => mcp_error(
                            context,
                            422,
                            "MCP_DNS_UNAVAILABLE",
                            "MCP destination cannot be resolved",
                        ),
                        McpDestinationError::Disallowed => mcp_error(
                            context,
                            422,
                            "MCP_SSRF_BLOCKED",
                            "MCP destination is not permitted",
                        ),
                        McpDestinationError::Cancelled => ServiceConfigError::Cancelled,
                    })?;
            }
        }
        Ok(())
    }
    async fn call(
        &self,
        operation: &str,
        id: &McpContext,
        arguments: Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        self.repository(context)?
            .call(operation, id, arguments)
            .await
            .map_err(|e| repository_error(context, e))
    }
    async fn audit(
        &self,
        id: &McpContext,
        action: &str,
        resource_type: &str,
        resource_id: &str,
        mut summary: Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<String, ServiceConfigError> {
        let fallback = format!("request:{}", request_id(context));
        let Some(audit) = self.dependencies.audit.as_ref() else {
            return Ok(fallback);
        };
        if let Some(fields) = summary.as_object_mut() {
            for key in ["secret_ref", "token", "authorization", "credential"] {
                fields.remove(key);
            }
        }
        match audit.call("audit_mutation",id,json!({"action":action,"resource_type":resource_type,"resource_id":resource_id,"summary":summary})).await {
            Ok(value) if !value.is_null()=>Ok(format!("audit:{}",python_value_to_string(&value))),
            Err(McpError::Cancelled)=>Err(ServiceConfigError::Cancelled),
            _=>Ok(fallback),
        }
    }
    async fn discover(
        &self,
        server_id: &str,
        connection_id: &str,
        principal_type: &str,
        id: &McpContext,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let service = self.dependencies.discovery.as_ref().ok_or_else(|| {
            mcp_error(
                context,
                503,
                "MCP_DISCOVERY_UNAVAILABLE",
                "MCP discovery service unavailable",
            )
        })?;
        // The Python call evaluates repository=_get_repository before entering broker.
        self.repository(context)?;
        if !service.repository_matches(
            self.dependencies
                .repository
                .as_ref()
                .expect("repository checked"),
        ) {
            return Err(repository_error(
                context,
                McpError::Broker("MCP_REPOSITORY_MISMATCH".into()),
            ));
        }
        let result = service
            .discover(id, server_id, connection_id, principal_type)
            .await
            .map_err(|e| repository_error(context, e))?;
        let summary = json!({"changed":result["changed"].as_array().map_or(0,Vec::len),"removed":result["removed"].as_array().map_or(0,Vec::len),"breaking":python_truthy(&result["breaking"])});
        let audit = self
            .audit(id, "discover", "mcp_server", server_id, summary, context)
            .await?;
        let mut response = project_discovery(&result)?;
        response["request_id"] = json!(request_id(context));
        response["audit_ref"] = json!(audit);
        Ok(response)
    }
    /// The caller supplies an already-resolved identity. HTTP syntax parsing and
    /// dependency failures belong to handle/dispatch; this preserves ordering for
    /// deterministic non-network fixtures and native host adapters.
    pub async fn execute(
        &self,
        mut op: McpRegistryOperation,
        identity: &McpRegistryIdentity,
        body: &Value,
        context: &mut ConfigAuditRequestContext,
    ) -> Result<Value, ServiceConfigError> {
        let payload = normalize_request(&mut op, body, context)?;
        self.authorize(identity, op.write(), context)?;
        let id = McpContext {
            tenant_id: identity.tenant_id.clone(),
            user_id: identity.user_id.clone(),
            authenticated: identity.authenticated,
        };
        match op {
            McpRegistryOperation::CreateServer => {
                self.destinations(&payload, context).await?;
                let mut args = payload.clone();
                args.as_object_mut().unwrap().remove("transport");
                let row = self.call("create_server", &id, args, context).await?;
                let server_id = required_text(&row, "server_id")?;
                let audit = self
                    .audit(
                        &id,
                        "create",
                        "mcp_server",
                        server_id,
                        json!({"name":payload["name"],"auth_method":payload["auth_method"]}),
                        context,
                    )
                    .await?;
                Ok(
                    json!({"server":project_server(&row)?,"request_id":request_id(context),"audit_ref":audit}),
                )
            }
            McpRegistryOperation::ListServers | McpRegistryOperation::LegacyListServers => {
                let legacy = matches!(op, McpRegistryOperation::LegacyListServers);
                let rows = self.call("list_servers", &id, json!({}), context).await?;
                let servers = project_list(&rows, project_server)?;
                Ok(if legacy {
                    json!({"servers":servers})
                } else {
                    json!({"total":servers.len(),"servers":servers})
                })
            }
            McpRegistryOperation::GetServer { server_id } => project_server(
                &self
                    .call("get_server", &id, json!({"server_id":server_id}), context)
                    .await?,
            ),
            McpRegistryOperation::UpdateServer { server_id } => {
                self.destinations(&payload, context).await?;
                let row = self
                    .call(
                        "update_server",
                        &id,
                        json!({"server_id":server_id,"changes":payload}),
                        context,
                    )
                    .await?;
                let mut fields = payload
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>();
                fields.sort();
                let audit = self
                    .audit(
                        &id,
                        "update",
                        "mcp_server",
                        &server_id,
                        json!({"fields":fields}),
                        context,
                    )
                    .await?;
                Ok(
                    json!({"server":project_server(&row)?,"request_id":request_id(context),"audit_ref":audit}),
                )
            }
            McpRegistryOperation::DeleteServer { server_id } => {
                self.call(
                    "delete_server",
                    &id,
                    json!({"server_id":server_id}),
                    context,
                )
                .await?;
                let audit = self
                    .audit(&id, "delete", "mcp_server", &server_id, json!({}), context)
                    .await?;
                Ok(json!({"status":"deleted","request_id":request_id(context),"audit_ref":audit}))
            }
            McpRegistryOperation::CreateConnection { server_id } => {
                let mut args = payload.clone();
                args["server_id"] = json!(server_id);
                let row = self.call("create_connection", &id, args, context).await?;
                let connection_id = required_text(&row, "connection_id")?;
                let audit=self.audit(&id,"create","mcp_connection",connection_id,json!({"server_id":server_id,"principal_type":payload["principal_type"],"owner_user_id":payload["owner_user_id"]}),context).await?;
                Ok(
                    json!({"connection":project_connection(&row)?,"request_id":request_id(context),"audit_ref":audit}),
                )
            }
            McpRegistryOperation::ListConnections { server_id } => {
                let rows = self
                    .call(
                        "list_connections",
                        &id,
                        json!({"server_id":server_id}),
                        context,
                    )
                    .await?;
                let connections = project_list(&rows, project_connection)?;
                Ok(json!({"total":connections.len(),"connections":connections}))
            }
            McpRegistryOperation::RevokeConnection { connection_id } => {
                self.call(
                    "revoke_connection",
                    &id,
                    json!({"connection_id":connection_id}),
                    context,
                )
                .await?;
                let audit = self
                    .audit(
                        &id,
                        "revoke",
                        "mcp_connection",
                        &connection_id,
                        json!({}),
                        context,
                    )
                    .await?;
                Ok(json!({"status":"revoked","request_id":request_id(context),"audit_ref":audit}))
            }
            McpRegistryOperation::GrantChannel { connection_id } => {
                let mut args = payload.clone();
                args["connection_id"] = json!(connection_id);
                self.call("grant_channel", &id, args, context).await?;
                let audit = self
                    .audit(
                        &id,
                        "grant_channel",
                        "mcp_connection",
                        &connection_id,
                        json!({"tool_id":payload["tool_id"],"channel":payload["channel"]}),
                        context,
                    )
                    .await?;
                Ok(json!({"status":"granted","request_id":request_id(context),"audit_ref":audit}))
            }
            McpRegistryOperation::ListTools { server_id } => {
                let rows = self
                    .call("list_tools", &id, json!({"server_id":server_id}), context)
                    .await?;
                let tools = project_list(&rows, project_tool)?;
                Ok(json!({"total":tools.len(),"tools":tools}))
            }
            McpRegistryOperation::Discover { server_id } => {
                self.discover(
                    &server_id,
                    required_text(&payload, "connection_id")?,
                    required_text(&payload, "principal_type")?,
                    &id,
                    context,
                )
                .await
            }
            McpRegistryOperation::LegacyListTools => {
                let servers = self.call("list_servers", &id, json!({}), context).await?;
                let mut tools = Vec::new();
                for server in servers.as_array().ok_or(ServiceConfigError::Internal)? {
                    let rows = self
                        .call(
                            "list_tools",
                            &id,
                            json!({"server_id":required_text(server,"server_id")?}),
                            context,
                        )
                        .await?;
                    tools.extend(
                        rows.as_array()
                            .ok_or(ServiceConfigError::Internal)?
                            .iter()
                            .cloned(),
                    );
                }
                Ok(json!({"total":tools.len(),"tools":tools}))
            }
            McpRegistryOperation::LegacyRefresh {
                server_name,
                connection_id,
                principal_type,
            } => {
                let servers = self.call("list_servers", &id, json!({}), context).await?;
                let server = servers
                    .as_array()
                    .ok_or(ServiceConfigError::Internal)?
                    .iter()
                    .find(|v| {
                        v["server_id"].as_str() == Some(server_name.as_str())
                            || v["name"].as_str() == Some(server_name.as_str())
                    })
                    .ok_or_else(|| {
                        mcp_error(context, 404, "MCP_SERVER_NOT_FOUND", "MCP server not found")
                    })?;
                let connection = connection_id.ok_or_else(|| {
                    mcp_error(
                        context,
                        422,
                        "MCP_CONNECTION_SELECTION_REQUIRED",
                        "An explicit MCP credential connection is required",
                    )
                })?;
                self.authorize(identity, true, context)?; // source delegates to public discover handler
                self.discover(
                    required_text(server, "server_id")?,
                    &connection,
                    &principal_type,
                    &id,
                    context,
                )
                .await
            }
        }
    }
}

fn normalize_request(
    op: &mut McpRegistryOperation,
    body: &Value,
    context: &mut ConfigAuditRequestContext,
) -> Result<Value, ServiceConfigError> {
    let mut errors = Vec::new();
    let path_id = match op {
        McpRegistryOperation::GetServer { server_id }
        | McpRegistryOperation::UpdateServer { server_id }
        | McpRegistryOperation::DeleteServer { server_id }
        | McpRegistryOperation::CreateConnection { server_id }
        | McpRegistryOperation::ListConnections { server_id }
        | McpRegistryOperation::ListTools { server_id }
        | McpRegistryOperation::Discover { server_id } => Some(("server_id", server_id)),
        McpRegistryOperation::RevokeConnection { connection_id }
        | McpRegistryOperation::GrantChannel { connection_id } => {
            Some(("connection_id", connection_id))
        }
        _ => None,
    };
    if let Some((field, id)) = path_id {
        match validation::uuid_value(&json!(*id)) {
            Ok(value) => *id = value.as_str().unwrap().into(),
            Err(kind) => errors.push(field_error(["path".into(), field.into()], kind)),
        }
    }
    if let McpRegistryOperation::LegacyRefresh {
        connection_id,
        principal_type,
        ..
    } = op
    {
        let mut legacy_errors = Vec::new();
        if let Some(id) = connection_id {
            match validation::uuid_value(&json!(*id)) {
                Ok(value) => *id = value.as_str().unwrap().into(),
                Err(_) => {
                    let detail = uuid::Uuid::parse_str(id)
                        .expect_err("validated invalid UUID")
                        .to_string();
                    // Pydantic's UUID parser names the simple-format length;
                    // the native UUID version omits that phrase.
                    let detail = match detail.strip_prefix("invalid length: found ") {
                        Some(length) if !id.contains('-') => format!(
                            "invalid length: expected length 32 for simple format, found {length}"
                        ),
                        _ => detail,
                    };
                    legacy_errors.push(json!({"type":"uuid_parsing","loc":["query","connection_id"],"msg":format!("Input should be a valid UUID, {detail}"),"input":id,"ctx":{"error":detail}}));
                }
            }
        }
        if !matches!(
            principal_type.as_str(),
            "service_account" | "user_delegated"
        ) {
            legacy_errors.push(json!({"type":"literal_error","loc":["query","principal_type"],"msg":"Input should be 'service_account' or 'user_delegated'","input":principal_type,"ctx":{"expected":"'service_account' or 'user_delegated'"}}));
        }
        if !legacy_errors.is_empty() {
            return Err(ServiceConfigError::validation(legacy_errors));
        }
    }
    let payload = match op.body_kind() {
        Some(kind) => match validation::validate_body(kind, body) {
            Ok(value) => value,
            Err(body_errors) => {
                errors.extend(body_errors);
                Value::Null
            }
        },
        None => Value::Null,
    };
    if errors.is_empty() {
        Ok(payload)
    } else {
        Err(redacted_validation(context, errors))
    }
}
fn required_text<'a>(row: &'a Value, key: &str) -> Result<&'a str, ServiceConfigError> {
    row.get(key)
        .and_then(Value::as_str)
        .ok_or(ServiceConfigError::Internal)
}
fn project_list(
    rows: &Value,
    projection: fn(&Value) -> Result<Value, ServiceConfigError>,
) -> Result<Vec<Value>, ServiceConfigError> {
    rows.as_array()
        .ok_or(ServiceConfigError::Internal)?
        .iter()
        .map(projection)
        .collect()
}
/// These public projections are explicit allowlists, independently of repository
/// redaction. Neither an extra column nor a credential reference can escape.
fn projection(
    row: &Value,
    required: &[&str],
    optional: &[&str],
    lists: &[&str],
) -> Result<Value, ServiceConfigError> {
    let row = row.as_object().ok_or(ServiceConfigError::Internal)?;
    let mut out = Map::new();
    for &key in required {
        out.insert(
            key.into(),
            row.get(key).cloned().ok_or(ServiceConfigError::Internal)?,
        );
    }
    for &key in optional {
        out.insert(key.into(), row.get(key).cloned().unwrap_or(Value::Null));
    }
    for &key in lists {
        out.insert(
            key.into(),
            row.get(key).cloned().unwrap_or_else(|| json!([])),
        );
    }
    Ok(Value::Object(out))
}
fn response_fields(
    out: &mut Value,
    strings: &[&str],
    uuids: &[&str],
    ints: &[&str],
    bools: &[&str],
    datetimes: &[&str],
    nullable: &[&str],
) -> Result<(), ServiceConfigError> {
    for &key in strings {
        if !(out[key].is_string() || (nullable.contains(&key) && out[key].is_null())) {
            return Err(ServiceConfigError::Internal);
        }
    }
    for &key in uuids {
        out[key] = validation::uuid_value(&out[key]).map_err(|_| ServiceConfigError::Internal)?;
    }
    for &key in ints {
        out[key] = json!(
            crate::rate_config_api::integer(&out[key]).map_err(|_| ServiceConfigError::Internal)?
        );
    }
    for &key in bools {
        out[key] = json!(
            crate::rate_config_api::boolean(&out[key]).map_err(|_| ServiceConfigError::Internal)?
        );
    }
    for &key in datetimes {
        if !(out[key].is_null() && nullable.contains(&key)) {
            out[key] = validation::datetime(&out[key]).map_err(|_| ServiceConfigError::Internal)?;
        }
    }
    Ok(())
}
fn string_list(value: &Value) -> Result<(), ServiceConfigError> {
    if value
        .as_array()
        .is_some_and(|a| a.iter().all(Value::is_string))
    {
        Ok(())
    } else {
        Err(ServiceConfigError::Internal)
    }
}
pub fn project_server(row: &Value) -> Result<Value, ServiceConfigError> {
    let mut out = projection(
        row,
        &[
            "server_id",
            "name",
            "description",
            "base_url",
            "transport",
            "auth_method",
            "timeout_ms",
            "max_concurrency",
            "response_limit_bytes",
            "enabled",
            "health_status",
            "circuit_state",
            "consecutive_failures",
            "created_at",
            "updated_at",
        ],
        &[
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "last_health_at",
            "last_error_code",
        ],
        &["allowed_origins"],
    )?;
    response_fields(
        &mut out,
        &[
            "name",
            "description",
            "base_url",
            "transport",
            "auth_method",
            "health_status",
            "circuit_state",
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "last_error_code",
        ],
        &["server_id"],
        &[
            "timeout_ms",
            "max_concurrency",
            "response_limit_bytes",
            "consecutive_failures",
        ],
        &["enabled"],
        &["last_health_at", "created_at", "updated_at"],
        &[
            "oauth_metadata_url",
            "oauth_resource",
            "oauth_audience",
            "last_health_at",
            "last_error_code",
        ],
    )?;
    if out["transport"] != "streamable_http"
        || !matches!(
            out["auth_method"].as_str(),
            Some("none" | "bearer" | "oauth")
        )
    {
        return Err(ServiceConfigError::Internal);
    }
    string_list(&out["allowed_origins"])?;
    Ok(out)
}
pub fn project_connection(row: &Value) -> Result<Value, ServiceConfigError> {
    let mut out = projection(
        row,
        &[
            "connection_id",
            "server_id",
            "principal_type",
            "enabled",
            "credential_configured",
            "created_at",
            "updated_at",
        ],
        &["owner_user_id", "audience", "expires_at", "revoked_at"],
        &["scopes"],
    )?;
    response_fields(
        &mut out,
        &["principal_type", "owner_user_id", "audience"],
        &["connection_id", "server_id"],
        &[],
        &["enabled", "credential_configured"],
        &["expires_at", "revoked_at", "created_at", "updated_at"],
        &["owner_user_id", "audience", "expires_at", "revoked_at"],
    )?;
    if !matches!(
        out["principal_type"].as_str(),
        Some("service_account" | "user_delegated")
    ) {
        return Err(ServiceConfigError::Internal);
    }
    string_list(&out["scopes"])?;
    Ok(out)
}
pub fn project_tool(row: &Value) -> Result<Value, ServiceConfigError> {
    let mut out = projection(
        row,
        &[
            "tool_id",
            "server_id",
            "upstream_name",
            "runtime_name",
            "enabled",
            "snapshot_id",
            "schema_version",
            "schema_hash",
            "description",
            "input_schema",
            "risk_level",
            "read_only",
            "discovered_at",
        ],
        &[],
        &[],
    )?;
    response_fields(
        &mut out,
        &[
            "upstream_name",
            "runtime_name",
            "schema_hash",
            "description",
            "risk_level",
        ],
        &["tool_id", "server_id", "snapshot_id"],
        &["schema_version"],
        &["enabled", "read_only"],
        &["discovered_at"],
        &[],
    )?;
    if !out["input_schema"].is_object()
        || !matches!(
            out["risk_level"].as_str(),
            Some("low" | "medium" | "high" | "critical")
        )
    {
        return Err(ServiceConfigError::Internal);
    }
    Ok(out)
}
fn project_discovery(row: &Value) -> Result<Value, ServiceConfigError> {
    let mut out = projection(
        row,
        &["server_id", "changed", "unchanged", "removed", "breaking"],
        &[],
        &[],
    )?;
    response_fields(&mut out, &[], &["server_id"], &[], &["breaking"], &[], &[])?;
    for key in ["changed", "unchanged"] {
        if !out[key]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_object))
        {
            return Err(ServiceConfigError::Internal);
        }
    }
    string_list(&out["removed"])?;
    Ok(out)
}
#[derive(Clone)]
struct HttpState {
    api: Arc<McpRegistryApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
/// Unmounted factory. The embedding application controls authentication middleware
/// and must not mount this without trusted identity/destination collaborators.
pub fn router(
    api: Arc<McpRegistryApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    let mut router = Router::new();
    let mut paths = Vec::new();
    for &(_, path, _) in MCP_REGISTRY_ROUTES {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    for path in paths {
        router = router.route(&format!("/api/v1{path}"), any(dispatch));
    }
    router
        .fallback(fallback)
        .with_state(HttpState { api, cancellation })
}

fn template_matches(template: &str, path: &str) -> bool {
    let expected = template.split('/').collect::<Vec<_>>();
    let actual = path.split('/').collect::<Vec<_>>();
    expected.len() == actual.len()
        && expected.iter().zip(actual).all(|(expected, actual)| {
            if expected.starts_with('{') && expected.ends_with('}') {
                !actual.is_empty()
            } else {
                *expected == actual
            }
        })
}
async fn fallback(request: Request) -> Response {
    let path = request.uri().path();
    if path.ends_with('/') {
        let canonical = path.trim_end_matches('/');
        if MCP_REGISTRY_ROUTES
            .iter()
            .any(|(_, route, _)| template_matches(&format!("/api/v1{route}"), canonical))
        {
            // Starlette derives its origin from the ASGI scope/Host. Proxy headers
            // are deliberately not trusted here; host middleware owns those.
            let scheme = request.uri().scheme_str().unwrap_or("http");
            let authority = request
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .or_else(|| request.uri().authority().map(|a| a.as_str()));
            let suffix = request
                .uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default();
            let location = authority
                .map(|host| format!("{scheme}://{host}{canonical}{suffix}"))
                .unwrap_or_else(|| format!("{canonical}{suffix}"));
            return (StatusCode::TEMPORARY_REDIRECT, [("location", location)]).into_response();
        }
    }
    (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response()
}

async fn respond(
    state: &HttpState,
    result: Result<Value, ServiceConfigError>,
    status: u16,
    context: &ConfigAuditRequestContext,
) -> Response {
    let mut response = match result {
        Ok(value) => (
            StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
            Json(value),
        )
            .into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(ServiceConfigError::Internal | ServiceConfigError::Exception(_)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
        }
        Err(error) => {
            let (status, body) = error.response(context).expect("noncancelled error");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    };
    response.extensions_mut().insert(context.clone());
    response
}
fn query_parameters(query: &str) -> HashMap<String, String> {
    // Url's form decoder handles percent encodings, UTF-8 replacement and '+';
    // collecting preserves Starlette's last-value behavior for duplicate keys.
    reqwest::Url::parse(&format!("https://mcp.invalid/?{query}"))
        .map(|url| {
            url.query_pairs()
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default()
}
async fn dispatch(
    State(state): State<HttpState>,
    matched: MatchedPath,
    params: Result<Path<HashMap<String, String>>, axum::extract::rejection::PathRejection>,
    request: Request,
) -> Response {
    let params = match params {
        Ok(Path(p)) => p,
        Err(error) => return error.into_response(),
    };
    if params.values().any(|v| v.contains('/')) {
        return (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))).into_response();
    }
    let method = request.method().as_str();
    let path = matched
        .as_str()
        .strip_prefix("/api/v1")
        .unwrap_or(matched.as_str());
    let id = |field: &str| params.get(field).cloned().unwrap_or_default();
    let query = query_parameters(request.uri().query().unwrap_or(""));
    let op = match (method, path) {
        ("POST", "/mcp/servers") => McpRegistryOperation::CreateServer,
        ("GET", "/mcp/servers") => McpRegistryOperation::ListServers,
        ("GET", "/mcp/servers/{server_id}") => McpRegistryOperation::GetServer {
            server_id: id("server_id"),
        },
        ("PATCH", "/mcp/servers/{server_id}") => McpRegistryOperation::UpdateServer {
            server_id: id("server_id"),
        },
        ("DELETE", "/mcp/servers/{server_id}") => McpRegistryOperation::DeleteServer {
            server_id: id("server_id"),
        },
        ("POST", "/mcp/servers/{server_id}/connections") => {
            McpRegistryOperation::CreateConnection {
                server_id: id("server_id"),
            }
        }
        ("GET", "/mcp/servers/{server_id}/connections") => McpRegistryOperation::ListConnections {
            server_id: id("server_id"),
        },
        ("DELETE", "/mcp/connections/{connection_id}") => McpRegistryOperation::RevokeConnection {
            connection_id: id("connection_id"),
        },
        ("PUT", "/mcp/connections/{connection_id}/channel-grants") => {
            McpRegistryOperation::GrantChannel {
                connection_id: id("connection_id"),
            }
        }
        ("GET", "/mcp/servers/{server_id}/tools") => McpRegistryOperation::ListTools {
            server_id: id("server_id"),
        },
        ("POST", "/mcp/servers/{server_id}/discover") => McpRegistryOperation::Discover {
            server_id: id("server_id"),
        },
        ("GET", "/assistant/mcp/servers") => McpRegistryOperation::LegacyListServers,
        ("GET", "/assistant/mcp/tools") => McpRegistryOperation::LegacyListTools,
        ("POST", "/assistant/mcp/servers/{server_name}/refresh") => {
            McpRegistryOperation::LegacyRefresh {
                server_name: id("server_name"),
                connection_id: query.get("connection_id").cloned(),
                principal_type: query
                    .get("principal_type")
                    .cloned()
                    .unwrap_or_else(|| "service_account".into()),
            }
        }
        _ => {
            let allow = MCP_REGISTRY_ROUTES
                .iter()
                .find(|(_, p, _)| *p == path)
                .map(|(method, _, _)| *method)
                .unwrap_or("");
            return (
                StatusCode::METHOD_NOT_ALLOWED,
                [("allow", allow)],
                Json(json!({"detail":"Method Not Allowed"})),
            )
                .into_response();
        }
    };
    let status = op.status();
    let headers = request.headers().clone();
    let mut context = request
        .extensions()
        .get::<ConfigAuditRequestContext>()
        .cloned()
        .or_else(|| {
            request
                .extensions()
                .get::<Extension<ConfigAuditRequestContext>>()
                .map(|Extension(c)| c.clone())
        })
        .unwrap_or_else(|| state.api.dependencies.identity.context(&headers));
    context.headers = headers.clone();
    let body = if op.body_kind().is_some() {
        let bytes = match Bytes::from_request(request, &state).await {
            Ok(v) => v,
            Err(error) => return error.into_response(),
        };
        if bytes.is_empty() {
            Value::Null
        } else if crate::http_content_type::is_json_content_type(&headers) {
            match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(error) => {
                    // Exact Python decoder positions for non-ASCII malformed JSON
                    // remain a host transport parity boundary, never raw input.
                    let prefix = bytes
                        .split(|b| *b == b'\n')
                        .take(error.line().saturating_sub(1))
                        .map(|line| line.len() + 1)
                        .sum::<usize>();
                    let offset = prefix
                        + if error.is_eof() {
                            error.column()
                        } else {
                            error.column().saturating_sub(1)
                        };
                    let failure = redacted_validation(
                        &mut context,
                        vec![field_error(
                            ["body".into(), offset.to_string()],
                            "json_invalid",
                        )],
                    );
                    return respond(&state, Err(failure), status, &context).await;
                }
            }
        } else {
            json!(String::from_utf8_lossy(&bytes))
        }
    } else {
        Value::Null
    };
    let identity = match state
        .api
        .dependencies
        .identity
        .resolve(&headers, &op.path())
        .await
    {
        Ok(id) => id,
        Err(error) => return respond(&state, Err(error), status, &context).await,
    };
    let result = state.api.execute(op, &identity, &body, &mut context).await;
    let mut response = respond(&state, result, status, &context).await;
    // Retain canonical mutation/error correlation for host middleware without
    // inventing an additional wire header absent from the source route.
    response.extensions_mut().insert(context);
    response
}
