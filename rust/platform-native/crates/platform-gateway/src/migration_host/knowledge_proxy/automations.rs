//! Automation effects retain current-user ACLs across the existing signed HTTP boundary.
use super::*;
use crate::migration_agent::{Actor, AgentError, AgentFuture, automations::AutomationKnowledge};
use serde_json::Value;
impl KnowledgeProxy {
    async fn automation_call(
        &self,
        actor: &Actor,
        method: Method,
        item: &str,
        body: Value,
        revision: Option<i64>,
    ) -> std::result::Result<Value, AgentError> {
        let current = crate::migration_knowledge_query::actor::CurrentActorStore {
            pool: self
                .sdk_control
                .as_ref()
                .ok_or_else(|| AgentError::code(503, "AUTOMATION_AUTHORITY_UNAVAILABLE"))?
                .pool
                .clone(),
        }
        .resolve(&actor.tenant_id, &actor.user_id)
        .await
        .map_err(|error| match error {
            crate::migration_knowledge_query::types::QueryError::Forbidden => {
                AgentError::code(403, "AUTOMATION_CREATOR_REVOKED")
            }
            _ => AgentError::code(503, "AUTOMATION_ACTOR_UNAVAILABLE"),
        })?;
        let user = ContentIdentity {
            tenant_id: current.tenant_id,
            user_id: current.user_id,
            is_authenticated: true,
            roles: current.roles,
            tier: current.user_tier,
            ..Default::default()
        };
        let mut headers = HeaderMap::new();
        headers.insert("content-type", "application/json".parse().unwrap());
        if let Some(revision) = revision {
            headers.insert(
                "if-match",
                format!("\"{revision}\"")
                    .parse()
                    .map_err(|_| AgentError::code(422, "AUTOMATION_REVISION_INVALID"))?,
            );
        }
        let path = format!("space-items/{}/content", item);
        let bytes = if method == Method::GET {
            vec![]
        } else {
            serde_json::to_vec(&body)
                .map_err(|_| AgentError::code(422, "AUTOMATION_CONTENT_INVALID"))?
        };
        let response = self
            .forward(user, method, path, String::new(), headers, bytes)
            .await
            .map_err(|_| AgentError::code(503, "AUTOMATION_KNOWLEDGE_UNAVAILABLE"))?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .map_err(|_| AgentError::code(502, "AUTOMATION_KNOWLEDGE_RESPONSE_INVALID"))?;
        if !status.is_success() {
            return Err(AgentError::code(
                status.as_u16(),
                "AUTOMATION_KNOWLEDGE_DENIED",
            ));
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AgentError::code(502, "AUTOMATION_KNOWLEDGE_RESPONSE_INVALID"))
    }
}
impl AutomationKnowledge for KnowledgeProxy {
    fn read<'a>(&'a self, actor: &'a Actor, item: &'a str) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            self.automation_call(actor, Method::GET, item, Value::Null, None)
                .await
        })
    }
    fn append<'a>(
        &'a self,
        actor: &'a Actor,
        item: &'a str,
        markdown: &'a str,
    ) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            let current = self
                .automation_call(actor, Method::GET, item, Value::Null, None)
                .await?;
            let revision = current["revision"]
                .as_i64()
                .ok_or_else(|| AgentError::code(502, "AUTOMATION_KNOWLEDGE_RESPONSE_INVALID"))?;
            let body = if current["content_format"] == "blocknote_json" {
                let mut blocks = current["blocks"].as_array().cloned().ok_or_else(|| {
                    AgentError::code(502, "AUTOMATION_KNOWLEDGE_RESPONSE_INVALID")
                })?;
                blocks.push(json!({"id":uuid::Uuid::new_v4().to_string(),"type":"paragraph","content":[{"type":"text","text":markdown,"styles":{}}],"children":[]}));
                json!({"content_format":"blocknote_json","blocks":blocks,"markdown":""})
            } else {
                json!({"content_format":"markdown","markdown":format!("{}\n\n{}",current["markdown"].as_str().unwrap_or(""),markdown)})
            };
            // One CAS write. A conflict or transport uncertainty is surfaced, never replayed.
            self.automation_call(actor, Method::PUT, item, body, Some(revision))
                .await
        })
    }
}
