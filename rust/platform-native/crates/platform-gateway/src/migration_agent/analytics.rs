//! Operations surfaces use the shared trace repository and current Agent ACL.
use super::{Actor, AgentError, routes::AgentState};
use crate::migration_trace::{AgentOperationsFilter, TraceFilter};
use serde_json::{Value, json};
use std::collections::HashMap;
impl AgentState {
    pub async fn analytics(
        &self,
        actor: &Actor,
        agent: &str,
        query: &HashMap<String, String>,
    ) -> Result<Value, AgentError> {
        let invalid = || AgentError::code(422, "INVALID_ANALYTICS_FILTER");
        let limit = query
            .get("limit")
            .map(|s| s.parse::<i64>())
            .transpose()
            .map_err(|_| invalid())?
            .unwrap_or(50);
        let offset = query
            .get("offset")
            .map(|s| s.parse::<i64>())
            .transpose()
            .map_err(|_| invalid())?
            .unwrap_or(0);
        if !(1..=200).contains(&limit) || offset < 0 {
            return Err(invalid());
        }
        let version = query
            .get("agent_version_id")
            .map(|s| super::uuid(s))
            .transpose()?;
        let publication = query
            .get("publication_id")
            .map(|s| super::uuid(s))
            .transpose()?;
        let channel = query.get("channel").cloned();
        if channel
            .as_deref()
            .is_some_and(|s| !matches!(s, "preview" | "hosted" | "embed" | "api" | "builtin"))
        {
            return Err(invalid());
        }
        let after = query
            .get("started_after")
            .map(|s| chrono::DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&chrono::Utc)))
            .transpose()
            .map_err(|_| invalid())?;
        let before = query
            .get("started_before")
            .map(|s| chrono::DateTime::parse_from_rfc3339(s).map(|t| t.with_timezone(&chrono::Utc)))
            .transpose()
            .map_err(|_| invalid())?;
        let row = self.repository.get(actor, agent).await?;
        let filter = AgentOperationsFilter {
            agent_version_id: version.clone(),
            publication_id: publication.clone(),
            channel: channel.clone(),
            started_after: after,
            started_before: before,
        };
        let map_error = |e: crate::migration_trace::TraceError| AgentError::code(e.status, e.code);
        let metrics = self
            .traces
            .get_agent_operations_summary(&actor.tenant_id, agent, &filter)
            .await
            .map_err(map_error)?;
        let (mut traces, total) = self
            .traces
            .list_traces(
                &actor.tenant_id,
                &TraceFilter {
                    agent_id: Some(agent.into()),
                    agent_version_id: version,
                    publication_id: publication,
                    channel,
                    status: query.get("status").cloned(),
                    started_after: after,
                    started_before: before,
                    limit,
                    offset,
                    ..Default::default()
                },
            )
            .await
            .map_err(map_error)?;
        for row in &mut traces {
            for key in ["input_preview", "output_preview"] {
                let value = crate::migration_compat::eval_observation::redact_trace_text(
                    row[key].as_str().unwrap_or(""),
                    Some(500),
                );
                row[key] = crate::metrics_redaction::redact_sensitive_text(&value).into();
            }
            row["metadata"] = json!({});
            row["privacy"] = json!({});
        }
        let filters: serde_json::Map<String, Value> = [
            "agent_version_id",
            "publication_id",
            "channel",
            "started_after",
            "started_before",
            "status",
        ]
        .into_iter()
        .filter_map(|k| query.get(k).map(|v| (k.into(), json!(v))))
        .chain(std::iter::once(("agent_id".into(), json!(agent))))
        .collect();
        Ok(
            json!({"agent_id":agent,"caller_role":row["caller_role"].as_str().unwrap_or("viewer"),"metrics":metrics,"traces":traces,"total":total,"limit":limit,"offset":offset,"filters":filters}),
        )
    }
}
