//! Agent resource seals reusing live Knowledge and immutable Skill authorities.
use super::studio_sql as queries;
use crate::migration_agent::{
    Actor, AgentError, AgentFuture,
    entry::{SourceAuthority, SourceRefs},
    studio::{SealedCapabilities, StudioResources},
};
use crate::migration_content::skills::manifest_from_artifact;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub struct NativeStudioResources {
    pub pool: PgPool,
    pub knowledge: Arc<dyn SourceAuthority>,
}
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn validation(field: &str, code: &str, message: &str) -> AgentError {
    AgentError::validation(vec![json!({"field":field,"code":code,"message":message})])
}
impl NativeStudioResources {
    async fn seal(
        &self,
        actor: &Actor,
        spec: &Value,
        seal_version: bool,
        conn: &mut PgConnection,
    ) -> Result<SealedCapabilities, AgentError> {
        let mut seen = BTreeSet::new();
        let mut bindings = Vec::new();
        for b in spec["capabilities"].as_array().into_iter().flatten() {
            let (Some(kind), Some(resource)) = (b["type"].as_str(), b["resource_id"].as_str())
            else {
                continue;
            };
            if !seen.insert((kind.to_owned(), resource.to_owned())) {
                continue;
            }
            bindings.push(json!({"capability_type":kind,"resource_id":resource,"resource_version":b["resource_version"],"schema_hash":b["schema_hash"],"config":if b["config"].is_object(){b["config"].clone()}else{json!({})}}));
        }
        let mut ids = BTreeSet::new();
        for b in &bindings {
            if b["capability_type"] == "skill" {
                let id = uuid::Uuid::parse_str(text(b, "resource_id")).map_err(|_| {
                    validation(
                        "capabilities",
                        "AGENT_SKILL_VERSION_INVALID",
                        "Skill bindings require an exact skill_version_id",
                    )
                })?;
                ids.insert(id.to_string());
            }
        }
        let mut artifacts = BTreeMap::new();
        if !ids.is_empty() {
            let query = format!(
                "SELECT to_jsonb(selected) FROM ({}) selected",
                queries::SKILLS_0
            );
            let rows: Vec<Value> = sqlx::query_scalar(&query)
                .bind(&actor.tenant_id)
                .bind(ids.iter().cloned().collect::<Vec<_>>())
                .bind(&actor.user_id)
                .fetch_all(ai_platform_gateway_foundation::tenant_database::connection(
                    &mut *conn,
                    &actor.tenant_id,
                ))
                .await?;
            for row in rows {
                let manifest = manifest_from_artifact(&row).map_err(|e| {
                    validation(
                        "capabilities",
                        e.detail
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("SKILL_VERSION_CONTENT_INVALID"),
                        "Skill version content is unavailable",
                    )
                })?;
                let id = text(&row, "version_id").to_owned();
                artifacts.insert(id.clone(),json!({"skill_id":row["skill_id"],"skill_version_id":id,"skill_name":manifest["name"],"content_hash":manifest["content_hash"]}));
            }
            if artifacts.keys().cloned().collect::<BTreeSet<_>>() != ids {
                return Err(validation(
                    "capabilities",
                    "AGENT_SKILL_VERSION_UNAVAILABLE",
                    "one or more Skill versions are unavailable",
                ));
            }
            let names: BTreeSet<_> = artifacts.values().map(|v| text(v, "skill_name")).collect();
            if names.len() != artifacts.len() {
                return Err(validation(
                    "capabilities",
                    "AGENT_SKILL_NAME_CONFLICT",
                    "an Agent cannot bind multiple versions of one Skill",
                ));
            }
        }
        let mut errors = Vec::new();
        for (index, b) in bindings.iter_mut().enumerate() {
            match text(b, "capability_type") {
                "skill" => {
                    let id = uuid::Uuid::parse_str(text(b, "resource_id"))
                        .map_err(|_| {
                            validation(
                                "capabilities",
                                "AGENT_SKILL_VERSION_INVALID",
                                "Skill bindings require an exact skill_version_id",
                            )
                        })?
                        .to_string();
                    let a = &artifacts[&id];
                    b["resource_id"] = a["skill_name"].clone();
                    b["resource_version"] = a["skill_version_id"].clone();
                    b["schema_hash"] = a["content_hash"].clone();
                }
                "mcp" | "connector" if seal_version => {
                    if let Some(code) = validate_binding(conn, &actor.tenant_id, b).await? {
                        errors.push(json!({"field":format!("capabilities[{index}]"),"code":code,"message":"Capability binding is unavailable or changed"}));
                    }
                }
                _ => {}
            }
        }
        if !errors.is_empty() {
            return Err(AgentError::validation(errors));
        }
        Ok(SealedCapabilities {
            capabilities: bindings,
            skills: artifacts.into_values().collect(),
        })
    }
}
impl StudioResources for NativeStudioResources {
    fn authorize_knowledge<'a>(
        &'a self,
        actor: &'a Actor,
        ids: &'a [String],
    ) -> AgentFuture<'a, BTreeSet<String>> {
        Box::pin(async move {
            let refs = SourceRefs {
                datasets: ids.iter().cloned().collect(),
                ..Default::default()
            };
            if refs.datasets.is_empty() {
                return Ok(BTreeSet::new());
            }
            if self
                .knowledge
                .allowed(&actor.scope("agent-resource-authorization"), &refs)
                .await?
            {
                Ok(refs.datasets)
            } else {
                Ok(BTreeSet::new())
            }
        })
    }
    fn seal_capabilities<'a>(
        &'a self,
        actor: &'a Actor,
        spec: &'a Value,
        seal_version: bool,
    ) -> AgentFuture<'a, SealedCapabilities> {
        Box::pin(async move {
            let mut conn = self.pool.acquire().await?;
            self.seal(actor, spec, seal_version, &mut conn).await
        })
    }
    fn seal_capabilities_in<'a>(
        &'a self,
        actor: &'a Actor,
        spec: &'a Value,
        seal_version: bool,
        conn: &'a mut PgConnection,
    ) -> AgentFuture<'a, SealedCapabilities> {
        Box::pin(self.seal(actor, spec, seal_version, conn))
    }
}
async fn validate_binding(
    conn: &mut PgConnection,
    tenant: &str,
    b: &Value,
) -> Result<Option<&'static str>, AgentError> {
    let config = &b["config"];
    let principal = text(config, "principal_type");
    let principals = ["service_account", "user_delegated"];
    let resource = text(b, "resource_id");
    let risk = text(config, "risk");
    if b["capability_type"] == "mcp" {
        let connection = text(config, "connection_id");
        let hash = text(b, "schema_hash");
        if !principals.contains(&principal) || connection.is_empty() || hash.is_empty() {
            return Ok(Some("MCP_BINDING_INCOMPLETE"));
        }
        let query = format!(
            "SELECT to_jsonb(selected) FROM ({}) selected",
            queries::BINDING_0
        );
        let tool: Option<Value> = sqlx::query_scalar(&query)
            .bind(tenant)
            .bind(resource)
            .bind(connection)
            .bind(principal)
            .fetch_optional(ai_platform_gateway_foundation::tenant_database::connection(
                &mut *conn, tenant,
            ))
            .await?;
        let Some(tool) = tool else {
            return Ok(Some("MCP_CAPABILITY_UNAVAILABLE"));
        };
        if text(&tool, "schema_hash") != hash.strip_prefix("sha256:").unwrap_or(hash) {
            return Ok(Some("MCP_SCHEMA_CHANGED"));
        }
        if risk.is_empty() || text(&tool, "risk_level") != risk {
            return Ok(Some("MCP_RISK_CHANGED"));
        }
        if tool["auth_method"] == "oauth"
            && (text(&tool, "oauth_audience").is_empty()
                || tool["audience"] != tool["oauth_audience"]
                || tool["scopes"].as_array().is_none_or(Vec::is_empty))
        {
            return Ok(Some("MCP_OAUTH_BINDING_INVALID"));
        }
        return Ok(None);
    }
    let provider = text(config, "provider");
    let grant = text(config, "grant_id");
    let tool = text(config, "tool_name");
    if provider.is_empty() || tool != resource {
        return Ok(Some("CONNECTOR_BINDING_INCOMPLETE"));
    }
    if !principal.is_empty() || !grant.is_empty() {
        if provider != "confluence"
            || !principals.contains(&principal)
            || grant.is_empty()
            || !["confluence_read", "confluence_write"].contains(&tool)
        {
            return Ok(Some("CONNECTOR_BINDING_INCOMPLETE"));
        }
        if risk
            != if tool == "confluence_read" {
                "low"
            } else {
                "high"
            }
        {
            return Ok(Some("CONNECTOR_RISK_INVALID"));
        }
        let query = format!(
            "SELECT to_jsonb(selected) FROM ({}) selected",
            queries::BINDING_1
        );
        let row: Option<Value> = sqlx::query_scalar(&query)
            .bind(tenant)
            .bind(grant)
            .bind(provider)
            .bind(principal)
            .fetch_optional(ai_platform_gateway_foundation::tenant_database::connection(
                &mut *conn, tenant,
            ))
            .await?;
        let Some(row) = row else {
            return Ok(Some("CONNECTOR_CAPABILITY_UNAVAILABLE"));
        };
        let scopes: Vec<_> = row["scopes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_lowercase)
            .collect();
        if !scopes.iter().any(|s| {
            s == if tool == "confluence_read" {
                "read"
            } else {
                "write"
            } || s.starts_with("write:")
                || tool == "confluence_read" && s.starts_with("read:")
        }) {
            return Ok(Some("CONNECTOR_SCOPE_DENIED"));
        }
        return Ok(None);
    }
    let query = format!(
        "SELECT to_jsonb(selected) FROM ({}) selected",
        queries::BINDING_2
    );
    let row: Option<Value> = sqlx::query_scalar(&query)
        .bind(provider)
        .bind(tenant)
        .fetch_optional(ai_platform_gateway_foundation::tenant_database::connection(
            &mut *conn, tenant,
        ))
        .await?;
    let Some(row) = row else {
        return Ok(Some("CONNECTOR_CATALOG_UNAVAILABLE"));
    };
    if row["enabled"] != true {
        return Ok(Some("CONNECTOR_CATALOG_UNAVAILABLE"));
    }
    if row["mode"] == "ingest" {
        return Ok(Some("CONNECTOR_CATALOG_INGEST_ONLY"));
    }
    Ok(None)
}
