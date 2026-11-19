//! Runtime authorization filters immutable sealed bindings; resolver output can
//! never replace a Version's risk, schema, principal or configuration.
use super::{Actor, AgentError, AgentFuture, text};
use crate::{
    connector_principal_store::ConnectorPrincipalRepository, mcp_registry_store::McpContext,
    migration_content::skills::SkillRepository,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub trait RuntimeResources: Send + Sync {
    fn effective<'a>(
        &'a self,
        actor: &'a Actor,
        bindings: &'a [Value],
        channel: &'a str,
        policy: &'a Value,
    ) -> AgentFuture<'a, Vec<Value>>;
}
pub trait RuntimeKnowledge: Send + Sync {
    fn effective<'a>(
        &'a self,
        actor: &'a Actor,
        bindings: &'a [Value],
        channel: &'a str,
        agent: &'a str,
    ) -> AgentFuture<'a, BTreeSet<String>>;
}
pub fn authenticated(actor: &Actor) -> bool {
    actor.tier != "anonymous"
        && !actor.roles.iter().any(|s| s == "guest")
        && !actor.user_id.is_empty()
}
pub struct NativeRuntimeResources {
    pub pool: PgPool,
    pub connectors: Arc<ConnectorPrincipalRepository>,
    pub skills: Arc<SkillRepository>,
    pub mcp_enabled: bool,
}
impl NativeRuntimeResources {
    async fn authorize_mcp(
        &self,
        actor: &Actor,
        binding: &Value,
        channel: &str,
    ) -> Result<(), AgentError> {
        let config = &binding["config"];
        let principal = text(config, "principal_type");
        if !matches!(principal, "service_account" | "user_delegated")
            || !matches!(
                channel,
                "preview" | "hosted_private" | "hosted_public" | "embed" | "api"
            )
        {
            return Err(AgentError::code(403, "MCP_PRINCIPAL_POLICY_DENIED"));
        }
        let connection = super::uuid(text(config, "connection_id"))?;
        let schema = text(binding, "schema_hash").trim_start_matches("sha256:");
        let row:Value=sqlx::query_scalar("SELECT jsonb_build_object('tool_id',t.tool_id,'schema_hash',s.schema_hash,'risk_level',s.risk_level,'circuit_state',srv.circuit_state,'circuit_open_until',srv.circuit_open_until,'auth_method',srv.auth_method,'oauth_audience',srv.oauth_audience,'audience',c.audience,'owner_user_id',c.owner_user_id) FROM mcp_tools t JOIN mcp_tool_snapshots s ON s.tenant_id=t.tenant_id AND s.snapshot_id=t.current_snapshot_id JOIN mcp_servers srv ON srv.tenant_id=t.tenant_id AND srv.server_id=t.server_id JOIN mcp_connections c ON c.tenant_id=t.tenant_id AND c.server_id=t.server_id WHERE t.tenant_id=$1 AND t.runtime_name=$2 AND s.schema_hash=$3 AND c.connection_id=$4::uuid AND c.principal_type=$5 AND t.enabled=TRUE AND srv.enabled=TRUE AND srv.deleted_at IS NULL AND c.enabled=TRUE AND c.revoked_at IS NULL AND(c.expires_at IS NULL OR c.expires_at>NOW())").bind(&actor.tenant_id).bind(binding["resource_id"].as_str().or_else(||binding["id"].as_str()).unwrap_or("")).bind(schema).bind(&connection).bind(principal).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &actor.tenant_id)).await?.ok_or_else(||AgentError::code(403,"MCP_CAPABILITY_UNAVAILABLE"))?;
        let risk = config["risk"]
            .as_str()
            .or_else(|| binding["risk"].as_str())
            .unwrap_or("");
        if row["risk_level"] != risk {
            return Err(AgentError::code(403, "MCP_RISK_CHANGED"));
        }
        if row["circuit_state"] == "open"
            && row["circuit_open_until"].as_str().is_none_or(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .map(|v| v.with_timezone(&chrono::Utc) > chrono::Utc::now())
                    .unwrap_or(true)
            })
        {
            return Err(AgentError::code(403, "MCP_CIRCUIT_OPEN"));
        }
        if row["auth_method"] == "oauth"
            && (text(&row, "oauth_audience").is_empty() || row["audience"] != row["oauth_audience"])
        {
            return Err(AgentError::code(403, "MCP_OAUTH_AUDIENCE_MISMATCH"));
        }
        let public = matches!(channel, "hosted_public" | "embed");
        if principal == "user_delegated"
            && (!authenticated(actor) || row["owner_user_id"] != actor.user_id || public)
        {
            return Err(AgentError::code(403, "MCP_DELEGATED_PRINCIPAL_DENIED"));
        }
        if principal == "service_account" && !authenticated(actor) && !public {
            return Err(AgentError::code(403, "MCP_SERVICE_PRINCIPAL_DENIED"));
        }
        if public {
            let allowed:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mcp_channel_grants WHERE tenant_id=$1 AND connection_id=$2::uuid AND tool_id=$3::uuid AND channel=$4 AND enabled=TRUE AND approved_schema_hash=$5 AND read_only_only=TRUE)").bind(&actor.tenant_id).bind(&connection).bind(text(&row,"tool_id")).bind(channel).bind(schema).fetch_one(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &actor.tenant_id)).await?;
            if !allowed {
                return Err(AgentError::code(403, "MCP_PUBLIC_CHANNEL_DENIED"));
            }
        }
        Ok(())
    }
}
impl RuntimeResources for NativeRuntimeResources {
    fn effective<'a>(
        &'a self,
        actor: &'a Actor,
        bindings: &'a [Value],
        channel: &'a str,
        _policy: &'a Value,
    ) -> AgentFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let context = McpContext {
                tenant_id: actor.tenant_id.clone(),
                user_id: actor.user_id.clone(),
                authenticated: authenticated(actor),
            };
            let mut seen = BTreeSet::new();
            let mut allowed = Vec::new();
            for binding in bindings {
                let kind = binding["capability_type"]
                    .as_str()
                    .or_else(|| binding["type"].as_str())
                    .unwrap_or("");
                let resource = binding["resource_id"]
                    .as_str()
                    .or_else(|| binding["id"].as_str())
                    .unwrap_or("");
                if resource.is_empty() || !seen.insert((kind.to_owned(), resource.to_owned())) {
                    continue;
                }
                let accepted = match kind {
                    "native" | "model_native" => true,
                    "skill" => {
                        let version = text(binding, "resource_version");
                        if version.is_empty() {
                            false
                        } else {
                            self.skills
                                .authorize_version(&actor.tenant_id, &actor.user_id, version, false)
                                .await
                                .is_ok_and(|a| a["name"] == resource && a["version_id"] == version)
                        }
                    }
                    _ if !self.mcp_enabled => false,
                    "mcp" => {
                        if text(binding, "schema_hash").is_empty()
                            || text(&binding["config"], "connection_id").is_empty()
                            || text(&binding["config"], "principal_type").is_empty()
                        {
                            false
                        } else {
                            self.authorize_mcp(actor, binding, channel).await.is_ok()
                        }
                    }
                    "connector" => self
                        .connectors
                        .authorize_connector_binding(&context, binding, channel)
                        .await
                        .ok()
                        .flatten()
                        .is_some(),
                    _ => false,
                };
                if accepted {
                    allowed.push(binding.clone());
                }
            }
            Ok(allowed)
        })
    }
}
