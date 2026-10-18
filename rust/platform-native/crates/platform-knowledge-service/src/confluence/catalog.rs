//! Source status projection with strictly owned bindings and current setup capabilities.
use super::*;
use axum::{
    extract::{Request, State},
    middleware::Next,
};
pub async fn intercept(State(s): State<Arc<Sources>>, request: Request, next: Next) -> Response {
    let parts = request.uri().path().split('/').collect::<Vec<_>>();
    if request.method() == axum::http::Method::GET
        && parts.len() == 6
        && parts[1..4] == ["api", "v1", "knowledge"]
        && parts[5] == "sources"
    {
        let Some(user) = request.extensions().get::<query::types::UserContext>() else {
            return Error::forbidden().into_response();
        };
        return match s.catalog(user, parts[4]).await {
            Ok(v) => Json(v).into_response(),
            Err(e) => e.into_response(),
        };
    }
    next.run(request).await
}
impl Sources {
    pub async fn catalog(&self, user: &query::types::UserContext, dataset: &str) -> Result<Value> {
        let observed = self
            .store
            .require_dataset_access(user, dataset, "viewer")
            .await?;
        if observed.index_config["retrieval"]
            .get("_index_deletion_fence")
            .is_some()
        {
            return Err(Error::new(
                StatusCode::BAD_REQUEST,
                "dataset index deletion is pending; indexed content is unavailable",
            ));
        }
        let counts: Vec<(Option<String>, i64)> = sqlx::query_as(
            "SELECT source_type,COUNT(*) FROM documents WHERE dataset_id=$1 GROUP BY source_type",
        )
        .bind(dataset)
        .fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool))
        .await?;
        let mut bindings = vec![];
        let mut status = if self.enabled {
            "available"
        } else {
            "not_configured"
        };
        if self.enabled {
            match self.binding_catalog(user, dataset).await {
                Ok(v) => bindings = v,
                Err(_) => status = "unavailable",
            }
        }
        let available = status == "available";
        let binding = available && self.actor(user, dataset, true, true).await.is_ok();
        let controls = available && self.actor(user, dataset, true, false).await.is_ok();
        let encryption=credentials::seal(&json!({"connection_id":"readiness","tenant_id":"readiness","owner_id":"readiness","domain":"readiness.atlassian.net"}),"readiness@example.invalid","readiness-probe",&self.encryption_key).is_ok();
        let capability = |allowed: bool| {
            if !available {
                json!({"available":false,"reason":"source_setup_unavailable"})
            } else if !allowed {
                json!({"available":false,"reason":"current_editor_required"})
            } else if !encryption {
                json!({"available":false,"reason":"encryption_key_required"})
            } else {
                json!({"available":true,"reason":null})
            }
        };
        let count = |kind: &str| {
            counts
                .iter()
                .find(|(k, _)| k.as_deref() == Some(kind))
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        let result = json!({"file_uploads":{"count":count("upload")},"url_imports":{"count":count("url")},"confluence_bindings":bindings,"connector_status":status,"source_binding_available":binding,"source_connection_controls_available":controls,"source_connection_reconnect":capability(controls),"source_connection_setup":capability(binding),"total_documents":counts.iter().map(|(_,n)|n).sum::<i64>()});
        let current = self
            .store
            .require_dataset_access(user, dataset, "viewer")
            .await?;
        if current.tenant_id != observed.tenant_id
            || current.content_revision != observed.content_revision
            || current.index_config["retrieval"]
                .get("_index_deletion_fence")
                .is_some()
        {
            return Err(Error::conflict(
                "dataset content generation changed during read; retry the request",
            ));
        }
        Ok(result)
    }
    async fn binding_catalog(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
    ) -> Result<Vec<Value>> {
        let editor = self
            .store
            .require_dataset_access(user, dataset, "editor")
            .await
            .is_ok();
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE tenant_id=$1 AND dataset_id=$2 ORDER BY created_at DESC").bind(&user.tenant_id).bind(dataset).fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        let mut out = vec![];
        for row in rows {
            if !owned(&row, user) {
                continue;
            }
            let connection = self.connection(text(&row, "connection_id")).await?;
            let manual = editor
                && text(&row, "created_by") == user.user_id
                && connection
                    .as_ref()
                    .is_some_and(|c| owned(c, user) && c["status"] == "active");
            let task:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM confluence_sync_tasks t WHERE binding_id=$1 AND owner_id=$2 ORDER BY created_at DESC,task_id DESC LIMIT 1").bind(text(&row,"binding_id")).bind(&user.user_id).fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
            out.push(json!({"binding_id":row["binding_id"],"manual_sync_available":manual,"current_sync_task":task.as_ref().map(super::sync::task_receipt),"space_name":row["space_name"],"page_count":row["synced_page_count"].as_i64().unwrap_or(0),"status":row["status"],"last_success_at":row["last_sync_at"],"has_problem":!["completed","success"].contains(&text(&row,"status").to_lowercase().as_str())&&!text(&row,"last_error").is_empty()}));
        }
        Ok(out)
    }
}
