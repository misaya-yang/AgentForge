//! Opaque, principal-scoped published attachment handles over migrated storage.
use super::{
    Actor, AgentError, public::PublicRuntime, routes::AgentState, studio::AgentRepository, text,
};
use axum::{
    Json,
    extract::{FromRequest, Multipart, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
impl AgentRepository {
    pub async fn create_runtime_attachment(
        &self,
        actor: &Actor,
        publication: &str,
        channel: &str,
        key: &str,
        filename: &str,
        mime: &str,
        size: i64,
    ) -> Result<Value, AgentError> {
        let mut tx =
            ai_platform_gateway_foundation::tenant_database::begin(&self.pool, &actor.tenant_id)
                .await?;
        let p:Value=sqlx::query_scalar("SELECT jsonb_build_object('agent_id',p.agent_id,'max_storage_bytes',COALESCE(g.max_storage_bytes,10737418240)::bigint,'alert_threshold_percent',COALESCE(g.alert_threshold_percent,90)::int) FROM agent_publications p JOIN agents a ON a.tenant_id=p.tenant_id AND a.agent_id=p.agent_id LEFT JOIN agent_governance_policies g ON g.tenant_id=p.tenant_id AND g.agent_id=p.agent_id WHERE p.tenant_id=$1 AND p.publication_id=$2::uuid AND p.channel=$3 AND p.status='active' AND a.deleted_at IS NULL FOR UPDATE OF a").bind(&actor.tenant_id).bind(publication).bind(channel).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(409,"PUBLICATION_DISABLED"))?;
        let current:i64=sqlx::query_scalar("SELECT COALESCE(SUM(a.size_bytes),0)::bigint FROM agent_runtime_attachments a JOIN agent_publications p ON p.tenant_id=a.tenant_id AND p.publication_id=a.publication_id WHERE p.tenant_id=$1 AND p.agent_id=$2::uuid AND a.deleted_at IS NULL").bind(&actor.tenant_id).bind(text(&p,"agent_id")).fetch_one(&mut *tx).await?;
        let total = current.saturating_add(size);
        let limit = p["max_storage_bytes"].as_i64().unwrap_or(0);
        let threshold = p["alert_threshold_percent"].as_i64().unwrap_or(90);
        if i128::from(total) * 100 >= i128::from(limit) * i128::from(threshold) {
            let action = if total >= limit {
                "quota_exceeded"
            } else {
                "quota_threshold_reached"
            };
            sqlx::query("INSERT INTO audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,status) SELECT 'agent_studio','system:agent-governance',$1,'agent',$2,$3,$4::jsonb,'success' WHERE NOT EXISTS(SELECT 1 FROM audit_logs WHERE tenant_id=$1 AND agent_id=$2::uuid AND action=$3 AND request_summary->>'quota'='storage_bytes' AND created_at>=date_trunc('day',NOW()))").bind(&actor.tenant_id).bind(text(&p,"agent_id")).bind(action).bind(json!({"publication_id":publication,"quota":"storage_bytes","usage":total,"limit":limit,"threshold_percent":threshold})).execute(&mut *tx).await?;
        }
        if total > limit {
            tx.commit().await?;
            return Err(AgentError::code(
                429,
                "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED",
            ));
        }
        let row:Value=sqlx::query_scalar("INSERT INTO agent_runtime_attachments AS a(tenant_id,publication_id,principal_id,channel,storage_key,filename,mime_type,size_bytes) VALUES($1,$2::uuid,$3,$4,$5,$6,$7,$8) RETURNING jsonb_build_object('artifact_id',a.attachment_id,'filename',a.filename,'mime_type',a.mime_type,'size_bytes',a.size_bytes,'expires_at',a.expires_at)").bind(&actor.tenant_id).bind(publication).bind(&actor.user_id).bind(channel).bind(key).bind(filename).bind(mime).bind(size).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(row)
    }
    pub async fn resolve_runtime_attachments(
        &self,
        actor: &Actor,
        publication: &str,
        channel: &str,
        ids: &[String],
    ) -> Result<Vec<Value>, AgentError> {
        let ids: Vec<String> = ids
            .iter()
            .map(|s| {
                super::uuid(s)
                    .map_err(|_| AgentError::code(404, "AGENT_RUNTIME_ATTACHMENT_NOT_FOUND"))
            })
            .collect::<Result<_, _>>()?;
        if ids.iter().collect::<BTreeSet<_>>().len() != ids.len() {
            return Err(AgentError::code(409, "AGENT_RUNTIME_ATTACHMENT_DUPLICATE"));
        }
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('artifact_id',attachment_id,'filename',filename,'mime_type',mime_type,'file_path','/'||ltrim(storage_key,'/')) FROM agent_runtime_attachments WHERE tenant_id=$1 AND publication_id=$2::uuid AND principal_id=$3 AND channel=$4 AND attachment_id=ANY($5::text[]::uuid[]) AND expires_at>NOW()").bind(&actor.tenant_id).bind(publication).bind(&actor.user_id).bind(channel).bind(&ids).fetch_all(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &actor.tenant_id)).await?;
        if rows.len() != ids.len() {
            return Err(AgentError::code(404, "AGENT_RUNTIME_ATTACHMENT_NOT_FOUND"));
        }
        ids.iter()
            .map(|id| {
                rows.iter()
                    .find(|v| v["artifact_id"] == id.as_str())
                    .cloned()
                    .ok_or_else(|| AgentError::code(404, "AGENT_RUNTIME_ATTACHMENT_NOT_FOUND"))
            })
            .collect()
    }
}
impl PublicRuntime {
    pub async fn upload_attachment(
        &self,
        state: &AgentState,
        query: HashMap<String, String>,
        request: Request,
    ) -> Result<Response, AgentError> {
        if !state.runtime_enabled {
            return Err(AgentError::code(503, "AGENT_RUNTIME_DISABLED"));
        }
        let path = request.uri().path().to_owned();
        let headers = request.headers().clone();
        super::public::reject_forgery(&headers, None)?;
        let ip = request
            .extensions()
            .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
            .map(|v| v.0.ip().to_string())
            .unwrap_or_default();
        let api = path.starts_with("/api/v1/agent-runtime/");
        let rest = path
            .strip_prefix(if api {
                "/api/v1/agent-runtime/"
            } else {
                "/api/v1/public/agents/"
            })
            .ok_or_else(|| AgentError::code(404, "NOT_FOUND"))?;
        let id = super::uuid(rest.split('/').next().unwrap_or(""))?;
        let channel = if api {
            "api"
        } else {
            query.get("channel").map(String::as_str).unwrap_or("hosted")
        };
        let (resolution, actor) = if api {
            let token = headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.strip_prefix("Bearer "))
                .unwrap_or("");
            self.resolve_api(&state.repository, token, &id, "attachments:write", None)
                .await?
        } else {
            let actor = self.identities.resolve(&headers, &path).await?;
            self.resolve_browser(&state.repository, actor, &headers, &id, channel, None, None)
                .await?
        };
        self.enforce_limits(
            &state.repository,
            &resolution["publication"],
            &actor.user_id,
            &ip,
        )
        .await?;
        let snapshot = state.build_snapshot(&actor, &resolution, channel).await?;
        if snapshot["channel_policy"]["attachments"] != true {
            return Err(AgentError::code(422, "AGENT_RUNTIME_ATTACHMENTS_FORBIDDEN"));
        }
        let storage = self
            .storage
            .as_ref()
            .ok_or_else(|| AgentError::code(503, "AGENT_RUNTIME_ATTACHMENT_STORAGE_UNAVAILABLE"))?;
        let mut multipart = Multipart::from_request(request, &())
            .await
            .map_err(|_| AgentError::code(422, "AGENT_RUNTIME_ATTACHMENT_INVALID"))?;
        while let Some(mut field) = multipart
            .next_field()
            .await
            .map_err(|_| AgentError::code(422, "AGENT_RUNTIME_ATTACHMENT_INVALID"))?
        {
            if field.name() != Some("file") {
                continue;
            }
            let filename = field.file_name().unwrap_or("").trim().to_owned();
            if filename.is_empty()
                || filename.chars().count() > 255
                || filename.contains(['\0', '/', '\\'])
            {
                return Err(AgentError::code(422, "AGENT_RUNTIME_ATTACHMENT_INVALID"));
            }
            let ext = crate::migration_content::files::validate_extension(&filename)
                .map_err(|_| AgentError::code(422, "AGENT_RUNTIME_ATTACHMENT_INVALID"))?;
            let mime = crate::migration_content::artifacts::mime_type(&ext);
            let mut content = Vec::new();
            while let Some(bytes) = field.chunk().await.map_err(|_| {
                AgentError::code(503, "AGENT_RUNTIME_ATTACHMENT_STORAGE_UNAVAILABLE")
            })? {
                if bytes.len() > self.max_attachment_bytes.saturating_sub(content.len()) {
                    return Err(AgentError::code(413, "AGENT_RUNTIME_ATTACHMENT_TOO_LARGE"));
                }
                content.extend_from_slice(&bytes);
            }
            let size = content.len() as i64;
            let owner = format!(
                "ar_{}",
                &super::spec::hex(&Sha256::digest(
                    format!("{}:{}", actor.tenant_id, actor.user_id).as_bytes()
                ))[..40]
            );
            let name = format!(
                "{}_{}.{}",
                &uuid::Uuid::new_v4().simple().to_string()[..8],
                chrono::Utc::now().format("%Y%m%d_%H%M%S"),
                ext
            );
            let key = format!("uploads/{}/{owner}/{name}", actor.tenant_id);
            let publication = text(&resolution["publication"], "publication_id");
            let metadata = BTreeMap::from([
                ("agent_runtime".into(), "true".into()),
                ("publication_id".into(), publication.into()),
                ("channel".into(), channel.into()),
                ("original_filename".into(), filename.clone()),
            ]);
            storage
                .upload(&key, content, mime, metadata)
                .await
                .map_err(|_| {
                    AgentError::code(503, "AGENT_RUNTIME_ATTACHMENT_STORAGE_UNAVAILABLE")
                })?;
            let mut row = match state
                .repository
                .create_runtime_attachment(
                    &actor,
                    publication,
                    channel,
                    &key,
                    &filename,
                    mime,
                    size,
                )
                .await
            {
                Ok(row) => row,
                Err(e) => {
                    let _ = storage.delete(&key).await;
                    return Err(e);
                }
            };
            row["request_id"] = uuid::Uuid::new_v4().to_string().into();
            return Ok((StatusCode::OK, Json(row)).into_response());
        }
        Err(AgentError::code(422, "AGENT_RUNTIME_ATTACHMENT_INVALID"))
    }
}
