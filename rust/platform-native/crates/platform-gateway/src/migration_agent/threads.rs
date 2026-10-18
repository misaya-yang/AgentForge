//! Native Thread/Turn/Item persistence and prompt-independent runtime assignment.
use super::{Actor, AgentError, Scope, spec, text, uuid};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeThread {
    pub runtime_thread_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub kernel_owner: String,
    pub source_kind: String,
    pub import_status: String,
    pub last_sequence: i64,
    #[serde(default)]
    pub dynamic_tool_fingerprint: Option<String>,
}
impl RuntimeThread {
    pub fn scope(&self) -> Scope {
        Scope {
            tenant_id: self.tenant_id.clone(),
            user_id: self.user_id.clone(),
            session_id: self.session_id.clone(),
        }
    }
    pub fn payload(&self) -> Value {
        json!({"schema_version":"agent-thread/v2","id":self.runtime_thread_id,"thread_id":self.runtime_thread_id,"session_id":self.session_id,"runtime":{"owner":self.kernel_owner,"source":self.source_kind},"import_status":self.import_status,"last_sequence":self.last_sequence})
    }
}
#[derive(Clone)]
pub struct AgentThreadStore {
    pub pool: PgPool,
}
impl AgentThreadStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    fn thread(value: Value) -> Result<RuntimeThread, AgentError> {
        serde_json::from_value(value)
            .map_err(|_| AgentError::code(503, "AGENT_RUNTIME_STORAGE_UNAVAILABLE"))
    }
    pub async fn get(&self, actor: &Actor, id: &str) -> Result<Option<RuntimeThread>, AgentError> {
        let value:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM assistant_runtime_threads t WHERE runtime_thread_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND deleted_at IS NULL").bind(uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &actor.tenant_id)).await?;
        value.map(Self::thread).transpose()
    }
    pub async fn get_for_session(
        &self,
        scope: &Scope,
    ) -> Result<Option<RuntimeThread>, AgentError> {
        let value:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(t) FROM assistant_runtime_threads t WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND deleted_at IS NULL").bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &scope.tenant_id)).await?;
        value.map(Self::thread).transpose()
    }
    pub async fn ensure_native(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<RuntimeThread, AgentError> {
        sqlx::query("SELECT ensure_assistant_runtime_thread($1::uuid,$2,$3,$4,'native')")
            .bind(uuid(id)?)
            .bind(&scope.tenant_id)
            .bind(&scope.user_id)
            .bind(&scope.session_id)
            .execute(ai_platform_gateway_foundation::tenant_database::executor(
                &self.pool,
                &scope.tenant_id,
            ))
            .await?;
        self.get_for_session(scope)
            .await?
            .ok_or_else(|| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED"))
    }
    pub async fn import_legacy(
        &self,
        scope: &Scope,
        id: &str,
    ) -> Result<RuntimeThread, AgentError> {
        let result =
            sqlx::query("SELECT * FROM import_assistant_legacy_session($1::uuid,$2,$3,$4)")
                .bind(uuid(id)?)
                .bind(&scope.tenant_id)
                .bind(&scope.user_id)
                .bind(&scope.session_id)
                .execute(ai_platform_gateway_foundation::tenant_database::executor(
                    &self.pool,
                    &scope.tenant_id,
                ))
                .await;
        if let Err(e) = result {
            let message = e.as_database_error().map(|e| e.message()).unwrap_or("");
            return Err(if message.contains("IMPORT_IN_FLIGHT") {
                AgentError::code(409, "AI_PLATFORM_AGENT_RUNTIME_IMPORT_IN_FLIGHT")
            } else if message.contains("SESSION_NOT_FOUND") || message.contains("SCOPE_MISMATCH") {
                AgentError::code(404, "AI_PLATFORM_AGENT_RUNTIME_SESSION_NOT_FOUND")
            } else {
                AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_IMPORT_FAILED")
            });
        }
        let row = self
            .get_for_session(scope)
            .await?
            .ok_or_else(|| AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_IMPORT_FAILED"))?;
        if row.import_status != "ready" {
            return Err(AgentError::code(
                503,
                "AI_PLATFORM_AGENT_RUNTIME_IMPORT_FAILED",
            ));
        }
        Ok(row)
    }
    pub async fn events(
        &self,
        actor: &Actor,
        id: &str,
        after: i64,
        limit: i64,
    ) -> Result<Vec<Value>, AgentError> {
        Ok(sqlx::query_scalar("SELECT jsonb_build_object('sequence',sequence,'event_id',event_id,'event_key',event_key,'turn_id',turn_id,'item_id',item_id,'event_type',event_type,'item_type',item_type,'status',status,'payload',payload,'created_at',created_at) FROM assistant_runtime_items WHERE runtime_thread_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND sequence>$4 ORDER BY sequence ASC LIMIT $5").bind(uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).bind(after.max(0)).bind(limit.clamp(1,1000)).fetch_all(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &actor.tenant_id)).await?)
    }
    pub async fn append_event(
        &self,
        thread: &RuntimeThread,
        key: &str,
        event_type: &str,
        payload: &Value,
        turn: Option<&str>,
        item: Option<&str>,
        status: Option<&str>,
    ) -> Result<i64, AgentError> {
        let event_id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("ai-platform:{}:{key}", thread.runtime_thread_id).as_bytes(),
        )
        .to_string();
        Ok(sqlx::query_scalar("SELECT append_assistant_runtime_item($1::uuid,$2::uuid,$3,$4,$5,$6::uuid,$7,$8,$9,$10,$11,$12,$13::jsonb,$14)").bind(&thread.runtime_thread_id).bind(&thread.runtime_thread_id).bind(&thread.tenant_id).bind(&thread.user_id).bind(&thread.session_id).bind(event_id).bind(key).bind(turn).bind(item).bind(event_type).bind(item.map(|_|"item")).bind(status).bind(payload).bind(spec::hash(payload)).fetch_one(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &thread.tenant_id)).await?)
    }
    pub async fn turn_metadata(
        &self,
        scope: &Scope,
        thread: &str,
        turn: &str,
    ) -> Result<Option<Value>, AgentError> {
        let row:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('snapshot',snapshot,'capability_revision',capability_revision,'kernel_revision',snapshot->>'kernel_revision') FROM assistant_runtime_snapshots WHERE run_id=$1::uuid AND runtime_thread_id=$2::uuid AND tenant_id=$3 AND user_id=$4 AND session_id=$5").bind(uuid(turn)?).bind(uuid(thread)?).bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &scope.tenant_id)).await?;
        Ok(row.map(|r|{let s=&r["snapshot"];let p=&s["reasoning"];let t=s["parameters"]["temperature"].as_f64().filter(|t|(0.0..=2.0).contains(t));json!({"requested_reasoning_option":p["requested_option"],"effective_reasoning_option":p["effective_option"],"reasoning_adapter_id":p["adapter_id"],"capability_revision":r["capability_revision"].as_i64().unwrap_or(1),"reasoning_fallback_reason":p["fallback_reason"],"kernel":"agent","kernel_revision":r["kernel_revision"],"model_id":s["model"]["id"],"temperature":t})}))
    }
    pub async fn assignment(&self, scope: &Scope) -> Result<Option<Value>, AgentError> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(a) FROM assistant_session_runtime_assignments a WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3").bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &scope.tenant_id)).await?)
    }
    pub async fn require_assignment(&self, scope: &Scope) -> Result<Value, AgentError> {
        let a = self
            .assignment(scope)
            .await?
            .ok_or_else(|| AgentError::code(404, "AGENT_RUNTIME_ASSIGNMENT_NOT_FOUND"))?;
        if text(&a, "runtime_owner") != "agent_runtime" {
            return Err(AgentError {
                status: 409,
                detail: json!({"code":"AGENT_RUNTIME_NOT_ASSIGNED","runtime_owner":a["runtime_owner"]}),
            });
        }
        Ok(a)
    }
    pub async fn bind_assignment(
        &self,
        scope: &Scope,
        revision: Option<&str>,
        reason: &str,
    ) -> Result<Value, AgentError> {
        let revision = revision
            .filter(|s| !s.is_empty())
            .unwrap_or("legacy-runtime");
        sqlx::query("INSERT INTO assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner,kernel_revision,assignment_reason) VALUES($1,$2,$3,'agent_runtime',$4,$5) ON CONFLICT(tenant_id,user_id,session_id) DO NOTHING").bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id).bind(revision).bind(reason).execute(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &scope.tenant_id)).await?;
        let existing = self
            .assignment(scope)
            .await?
            .ok_or_else(|| AgentError::code(409, "AGENT_RUNTIME_ASSIGNMENT_CONFLICT"))?;
        if text(&existing, "runtime_owner") != "agent_runtime"
            || text(&existing, "kernel_revision") != revision
        {
            return Err(AgentError::code(409, "AGENT_RUNTIME_ASSIGNMENT_CONFLICT"));
        }
        Ok(existing)
    }
    pub async fn bind_new_assignment(
        &self,
        scope: &Scope,
        revision: &str,
    ) -> Result<Value, AgentError> {
        if let Some(a) = self.assignment(scope).await? {
            if text(&a, "runtime_owner") != "agent_runtime" {
                return Err(AgentError::code(409, "AGENT_RUNTIME_ASSIGNMENT_CONFLICT"));
            }
            return Ok(a);
        }
        self.bind_assignment(scope, Some(revision), "single_kernel")
            .await
    }
    pub async fn owned_session(&self, scope: &Scope) -> Result<Value, AgentError> {
        sqlx::query_scalar("SELECT to_jsonb(s) FROM assistant.sessions s WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3").bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(&self.pool, &scope.tenant_id)).await?.ok_or_else(||AgentError::code(404,"SESSION_NOT_FOUND"))
    }
}
pub fn safe_history_event(event: &Value) -> Value {
    let kind = text(event, "event_type");
    if !matches!(
        kind,
        "run_finished" | "run_error" | "cancelled" | "side_effect_unknown"
    ) {
        return event.clone();
    }
    let data = &event["data"];
    let mut safe = json!({});
    let status = text(data, "status");
    if matches!(status, "succeeded" | "failed" | "cancelled" | "blocked") {
        safe["status"] = status.into();
    }
    if status == "cancelled"
        && text(data, "error_code") == "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED"
    {
        safe["terminal_reason"] = "runtime_restart_interrupted".into();
    }
    if data["terminal_envelope"]["exit_reason"] == "side_effect_unknown" {
        safe["terminal_envelope"] = json!({"exit_reason":"side_effect_unknown"});
    }
    json!({"event_type":kind,"data":safe,"timestamp":event["timestamp"]})
}
pub fn visible_message_text(input: &str) -> String {
    let mut text = input.to_owned();
    let start = "[AI_PLATFORM_READONLY_CONTEXT_V1]";
    let end = "[/AI_PLATFORM_READONLY_CONTEXT_V1]";
    while let Some(a) = text.find(start) {
        if let Some(b) = text[a + start.len()..].find(end) {
            text.replace_range(a..a + start.len() + b + end.len(), "");
        } else {
            break;
        }
    }
    text.trim().to_owned()
}
