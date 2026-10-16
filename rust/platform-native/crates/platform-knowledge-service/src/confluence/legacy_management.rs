//! Source-equivalent dormant legacy management. It deliberately retains the
//! source's tier/owner ACL instead of substituting the manual dataset authority.
use super::*;
use query::types::UserContext;
use std::collections::BTreeSet;
#[derive(Clone)]
pub struct LegacyManagement {
    pub sources: Arc<Sources>,
    pub(super) clients: Arc<legacy_clients::LegacyClients>,
}
fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty())
}
pub fn verify_access(resource: &Value, user: &UserContext) -> Result<()> {
    if user.user_tier == "admin" || (!owner(resource).is_empty() && owner(resource) == user.user_id)
    {
        Ok(())
    } else {
        Err(Error::new(StatusCode::FORBIDDEN, "Access denied"))
    }
}
pub fn verify_import_access(
    connection: &Value,
    tenant: Option<&str>,
    created_by: Option<&str>,
    user: Option<&UserContext>,
) -> Result<()> {
    if let Some(tenant) = present(tenant) {
        if !text(connection, "tenant_id").is_empty() && text(connection, "tenant_id") != tenant {
            return Err(Error::new(StatusCode::FORBIDDEN, "Access denied"));
        }
    }
    if let Some(user) = user {
        verify_access(connection, user)
    } else if !owner(connection).is_empty() && Some(owner(connection)) != created_by {
        Err(Error::new(StatusCode::FORBIDDEN, "Access denied"))
    } else {
        Ok(())
    }
}
impl LegacyManagement {
    pub fn new(sources: Arc<Sources>) -> Self {
        Self {
            sources,
            clients: Arc::new(legacy_clients::LegacyClients::new(
                std::time::Duration::from_secs(300),
            )),
        }
    }
    pub async fn binding(&self, id: &str) -> Result<Option<Value>> {
        Ok(sqlx::query_scalar(
            "SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE binding_id=$1",
        )
        .bind(id)
        .fetch_optional(&self.sources.pool)
        .await?)
    }
    pub async fn get_binding(&self, id: &str, user: &UserContext) -> Result<Value> {
        let row = self
            .binding(id)
            .await?
            .ok_or_else(|| Error::missing("Binding not found"))?;
        verify_access(&row, user)?;
        Ok(row)
    }
    pub async fn get_connection(&self, id: &str, user: &UserContext) -> Result<Value> {
        let row = self
            .sources
            .connection(id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))?;
        verify_access(&row, user)?;
        Ok(row)
    }
    pub async fn get_page(&self, id: &str, user: &UserContext) -> Result<Value> {
        let row: Value =
            sqlx::query_scalar("SELECT to_jsonb(p) FROM confluence_pages p WHERE id=$1")
                .bind(id)
                .fetch_optional(&self.sources.pool)
                .await?
                .ok_or_else(|| Error::missing("Page record not found"))?;
        self.get_binding(text(&row, "binding_id"), user).await?;
        Ok(row)
    }
    async fn binding_rows(
        &self,
        connection: Option<&str>,
        tenant: Option<&str>,
        dataset: Option<&str>,
    ) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE ($1::text IS NULL OR connection_id=$1) AND ($2::text IS NULL OR tenant_id=$2) AND ($3::text IS NULL OR dataset_id=$3) ORDER BY created_at DESC LIMIT 100").bind(present(connection)).bind(present(tenant)).bind(present(dataset)).fetch_all(&self.sources.pool).await?)
    }
    pub async fn list_bindings(
        &self,
        user: &UserContext,
        connection: Option<&str>,
        tenant: Option<&str>,
        dataset: Option<&str>,
    ) -> Result<Vec<Value>> {
        let rows = self.binding_rows(connection, tenant, dataset).await?;
        Ok(rows
            .into_iter()
            .filter(|r| user.user_tier == "admin" || owner(r) == user.user_id)
            .collect())
    }
    pub async fn list_all_bindings(
        &self,
        user: &UserContext,
        tenant: &str,
        connection: Option<&str>,
        dataset: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<Value>> {
        Ok(self
            .list_bindings(user, connection, Some(tenant), dataset)
            .await?
            .into_iter()
            .filter(|r| present(status).is_none_or(|s| text(r, "status") == s))
            .collect())
    }
    async fn pages(
        &self,
        binding: &str,
        status: Option<&str>,
        limit: Option<i64>,
        offset: i64,
        synced_only: bool,
    ) -> Result<Vec<Value>> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(cp) || jsonb_build_object('document_status',d.status,'document_progress',d.progress,'effective_status',CASE WHEN cp.status='synced' AND cp.document_id IS NOT NULL AND (d.document_id IS NULL OR COALESCE(d.segment_count,0)=0) THEN 'needs_resync' ELSE cp.status END) FROM confluence_pages cp LEFT JOIN documents d ON cp.document_id=d.document_id WHERE cp.binding_id=$1 AND ($2::text IS NULL OR cp.status=$2) AND (NOT $3 OR cp.document_id IS NOT NULL) ORDER BY cp.title ASC LIMIT $4 OFFSET $5").bind(binding).bind(present(status)).bind(synced_only).bind(limit).bind(offset).fetch_all(&self.sources.pool).await?)
    }
    pub async fn list_pages(
        &self,
        binding: &str,
        user: &UserContext,
        status: Option<&str>,
        limit: i64,
        offset: i64,
        synced_only: bool,
    ) -> Result<Vec<Value>> {
        self.get_binding(binding, user).await?;
        self.pages(binding, status, Some(limit), offset, synced_only)
            .await
    }
    pub async fn cleanup_unsynced_pages(&self, binding: &str, user: &UserContext) -> Result<Value> {
        self.get_binding(binding, user).await?;
        let deleted =
            sqlx::query("DELETE FROM confluence_pages WHERE binding_id=$1 AND document_id IS NULL")
                .bind(binding)
                .execute(&self.sources.pool)
                .await?
                .rows_affected();
        Ok(json!({"binding_id":binding,"deleted":deleted}))
    }
    pub async fn refresh_binding_stats(&self, binding: &str) -> Result<()> {
        let pages = self.pages(binding, None, Some(10000), 0, false).await?;
        self.persist_binding(binding,&json!({"total_page_count":pages.len(),"synced_page_count":pages.iter().filter(|p|p["status"]=="synced").count()})).await
    }
    pub async fn list_sync_tasks(
        &self,
        user: &UserContext,
        binding: Option<&str>,
        status: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Value>> {
        let binding = present(binding);
        if let Some(binding) = binding {
            self.get_binding(binding, user).await?;
        }
        let mut tasks:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM confluence_sync_tasks t WHERE ($1::text IS NULL OR binding_id=$1) AND ($2::text IS NULL OR status=$2) ORDER BY priority DESC,created_at ASC LIMIT $3").bind(binding).bind(present(status)).bind(limit).fetch_all(&self.sources.pool).await?;
        if binding.is_none() && user.user_tier != "admin" {
            let bindings = self.list_bindings(user, None, None, None).await?;
            let ids = bindings
                .iter()
                .map(|b| text(b, "binding_id"))
                .collect::<BTreeSet<_>>();
            tasks.retain(|t| ids.contains(text(t, "binding_id")));
        }
        Ok(tasks)
    }
    pub async fn get_sync_task(&self, id: &str, user: &UserContext) -> Result<Option<Value>> {
        let row: Option<Value> =
            sqlx::query_scalar("SELECT to_jsonb(t) FROM confluence_sync_tasks t WHERE task_id=$1")
                .bind(id)
                .fetch_optional(&self.sources.pool)
                .await?;
        if let Some(row) = &row {
            if text(row, "binding_id").is_empty() {
                verify_access(row, user)?;
            } else {
                self.get_binding(text(row, "binding_id"), user).await?;
            }
        }
        Ok(row)
    }
    pub(super) async fn credentials(
        &self,
        connection: &str,
    ) -> Result<Arc<credentials::Credentials>> {
        self.clients.get(&self.sources, connection).await
    }
    async fn root_title(&self, connection: &str, id: &str) -> String {
        match self.credentials(connection).await {
            Ok(c) => self
                .sources
                .page(&c, id)
                .await
                .ok()
                .and_then(|p| p["title"].as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("Page {id}")),
            Err(_) => format!("Page {id}"),
        }
    }
    pub async fn update_binding(&self, id: &str, updates: &Value) -> Result<Option<Value>> {
        let Some(binding) = self.binding(id).await? else {
            return Ok(None);
        };
        let allowed = [
            "root_page_id",
            "root_page_ids",
            "root_page_titles",
            "include_patterns",
            "exclude_patterns",
            "max_depth",
            "include_attachments",
            "include_comments",
            "status",
            "sync_images",
            "image_max_size_bytes",
            "sync_mode",
            "polling_interval_minutes",
            "sync_enabled",
        ];
        let mut filtered = serde_json::Map::new();
        if let Some(updates) = updates.as_object() {
            for (k, v) in updates {
                if allowed.contains(&k.as_str()) {
                    filtered.insert(k.clone(), v.clone());
                }
            }
        }
        if let Some(raw) = filtered.get("root_page_ids").cloned() {
            let ids = if raw.is_null() || raw == "" {
                vec![]
            } else if let Some(ids) = raw.as_array() {
                ids.clone()
            } else {
                vec![raw]
            };
            let mut titles = Vec::new();
            for page in &ids {
                titles.push(
                    self.root_title(text(&binding, "connection_id"), &client::scalar(page))
                        .await,
                );
            }
            filtered.insert(
                "root_page_id".into(),
                ids.first().cloned().unwrap_or(Value::Null),
            );
            filtered.insert(
                "root_page_title".into(),
                titles.first().map(|s| json!(s)).unwrap_or(Value::Null),
            );
            filtered.insert("root_page_ids".into(), json!(ids));
            filtered.insert("root_page_titles".into(), json!(titles));
        } else if let Some(root) = filtered.get("root_page_id").cloned() {
            let id = client::scalar(&root);
            if id.is_empty() {
                filtered.insert("root_page_title".into(), Value::Null);
                filtered.insert("root_page_ids".into(), json!([]));
                filtered.insert("root_page_titles".into(), json!([]));
            } else {
                let title = self.root_title(text(&binding, "connection_id"), &id).await;
                filtered.insert("root_page_title".into(), json!(title));
                filtered.insert("root_page_ids".into(), json!([root]));
                filtered.insert("root_page_titles".into(), json!([title]));
            }
        }
        self.persist_binding(id, &Value::Object(filtered)).await?;
        self.binding(id).await
    }
    pub(super) async fn persist_binding(&self, id: &str, updates: &Value) -> Result<()> {
        // Preserve the persistence whitelist too: source ignores the newer list
        // roots/image flags despite the legacy service's broader input whitelist.
        const ALLOWED: &[&str] = &[
            "space_id",
            "space_name",
            "include_patterns",
            "exclude_patterns",
            "max_depth",
            "include_attachments",
            "include_comments",
            "status",
            "last_sync_at",
            "synced_page_count",
            "total_page_count",
            "last_error",
            "root_page_id",
            "root_page_title",
            "sync_mode",
            "polling_interval_minutes",
            "last_incremental_sync_at",
            "sync_enabled",
            "next_sync_at",
        ];
        let keys = ALLOWED
            .iter()
            .copied()
            .filter(|k| updates.get(*k).is_some())
            .collect::<Vec<_>>();
        if keys.is_empty() {
            return Ok(());
        }
        let set = keys
            .iter()
            .map(|k| format!("{k}=input.{k}"))
            .collect::<Vec<_>>()
            .join(",");
        let statement = format!(
            "UPDATE confluence_space_bindings AS target SET {set},updated_at=NOW() FROM jsonb_populate_record(NULL::confluence_space_bindings,$2::jsonb) input WHERE target.binding_id=$1"
        );
        sqlx::query(&statement)
            .bind(id)
            .bind(updates)
            .execute(&self.sources.pool)
            .await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn user(tier: &str, id: &str) -> UserContext {
        UserContext {
            user_id: id.into(),
            tenant_id: "tenant".into(),
            user_tier: tier.into(),
            user_type: "user".into(),
            roles: vec![],
            is_authenticated: true,
        }
    }
    #[test]
    fn legacy_acl_uses_exact_tier_and_owner_fallback() {
        let row = json!({"owner_id":"","created_by":"u"});
        assert!(verify_access(&row, &user("normal", "u")).is_ok());
        assert!(verify_access(&row, &user("admin", "other")).is_ok());
        assert!(verify_access(&row, &user("normal", "other")).is_err());
        assert!(
            verify_import_access(
                &json!({"owner_id":"u","tenant_id":"a"}),
                Some("b"),
                Some("u"),
                None
            )
            .is_err()
        );
    }
}

/// Legacy connection DTO intentionally has no Debug implementation: it contains
/// authentication material. No HTTP route exposes this dormant constructor.
pub struct ConnectionCreate<'a> {
    pub tenant_id: &'a str,
    pub name: &'a str,
    pub domain: &'a str,
    pub email: &'a str,
    pub api_token: &'a str,
    pub sync_mode: &'a str,
    pub polling_interval_minutes: i32,
    pub created_by: Option<&'a str>,
}
impl LegacyManagement {
    async fn verify_credentials(&self, domain: &str, email: &str, token: &str) -> Result<()> {
        let credentials = credentials::Credentials {
            domain: domain.into(),
            email: email.into(),
            token: token.into(),
        };
        self.sources
            .provider(&credentials, &["spaces"], &[("limit", "1")])
            .await?;
        Ok(())
    }
    pub async fn create_connection(&self, request: ConnectionCreate<'_>) -> Result<Value> {
        self.verify_credentials(request.domain, request.email, request.api_token)
            .await?;
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO confluence_connections(connection_id,tenant_id,name,domain,email,api_token,sync_mode,polling_interval_minutes,status,last_sync_at,last_error,created_by,owner_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'active',NULL,NULL,$9,$9)").bind(&id).bind(request.tenant_id).bind(request.name).bind(request.domain).bind(request.email).bind(request.api_token).bind(request.sync_mode).bind(request.polling_interval_minutes).bind(request.created_by).execute(&self.sources.pool).await?;
        self.sources
            .connection(&id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))
    }
    pub async fn update_connection(
        &self,
        id: &str,
        tenant: &str,
        updates: &Value,
    ) -> Result<Value> {
        let connection = self
            .sources
            .connection(id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))?;
        if !text(&connection, "tenant_id").is_empty() && text(&connection, "tenant_id") != tenant {
            return Err(Error::new(StatusCode::FORBIDDEN, "Permission denied"));
        }
        if connection["email"] == credentials::MARKER
            || text(&connection, "api_token").starts_with("cfcred:")
        {
            return Err(Error::conflict(
                "Encrypted connection rotation is not supported by this legacy update path",
            ));
        }
        if ["domain", "email", "api_token"]
            .iter()
            .any(|k| updates.get(*k).is_some())
        {
            let value = |key| {
                updates
                    .get(key)
                    .unwrap_or(&connection[key])
                    .as_str()
                    .unwrap_or("")
            };
            self.verify_credentials(value("domain"), value("email"), value("api_token"))
                .await?;
            self.clients.invalidate(id).await;
        }
        const ALLOWED: &[&str] = &[
            "name",
            "domain",
            "email",
            "api_token",
            "sync_mode",
            "polling_interval_minutes",
            "status",
            "last_sync_at",
            "last_error",
        ];
        let set = ALLOWED
            .iter()
            .filter(|k| updates.get(**k).is_some())
            .map(|k| format!("{k}=input.{k}"))
            .collect::<Vec<_>>()
            .join(",");
        if !set.is_empty() {
            let statement = format!(
                "UPDATE confluence_connections AS target SET {set},updated_at=NOW() FROM jsonb_populate_record(NULL::confluence_connections,$2::jsonb) input WHERE target.connection_id=$1"
            );
            sqlx::query(&statement)
                .bind(id)
                .bind(updates)
                .execute(&self.sources.pool)
                .await?;
        }
        self.sources
            .connection(id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))
    }
    pub async fn delete_connection(&self, id: &str, tenant: &str) -> Result<bool> {
        let Some(connection) = self.sources.connection(id).await? else {
            return Ok(false);
        };
        if !text(&connection, "tenant_id").is_empty() && text(&connection, "tenant_id") != tenant {
            return Err(Error::new(StatusCode::FORBIDDEN, "Permission denied"));
        }
        self.clients.invalidate(id).await;
        Ok(
            sqlx::query("DELETE FROM confluence_connections WHERE connection_id=$1")
                .bind(id)
                .execute(&self.sources.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }
    pub async fn list_connections(
        &self,
        user: &UserContext,
        tenant: Option<&str>,
        status: Option<&str>,
    ) -> Result<Vec<Value>> {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(c) FROM confluence_connections c WHERE ($1::text IS NULL OR tenant_id=$1) AND ($2::text IS NULL OR status=$2) ORDER BY created_at DESC LIMIT 100").bind(present(tenant)).bind(present(status)).fetch_all(&self.sources.pool).await?;
        Ok(rows
            .into_iter()
            .filter(|c| user.user_tier == "admin" || owner(c) == user.user_id)
            .collect())
    }
    pub async fn test_connection(&self, id: &str, tenant: &str) -> Value {
        let result=async{let connection=self.sources.connection(id).await?.ok_or_else(||Error::missing("Connection not found"))?;if !text(&connection,"tenant_id").is_empty()&&text(&connection,"tenant_id")!=tenant{return Err(Error::new(StatusCode::FORBIDDEN,"Access denied"));}let c=self.credentials(id).await?;self.sources.provider(&c,&["spaces"],&[("limit","1")]).await?;let spaces=self.sources.list_spaces(&c,None,"current",5).await?;Ok::<_,Error>(json!({"status":"success","message":"Connection successful","spaces_available":true,"spaces_count":spaces.len(),"sample_spaces":spaces.iter().take(3).map(|s|json!({"key":s.space_key,"name":s.name})).collect::<Vec<_>>()}))}.await;
        match result {
            Ok(v) => v,
            Err(error) => json!({"status":"error","message":error.detail}),
        }
    }
    pub async fn discover_spaces(
        &self,
        id: &str,
        tenant: &str,
        type_filter: Option<&str>,
    ) -> Result<Vec<discovery::Space>> {
        let connection = self
            .sources
            .connection(id)
            .await?
            .ok_or_else(|| Error::missing("Connection not found"))?;
        if !text(&connection, "tenant_id").is_empty() && text(&connection, "tenant_id") != tenant {
            return Err(Error::new(StatusCode::FORBIDDEN, "Access denied"));
        }
        let c = self.credentials(id).await?;
        self.sources
            .list_spaces(&c, type_filter, "current", 100)
            .await
    }
    pub async fn get_sync_status(&self, id: &str, user: &UserContext) -> Result<Value> {
        let binding = self.get_binding(id, user).await?;
        let tasks = self.list_sync_tasks(user, Some(id), None, 1).await?;
        let pages = self.pages(id, None, Some(100), 0, false).await?;
        Ok(
            json!({"binding_id":id,"status":binding["status"],"total_pages":binding["total_page_count"].as_i64().unwrap_or(0),"synced_pages":pages.iter().filter(|p|p["status"]=="synced").count(),"failed_pages":pages.iter().filter(|p|p["status"]=="error").count(),"pending_pages":pages.iter().filter(|p|p["status"]=="pending").count(),"last_sync_at":binding["last_sync_at"],"current_task":tasks.first().map(|t|json!({"task_id":t["task_id"],"status":t["status"],"progress":t["progress"],"started_at":t["started_at"]}))}),
        )
    }
}
