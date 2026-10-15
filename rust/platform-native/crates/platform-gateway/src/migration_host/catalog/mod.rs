//! Gateway-owned Worker V2 catalog authority. Local resolution avoids Gateway self-HTTP.
mod descriptor;
pub mod http;
use crate::assistant_catalog::{CapabilityCatalog, lifecycle};
use crate::migration_agent::{AgentError, AgentFuture, Scope, control::RuntimeCatalog};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
fn error(code: &str) -> AgentError {
    AgentError::code(503, code)
}
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn pystr(v: &Value) -> String {
    match v {
        Value::String(v) => v.clone(),
        Value::Null => "None".into(),
        Value::Bool(v) => if *v { "True" } else { "False" }.into(),
        v => v.to_string(),
    }
}
#[derive(Clone)]
pub struct CatalogService {
    pool: PgPool,
    client: reqwest::Client,
    worker_url: String,
    internal_token: String,
    records: Arc<CapabilityCatalog>,
    web_search_configured: bool,
}
impl CatalogService {
    pub fn new(
        pool: PgPool,
        worker_url: String,
        internal_token: String,
        records: Arc<CapabilityCatalog>,
        web_search_configured: bool,
    ) -> Result<Self, AgentError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(2))
            .read_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|_| error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"))?;
        Ok(Self {
            pool,
            client,
            worker_url: worker_url.trim().trim_end_matches('/').into(),
            internal_token: internal_token.trim().into(),
            records,
            web_search_configured,
        })
    }
    async fn identity_policy(
        &self,
        scope: &Scope,
    ) -> Result<(BTreeSet<String>, Value), AgentError> {
        let unavailable = || error("CAPABILITY_CATALOG_IDENTITY_UNAVAILABLE");
        let user: Value =
            sqlx::query_scalar("SELECT to_jsonb(u) FROM users u WHERE user_id=$1 AND tenant_id=$2")
                .bind(&scope.user_id)
                .bind(&scope.tenant_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(|_| unavailable())?
                .ok_or_else(|| AgentError::code(403, "CAPABILITY_CATALOG_SCOPE_UNAUTHORIZED"))?;
        let mut subjects: BTreeSet<String> = user["roles"]
            .as_array()
            .into_iter()
            .flatten()
            .chain(user["permissions"].as_array().into_iter().flatten())
            .map(pystr)
            .collect();
        let roles:Vec<String>=sqlx::query_scalar("SELECT role_name FROM user_roles WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW()) ORDER BY granted_at").bind(&scope.user_id).fetch_all(&self.pool).await.map_err(|_|unavailable())?;
        subjects.extend(roles);
        // The canonical database authority tolerates missing permission tables and uses the documented fallbacks.
        let mut permissions:BTreeSet<String>=sqlx::query_scalar::<_,String>("SELECT DISTINCT rp.permission_code FROM user_roles ur JOIN role_permissions rp ON ur.role_name=rp.role_name WHERE ur.user_id=$1 AND (ur.expires_at IS NULL OR ur.expires_at>NOW())").bind(&scope.user_id).fetch_all(&self.pool).await.unwrap_or_default().into_iter().collect();
        if permissions.is_empty() {
            permissions.extend(sqlx::query_scalar::<_,String>("SELECT DISTINCT unnest(rr.permissions) FROM user_roles ur JOIN rbac_roles rr ON ur.role_name=rr.role_name WHERE ur.user_id=$1 AND (ur.expires_at IS NULL OR ur.expires_at>NOW())").bind(&scope.user_id).fetch_all(&self.pool).await.unwrap_or_default());
        }
        if permissions.is_empty() {
            permissions.extend(
                user["permissions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(pystr),
            );
            let own_roles: Vec<String> = user["roles"]
                .as_array()
                .into_iter()
                .flatten()
                .map(pystr)
                .collect();
            if !own_roles.is_empty() {
                permissions.extend(sqlx::query_scalar::<_,String>("SELECT unnest(permissions) FROM rbac_roles WHERE role_name=ANY($1::text[])").bind(own_roles).fetch_all(&self.pool).await.unwrap_or_default());
            }
        }
        permissions.extend(sqlx::query_scalar::<_,String>("SELECT permission_code FROM user_permissions WHERE user_id=$1 AND (expires_at IS NULL OR expires_at>NOW())").bind(&scope.user_id).fetch_all(&self.pool).await.unwrap_or_default());
        subjects.extend(permissions);
        let policy=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('allowed_tools',allowed_tools,'blocked_tools',blocked_tools,'allowed_categories',allowed_categories) FROM tenant_tool_policies WHERE tenant_id=$1").bind(&scope.tenant_id).fetch_optional(&self.pool).await.map_err(|_|error("CAPABILITY_CATALOG_POLICY_UNAVAILABLE"))?.unwrap_or(json!({}));
        Ok((subjects, policy))
    }
    pub async fn resolve(
        &self,
        scope: &Scope,
        model: &str,
        revision: i64,
        allowlist: Option<&[Value]>,
    ) -> Result<Value, AgentError> {
        if [
            &scope.tenant_id,
            &scope.user_id,
            &scope.session_id,
            &model.to_owned(),
        ]
        .iter()
        .any(|v| v.is_empty() || v.chars().count() > 255 || v.chars().any(|c| (c as u32) < 32))
            || revision < 1
            || allowlist.is_some_and(|a| a.iter().any(|v| !v.is_object()))
        {
            return Err(AgentError::code(403, "CAPABILITY_CATALOG_SCOPE_INVALID"));
        }
        let (subjects, policy) = self.identity_policy(scope).await?;
        let bindings = lifecycle::load_bindings(&self.pool, &scope.tenant_id).await
            .map_err(|_| error("CAPABILITY_CATALOG_PLUGIN_POLICY_UNAVAILABLE"))?;
        let lifecycle_denied = lifecycle::denied_capabilities(&self.records, &bindings)
            .map_err(|_| error("CAPABILITY_CATALOG_PLUGIN_POLICY_UNAVAILABLE"))?;
        let url = reqwest::Url::parse(&self.worker_url)
            .map_err(|_| error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"))?;
        if !["http", "https"].contains(&url.scheme())
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !["", "/"].contains(&url.path())
            || self.internal_token.is_empty()
        {
            return Err(error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"));
        }
        let headers = super::context::internal_http_headers(
            BTreeMap::from([
                (
                    "x-ai-platform-internal-token".into(),
                    self.internal_token.clone(),
                ),
                ("x-ai-tenant-id".into(), scope.tenant_id.clone()),
                ("x-ai-user-id".into(), scope.user_id.clone()),
                ("x-ai-session-id".into(), scope.session_id.clone()),
            ]),
            None,
            None,
            None,
        )
        .map_err(|_| error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"))?;
        let mut request = self.client.post(format!(
            "{}/internal/v2/capabilities/catalog",
            self.worker_url
        ));
        for (k, v) in headers {
            request = request.header(k, v);
        }
        let mut response=request.json(&json!({"tenant_id":scope.tenant_id,"user_id":scope.user_id,"session_id":scope.session_id,"capability_revision":revision,"schema_version":"capability-catalog/v2"})).send().await.map_err(|_|error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"))?;
        if response.status().as_u16() >= 400 {
            return Err(error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"));
        }
        // Deployment identity is independent of the request's model revision.
        // Tombstones preserve this expectation when every managed pack is off.
        verify_activation_identity(&self.records, response.headers().get("x-ai-platform-plugin-activation")
            .and_then(|value| value.to_str().ok()))?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(error("CAPABILITY_CATALOG_WORKER_UNAVAILABLE"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let envelope: Value =
            serde_json::from_slice(&bytes).map_err(|_| error("CAPABILITY_CATALOG_INVALID"))?;
        let keys = ["schema_version", "capability_revision", "capabilities"];
        if envelope
            .as_object()
            .is_none_or(|m| m.len() != 3 || keys.iter().any(|k| !m.contains_key(*k)))
            || envelope["schema_version"] != "capability-catalog/v2"
            || envelope["capability_revision"] != revision
            || envelope["capabilities"]
                .as_array()
                .is_none_or(|a| a.len() > 256)
        {
            return Err(error("CAPABILITY_CATALOG_INVALID"));
        }
        let allowed = descriptor::policy_set(&policy["allowed_tools"])?;
        let blocked = descriptor::policy_set(&policy["blocked_tools"])?;
        let categories = descriptor::policy_set(&policy["allowed_categories"])?;
        let mut seen = BTreeSet::new();
        let mut result = json!({"schema_version":"agent-capability-catalog/v1","capability_revision":revision,"tools":[],"mcp":[],"deferred":[]});
        for raw in envelope["capabilities"].as_array().unwrap() {
            let record = self.records.records().iter().find(|r| r["id"] == raw["id"]);
            let descriptor = descriptor::Descriptor::parse(raw, record)?;
            let name = text(raw, "name");
            if !seen.insert(name) {
                return Err(error("CAPABILITY_CATALOG_INVALID"));
            }
            if allowlist.is_none() && ["todo_read", "todo_write"].contains(&name)
                || name == "search_web" && !self.web_search_configured
                || !descriptor::permitted(&subjects, &descriptor.required)
                || !allowed.is_empty() && !allowed.contains(name)
                || blocked.contains(name)
                || lifecycle_denied.contains(name)
                || !categories.is_empty() && !categories.contains(&descriptor.category)
            {
                continue;
            }
            let projected = descriptor.project(&scope.tenant_id, revision);
            let key = if projected["read_only"] == true {
                if projected["kind"] == "mcp" {
                    "mcp"
                } else {
                    "tools"
                }
            } else {
                "deferred"
            };
            result[key].as_array_mut().unwrap().push(projected);
        }
        Ok(result)
    }
}
impl RuntimeCatalog for CatalogService {
    fn fetch<'a>(
        &'a self,
        scope: &'a Scope,
        model: &'a str,
        revision: i64,
        allowlist: Option<&'a [Value]>,
    ) -> AgentFuture<'a, Value> {
        Box::pin(self.resolve(scope, model, revision, allowlist))
    }
}

fn verify_activation_identity(records: &CapabilityCatalog, observed: Option<&str>) -> Result<(), AgentError> {
    let expected: BTreeSet<_> = records.records().iter()
        .filter_map(|record| record["plugin_metadata"]["activation_id"].as_str()).collect();
    if expected.len() > 1 || match expected.iter().next() {
        Some(expected) => observed != Some(*expected),
        None => observed.is_some_and(|value| value != "none"),
    } {
        return Err(error("CAPABILITY_PLUGIN_ACTIVATION_MISMATCH"));
    }
    Ok(())
}
