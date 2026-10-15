//! Durable data cleanup fences external deletion with a dedicated SQL session
//! lock and immutable plan/inventory receipts before any relational purge.
use super::{
    Actor, AgentError, deletion_contract as contract,
    governance::default_policy,
    memory_cleanup::RuntimeMemoryCleanup,
    spec,
    studio::{AgentRepository, audit, authorized},
    text,
};
use crate::migration_content::storage::ObjectStorage;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection};
use std::{collections::BTreeSet, sync::Arc};
fn error(code: &str) -> AgentError {
    AgentError::code(409, code)
}
fn digest(s: &str) -> String {
    spec::hex(&Sha256::digest(s.as_bytes()))
}
fn object(v: &Value) -> Result<Value, AgentError> {
    let v = if let Some(s) = v.as_str() {
        serde_json::from_str(s).map_err(|_| error("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"))?
    } else if v.is_null() {
        json!({})
    } else {
        v.clone()
    };
    if !v.is_object() {
        return Err(error("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"));
    }
    Ok(v)
}
fn at(v: &Value, key: &str) -> Result<DateTime<Utc>, AgentError> {
    DateTime::parse_from_rfc3339(text(v, key))
        .map(|v| v.with_timezone(&Utc))
        .map_err(|_| error("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"))
}
#[derive(Clone)]
pub struct AgentDataCleanup {
    pub repository: Arc<AgentRepository>,
    pub storage: Arc<dyn ObjectStorage>,
    pub memory: Arc<RuntimeMemoryCleanup>,
}
async fn memory_principals(
    conn: &mut PgConnection,
    actor: &Actor,
    agent: &str,
    scope: &str,
    subject: Option<&str>,
    cutoff: DateTime<Utc>,
) -> Result<Vec<String>, AgentError> {
    let stamp = if scope == "retention" {
        "updated_at"
    } else {
        "created_at"
    };
    let sql = format!(
        "SELECT jsonb_build_object('user_id',user_id,'agent_version_id',agent_version_id,'agent_draft_revision',agent_draft_revision) FROM assistant.sessions WHERE tenant_id=$1 AND agent_id=$2::uuid AND {stamp}<=$3 AND($4::text IS NULL OR user_id=$4)"
    );
    let rows: Vec<Value> = sqlx::query_scalar(&sql)
        .bind(&actor.tenant_id)
        .bind(agent)
        .bind(cutoff)
        .bind(subject)
        .fetch_all(&mut *conn)
        .await?;
    let mut handles = BTreeSet::new();
    let mut add = |caller: &str, version: &str| {
        let hash = digest(&format!("{caller}:{agent}:{version}"));
        handles.insert(format!("am_{}", &hash[..61]));
        handles.insert(format!("agent-memory:{hash}"));
    };
    for row in rows {
        let caller = text(&row, "user_id");
        if caller.is_empty() {
            continue;
        }
        let version = if let Some(id) = row["agent_version_id"].as_str() {
            format!("version:{id}")
        } else if let Some(r) = row["agent_draft_revision"].as_i64() {
            format!("draft:{r}")
        } else {
            continue;
        };
        add(caller, &version);
    }
    if matches!(scope, "user" | "tenant") {
        let callers: Vec<String> = if let Some(subject) = subject {
            vec![subject.into()]
        } else {
            sqlx::query_scalar("SELECT user_id FROM users WHERE tenant_id=$1")
                .bind(&actor.tenant_id)
                .fetch_all(&mut *conn)
                .await?
        };
        let versions:Vec<String>=sqlx::query_scalar("SELECT agent_version_id::text FROM agent_versions WHERE tenant_id=$1 AND agent_id=$2::uuid AND created_at<=$3").bind(&actor.tenant_id).bind(agent).bind(cutoff).fetch_all(&mut *conn).await?;
        let revision:Option<i64>=sqlx::query_scalar("SELECT revision::bigint FROM agent_drafts WHERE tenant_id=$1 AND agent_id=$2::uuid AND created_at<=$3").bind(&actor.tenant_id).bind(agent).bind(cutoff).fetch_optional(&mut *conn).await?;
        for caller in callers {
            for v in &versions {
                add(&caller, &format!("version:{v}"));
            }
            for r in 1..=revision.unwrap_or(0) {
                add(&caller, &format!("draft:{r}"));
            }
        }
    }
    Ok(handles.into_iter().collect())
}
impl AgentRepository {
    pub async fn prepare_data_deletion(
        &self,
        actor: &Actor,
        agent: &str,
        scope: &str,
        subject: Option<&str>,
        key: &str,
    ) -> Result<Value, AgentError> {
        if !matches!(scope, "retention" | "user" | "tenant") {
            return Err(error("AGENT_DATA_DELETION_SCOPE_INVALID"));
        }
        if (scope == "user") != subject.is_some_and(|s| !s.is_empty()) {
            return Err(error("AGENT_DATA_DELETION_SUBJECT_INVALID"));
        }
        if key.is_empty() || key.len() > 255 {
            return Err(error("AGENT_DATA_DELETION_IDEMPOTENCY_KEY_INVALID"));
        }
        let mut tx = self.pool.begin().await?;
        let sql = "SELECT to_jsonb(d) FROM agent_data_deletion_requests d WHERE tenant_id=$1 AND agent_id=$2::uuid AND scope=$3 AND idempotency_key=$4";
        if let Some(old) = sqlx::query_scalar::<_, Value>(sql)
            .bind(&actor.tenant_id)
            .bind(agent)
            .bind(scope)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?
        {
            if !actor.is_admin() && old["requested_by"] != actor.user_id {
                return Err(AgentError::code(404, "AGENT_NOT_FOUND"));
            }
            if matches!(text(&old, "status"), "completed" | "blocked") {
                return Ok(old);
            }
        }
        authorized(&mut tx, actor, agent, "owner", true).await?;
        if let Some(old) = sqlx::query_scalar::<_, Value>(&format!("{sql} FOR UPDATE"))
            .bind(&actor.tenant_id)
            .bind(agent)
            .bind(scope)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?
        {
            if !actor.is_admin() && old["requested_by"] != actor.user_id {
                return Err(AgentError::code(404, "AGENT_NOT_FOUND"));
            }
            return Ok(old);
        }
        let mut policy = default_policy(&actor.tenant_id, agent, &actor.user_id);
        if let Some(p)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(p) FROM agent_governance_policies p WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(agent).fetch_optional(&mut *tx).await?{policy.as_object_mut().expect("policy").extend(p.as_object().expect("row").clone());}
        if policy["legal_hold"] == true {
            let row:Value=sqlx::query_scalar("INSERT INTO agent_data_deletion_requests AS d(tenant_id,agent_id,scope,subject_user_id,idempotency_key,status,error_code,requested_by,completed_at) VALUES($1,$2::uuid,$3,$4,$5,'blocked','AGENT_LEGAL_HOLD_ACTIVE',$6,NOW()) RETURNING to_jsonb(d)").bind(&actor.tenant_id).bind(agent).bind(scope).bind(subject).bind(key).bind(&actor.user_id).fetch_one(&mut *tx).await?;
            audit(
                &mut tx,
                actor,
                agent,
                "data_deletion_blocked",
                json!({"scope":scope,"subject_user_id":subject}),
            )
            .await?;
            tx.commit().await?;
            return Ok(row);
        }
        let keys:Vec<String>=sqlx::query_scalar("SELECT DISTINCT a.storage_key FROM agent_runtime_attachments a JOIN agent_publications p ON p.tenant_id=a.tenant_id AND p.publication_id=a.publication_id WHERE a.tenant_id=$1 AND p.agent_id=$2::uuid AND a.deleted_at IS NULL AND($3::text IS NULL OR a.principal_id=$3) AND($4::boolean=FALSE OR a.expires_at<NOW() OR a.created_at<NOW()-make_interval(days=>$5::int)) ORDER BY a.storage_key").bind(&actor.tenant_id).bind(agent).bind(subject).bind(scope=="retention").bind(policy["attachment_retention_days"].as_i64().unwrap_or(1)as i32).fetch_all(&mut *tx).await?;
        let p = json!({"trace_retention_days":policy["trace_retention_days"],"runtime_retention_days":policy["runtime_retention_days"],"attachment_retention_days":policy["attachment_retention_days"]});
        let row:Value=sqlx::query_scalar("INSERT INTO agent_data_deletion_requests AS d(tenant_id,agent_id,scope,subject_user_id,idempotency_key,object_keys,deleted_counts,requested_by) VALUES($1,$2::uuid,$3,$4,$5,$6::jsonb,$7::jsonb,$8) RETURNING to_jsonb(d)").bind(&actor.tenant_id).bind(agent).bind(scope).bind(subject).bind(key).bind(json!(keys)).bind(json!({"policy":p})).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        let requested = at(&row, "requested_at")?;
        let cutoff = if scope == "retention" {
            requested - Duration::days(p["runtime_retention_days"].as_i64().unwrap_or(30))
        } else {
            requested
        };
        let handles = memory_principals(&mut tx, actor, agent, scope, subject, cutoff).await?;
        let plan = contract::build_plan(
            text(&row, "deletion_id"),
            &actor.tenant_id,
            agent,
            scope,
            subject,
            &cutoff.to_rfc3339(),
            handles,
        )?;
        let row:Value=sqlx::query_scalar("UPDATE agent_data_deletion_requests d SET deleted_counts=$2::jsonb WHERE deletion_id=$1::uuid RETURNING to_jsonb(d)").bind(text(&row,"deletion_id")).bind(json!({"policy":p,"runtime_cleanup_plan":plan})).fetch_one(&mut *tx).await?;
        audit(&mut tx,actor,agent,"data_deletion_requested",json!({"deletion_id":row["deletion_id"],"scope":scope,"subject_user_id":subject,"object_count":keys.len()})).await?;
        tx.commit().await?;
        Ok(row)
    }
}
struct Claim {
    conn: PgConnection,
    actor: Actor,
    agent: String,
    id: String,
    generation: i64,
    digest: String,
    row: Value,
    claimed: bool,
}
impl Claim {
    async fn acquire(
        repository: &AgentRepository,
        actor: &Actor,
        agent: &str,
        id: &str,
    ) -> Result<Self, AgentError> {
        let mut conn = repository.pool.acquire().await?.detach();
        let bytes = Sha256::digest(
            format!("agent-data-deletion:{}:{agent}:{id}", actor.tenant_id).as_bytes(),
        );
        let lock = i64::from_be_bytes(bytes[..8].try_into().expect("digest"));
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1::bigint)")
            .bind(lock)
            .fetch_one(&mut conn)
            .await?;
        let mut tx = conn.begin().await?;
        authorized(&mut tx, actor, agent, "owner", locked).await?;
        let sql = format!(
            "SELECT to_jsonb(d) FROM agent_data_deletion_requests d WHERE tenant_id=$1 AND agent_id=$2::uuid AND deletion_id=$3::uuid {}",
            if locked { "FOR UPDATE" } else { "" }
        );
        let mut row: Value = sqlx::query_scalar(&sql)
            .bind(&actor.tenant_id)
            .bind(agent)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| error("AGENT_DATA_DELETION_NOT_FOUND"))?;
        let (mut generation, mut claim_digest, mut claimed) = (0, String::new(), false);
        if locked && matches!(text(&row, "status"), "pending" | "failed") {
            let hold:Option<bool>=sqlx::query_scalar("SELECT legal_hold FROM agent_governance_policies WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(agent).fetch_optional(&mut *tx).await?;
            if hold == Some(true) {
                row=sqlx::query_scalar("UPDATE agent_data_deletion_requests d SET status='blocked',error_code='AGENT_LEGAL_HOLD_ACTIVE',completed_at=NOW() WHERE deletion_id=$1::uuid RETURNING to_jsonb(d)").bind(id).fetch_one(&mut *tx).await?;
                audit(
                    &mut tx,
                    actor,
                    agent,
                    "data_deletion_blocked",
                    json!({"deletion_id":id,"scope":row["scope"],"stage":"execution_claim"}),
                )
                .await?;
            } else {
                let mut counts = object(&row["deleted_counts"])?;
                let prior = object(&counts["cleanup_execution"])?;
                let previous = prior
                    .get("generation")
                    .map(|v| {
                        v.as_i64()
                            .filter(|n| *n >= 0)
                            .ok_or_else(|| error("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"))
                    })
                    .transpose()?
                    .unwrap_or(0);
                generation = previous
                    .checked_add(1)
                    .ok_or_else(|| error("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"))?;
                let token = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
                claim_digest = digest(&format!("{id}\0{generation}\0{token}"));
                let at: DateTime<Utc> = sqlx::query_scalar("SELECT NOW()")
                    .fetch_one(&mut *tx)
                    .await?;
                let recovered = !prior.as_object().expect("object").is_empty();
                counts["cleanup_execution"] = json!({"schema_version":"agent-data-deletion-execution/v1","state":"claimed","generation":generation,"claim_digest":claim_digest,"claimed_at":at.to_rfc3339(),"recovered":recovered});
                row=sqlx::query_scalar("UPDATE agent_data_deletion_requests d SET status='pending',deleted_counts=$2::jsonb,error_code='AGENT_DATA_DELETION_EXECUTION_IN_PROGRESS',attempt_count=attempt_count+1,last_attempt_at=NOW(),completed_at=NULL WHERE deletion_id=$1::uuid RETURNING to_jsonb(d)").bind(id).bind(counts).fetch_one(&mut *tx).await?;
                audit(
                    &mut tx,
                    actor,
                    agent,
                    "data_deletion_execution_claimed",
                    json!({"deletion_id":id,"generation":generation,"recovered":recovered}),
                )
                .await?;
                claimed = true;
            }
        }
        tx.commit().await?;
        row["execution_claimed"] = claimed.into();
        Ok(Self {
            conn,
            actor: actor.clone(),
            agent: agent.into(),
            id: id.into(),
            generation,
            digest: claim_digest,
            row,
            claimed,
        })
    }
    async fn fence(&mut self) -> Result<(), AgentError> {
        let row:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_data_deletion_requests d WHERE tenant_id=$1 AND agent_id=$2::uuid AND deletion_id=$3::uuid").bind(&self.actor.tenant_id).bind(&self.agent).bind(&self.id).fetch_optional(&mut self.conn).await?;
        let row = row.ok_or_else(|| error("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST"))?;
        let e = &row["deleted_counts"]["cleanup_execution"];
        if row["status"] != "pending"
            || e["state"] != "claimed"
            || e["schema_version"] != "agent-data-deletion-execution/v1"
            || e["generation"].as_i64() != Some(self.generation)
            || text(e, "claim_digest") != self.digest
        {
            return Err(error("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST"));
        }
        Ok(())
    }
    async fn freeze(&mut self, inventory: &Value) -> Result<(), AgentError> {
        self.fence().await?;
        let mut tx = self.conn.begin().await?;
        authorized(&mut tx, &self.actor, &self.agent, "owner", true).await?;
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_data_deletion_requests d WHERE tenant_id=$1 AND agent_id=$2::uuid AND deletion_id=$3::uuid FOR UPDATE").bind(&self.actor.tenant_id).bind(&self.agent).bind(&self.id).fetch_one(&mut *tx).await?;
        let mut counts = object(&row["deleted_counts"])?;
        contract::validate_inventory(inventory, &counts["runtime_cleanup_plan"])?;
        if !counts["runtime_cleanup_inventory"].is_null() {
            contract::validate_inventory(
                &counts["runtime_cleanup_inventory"],
                &counts["runtime_cleanup_plan"],
            )?;
            if counts["runtime_cleanup_inventory"]["inventory_digest"]
                != inventory["inventory_digest"]
            {
                return Err(error("AGENT_RUNTIME_CLEANUP_INVENTORY_CONFLICT"));
            }
        } else {
            counts["runtime_cleanup_inventory"] = inventory.clone();
            self.row=sqlx::query_scalar("UPDATE agent_data_deletion_requests d SET deleted_counts=$2::jsonb WHERE deletion_id=$1::uuid RETURNING to_jsonb(d)").bind(&self.id).bind(counts).fetch_one(&mut *tx).await?;
            audit(&mut tx,&self.actor,&self.agent,"data_deletion_inventory_frozen",json!({"deletion_id":self.id,"principal_count":inventory["principal_count"],"source_count":inventory["source_count"],"inventory_digest":inventory["inventory_digest"]})).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
impl AgentDataCleanup {
    pub async fn execute(
        &self,
        actor: &Actor,
        agent: &str,
        scope: &str,
        subject: Option<&str>,
        key: &str,
    ) -> Result<Value, AgentError> {
        let prepared = self
            .repository
            .prepare_data_deletion(actor, agent, scope, subject, key)
            .await?;
        if !matches!(text(&prepared, "status"), "pending" | "failed") {
            return Ok(prepared);
        }
        let mut claim = Claim::acquire(
            &self.repository,
            actor,
            agent,
            text(&prepared, "deletion_id"),
        )
        .await?;
        if !claim.claimed {
            return Ok(claim.row);
        }
        let keys: Vec<String> = claim.row["object_keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut storage_ok = true;
        for key in keys {
            claim.fence().await?;
            if self.storage.delete(&key).await.is_err() {
                storage_ok = false;
            }
            claim.fence().await?;
            if !matches!(self.storage.head(&key).await, Ok(None)) {
                storage_ok = false;
            }
        }
        let mut receipt = None;
        if storage_ok {
            let plan = claim.row["deleted_counts"]["runtime_cleanup_plan"].clone();
            let mut inventory = claim.row["deleted_counts"]["runtime_cleanup_inventory"].clone();
            if inventory.is_null() {
                if let Ok(value) = self.memory.inspect(&plan).await {
                    claim.freeze(&value).await?;
                    inventory = value;
                }
            }
            if !inventory.is_null() {
                claim.fence().await?;
                receipt = self.memory.execute(&plan, &inventory).await.ok();
            }
        }
        claim.finish(storage_ok, receipt.as_ref()).await
    }
}
impl Claim {
    async fn finish(
        &mut self,
        storage_ok: bool,
        receipt: Option<&Value>,
    ) -> Result<Value, AgentError> {
        self.fence().await?;
        let mut tx = self.conn.begin().await?;
        authorized(&mut tx, &self.actor, &self.agent, "owner", true).await?;
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_data_deletion_requests d WHERE tenant_id=$1 AND agent_id=$2::uuid AND deletion_id=$3::uuid FOR UPDATE").bind(&self.actor.tenant_id).bind(&self.agent).bind(&self.id).fetch_one(&mut *tx).await?;
        let mut counts = object(&row["deleted_counts"])?;
        let execution = &counts["cleanup_execution"];
        if row["status"] != "pending"
            || execution["state"] != "claimed"
            || execution["generation"].as_i64() != Some(self.generation)
            || execution["claim_digest"] != self.digest
        {
            return Err(error("AGENT_DATA_DELETION_EXECUTION_CLAIM_INVALID"));
        }
        let hold:Option<bool>=sqlx::query_scalar("SELECT legal_hold FROM agent_governance_policies WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&self.actor.tenant_id).bind(&self.agent).fetch_optional(&mut *tx).await?;
        let valid = receipt.is_some_and(|r| {
            contract::validate_receipt(
                r,
                &counts["runtime_cleanup_plan"],
                &counts["runtime_cleanup_inventory"],
            )
            .is_ok()
                && contract::completed(r)
        });
        let failure = if hold == Some(true) {
            Some((
                "AGENT_LEGAL_HOLD_RACE_DETECTED",
                "legal_hold_race_detected",
                "interrupted",
                "legal_hold",
                true,
            ))
        } else if !storage_ok {
            Some((
                "AGENT_STORAGE_CLEANUP_FAILED",
                "storage_cleanup_incomplete",
                "retryable",
                "storage",
                false,
            ))
        } else if !valid {
            Some((
                "AGENT_RUNTIME_CLEANUP_FAILED",
                "runtime_cleanup_incomplete",
                "retryable",
                "runtime_memory",
                false,
            ))
        } else {
            None
        };
        if let Some((code, reason, state, stage, completed)) = failure {
            counts["cleanup_execution"]
                .as_object_mut()
                .expect("validated execution")
                .remove("claim_digest");
            counts["cleanup_execution"]["state"] = state.into();
            counts["cleanup_execution"]["reason"] = reason.into();
            let row:Value=sqlx::query_scalar("UPDATE agent_data_deletion_requests d SET status='failed',error_code=$2,deleted_counts=$3::jsonb,completed_at=CASE WHEN $4::boolean THEN NOW() ELSE NULL END WHERE deletion_id=$1::uuid RETURNING to_jsonb(d)").bind(&self.id).bind(code).bind(counts).bind(completed).fetch_one(&mut *tx).await?;
            audit(
                &mut tx,
                &self.actor,
                &self.agent,
                "data_deletion_failed",
                json!({"deletion_id":self.id,"stage":stage}),
            )
            .await?;
            tx.commit().await?;
            return Ok(row);
        }
        let receipt = receipt.expect("validated receipt");
        let plan = &counts["runtime_cleanup_plan"];
        let inventory = &counts["runtime_cleanup_inventory"];
        let scope = text(&row, "scope");
        let subject = row["subject_user_id"].as_str();
        let cutoff = at(plan, "cutoff_at")?;
        let tenant = &self.actor.tenant_id;
        let agent = &self.agent;
        let stamp = if scope == "retention" {
            "updated_at"
        } else {
            "created_at"
        };
        let sessions:Vec<String>=sqlx::query_scalar(&format!("SELECT session_id FROM assistant.sessions WHERE tenant_id=$1 AND agent_id=$2::uuid AND {stamp}<=$3 AND($4::text IS NULL OR user_id=$4)")).bind(tenant).bind(agent).bind(cutoff).bind(subject).fetch_all(&mut *tx).await?;
        let mut deleted = json!({});
        for (name, table) in [
            ("checkpoints", "assistant_run_checkpoints"),
            ("runs", "assistant_runs"),
        ] {
            let sql = format!(
                "DELETE FROM {table} WHERE tenant_id=$1 AND agent_id=$2::uuid AND created_at<=$3 AND($4::text IS NULL OR user_id=$4)"
            );
            deleted[name] = sqlx::query(&sql)
                .bind(tenant)
                .bind(agent)
                .bind(cutoff)
                .bind(subject)
                .execute(&mut *tx)
                .await?
                .rows_affected()
                .into();
        }
        deleted["session_memory"] = sqlx::query(
            "DELETE FROM session_memory WHERE tenant_id=$1 AND session_id=ANY($2::varchar[])",
        )
        .bind(tenant)
        .bind(&sessions)
        .execute(&mut *tx)
        .await?
        .rows_affected()
        .into();
        let principals: Vec<_> = plan["principal_handles"]
            .as_array()
            .expect("validated plan")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for (name, table) in [
            ("user_memory", "user_memory"),
            ("memory_reflections", "assistant_memory_reflections"),
        ] {
            deleted[name]=sqlx::query(&format!("DELETE FROM {table} WHERE tenant_id=$1 AND user_id=ANY($2::varchar[]) AND updated_at<=$3")).bind(tenant).bind(&principals).bind(cutoff).execute(&mut *tx).await?.rows_affected().into();
        }
        let sum = |key: &str| {
            receipt["principals"]
                .as_array()
                .expect("validated receipt")
                .iter()
                .filter_map(|p| p[key].as_u64())
                .sum::<u64>()
        };
        deleted["memory_sources"] = sum("deleted_source_count").into();
        deleted["memory_vectors"] = sum("deleted_vector_count").into();
        deleted["runtime_cleanup"] = json!({"status":"completed","principal_count":principals.len(),"plan_digest":plan["plan_digest"],"inventory_digest":inventory["inventory_digest"],"receipt_digest":receipt["receipt_digest"]});
        deleted["sessions"]=sqlx::query(&format!("DELETE FROM assistant.sessions WHERE tenant_id=$1 AND agent_id=$2::uuid AND {stamp}<=$3 AND($4::text IS NULL OR user_id=$4)")).bind(tenant).bind(agent).bind(cutoff).bind(subject).execute(&mut *tx).await?.rows_affected().into();
        let requested = at(&row, "requested_at")?;
        let trace_cutoff = if scope == "retention" {
            requested
                - Duration::days(
                    counts["policy"]["trace_retention_days"]
                        .as_i64()
                        .unwrap_or(90),
                )
        } else {
            requested
        };
        deleted["traces"]=sqlx::query("DELETE FROM agent_traces WHERE tenant_id=$1 AND agent_id=$2::uuid AND created_at<=$3 AND($4::text IS NULL OR user_id=$4)").bind(tenant).bind(agent).bind(trace_cutoff).bind(subject).execute(&mut *tx).await?.rows_affected().into();
        for table in ["agent_runtime_idempotency", "agent_runtime_feedback"] {
            deleted[table]=sqlx::query(&format!("DELETE FROM {table} r USING agent_publications p WHERE p.tenant_id=$1 AND p.agent_id=$2::uuid AND r.tenant_id=p.tenant_id AND r.publication_id=p.publication_id AND r.created_at<=$3 AND($4::text IS NULL OR r.principal_id=$4)")).bind(tenant).bind(agent).bind(cutoff).bind(subject).execute(&mut *tx).await?.rows_affected().into();
        }
        let keys: Vec<_> = row["object_keys"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        deleted["attachments"]=sqlx::query("DELETE FROM agent_runtime_attachments a USING agent_publications p WHERE p.tenant_id=$1 AND p.agent_id=$2::uuid AND a.tenant_id=p.tenant_id AND a.publication_id=p.publication_id AND a.storage_key=ANY($3::text[])").bind(tenant).bind(agent).bind(keys).execute(&mut *tx).await?.rows_affected().into();
        let revoked = if matches!(scope, "user" | "tenant") {
            sqlx::query("UPDATE agent_api_tokens t SET revoked_at=COALESCE(revoked_at,NOW()) FROM agent_publications p WHERE p.tenant_id=$1 AND p.agent_id=$2::uuid AND t.tenant_id=p.tenant_id AND t.publication_id=p.publication_id AND t.revoked_at IS NULL AND($3::text IS NULL OR t.created_by=$3)").bind(tenant).bind(agent).bind(subject).execute(&mut *tx).await?.rows_affected()
        } else {
            0
        };
        deleted["api_tokens_revoked"] = revoked.into();
        if scope == "user" {
            for table in ["mcp_connections", "connector_credential_principals"] {
                sqlx::query(&format!("UPDATE {table} SET enabled=FALSE,revoked_at=COALESCE(revoked_at,NOW()),updated_by=$3,updated_at=NOW() WHERE tenant_id=$1 AND owner_user_id=$2 AND enabled=TRUE")).bind(tenant).bind(subject).bind(&self.actor.user_id).execute(&mut *tx).await?;
            }
        }
        deleted["semantic_cache"]=sqlx::query("DELETE FROM semantic_cache WHERE metadata->>'tenant_id'=$1 AND metadata->>'agent_id'=$2 AND created_at<=$3 AND($4::text IS NULL OR metadata->>'user_id'=$4 OR metadata->>'principal_id'=$4 OR metadata->>'caller_principal'=$4)").bind(tenant).bind(agent).bind(cutoff).bind(subject).execute(&mut *tx).await?.rows_affected().into();
        if scope == "tenant" {
            deleted["publications_disabled"]=sqlx::query("UPDATE agent_publications SET status='disabled',updated_by=$3,updated_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid AND status<>'disabled'").bind(tenant).bind(agent).bind(&self.actor.user_id).execute(&mut *tx).await?.rows_affected().into();
            deleted["mcp_channel_grants_revoked"]=sqlx::query("UPDATE mcp_channel_grants g SET enabled=FALSE,updated_at=NOW() WHERE g.tenant_id=$1 AND g.enabled=TRUE AND g.tool_id::text IN(SELECT c.resource_id FROM agent_version_capabilities c JOIN agent_versions v ON v.tenant_id=c.tenant_id AND v.agent_version_id=c.agent_version_id WHERE v.tenant_id=$1 AND v.agent_id=$2::uuid AND c.capability_type='mcp')").bind(tenant).bind(agent).execute(&mut *tx).await?.rows_affected().into();
            deleted["connector_grants_revoked"]=sqlx::query("UPDATE connector_credential_principals g SET enabled=FALSE,revoked_at=COALESCE(g.revoked_at,NOW()),updated_by=$3,updated_at=NOW() WHERE g.tenant_id=$1 AND g.enabled=TRUE AND g.grant_id::text IN(SELECT c.resource_id FROM agent_version_capabilities c JOIN agent_versions v ON v.tenant_id=c.tenant_id AND v.agent_version_id=c.agent_version_id WHERE v.tenant_id=$1 AND v.agent_id=$2::uuid AND c.capability_type='connector')").bind(tenant).bind(agent).bind(&self.actor.user_id).execute(&mut *tx).await?.rows_affected().into();
            for (name, table) in [
                ("draft_knowledge_bindings", "agent_draft_knowledge_bindings"),
                ("draft_skill_bindings", "agent_draft_skill_bindings"),
            ] {
                deleted[name]=sqlx::query(&format!("DELETE FROM {table} b USING agent_drafts d WHERE d.tenant_id=$1 AND d.agent_id=$2::uuid AND b.tenant_id=d.tenant_id AND b.draft_id=d.draft_id")).bind(tenant).bind(agent).execute(&mut *tx).await?.rows_affected().into();
            }
            let scrub = json!({"deleted":true});
            deleted["drafts_scrubbed"]=sqlx::query("UPDATE agent_drafts SET spec=$3::jsonb,spec_hash=$4,updated_by=$5,updated_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(tenant).bind(agent).bind(&scrub).bind(spec::hash(&scrub)).bind(&self.actor.user_id).execute(&mut *tx).await?.rows_affected().into();
            deleted["agents_disabled"]=sqlx::query("UPDATE agents SET current_draft_id=NULL,status='deleted',deleted_at=COALESCE(deleted_at,NOW()),updated_by=$3,updated_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(tenant).bind(agent).bind(&self.actor.user_id).execute(&mut *tx).await?.rows_affected().into();
            deleted["acl_members"] =
                sqlx::query("DELETE FROM agent_members WHERE tenant_id=$1 AND agent_id=$2::uuid")
                    .bind(tenant)
                    .bind(agent)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected()
                    .into();
        }
        deleted["cleanup_execution"] = json!({"schema_version":"agent-data-deletion-execution/v1","state":"completed","generation":self.generation});
        let result:Value=sqlx::query_scalar("UPDATE agent_data_deletion_requests d SET status='completed',deleted_counts=$2::jsonb,error_code=NULL,completed_at=NOW() WHERE deletion_id=$1::uuid RETURNING to_jsonb(d)").bind(&self.id).bind(&deleted).fetch_one(&mut *tx).await?;
        audit(
            &mut tx,
            &self.actor,
            &self.agent,
            "data_deletion_completed",
            json!({"deletion_id":self.id,"scope":scope,"counts":deleted}),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
}
