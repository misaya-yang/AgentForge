//! Explicit connector credential grants and catalog authorization.
//!
//! Canonical: `mcp_repository.py` connector methods, lines 1214–1457.
//! Reuses the existing supervised `McpDatabase`/`PgMcpDatabase` executor and
//! `SqlMcpRepository` audit writer. No connection, task, or mutation is started
//! during construction. Hosts retain the concrete PgMcpDatabase to drain cancelled
//! operations after aborting and joining request futures, before closing pools.
//! Management results are redacted. Authorization results are gateway-internal
//! and intentionally retain secret_ref; never serialize them as public responses.
use crate::{
    connector_principal_validation::{
        CONNECTOR_CHANNELS, CONNECTOR_METADATA_FIELDS, CONNECTOR_PRINCIPAL_FIELDS, principal_type,
        public_channel,
    },
    mcp_registry_store::{
        McpArgument, McpContext, McpDatabase, McpDatabaseError, McpError, McpFuture, McpRepository,
        McpStatement, SqlMcpRepository, validate_secret_ref,
    },
    protocol::ProtocolTimestamp,
    service_registry::python_truthy,
    usage_parser::python_value_to_string,
};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;

pub struct ConnectorPrincipalRepository {
    enabled: bool,
    database: Option<Arc<dyn McpDatabase>>,
    audit: SqlMcpRepository,
}
impl ConnectorPrincipalRepository {
    pub fn new(enabled: bool, database: Option<Arc<dyn McpDatabase>>) -> Self {
        Self {
            enabled,
            audit: SqlMcpRepository::new(enabled, database.clone()),
            database,
        }
    }
    fn database(&self) -> Result<&dyn McpDatabase, McpError> {
        if !self.enabled {
            return Err(McpError::Storage);
        }
        self.database.as_deref().ok_or(McpError::Storage)
    }
    async fn rows(
        &self,
        name: &'static str,
        sql: &str,
        arguments: Vec<McpArgument>,
        conflict: &str,
    ) -> Result<Vec<Value>, McpError> {
        self.database()?
            .run(McpStatement {
                name,
                sql: sql.into(),
                arguments,
                returns_rows: true,
            })
            .await
            .map_err(|error| match error {
                McpDatabaseError::Cancelled => McpError::Cancelled,
                McpDatabaseError::Database { sqlstate } if sqlstate.as_deref() == Some("23505") => {
                    McpError::Conflict(conflict.into())
                }
                McpDatabaseError::Database { .. } => McpError::Unhandled,
            })
    }
    pub async fn create_connector_principal(
        &self,
        context: &McpContext,
        provider: &str,
        arguments: &Value,
    ) -> Result<Value, McpError> {
        self.database()?;
        validate_principal(provider, arguments)?;
        let rows = self
            .rows(
                "create_connector_principal",
                r#"
            INSERT INTO connector_credential_principals (
                tenant_id, provider, principal_type, owner_user_id,
                secret_ref, scopes, audience, connection_metadata,
                allowed_channels, expires_at, created_by, updated_by
            ) VALUES (
                $1, $2, $3, $4, $5, $6::text[], $7, $8::jsonb,
                $9::text[], $10, $11, $11
            )
            RETURNING grant_id, provider, principal_type, owner_user_id,
                      scopes, audience, connection_metadata, allowed_channels,
                      expires_at, revoked_at, enabled, created_at, updated_at,
                      TRUE AS credential_configured"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    McpArgument::text(provider),
                    text(arguments, "principal_type")?,
                    text(arguments, "owner_user_id")?,
                    text(arguments, "secret_ref")?,
                    text_array(arguments, "scopes")?,
                    text(arguments, "audience")?,
                    McpArgument::Json(arguments["connection_metadata"].clone()),
                    text_array(arguments, "allowed_channels")?,
                    timestamp(arguments, "expires_at")?,
                    McpArgument::text(&context.user_id),
                ],
                "CONNECTOR_PRINCIPAL_CONFLICT",
            )
            .await?;
        redact_principal(rows.into_iter().next().ok_or(McpError::Unhandled)?)
    }
    pub async fn list_connector_principals(
        &self,
        context: &McpContext,
        provider: &str,
    ) -> Result<Vec<Value>, McpError> {
        self.rows(
            "list_connector_principals",
            r#"
            SELECT grant_id, provider, principal_type, owner_user_id,
                   scopes, audience, connection_metadata, allowed_channels,
                   expires_at, revoked_at, enabled, created_at, updated_at,
                   (secret_ref IS NOT NULL) AS credential_configured
            FROM connector_credential_principals
            WHERE tenant_id = $1 AND provider = $2
            ORDER BY created_at DESC, grant_id"#,
            vec![
                McpArgument::text(&context.tenant_id),
                McpArgument::text(provider),
            ],
            "MCP_RESOURCE_CONFLICT",
        )
        .await?
        .into_iter()
        .map(redact_principal)
        .collect()
    }
    pub async fn revoke_connector_principal(
        &self,
        context: &McpContext,
        grant_id: &str,
    ) -> Result<(), McpError> {
        let rows = self
            .rows(
                "revoke_connector_principal",
                r#"
            UPDATE connector_credential_principals
            SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                updated_at = NOW(), updated_by = $3
            WHERE tenant_id = $1 AND grant_id = $2::uuid
            RETURNING grant_id"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    McpArgument::text(grant_id),
                    McpArgument::text(&context.user_id),
                ],
                "MCP_RESOURCE_CONFLICT",
            )
            .await?;
        if rows.is_empty() {
            return Err(McpError::NotFound("CONNECTOR_PRINCIPAL_NOT_FOUND".into()));
        }
        Ok(())
    }
    /// Source deliberately accepts only Confluence grants, and does not impose
    /// catalog enabled/mode checks on this separate Secret-ref credential model.
    #[allow(clippy::too_many_arguments)]
    pub async fn authorize_connector_tool(
        &self,
        context: &McpContext,
        provider: &str,
        tool_name: &str,
        principal: &str,
        grant_id: &str,
        channel: &str,
    ) -> Result<Value, McpError> {
        self.database()?;
        if provider != "confluence"
            || !matches!(tool_name, "confluence_read" | "confluence_write")
            || !principal_type(principal)
        {
            return Err(authorization("CONNECTOR_PRINCIPAL_POLICY_DENIED"));
        }
        let mut item = self
            .rows(
                "authorize_connector_tool",
                r#"
            SELECT * FROM connector_credential_principals
            WHERE tenant_id = $1 AND grant_id = $2::uuid
              AND provider = $3 AND principal_type = $4
              AND enabled = TRUE AND revoked_at IS NULL
              AND (expires_at IS NULL OR expires_at > NOW())"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    McpArgument::text(grant_id),
                    McpArgument::text(provider),
                    McpArgument::text(principal),
                ],
                "MCP_RESOURCE_CONFLICT",
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| authorization("CONNECTOR_CAPABILITY_UNAVAILABLE"))?;
        if !item.is_object() {
            return Err(McpError::Unhandled);
        }
        item["connection_metadata"] = json_object(&item["connection_metadata"]);
        if principal == "user_delegated"
            && (!context.authenticated
                || context.user_id.is_empty()
                || item["owner_user_id"].as_str() != Some(context.user_id.as_str())
                || public_channel(channel))
        {
            return Err(authorization("CONNECTOR_DELEGATED_PRINCIPAL_DENIED"));
        }
        if principal == "service_account" && !context.authenticated && !public_channel(channel) {
            return Err(authorization("CONNECTOR_SERVICE_PRINCIPAL_DENIED"));
        }
        if !item["allowed_channels"]
            .as_array()
            .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(channel)))
        {
            return Err(authorization("CONNECTOR_CHANNEL_DENIED"));
        }
        if public_channel(channel)
            && (principal != "service_account" || tool_name != "confluence_read")
        {
            return Err(authorization("CONNECTOR_PUBLIC_CHANNEL_DENIED"));
        }
        let scopes = item["scopes"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|v| python_value_to_string(v).to_lowercase())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let permitted = if tool_name == "confluence_read" {
            scopes
                .iter()
                .any(|s| s == "read" || s.starts_with("read:") || s.starts_with("write:"))
        } else {
            scopes
                .iter()
                .any(|s| s == "write" || s.starts_with("write:"))
        };
        if !permitted {
            return Err(authorization("CONNECTOR_SCOPE_DENIED"));
        }
        Ok(item)
    }
    /// Unlike the legacy OAuth lookup, a disabled tenant row shadows global.
    /// Arbitrary configured providers/tool names remain allowed at this boundary.
    pub async fn authorize_connector_catalog(
        &self,
        context: &McpContext,
        provider: &str,
        tool_name: &str,
        channel: &str,
    ) -> Result<Value, McpError> {
        self.database()?;
        if provider.is_empty() || tool_name.is_empty() {
            return Err(authorization("CONNECTOR_CATALOG_INCOMPLETE"));
        }
        let config = self
            .rows(
                "authorize_connector_catalog_config",
                r#"
            SELECT provider, mode, enabled FROM connector_configs
            WHERE provider = $1 AND (tenant_id = $2 OR tenant_id = '')
            ORDER BY tenant_id DESC LIMIT 1"#,
                vec![
                    McpArgument::text(provider),
                    McpArgument::text(&context.tenant_id),
                ],
                "MCP_RESOURCE_CONFLICT",
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| authorization("CONNECTOR_CATALOG_UNAVAILABLE"))?;
        if !python_truthy(&config["enabled"]) {
            return Err(authorization("CONNECTOR_CATALOG_UNAVAILABLE"));
        }
        if config["mode"] == "ingest" {
            return Err(authorization("CONNECTOR_CATALOG_INGEST_ONLY"));
        }
        if public_channel(channel) || !context.authenticated || context.user_id.is_empty() {
            return Err(authorization("CONNECTOR_CATALOG_PRINCIPAL_DENIED"));
        }
        let connection = self
            .rows(
                "authorize_connector_catalog_user",
                r#"
            SELECT user_id, provider, status FROM user_connectors
            WHERE tenant_id = $1 AND user_id = $2 AND provider = $3"#,
                vec![
                    McpArgument::text(&context.tenant_id),
                    McpArgument::text(&context.user_id),
                    McpArgument::text(provider),
                ],
                "MCP_RESOURCE_CONFLICT",
            )
            .await?
            .into_iter()
            .next();
        if !connection.is_some_and(|row| row["status"] == "connected") {
            return Err(authorization("CONNECTOR_CATALOG_NOT_CONNECTED"));
        }
        Ok(json!({"provider":provider,"tool_name":tool_name,"user_id":context.user_id}))
    }
    /// Reverse-consumer seam matching the connector branch of the canonical
    /// capability resolver. A partial grant is filtered, never treated as catalog.
    /// None means this binding is excluded, as in the source resolver's continue.
    pub async fn authorize_connector_binding(
        &self,
        context: &McpContext,
        binding: &Value,
        channel: &str,
    ) -> Result<Option<Value>, McpError> {
        let config = binding
            .get("config")
            .filter(|v| v.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let resource = python_fallback_text(&binding["resource_id"], &binding["id"]);
        let provider = python_fallback_text(&config["provider"], &Value::Null);
        let principal = python_fallback_text(&config["principal_type"], &Value::Null);
        let grant = python_fallback_text(&config["grant_id"], &Value::Null);
        let tool = if python_truthy(&config["tool_name"]) {
            python_value_to_string(&config["tool_name"])
        } else {
            resource.clone()
        };
        if tool != resource || provider.is_empty() || resource.is_empty() {
            return Ok(None);
        }
        let result = if !grant.is_empty() || !principal.is_empty() {
            if grant.is_empty() || principal.is_empty() {
                return Ok(None);
            }
            self.authorize_connector_tool(
                context, &provider, &resource, &principal, &grant, channel,
            )
            .await
        } else {
            self.authorize_connector_catalog(context, &provider, &resource, channel)
                .await
        };
        match result {
            Ok(_) => Ok(Some(binding.clone())),
            Err(
                McpError::Authorization(_)
                | McpError::Storage
                | McpError::Validation(_)
                | McpError::Conflict(_)
                | McpError::NotFound(_),
            ) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
impl McpRepository for ConnectorPrincipalRepository {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        context: &'a McpContext,
        arguments: Value,
    ) -> McpFuture<'a, Value> {
        Box::pin(async move {
            self.database()?;
            match operation {
                "create_connector_principal" => {
                    self.create_connector_principal(
                        context,
                        required(&arguments, "provider")?,
                        &arguments,
                    )
                    .await
                }
                "list_connector_principals" => Ok(Value::Array(
                    self.list_connector_principals(context, required(&arguments, "provider")?)
                        .await?,
                )),
                "revoke_connector_principal" => {
                    self.revoke_connector_principal(context, required(&arguments, "grant_id")?)
                        .await?;
                    Ok(Value::Null)
                }
                "authorize_connector_tool" => {
                    self.authorize_connector_tool(
                        context,
                        required(&arguments, "provider")?,
                        required(&arguments, "tool_name")?,
                        required(&arguments, "principal_type")?,
                        required(&arguments, "grant_id")?,
                        required(&arguments, "channel")?,
                    )
                    .await
                }
                "authorize_connector_catalog" => {
                    self.authorize_connector_catalog(
                        context,
                        required(&arguments, "provider")?,
                        required(&arguments, "tool_name")?,
                        required(&arguments, "channel")?,
                    )
                    .await
                }
                "audit_mutation" => self.audit.call(operation, context, arguments).await,
                _ => Err(McpError::Unhandled),
            }
        })
    }
}
fn validation(code: &str) -> McpError {
    McpError::Validation(code.into())
}
fn authorization(code: &str) -> McpError {
    McpError::Authorization(code.into())
}
fn required<'a>(value: &'a Value, key: &str) -> Result<&'a str, McpError> {
    value[key].as_str().ok_or(McpError::Unhandled)
}
fn optional<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>, McpError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(McpError::Unhandled),
    }
}
fn text(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    Ok(McpArgument::Text(optional(value, key)?.map(str::to_owned)))
}
fn strings(value: &Value) -> Result<Vec<String>, McpError> {
    value
        .as_array()
        .ok_or(McpError::Unhandled)?
        .iter()
        .map(|v| v.as_str().map(str::to_owned).ok_or(McpError::Unhandled))
        .collect()
}
fn text_array(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    Ok(McpArgument::TextArray(Some(strings(&value[key])?)))
}
fn timestamp(value: &Value, key: &str) -> Result<McpArgument, McpError> {
    Ok(McpArgument::Timestamp(match optional(value, key)? {
        None => None,
        Some(value) => {
            match ProtocolTimestamp::parse_isoformat(value).map_err(|_| McpError::Unhandled)? {
                ProtocolTimestamp::Offset(value) => Some(value.with_timezone(&Utc)),
                ProtocolTimestamp::Naive(_) => return Err(McpError::Unhandled),
            }
        }
    }))
}
fn json_object(value: &Value) -> Value {
    if value.is_object() {
        return value.clone();
    }
    value
        .as_str()
        .and_then(|v| serde_json::from_str::<Value>(v).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}
fn redact_principal(row: Value) -> Result<Value, McpError> {
    if !row.is_object() {
        return Err(McpError::Unhandled);
    }
    let mut result = serde_json::Map::new();
    for &key in CONNECTOR_PRINCIPAL_FIELDS {
        if let Some(value) = row.get(key) {
            result.insert(key.into(), value.clone());
        }
    }
    result.insert(
        "connection_metadata".into(),
        json_object(&row["connection_metadata"]),
    );
    Ok(Value::Object(result))
}
fn python_fallback_text(value: &Value, fallback: &Value) -> String {
    if python_truthy(value) {
        python_value_to_string(value)
    } else if python_truthy(fallback) {
        python_value_to_string(fallback)
    } else {
        String::new()
    }
}
fn validate_principal(provider: &str, value: &Value) -> Result<(), McpError> {
    let principal = value["principal_type"].as_str().unwrap_or("");
    if provider != "confluence" || !principal_type(principal) {
        return Err(validation("CONNECTOR_PRINCIPAL_INVALID"));
    }
    if principal == "service_account" && !value["owner_user_id"].is_null() {
        return Err(validation("CONNECTOR_PRINCIPAL_OWNER_INVALID"));
    }
    if principal == "user_delegated" && !python_truthy(&value["owner_user_id"]) {
        return Err(validation("CONNECTOR_PRINCIPAL_OWNER_REQUIRED"));
    }
    let channels = strings(&value["allowed_channels"])?;
    if channels
        .iter()
        .any(|v| !CONNECTOR_CHANNELS.contains(&v.as_str()))
    {
        return Err(validation("CONNECTOR_CHANNEL_INVALID"));
    }
    if principal == "user_delegated" && channels.iter().any(|v| public_channel(v)) {
        return Err(validation("CONNECTOR_PUBLIC_CHANNEL_DENIED"));
    }
    if !python_truthy(&value["scopes"]) {
        return Err(validation("CONNECTOR_SCOPES_REQUIRED"));
    }
    validate_secret_ref(optional(value, "secret_ref")?)?;
    if value["connection_metadata"]
        .as_object()
        .ok_or(McpError::Unhandled)?
        .keys()
        .any(|key| !CONNECTOR_METADATA_FIELDS.contains(&key.as_str()))
    {
        return Err(validation("CONNECTOR_METADATA_INVALID"));
    }
    Ok(())
}
