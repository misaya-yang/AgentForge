//! Authoritative Agent policy, quotas, audit, cache epoch and credential revocation.
use super::{
    Actor, AgentError,
    studio::{AgentRepository, audit},
    text,
};
use serde_json::{Value, json};
use sqlx::Row;
#[path = "governance_sql.rs"]
mod sql;
pub fn default_policy(tenant: &str, agent: &str, user: &str) -> Value {
    json!({"tenant_id":tenant,"agent_id":agent,"trace_retention_days":90,"runtime_retention_days":30,"attachment_retention_days":1,"legal_hold":false,"principal_requests_per_minute":30,"principal_requests_per_day":1000,"ip_requests_per_minute":60,"ip_requests_per_day":2000,"publication_requests_per_minute":300,"publication_requests_per_day":10000,"max_agents_per_tenant":100,"max_active_publications":10,"max_concurrent_runs":25,"max_daily_tokens":10000000,"max_daily_mcp_calls":100000,"max_storage_bytes":10737418240_i64,"alert_threshold_percent":90,"cache_epoch":0,"updated_by":user,"created_at":null,"updated_at":null})
}
const POLICY_FIELDS: &[&str] = &[
    "trace_retention_days",
    "runtime_retention_days",
    "attachment_retention_days",
    "legal_hold",
    "principal_requests_per_minute",
    "principal_requests_per_day",
    "ip_requests_per_minute",
    "ip_requests_per_day",
    "publication_requests_per_minute",
    "publication_requests_per_day",
    "max_agents_per_tenant",
    "max_active_publications",
    "max_concurrent_runs",
    "max_daily_tokens",
    "max_daily_mcp_calls",
    "max_storage_bytes",
    "alert_threshold_percent",
];
impl AgentRepository {
    pub async fn governance(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        self.authorize_object(&mut c, actor, id, "viewer", false)
            .await?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_governance_policies p WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(&actor.tenant_id).bind(id).fetch_optional(ai_platform_gateway_foundation::tenant_database::connection(&mut *c, &actor.tenant_id)).await?.unwrap_or_else(||default_policy(&actor.tenant_id,id,&actor.user_id)))
    }
    pub async fn update_governance(
        &self,
        actor: &Actor,
        id: &str,
        changes: &Value,
    ) -> Result<Value, AgentError> {
        let changes = changes
            .as_object()
            .filter(|m| !m.is_empty() && m.keys().all(|k| POLICY_FIELDS.contains(&k.as_str())))
            .ok_or_else(|| AgentError::code(400, "AGENT_GOVERNANCE_POLICY_INVALID"))?;
        for (k, v) in changes {
            if k == "legal_hold" {
                if !v.is_boolean() {
                    return Err(AgentError::code(422, "AGENT_GOVERNANCE_POLICY_INVALID"));
                }
            } else if v.as_i64().is_none_or(|n| {
                n < 1
                    || n > match k.as_str() {
                        "trace_retention_days"
                        | "runtime_retention_days"
                        | "attachment_retention_days" => 3650,
                        "principal_requests_per_minute"
                        | "ip_requests_per_minute"
                        | "publication_requests_per_minute"
                        | "max_concurrent_runs" => 1_000_000,
                        "principal_requests_per_day"
                        | "ip_requests_per_day"
                        | "publication_requests_per_day" => 100_000_000,
                        "max_agents_per_tenant" | "max_active_publications" => 100_000,
                        "max_daily_tokens" | "max_storage_bytes" => 10_000_000_000_000,
                        "max_daily_mcp_calls" => 10_000_000_000,
                        "alert_threshold_percent" => 100,
                        _ => 0,
                    }
            }) {
                return Err(AgentError::code(422, "AGENT_GOVERNANCE_POLICY_INVALID"));
            }
        }
        let mut tx =
            ai_platform_gateway_foundation::tenant_database::begin(&self.pool, &actor.tenant_id)
                .await?;
        self.authorize_object(&mut tx, actor, id, "owner", true)
            .await?;
        let mut current:Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_governance_policies p WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(id).fetch_optional(&mut *tx).await?.unwrap_or_else(||default_policy(&actor.tenant_id,id,&actor.user_id));
        if changes.get("legal_hold") == Some(&Value::Bool(true)) && current["legal_hold"] != true {
            let cleanups:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_data_deletion_requests d WHERE tenant_id=$1 AND agent_id=$2::uuid AND status IN ('pending','failed') ORDER BY requested_at,deletion_id FOR UPDATE").bind(&actor.tenant_id).bind(id).fetch_all(&mut *tx).await?;
            for cleanup in cleanups {
                let invalid =
                    || AgentError::code(409, "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID");
                let mut counts = cleanup["deleted_counts"].clone();
                if counts.is_null() {
                    counts = json!({});
                }
                if let Some(s) = counts.as_str() {
                    counts = serde_json::from_str(s).map_err(|_| invalid())?;
                }
                if !counts.is_object() {
                    return Err(invalid());
                }
                let mut execution = counts["cleanup_execution"].clone();
                if execution.is_null() {
                    execution = json!({});
                }
                if !execution.is_object() {
                    return Err(invalid());
                }
                let status = text(&execution, "state");
                let attempts = cleanup["attempt_count"].as_i64().unwrap_or(0);
                if status == "claimed"
                    || (cleanup["status"] == "pending" && attempts > 0 && status.is_empty())
                {
                    return Err(AgentError::code(409, "AGENT_LEGAL_HOLD_CLEANUP_ACTIVE"));
                }
                let interrupted =
                    !execution.as_object().expect("object").is_empty() || attempts > 0;
                execution
                    .as_object_mut()
                    .expect("object")
                    .remove("claim_digest");
                execution["schema_version"] = "agent-data-deletion-execution/v1".into();
                execution["state"] = "blocked".into();
                execution["blocked_after_execution_started"] = interrupted.into();
                counts["cleanup_execution"] = execution;
                let code = if interrupted {
                    "AGENT_LEGAL_HOLD_ACTIVE_AFTER_INTERRUPTED_CLEANUP"
                } else {
                    "AGENT_LEGAL_HOLD_ACTIVE"
                };
                sqlx::query("UPDATE agent_data_deletion_requests SET status='blocked',error_code=$2,deleted_counts=$3::jsonb,completed_at=NOW() WHERE deletion_id=$1::uuid").bind(text(&cleanup,"deletion_id")).bind(code).bind(counts).execute(&mut *tx).await?;
                audit(&mut tx,actor,id,"data_deletion_blocked",json!({"deletion_id":cleanup["deletion_id"],"stage":"legal_hold_activation","interrupted":interrupted})).await?;
            }
        }
        current
            .as_object_mut()
            .expect("policy")
            .extend(changes.clone());
        let n = |k: &str| current[k].as_i64().unwrap_or(0);
        let row: Value = sqlx::query_scalar(sql::POLICY_UPSERT)
            .bind(&actor.tenant_id)
            .bind(id)
            .bind(n("trace_retention_days") as i32)
            .bind(n("runtime_retention_days") as i32)
            .bind(n("attachment_retention_days") as i32)
            .bind(current["legal_hold"].as_bool().unwrap_or(false))
            .bind(n("principal_requests_per_minute") as i32)
            .bind(n("principal_requests_per_day") as i32)
            .bind(n("ip_requests_per_minute") as i32)
            .bind(n("ip_requests_per_day") as i32)
            .bind(n("publication_requests_per_minute") as i32)
            .bind(n("publication_requests_per_day") as i32)
            .bind(n("alert_threshold_percent") as i32)
            .bind(n("max_agents_per_tenant") as i32)
            .bind(n("max_active_publications") as i32)
            .bind(n("max_concurrent_runs") as i32)
            .bind(n("max_daily_tokens"))
            .bind(n("max_daily_mcp_calls"))
            .bind(n("max_storage_bytes"))
            .bind(&actor.user_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut fields: Vec<_> = changes.keys().cloned().collect();
        fields.sort();
        audit(
            &mut tx,
            actor,
            id,
            "governance_policy_update",
            json!({"fields":fields,"after":changes}),
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }
    pub async fn invalidate_caches(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let mut tx =
            ai_platform_gateway_foundation::tenant_database::begin(&self.pool, &actor.tenant_id)
                .await?;
        self.authorize_object(&mut tx, actor, id, "owner", true)
            .await?;
        let mut row: Value = sqlx::query_scalar(sql::CACHE_UPSERT)
            .bind(&actor.tenant_id)
            .bind(id)
            .bind(&actor.user_id)
            .fetch_one(&mut *tx)
            .await?;
        let deleted:i64=sqlx::query_scalar("WITH doomed AS(DELETE FROM semantic_cache WHERE metadata->>'tenant_id'=$1 AND metadata->>'agent_id'=$2 RETURNING 1) SELECT COUNT(*) FROM doomed").bind(&actor.tenant_id).bind(id).fetch_one(&mut *tx).await?;
        audit(
            &mut tx,
            actor,
            id,
            "cache_invalidate",
            json!({"cache_epoch":row["cache_epoch"],"rows":deleted}),
        )
        .await?;
        tx.commit().await?;
        row["deleted_cache_rows"] = deleted.into();
        Ok(row)
    }
    pub async fn revoke_credentials(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let mut tx =
            ai_platform_gateway_foundation::tenant_database::begin(&self.pool, &actor.tenant_id)
                .await?;
        self.authorize_object(&mut tx, actor, id, "owner", true)
            .await?;
        let tokens = sqlx::query(sql::REVOKE0)
            .bind(&actor.tenant_id)
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let mcp = sqlx::query(sql::REVOKE1)
            .bind(&actor.tenant_id)
            .bind(id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let connectors = sqlx::query(sql::REVOKE2)
            .bind(&actor.tenant_id)
            .bind(id)
            .bind(&actor.user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
        let counts =
            json!({"api_tokens":tokens,"mcp_channel_grants":mcp,"connector_grants":connectors});
        audit(&mut tx, actor, id, "credentials_revoke", counts.clone()).await?;
        tx.commit().await?;
        Ok(counts)
    }
    pub async fn audit_events(
        &self,
        actor: &Actor,
        id: &str,
        filters: &Value,
        limit: i64,
        offset: i64,
    ) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        self.authorize_object(&mut c, actor, id, "owner", false)
            .await?;
        let sql = "SELECT to_jsonb(a)||jsonb_build_object('_total',COUNT(*) OVER()) FROM audit_logs a WHERE tenant_id=$1 AND agent_id=$2::uuid AND event_type='agent_studio' AND($3::text IS NULL OR agent_version_id=$3::uuid) AND($4::text IS NULL OR publication_id=$4::uuid) AND($5::text IS NULL OR channel=$5) AND($6::text IS NULL OR action=$6) AND($7::text IS NULL OR created_at>=$7::timestamptz) AND($8::text IS NULL OR created_at<=$8::timestamptz) ORDER BY created_at DESC,id DESC LIMIT $9 OFFSET $10";
        let mut rows: Vec<Value> = sqlx::query_scalar(sql)
            .bind(&actor.tenant_id)
            .bind(id)
            .bind(filters["agent_version_id"].as_str())
            .bind(filters["publication_id"].as_str())
            .bind(filters["channel"].as_str())
            .bind(filters["action"].as_str())
            .bind(filters["started_after"].as_str())
            .bind(filters["started_before"].as_str())
            .bind(limit.clamp(1, 200))
            .bind(offset.max(0))
            .fetch_all(ai_platform_gateway_foundation::tenant_database::connection(
                &mut *c,
                &actor.tenant_id,
            ))
            .await?;
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_logs a WHERE tenant_id=$1 AND agent_id=$2::uuid AND event_type='agent_studio' AND($3::text IS NULL OR agent_version_id=$3::uuid) AND($4::text IS NULL OR publication_id=$4::uuid) AND($5::text IS NULL OR channel=$5) AND($6::text IS NULL OR action=$6) AND($7::text IS NULL OR created_at>=$7::timestamptz) AND($8::text IS NULL OR created_at<=$8::timestamptz)")
            .bind(&actor.tenant_id).bind(id).bind(filters["agent_version_id"].as_str()).bind(filters["publication_id"].as_str()).bind(filters["channel"].as_str()).bind(filters["action"].as_str()).bind(filters["started_after"].as_str()).bind(filters["started_before"].as_str()).fetch_one(ai_platform_gateway_foundation::tenant_database::connection(&mut *c, &actor.tenant_id)).await?;
        for row in &mut rows {
            row.as_object_mut().expect("row").remove("_total");
            for k in ["request_summary", "response_summary"] {
                let v = if let Some(s) = row[k].as_str() {
                    serde_json::from_str(s).unwrap_or(json!({}))
                } else {
                    row[k].clone()
                };
                row[k] = super::spec::redact(&v);
            }
        }
        Ok(json!({"events":rows,"total":total,"limit":limit,"offset":offset}))
    }
}
pub async fn runtime_usage(
    repository: &AgentRepository,
    publication: &Value,
) -> Result<Value, AgentError> {
    let tenant = text(publication, "tenant_id");
    let agent = text(publication, "agent_id");
    let id = text(publication, "publication_id");
    let mut tx = repository.pool.begin().await?;
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_publications WHERE tenant_id=$1 AND agent_id=$2::uuid AND publication_id=$3::uuid AND status='active')").bind(tenant).bind(agent).bind(id).fetch_one(&mut *tx).await?;
    if !exists {
        return Err(AgentError::code(409, "PUBLICATION_DISABLED"));
    }
    let mut policy = default_policy(tenant, agent, "system:agent-governance");
    if let Some(row)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(p) FROM agent_governance_policies p WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(tenant).bind(agent).fetch_optional(&mut *tx).await?{if let Some(m)=row.as_object(){policy.as_object_mut().expect("policy").extend(m.clone());}}
    let row = sqlx::query(sql::USAGE_SQL)
        .bind(tenant)
        .bind(agent)
        .fetch_one(&mut *tx)
        .await?;
    let mut usage = json!({});
    let mut exceeded = Vec::new();
    let threshold = policy["alert_threshold_percent"].as_i64().unwrap_or(90);
    for (quota, field, code) in [
        (
            "concurrent_runs",
            "max_concurrent_runs",
            "AGENT_RUNTIME_CONCURRENCY_QUOTA_EXCEEDED",
        ),
        (
            "daily_tokens",
            "max_daily_tokens",
            "AGENT_RUNTIME_TOKEN_QUOTA_EXCEEDED",
        ),
        (
            "daily_mcp_calls",
            "max_daily_mcp_calls",
            "AGENT_RUNTIME_MCP_QUOTA_EXCEEDED",
        ),
        (
            "storage_bytes",
            "max_storage_bytes",
            "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED",
        ),
    ] {
        let n: i64 = row.try_get(quota)?;
        let limit = policy[field].as_i64().unwrap_or(0);
        usage[quota] = n.into();
        if i128::from(n) * 100 >= i128::from(limit) * i128::from(threshold) {
            let action = if n >= limit {
                "quota_exceeded"
            } else {
                "quota_threshold_reached"
            };
            sqlx::query("INSERT INTO audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,status) SELECT 'agent_studio','system:agent-governance',$1::varchar,'agent',$2::varchar,$3::varchar,$4::jsonb,'success' WHERE NOT EXISTS(SELECT 1 FROM audit_logs WHERE tenant_id=$1::varchar AND agent_id=$2::uuid AND action=$3::varchar AND request_summary->>'quota'=$5::text AND created_at>=date_trunc('day',NOW()))").bind(tenant).bind(agent).bind(action).bind(json!({"publication_id":id,"quota":quota,"usage":n,"limit":limit,"threshold_percent":threshold})).bind(quota).execute(&mut *tx).await?;
        }
        if n >= limit {
            exceeded.push(code);
        }
    }
    tx.commit().await?;
    Ok(json!({"policy":policy,"usage":usage,"exceeded":exceeded}))
}
