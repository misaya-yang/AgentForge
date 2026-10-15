//! Durable release evaluation lifecycle, idempotent publication and rollback.
use super::{
    Actor, AgentError, release_contract as contract,
    routes::AgentState,
    spec,
    studio::{AgentRepository, audit, authorized},
    text,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
fn error(code: &str) -> AgentError {
    AgentError::code(409, code)
}
fn stale(revision: i64) -> AgentError {
    AgentError {
        status: 409,
        detail: json!({"code":"AGENT_EVAL_STALE","current_revision":revision}),
    }
}
fn key_hash(key: &str) -> Result<String, AgentError> {
    if key.trim().is_empty() || key.len() > 255 || key.chars().any(char::is_control) {
        return Err(AgentError::code(
            400,
            "AGENT_RELEASE_IDEMPOTENCY_KEY_INVALID",
        ));
    }
    Ok(spec::hex(&Sha256::digest(key.as_bytes())))
}
fn audience(actor: &Actor, agent: &str, channel: &str, auth: &str) -> Actor {
    let mut user = actor.clone();
    if channel == "api" {
        user.user_id = format!("agent-token:release-check:{agent}");
        user.tier = "service".into();
        user.roles = vec!["agent_runtime".into()];
    } else if matches!(channel, "hosted" | "embed") {
        match auth {
            "tenant" => {
                user.user_id = format!("agent-release-tenant:{agent}");
                user.tier = "normal".into();
                user.roles = vec!["user".into()];
            }
            "public" | "token" => {
                user.user_id = format!("agent-release-public:{agent}");
                user.tier = "anonymous".into();
                user.roles = vec!["guest".into()];
            }
            _ => {}
        }
    }
    user
}
async fn dataset(
    conn: &mut PgConnection,
    tenant: &str,
    id: &str,
    lock: bool,
) -> Result<Value, AgentError> {
    let sql = format!(
        "SELECT to_jsonb(d) FROM eval_datasets d WHERE tenant_id=$1 AND dataset_id=$2::uuid {}",
        if lock { "FOR UPDATE" } else { "" }
    );
    let d: Value = sqlx::query_scalar(&sql)
        .bind(tenant)
        .bind(super::uuid(id)?)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| error("AGENT_EVAL_DATASET_NOT_FOUND"))?;
    let sql = format!(
        "SELECT jsonb_build_object('example_id',example_id,'split',split,'input',input,'expected_output',expected_output,'metadata',metadata,'source_trace_id',source_trace_id,'source_span_id',source_span_id) FROM eval_examples WHERE tenant_id=$1 AND dataset_id=$2::uuid ORDER BY COALESCE(metadata->>'case_id',example_id::text),example_id {}",
        if lock { "FOR SHARE" } else { "" }
    );
    let examples: Vec<Value> = sqlx::query_scalar(&sql)
        .bind(tenant)
        .bind(id)
        .fetch_all(&mut *conn)
        .await?;
    let manifest = crate::migration_compat::eval_queue::build_eval_dataset_manifest(&d, &examples);
    Ok(
        json!({"dataset_id":d["dataset_id"],"tenant_id":d["tenant_id"],"version":d["version"].as_str().unwrap_or("v1"),"manifest_hash":spec::hash(&manifest),"example_count":manifest["examples"].as_array().map(Vec::len).unwrap_or(0)}),
    )
}
async fn eval_result(
    conn: &mut PgConnection,
    tenant: &str,
    mut row: Value,
) -> Result<Value, AgentError> {
    let events:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(e) FROM agent_release_evaluation_events e WHERE tenant_id=$1 AND evaluation_id=$2::uuid ORDER BY sequence").bind(tenant).bind(text(&row,"evaluation_id")).fetch_all(conn).await?;
    row["events"] = json!(events);
    if row["stale"].is_null() {
        row["stale"] = false.into();
    }
    if row["stale"] == true {
        row["status"] = "stale".into();
        row["stale_reasons"] = json!(["draft_changed"]);
    } else {
        row["stale_reasons"] = json!([]);
    }
    Ok(row)
}
async fn eval_event(
    conn: &mut PgConnection,
    tenant: &str,
    id: &str,
    status: &str,
    summary: &Value,
) -> Result<(), AgentError> {
    sqlx::query("INSERT INTO agent_release_evaluation_events(tenant_id,evaluation_id,sequence,status,summary) SELECT $1,$2::uuid,COALESCE(MAX(sequence),0)+1,$3,$4::jsonb FROM agent_release_evaluation_events WHERE tenant_id=$1 AND evaluation_id=$2::uuid").bind(tenant).bind(id).bind(status).bind(summary).execute(conn).await?;
    Ok(())
}
async fn release_result(
    conn: &mut PgConnection,
    tenant: &str,
    request: &Value,
    replayed: bool,
) -> Result<Value, AgentError> {
    let v: Value = sqlx::query_scalar(
        "SELECT to_jsonb(v) FROM agent_versions v WHERE tenant_id=$1 AND agent_version_id=$2::uuid",
    )
    .bind(tenant)
    .bind(text(request, "result_version_id"))
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| error("AGENT_RELEASE_RESULT_UNAVAILABLE"))?;
    let p:Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p WHERE tenant_id=$1 AND publication_id=$2::uuid").bind(tenant).bind(text(request,"result_publication_id")).fetch_optional(&mut *conn).await?.ok_or_else(||error("AGENT_RELEASE_RESULT_UNAVAILABLE"))?;
    let e: Value = sqlx::query_scalar(
        "SELECT to_jsonb(e) FROM agent_publish_events e WHERE tenant_id=$1 AND event_id=$2::uuid",
    )
    .bind(tenant)
    .bind(text(request, "result_event_id"))
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(|| error("AGENT_RELEASE_RESULT_UNAVAILABLE"))?;
    Ok(json!({"version":v,"publication":p,"event":e,"idempotent_replay":replayed}))
}
impl AgentState {
    async fn validate_existing_version_resources(
        &self,
        conn: &mut PgConnection,
        actor: &Actor,
        version: &Value,
    ) -> Result<(), AgentError> {
        let errors = spec::validate(&version["resolved_spec"]);
        if !errors.is_empty() {
            return Err(AgentError::validation(errors));
        }
        let bindings:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('type',capability_type,'resource_id',resource_id,'resource_version',resource_version,'schema_hash',schema_hash,'config',config) FROM agent_version_capabilities WHERE tenant_id=$1 AND agent_version_id=$2::uuid ORDER BY capability_type,resource_id").bind(&actor.tenant_id).bind(text(version,"agent_version_id")).fetch_all(&mut *conn).await?;
        let count = bindings.iter().filter(|v| v["type"] == "skill").count() as i64;
        if count > 0 {
            let active:i64=sqlx::query_scalar("SELECT COUNT(*) FROM agent_version_skill_bindings binding JOIN assistant_skill_versions version ON version.tenant_id=binding.tenant_id AND version.version_id=binding.skill_version_id JOIN assistant_skills skill ON skill.tenant_id=version.tenant_id AND skill.skill_id=version.skill_id WHERE binding.tenant_id=$1 AND binding.agent_version_id=$2::uuid AND skill.user_id=$3 AND skill.enabled=TRUE AND skill.status='active' AND skill.deleted_at IS NULL AND version.status='active' AND version.artifact_type='tenant_instruction' AND NOT EXISTS(SELECT 1 FROM assistant_skill_version_revocations revoked WHERE revoked.tenant_id=version.tenant_id AND revoked.version_id=version.version_id)").bind(&actor.tenant_id).bind(text(version,"agent_version_id")).bind(&actor.user_id).fetch_one(&mut *conn).await?;
            if active != count {
                return Err(AgentError::validation(vec![
                    json!({"field":"capabilities","code":"AGENT_SKILL_VERSION_UNAVAILABLE","message":"one or more Skill versions are unavailable"}),
                ]));
            }
        }
        let mut capability_spec = version["resolved_spec"].clone();
        capability_spec["capabilities"] = json!(
            bindings
                .into_iter()
                .filter(|v| v["type"] != "skill")
                .collect::<Vec<_>>()
        );
        self.repository
            .resources
            .seal_capabilities_in(actor, &capability_spec, true, conn)
            .await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT dataset_id::text FROM agent_version_knowledge_bindings WHERE tenant_id=$1 AND agent_version_id=$2::uuid ORDER BY dataset_id").bind(&actor.tenant_id).bind(text(version,"agent_version_id")).fetch_all(&mut *conn).await?;
        if !ids.is_empty()
            && self
                .repository
                .resources
                .authorize_knowledge(actor, &ids)
                .await?
                != ids.iter().cloned().collect()
        {
            return Err(AgentError::validation(vec![
                json!({"field":"knowledge","code":"AGENT_KNOWLEDGE_UNAVAILABLE","message":"one or more Dataset bindings are unavailable"}),
            ]));
        }
        Ok(())
    }
    pub async fn decorate_evaluation(&self, actor: &Actor, mut row: Value) -> Value {
        let mut reasons: std::collections::BTreeSet<String> = row["stale_reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect();
        if row["stale"] == true || row["status"] == "stale" {
            row["stale"] = true.into();
            row["status"] = "stale".into();
            if reasons.is_empty() {
                reasons.insert("draft_changed".into());
            }
        } else if row["status"] == "passed" {
            match self
                .resolve_release_candidate(
                    actor,
                    text(&row, "agent_id"),
                    row["draft_revision"].as_i64().unwrap_or(0),
                    text(&row, "channel"),
                    text(&row, "auth_mode"),
                    &row["channel_policy"],
                    row["dataset_id"].as_str(),
                )
                .await
            {
                Ok(candidate) => {
                    for (reason, field) in [
                        ("runtime_fingerprint_changed", "runtime_fingerprint_hash"),
                        ("release_identity_changed", "release_identity_hash"),
                        ("evaluation_identity_changed", "evaluation_identity_hash"),
                        ("eval_dataset_changed", "dataset_manifest_hash"),
                    ] {
                        if !row[field].is_null() && row[field] != candidate[field] {
                            reasons.insert(reason.into());
                        }
                    }
                }
                Err(error) => {
                    reasons.insert(
                        error.detail["code"]
                            .as_str()
                            .unwrap_or("runtime_unavailable")
                            .to_lowercase(),
                    );
                }
            }
            if !reasons.is_empty() {
                row["stale"] = true.into();
                row["status"] = "stale".into();
            }
        }
        row["stale_reasons"] = json!(reasons);
        row
    }
    pub async fn resolve_release_candidate(
        &self,
        actor: &Actor,
        agent: &str,
        revision: i64,
        channel: &str,
        auth: &str,
        policy: &Value,
        dataset_id: Option<&str>,
    ) -> Result<Value, AgentError> {
        if !matches!(channel, "hosted" | "embed" | "api")
            || !matches!(auth, "private" | "tenant" | "public" | "token")
            || (auth == "public" && policy["high_risk_tools"] == true)
            || (auth != "public" && !policy["expires_at"].is_null())
        {
            return Err(AgentError::code(422, "AGENT_RELEASE_POLICY_INVALID"));
        }
        let policy = contract::normalized_policy(policy)?;
        contract::public_expiry(auth, &policy)?;
        let mut resolution = self
            .repository
            .resolve_preview(actor, agent, revision)
            .await?;
        resolution["publication"] =
            json!({"publication_id":null,"channel":channel,"auth_mode":auth,"policy":policy});
        let caller = audience(actor, agent, channel, auth);
        let snapshot = self.build_snapshot(&caller, &resolution, channel).await?;
        let model = self
            .control
            .models
            .model(
                &caller.tenant_id,
                text(&snapshot["model"], "id"),
                snapshot["model"]["provider"].as_str(),
            )
            .await?;
        let provider = self
            .control
            .providers
            .provider(&caller.tenant_id, text(&snapshot["model"], "provider"))
            .await?;
        self.control
            .providers
            .runtime_config(&caller.tenant_id, text(&snapshot["model"], "provider"))
            .await?;
        let evidence = contract::model_evidence("database", &model, &provider, true);
        let d = if let Some(id) = dataset_id {
            let mut c = self.repository.pool.acquire().await?;
            Some(dataset(&mut c, &actor.tenant_id, id, false).await?)
        } else {
            None
        };
        contract::candidate(
            &resolution,
            &snapshot,
            channel,
            auth,
            &policy,
            d.as_ref(),
            &evidence,
        )
    }
    async fn revalidate_release(
        &self,
        conn: &mut PgConnection,
        actor: &Actor,
        candidate: &Value,
        draft: &Value,
    ) -> Result<(), AgentError> {
        if candidate["draft_id"] != draft["draft_id"]
            || candidate["draft_revision"] != draft["revision"]
            || candidate["spec_hash"] != draft["spec_hash"]
        {
            return Err(stale(draft["revision"].as_i64().unwrap_or(0)));
        }
        let errors = spec::validate(&draft["spec"]);
        if !errors.is_empty() {
            return Err(AgentError::validation(errors));
        }
        self.repository
            .resources
            .seal_capabilities_in(actor, &draft["spec"], true, conn)
            .await?;
        let ids: Vec<String> = draft["spec"]["knowledge"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|v| v["dataset_id"].as_str())
            .map(str::to_owned)
            .collect();
        if self
            .repository
            .resources
            .authorize_knowledge(actor, &ids)
            .await?
            != ids.iter().cloned().collect()
        {
            return Err(error("AGENT_KNOWLEDGE_UNAVAILABLE"));
        }
        let proof = &candidate["model_authorization"];
        let row:Value=sqlx::query_scalar("SELECT jsonb_build_object('model',to_jsonb(m),'provider',to_jsonb(p)) FROM llm_models m JOIN llm_providers p ON p.tenant_id=m.tenant_id AND p.provider_id=m.provider_id WHERE m.tenant_id=$1 AND m.provider_id=$2 AND m.model_id=$3 FOR SHARE OF m,p").bind(&actor.tenant_id).bind(text(proof,"provider_id")).bind(text(proof,"model_id")).fetch_optional(&mut *conn).await?.ok_or_else(||error("AGENT_RUNTIME_MODEL_UNAVAILABLE"))?;
        let caller = audience(
            actor,
            text(candidate, "agent_id"),
            text(candidate, "channel"),
            text(candidate, "auth_mode"),
        );
        if row["model"]["is_enabled"] != true
            || row["provider"]["is_enabled"] != true
            || !super::entry::user_can_access_model(
                &caller,
                row["model"]["access_level"].as_str().unwrap_or("public"),
            )
        {
            return Err(error("AGENT_RUNTIME_MODEL_FORBIDDEN"));
        }
        let current = contract::model_evidence("database", &row["model"], &row["provider"], true);
        if &current != proof {
            return Err(error("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE"));
        }
        self.control
            .providers
            .runtime_config(&actor.tenant_id, text(proof, "provider_id"))
            .await?;
        if let Some(id) = candidate["dataset_id"].as_str() {
            let d = dataset(conn, &actor.tenant_id, id, true).await?;
            if d["version"] != candidate["dataset_version"]
                || d["manifest_hash"] != candidate["dataset_manifest_hash"]
            {
                return Err(error("AGENT_EVAL_DATASET_STALE"));
            }
        }
        Ok(())
    }
    pub async fn create_release_evaluation(
        &self,
        actor: &Actor,
        id: &str,
        candidate: &Value,
        profile: &Value,
    ) -> Result<Value, AgentError> {
        if candidate["tenant_id"] != actor.tenant_id || candidate["agent_id"] != id {
            return Err(error("AGENT_EVAL_TARGET_MISMATCH"));
        }
        contract::public_expiry(text(candidate, "auth_mode"), &candidate["channel_policy"])?;
        let mut tx = self.repository.pool.begin().await?;
        let agent = authorized(&mut tx, actor, id, "owner", true).await?;
        if matches!(text(&agent, "status"), "archived" | "deleted") {
            return Err(error("AGENT_ARCHIVED"));
        }
        let draft:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(id).fetch_one(&mut *tx).await?;
        self.revalidate_release(&mut tx, actor, candidate, &draft)
            .await?;
        let evaluation = uuid::Uuid::new_v4().to_string();
        let validation = json!({"schema_version":"agent-release-validation/v1","draft_revision":draft["revision"],"spec_hash":draft["spec_hash"],"runtime_fingerprint_hash":candidate["runtime_fingerprint_hash"],"release_identity_hash":candidate["release_identity_hash"],"evaluation_identity_hash":candidate["evaluation_identity_hash"],"dataset_manifest_hash":candidate["dataset_manifest_hash"],"resource_authorization_rechecked":true,"secret_safety_rechecked":true,"lifecycle":"queued"});
        let gate = json!({"schema_version":"agent-release-gate/v1","status":"queued","profile_id":profile["profile_id"],"profile_version":profile["profile_version"],"execution_scope":profile["execution_scope"],"model_quality_evaluated":false,"blocking_findings":[],"non_blocking_findings":[]});
        let row:Value=sqlx::query_scalar("INSERT INTO agent_release_evaluations AS e(tenant_id,evaluation_id,agent_id,draft_id,draft_revision,spec_hash,runtime_fingerprint,runtime_fingerprint_hash,release_identity_hash,evaluation_identity_hash,profile_id,profile_version,dataset_id,dataset_version,dataset_manifest_hash,channel,auth_mode,channel_policy,channel_policy_hash,status,validation_snapshot,gate_snapshot,created_by,completed_at) VALUES($1,$2::uuid,$3::uuid,$4::uuid,$5,$6,$7::jsonb,$8,$9,$10,$11,$12,$13::uuid,$14,$15,$16,$17,$18::jsonb,$19,'queued',$20::jsonb,$21::jsonb,$22,NULL) RETURNING to_jsonb(e)").bind(&actor.tenant_id).bind(&evaluation).bind(id).bind(text(&draft,"draft_id")).bind(draft["revision"].as_i64().unwrap_or(0)).bind(text(&draft,"spec_hash")).bind(&candidate["runtime_fingerprint"]).bind(text(candidate,"runtime_fingerprint_hash")).bind(text(candidate,"release_identity_hash")).bind(text(candidate,"evaluation_identity_hash")).bind(text(profile,"profile_id")).bind(text(profile,"profile_version")).bind(candidate["dataset_id"].as_str()).bind(candidate["dataset_version"].as_str()).bind(candidate["dataset_manifest_hash"].as_str()).bind(text(candidate,"channel")).bind(text(candidate,"auth_mode")).bind(&candidate["channel_policy"]).bind(text(candidate,"channel_policy_hash")).bind(validation).bind(gate).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        eval_event(&mut tx,&actor.tenant_id,&evaluation,"queued",&json!({"profile_id":profile["profile_id"],"profile_version":profile["profile_version"]})).await?;
        audit(&mut tx,actor,id,"release_evaluation_queued",json!({"evaluation_id":evaluation,"draft_revision":draft["revision"],"profile_id":profile["profile_id"]})).await?;
        let result = eval_result(&mut tx, &actor.tenant_id, row).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn execute_release_evaluation(
        &self,
        actor: &Actor,
        agent: &str,
        id: &str,
        profile: &Value,
    ) -> Result<Value, AgentError> {
        let running = self
            .repository
            .transition_evaluation(actor, agent, id, false)
            .await?;
        if running["execution_claimed"] != true {
            return Ok(running);
        }
        let started = std::time::Instant::now();
        let resolved = if running["profile_id"] != profile["profile_id"]
            || running["profile_version"] != profile["profile_version"]
        {
            Err(error("AGENT_RELEASE_PROFILE_STALE"))
        } else {
            self.resolve_release_candidate(
                actor,
                agent,
                running["draft_revision"].as_i64().unwrap_or(0),
                text(&running, "channel"),
                text(&running, "auth_mode"),
                &running["channel_policy"],
                running["dataset_id"].as_str(),
            )
            .await
        };
        let (candidate, gate) = match resolved {
            Ok(candidate) => {
                let mismatch = [
                    "runtime_fingerprint_hash",
                    "release_identity_hash",
                    "evaluation_identity_hash",
                    "dataset_manifest_hash",
                ]
                .iter()
                .any(|k| candidate[*k] != running[*k]);
                if mismatch {
                    (
                        json!({}),
                        failed_gate(
                            profile,
                            "AGENT_EVAL_STALE",
                            started.elapsed().as_secs_f64() * 1000.0,
                        ),
                    )
                } else {
                    let gate = contract::evaluate(
                        &candidate,
                        profile,
                        started.elapsed().as_secs_f64() * 1000.0,
                    )?;
                    (candidate, gate)
                }
            }
            Err(e) => (
                json!({}),
                failed_gate(
                    profile,
                    text(&e.detail, "code"),
                    started.elapsed().as_secs_f64() * 1000.0,
                ),
            ),
        };
        self.complete_release_evaluation(actor, agent, id, &candidate, &gate)
            .await
    }
    pub async fn complete_release_evaluation(
        &self,
        actor: &Actor,
        agent: &str,
        id: &str,
        candidate: &Value,
        gate: &Value,
    ) -> Result<Value, AgentError> {
        let status = text(gate, "status");
        if !matches!(status, "passed" | "failed") {
            return Err(error("AGENT_EVAL_STATUS_INVALID"));
        }
        let mut tx = self.repository.pool.begin().await?;
        authorized(&mut tx, actor, agent, "owner", false).await?;
        let mut row:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM agent_release_evaluations e WHERE tenant_id=$1 AND agent_id=$2::uuid AND evaluation_id=$3::uuid FOR UPDATE").bind(&actor.tenant_id).bind(agent).bind(id).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(404,"AGENT_EVAL_NOT_FOUND"))?;
        if matches!(text(&row, "status"), "cancelled" | "passed" | "failed") {
            return eval_result(&mut tx, &actor.tenant_id, row).await;
        }
        if row["status"] != "running" {
            return Err(error("AGENT_EVAL_LIFECYCLE_INVALID"));
        }
        if status == "passed" {
            if [
                "draft_id",
                "draft_revision",
                "spec_hash",
                "runtime_fingerprint_hash",
                "release_identity_hash",
                "evaluation_identity_hash",
                "channel_policy_hash",
            ]
            .iter()
            .any(|k| candidate[*k] != row[*k])
            {
                return Err(stale(row["draft_revision"].as_i64().unwrap_or(0)));
            }
            let d:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(agent).fetch_one(&mut *tx).await?;
            self.revalidate_release(&mut tx, actor, candidate, &d)
                .await?;
        }
        let blocking = gate["blocking_findings"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        let nonblocking = gate["non_blocking_findings"]
            .as_array()
            .map(Vec::len)
            .unwrap_or(0);
        let mut validation = super::object(&row["validation_snapshot"]);
        validation["lifecycle"] = status.into();
        validation["resource_authorization_rechecked"] = (status == "passed").into();
        validation["blocking_finding_count"] = blocking.into();
        validation["non_blocking_finding_count"] = nonblocking.into();
        row=sqlx::query_scalar("UPDATE agent_release_evaluations e SET status=$4,validation_snapshot=$5::jsonb,gate_snapshot=$6::jsonb,completed_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid AND evaluation_id=$3::uuid RETURNING to_jsonb(e)").bind(&actor.tenant_id).bind(agent).bind(id).bind(status).bind(validation).bind(gate).fetch_one(&mut *tx).await?;
        eval_event(
            &mut tx,
            &actor.tenant_id,
            id,
            status,
            &json!({"blocking_finding_count":blocking,"non_blocking_finding_count":nonblocking}),
        )
        .await?;
        audit(
            &mut tx,
            actor,
            agent,
            "release_evaluation_completed",
            json!({"evaluation_id":id,"status":status}),
        )
        .await?;
        let result = eval_result(&mut tx, &actor.tenant_id, row).await?;
        tx.commit().await?;
        Ok(result)
    }
}
fn failed_gate(profile: &Value, code: &str, ms: f64) -> Value {
    json!({"schema_version":"agent-release-gate/v1","status":"failed","profile_id":profile["profile_id"],"profile_version":profile["profile_version"],"execution_scope":profile["execution_scope"],"model_quality_evaluated":false,"blocking_findings":[{"code":code,"field":"candidate","message":"Release candidate validation failed"}],"non_blocking_findings":[],"metrics":{"critical_pass_rate":0.0,"configured_critical_pass_rate":1.0,"validation_duration_ms":ms,"provider_cost_cents":0.0,"evaluator_results":[]}})
}
impl AgentRepository {
    pub async fn release_evaluations(
        &self,
        actor: &Actor,
        agent: &str,
        limit: i64,
    ) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, agent, "viewer", false).await?;
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('stale',e.status='passed' AND(d.revision<>e.draft_revision OR d.spec_hash<>e.spec_hash)) FROM agent_release_evaluations e JOIN agent_drafts d ON d.tenant_id=e.tenant_id AND d.agent_id=e.agent_id AND d.draft_id=e.draft_id WHERE e.tenant_id=$1 AND e.agent_id=$2::uuid ORDER BY e.created_at DESC,e.evaluation_id DESC LIMIT $3").bind(&actor.tenant_id).bind(agent).bind(limit.clamp(1,100)).fetch_all(&mut *c).await?;
        let mut result = Vec::new();
        for row in rows {
            result.push(eval_result(&mut c, &actor.tenant_id, row).await?);
        }
        Ok(json!({"evaluations":result}))
    }
    pub async fn release_evaluation(
        &self,
        actor: &Actor,
        agent: &str,
        id: &str,
        role: &str,
    ) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, agent, role, false).await?;
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(e)||jsonb_build_object('stale',e.status='passed' AND(d.revision<>e.draft_revision OR d.spec_hash<>e.spec_hash)) FROM agent_release_evaluations e JOIN agent_drafts d ON d.tenant_id=e.tenant_id AND d.agent_id=e.agent_id AND d.draft_id=e.draft_id WHERE e.tenant_id=$1 AND e.agent_id=$2::uuid AND e.evaluation_id=$3::uuid").bind(&actor.tenant_id).bind(agent).bind(super::uuid(id)?).fetch_optional(&mut *c).await?.ok_or_else(||AgentError::code(404,"AGENT_EVAL_NOT_FOUND"))?;
        eval_result(&mut c, &actor.tenant_id, row).await
    }
    pub async fn transition_evaluation(
        &self,
        actor: &Actor,
        agent: &str,
        id: &str,
        cancel: bool,
    ) -> Result<Value, AgentError> {
        let mut tx = self.pool.begin().await?;
        authorized(&mut tx, actor, agent, "owner", false).await?;
        let mut row:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM agent_release_evaluations e WHERE tenant_id=$1 AND agent_id=$2::uuid AND evaluation_id=$3::uuid FOR UPDATE").bind(&actor.tenant_id).bind(agent).bind(super::uuid(id)?).fetch_optional(&mut *tx).await?.ok_or_else(||AgentError::code(404,"AGENT_EVAL_NOT_FOUND"))?;
        let old = text(&row, "status").to_owned();
        if cancel && matches!(old.as_str(), "passed" | "failed") {
            return Err(error("AGENT_EVAL_TERMINAL"));
        }
        if (cancel && old == "cancelled") || (!cancel && old != "queued") {
            let mut result = eval_result(&mut tx, &actor.tenant_id, row).await?;
            result["execution_claimed"] = false.into();
            return Ok(result);
        }
        let status = if cancel { "cancelled" } else { "running" };
        let mut validation = super::object(&row["validation_snapshot"]);
        let mut gate = super::object(&row["gate_snapshot"]);
        validation["lifecycle"] = status.into();
        gate["status"] = status.into();
        if cancel {
            validation["cancelled_from"] = old.clone().into();
            gate["blocking_findings"] = json!([]);
            gate["non_blocking_findings"] = json!([]);
        }
        row=sqlx::query_scalar("UPDATE agent_release_evaluations e SET status=$4,validation_snapshot=$5::jsonb,gate_snapshot=$6::jsonb,started_at=CASE WHEN $4='running' THEN NOW() ELSE started_at END,completed_at=CASE WHEN $4='cancelled' THEN NOW() ELSE completed_at END WHERE tenant_id=$1 AND agent_id=$2::uuid AND evaluation_id=$3::uuid RETURNING to_jsonb(e)").bind(&actor.tenant_id).bind(agent).bind(id).bind(status).bind(validation).bind(&gate).fetch_one(&mut *tx).await?;
        let summary = if cancel {
            json!({"cancelled_from":old})
        } else {
            json!({"execution_scope":gate["execution_scope"]})
        };
        eval_event(&mut tx, &actor.tenant_id, id, status, &summary).await?;
        if cancel {
            audit(
                &mut tx,
                actor,
                agent,
                "release_evaluation_cancelled",
                json!({"evaluation_id":id,"cancelled_from":old}),
            )
            .await?;
        }
        let mut result = eval_result(&mut tx, &actor.tenant_id, row).await?;
        result["execution_claimed"] = (!cancel).into();
        tx.commit().await?;
        Ok(result)
    }
    pub async fn replay_release(
        &self,
        actor: &Actor,
        agent: &str,
        operation: &str,
        key: &str,
        payload: &Value,
        publication: Option<&str>,
    ) -> Result<Option<Value>, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, agent, "owner", false).await?;
        let request:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(r) FROM agent_release_requests r WHERE tenant_id=$1 AND operation=$2 AND idempotency_key_hash=$3").bind(&actor.tenant_id).bind(operation).bind(key_hash(key)?).fetch_optional(&mut *c).await?;
        let Some(r) = request else { return Ok(None) };
        let event:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM agent_publish_events e WHERE tenant_id=$1 AND event_id=$2::uuid").bind(&actor.tenant_id).bind(text(&r,"result_event_id")).fetch_optional(&mut *c).await?.ok_or_else(||error("AGENT_RELEASE_IDEMPOTENCY_CONFLICT"))?;
        let matches = r["agent_id"] == agent
            && event["operation"] == operation
            && text(&event, "reason") == text(payload, "reason")
            && if operation == "promote" {
                r["evaluation_id"] == payload["evaluation_id"]
                    && event["validation_snapshot"]["experiment_run_id"]
                        == payload["experiment_run_id"]
            } else {
                r["result_publication_id"].as_str() == publication
                    && r["result_version_id"] == payload["target_version_id"]
            };
        if !matches {
            return Err(error("AGENT_RELEASE_IDEMPOTENCY_CONFLICT"));
        }
        Ok(Some(
            release_result(&mut c, &actor.tenant_id, &r, true).await?,
        ))
    }
}
async fn publication_quota(
    conn: &mut PgConnection,
    tenant: &str,
    agent: &str,
) -> Result<(), AgentError> {
    let limit:i32=sqlx::query_scalar("SELECT COALESCE((SELECT max_active_publications FROM agent_governance_policies WHERE tenant_id=$1 AND agent_id=$2::uuid),10)::int").bind(tenant).bind(agent).fetch_one(&mut *conn).await?;
    let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM agent_publications WHERE tenant_id=$1 AND agent_id=$2::uuid AND status='active'").bind(tenant).bind(agent).fetch_one(conn).await?;
    if count >= i64::from(limit) {
        return Err(error("AGENT_ACTIVE_PUBLICATION_QUOTA_EXCEEDED"));
    }
    Ok(())
}
impl AgentState {
    pub async fn publish_agent(
        &self,
        actor: &Actor,
        agent: &str,
        key: &str,
        payload: &Value,
    ) -> Result<Value, AgentError> {
        let reason = text(payload, "reason");
        if reason.chars().count() > 1000
            || crate::migration_compat::eval_observation::redact_trace_text(reason, None) != reason
        {
            return Err(AgentError::code(422, "AGENT_RELEASE_REASON_INVALID"));
        }

        if let Some(replay) = self
            .repository
            .replay_release(actor, agent, "promote", key, payload, None)
            .await?
        {
            return Ok(replay);
        }
        let evaluation_id = super::uuid(text(payload, "evaluation_id"))?;
        let evaluated = self
            .repository
            .release_evaluation(actor, agent, &evaluation_id, "owner")
            .await?;
        if evaluated["stale"] == true {
            return Err(stale(evaluated["draft_revision"].as_i64().unwrap_or(0)));
        }
        let candidate = self
            .resolve_release_candidate(
                actor,
                agent,
                evaluated["draft_revision"].as_i64().unwrap_or(0),
                text(&evaluated, "channel"),
                text(&evaluated, "auth_mode"),
                &evaluated["channel_policy"],
                evaluated["dataset_id"].as_str(),
            )
            .await?;
        let selected = if let Some(run) = payload["experiment_run_id"].as_str() {
            let target:Value=sqlx::query_scalar("SELECT target_snapshot FROM eval_experiment_runs WHERE tenant_id=$1 AND run_id=$2::uuid").bind(&actor.tenant_id).bind(super::uuid(run)?).fetch_optional(&self.repository.pool).await?.ok_or_else(||error("AGENT_EVAL_RUN_UNAVAILABLE"))?;
            if target["candidate_type"] != "agent_version" || target["agent_id"] != agent {
                return Err(error("AGENT_EVAL_RUN_IDENTITY_MISMATCH"));
            }
            let version = super::uuid(text(&target, "agent_version_id"))?;
            let resolution = self
                .repository
                .resolve_version(actor, agent, &version)
                .await?;
            let snapshot = self.build_snapshot(actor, &resolution, "preview").await?;
            Some((version, format!("sha256:{}", spec::hash(&snapshot))))
        } else {
            None
        };
        let identity = json!({"operation":"promote","agent_id":agent,"evaluation_id":evaluation_id,"draft_id":candidate["draft_id"],"draft_revision":candidate["draft_revision"],"spec_hash":candidate["spec_hash"],"runtime_fingerprint_hash":candidate["runtime_fingerprint_hash"],"release_identity_hash":candidate["release_identity_hash"],"channel":candidate["channel"],"channel_policy_hash":candidate["channel_policy_hash"],"reason":text(payload,"reason"),"experiment_run_id":payload["experiment_run_id"],"selected_version_id":selected.as_ref().map(|v|&v.0),"selected_version_snapshot_hash":selected.as_ref().map(|v|&v.1)});
        let request_hash = spec::hash(&identity);
        let hashed = key_hash(key)?;
        let mut tx = self.repository.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "agent-release:{}:promote:{hashed}",
                actor.tenant_id
            ))
            .execute(&mut *tx)
            .await?;
        let a = authorized(&mut tx, actor, agent, "owner", true).await?;
        if matches!(text(&a, "status"), "archived" | "deleted") {
            return Err(error("AGENT_ARCHIVED"));
        }
        if let Some(old)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM agent_release_requests r WHERE tenant_id=$1 AND operation='promote' AND idempotency_key_hash=$2 FOR UPDATE").bind(&actor.tenant_id).bind(&hashed).fetch_optional(&mut *tx).await?{if old["request_hash"]!=request_hash||old["agent_id"]!=agent{return Err(error("AGENT_RELEASE_IDEMPOTENCY_CONFLICT"));}return release_result(&mut tx,&actor.tenant_id,&old,true).await;}
        let evaluation:Value=sqlx::query_scalar("SELECT to_jsonb(e) FROM agent_release_evaluations e WHERE tenant_id=$1 AND agent_id=$2::uuid AND evaluation_id=$3::uuid FOR SHARE").bind(&actor.tenant_id).bind(agent).bind(&evaluation_id).fetch_one(&mut *tx).await?;
        if evaluation["status"] != "passed" {
            return Err(AgentError {
                status: 409,
                detail: json!({"code":"AGENT_EVAL_NOT_PASSED","findings":evaluation["gate_snapshot"]["blocking_findings"]}),
            });
        }
        if [
            "draft_id",
            "draft_revision",
            "spec_hash",
            "runtime_fingerprint_hash",
            "release_identity_hash",
            "evaluation_identity_hash",
            "channel",
            "auth_mode",
            "channel_policy_hash",
            "dataset_id",
            "dataset_version",
            "dataset_manifest_hash",
        ]
        .iter()
        .any(|k| candidate[*k] != evaluation[*k])
        {
            return Err(stale(evaluation["draft_revision"].as_i64().unwrap_or(0)));
        }
        let expiry = contract::public_expiry(
            text(&evaluation, "auth_mode"),
            &evaluation["channel_policy"],
        )?;
        let draft:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(agent).fetch_one(&mut *tx).await?;
        self.revalidate_release(&mut tx, actor, &candidate, &draft)
            .await?;
        let mut run_evidence = json!({});
        let selected_version = if let Some((version, snapshot_hash)) = &selected {
            if !matches!(text(&evaluation, "auth_mode"), "private" | "tenant") {
                return Err(error("AGENT_EVAL_RUN_AUDIENCE_UNSUPPORTED"));
            }
            let run:Value=sqlx::query_scalar("SELECT to_jsonb(r) FROM eval_experiment_runs r WHERE tenant_id=$1 AND run_id=$2::uuid FOR SHARE").bind(&actor.tenant_id).bind(text(payload,"experiment_run_id")).fetch_optional(&mut *tx).await?.ok_or_else(||error("AGENT_EVAL_RUN_UNAVAILABLE"))?;
            if run["status"] != "succeeded" || run["run_mode"] != "live_candidate" {
                return Err(error("AGENT_EVAL_RUN_UNAVAILABLE"));
            }
            if run["dataset_id"] != evaluation["dataset_id"]
                || run["dataset_manifest_hash"] != evaluation["dataset_manifest_hash"]
            {
                return Err(error("AGENT_EVAL_RUN_DATASET_MISMATCH"));
            }
            let expected = json!({"agent_id":agent,"agent_version_id":version,"agent_spec_hash":draft["spec_hash"],"agent_runtime_snapshot_hash":snapshot_hash});
            let metrics = &run["metrics"];
            if run["target_snapshot"]["candidate_type"] != "agent_version"
                || expected.as_object().expect("object").iter().any(|(k, v)| {
                    &run["target_snapshot"][k] != v || &metrics["actual_fingerprint"][k] != v
                })
            {
                return Err(error("AGENT_EVAL_RUN_IDENTITY_MISMATCH"));
            }
            let total = metrics["total_trials"].as_i64().filter(|v| *v > 0);
            let gate = &metrics["gate"];
            if total.is_none()
                || metrics["attempted_trials"].as_i64() != total
                || metrics["completed_trials"].as_i64() != total
                || [
                    "failed_trials",
                    "execution_failed_trials",
                    "judge_failed_trials",
                    "unscored_trials",
                    "unknown_side_effect_trial_count",
                ]
                .iter()
                .any(|k| metrics[*k].as_i64() != Some(0))
                || run["score_summary"]["critical_failed_count"].as_i64() != Some(0)
                || run["score_summary"]["excluded_case_count"].as_i64() != Some(0)
                || gate["profile"] != "agent_version_task_suite"
                || !(gate["status"] == "pass"
                    || (gate["status"] == "warning" && gate["reason"] == "fixed_sample_only"))
            {
                return Err(error("AGENT_EVAL_RUN_HARD_FAILURE"));
            }
            let v:Value=sqlx::query_scalar("SELECT to_jsonb(v)||jsonb_build_object('revoked',EXISTS(SELECT 1 FROM agent_version_revocations r WHERE r.tenant_id=v.tenant_id AND r.agent_version_id=v.agent_version_id)) FROM agent_versions v WHERE tenant_id=$1 AND agent_id=$2::uuid AND agent_version_id=$3::uuid FOR SHARE OF v").bind(&actor.tenant_id).bind(agent).bind(version).fetch_optional(&mut *tx).await?.ok_or_else(||error("AGENT_EVAL_RUN_VERSION_STALE"))?;
            if v["revoked"] == true
                || v["bindings_sealed"] != true
                || v["source_draft_id"] != draft["draft_id"]
                || v["source_draft_revision"] != draft["revision"]
                || v["spec_hash"] != draft["spec_hash"]
            {
                return Err(error("AGENT_EVAL_RUN_VERSION_STALE"));
            }
            self.validate_existing_version_resources(&mut tx, actor, &v)
                .await?;
            run_evidence = json!({"experiment_run_id":payload["experiment_run_id"],"dataset_manifest_hash":run["dataset_manifest_hash"],"evaluator_suite_hash":run["evaluator_suite_hash"],"actual_fingerprint":expected,"quality_gate_status":gate["status"],"quality_evidence_scope":"fixed_sample_only","model_quality_evaluated":true,"general_quality_proven":false});
            Some(v)
        } else {
            None
        };
        let version=if let Some(v)=selected_version{v}else if let Some(v)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(v) FROM agent_versions v WHERE tenant_id=$1 AND agent_id=$2::uuid AND release_identity_hash=$3").bind(&actor.tenant_id).bind(agent).bind(text(&evaluation,"release_identity_hash")).fetch_optional(&mut *tx).await?{v}else{self.repository.insert_version(&mut tx,actor,agent,&draft,Some(&evaluation_id),Some(text(&evaluation,"release_identity_hash"))).await?};
        let old:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p WHERE tenant_id=$1 AND agent_id=$2::uuid AND channel=$3 FOR UPDATE").bind(&actor.tenant_id).bind(agent).bind(text(&evaluation,"channel")).fetch_optional(&mut *tx).await?;
        if old.as_ref().is_none_or(|p| p["status"] != "active") {
            publication_quota(&mut tx, &actor.tenant_id, agent).await?;
        }
        let from = old.as_ref().and_then(|p| p["version_id"].as_str());
        let before: Value = if let Some(from) = from {
            sqlx::query_scalar("SELECT resolved_spec FROM agent_versions WHERE tenant_id=$1 AND agent_id=$2::uuid AND agent_version_id=$3::uuid").bind(&actor.tenant_id).bind(agent).bind(from).fetch_optional(&mut *tx).await?.unwrap_or(json!({}))
        } else {
            json!({})
        };
        let publication: Value = if let Some(old) = &old {
            sqlx::query_scalar("UPDATE agent_publications p SET version_id=$4::uuid,auth_mode=$5,policy=$6::jsonb,expires_at=$8,status='active',updated_by=$7,updated_at=NOW() WHERE tenant_id=$1 AND publication_id=$2::uuid AND agent_id=$3::uuid RETURNING to_jsonb(p)").bind(&actor.tenant_id).bind(text(old,"publication_id")).bind(agent).bind(text(&version,"agent_version_id")).bind(text(&evaluation,"auth_mode")).bind(&evaluation["channel_policy"]).bind(&actor.user_id).bind(expiry).fetch_one(&mut *tx).await?
        } else {
            sqlx::query_scalar("INSERT INTO agent_publications AS p(tenant_id,agent_id,channel,version_id,auth_mode,policy,status,created_by,updated_by,expires_at) VALUES($1,$2::uuid,$3,$4::uuid,$5,$6::jsonb,'active',$7,$7,$8) RETURNING to_jsonb(p)").bind(&actor.tenant_id).bind(agent).bind(text(&evaluation,"channel")).bind(text(&version,"agent_version_id")).bind(text(&evaluation,"auth_mode")).bind(&evaluation["channel_policy"]).bind(&actor.user_id).bind(expiry).fetch_one(&mut *tx).await?
        };
        let mut validation = json!({"schema_version":"agent-publish-validation/v1","evaluation_id":evaluation_id,"profile_id":evaluation["profile_id"],"profile_version":evaluation["profile_version"],"draft_revision":evaluation["draft_revision"],"spec_hash":evaluation["spec_hash"],"runtime_fingerprint_hash":evaluation["runtime_fingerprint_hash"],"release_identity_hash":evaluation["release_identity_hash"],"resource_authorization_rechecked":true,"session_pinning":"existing_sessions_keep_version_new_sessions_use_pointer","diff":contract::structured_diff(&before,&draft["spec"])});
        validation
            .as_object_mut()
            .expect("object")
            .extend(run_evidence.as_object().expect("object").clone());
        let event:Value=sqlx::query_scalar("INSERT INTO agent_publish_events AS e(tenant_id,publication_id,agent_id,from_version_id,to_version_id,actor_id,reason,validation_snapshot,operation,release_evaluation_id,request_hash) VALUES($1,$2::uuid,$3::uuid,$4::uuid,$5::uuid,$6,$7,$8::jsonb,'promote',$9::uuid,$10) RETURNING to_jsonb(e)").bind(&actor.tenant_id).bind(text(&publication,"publication_id")).bind(agent).bind(from).bind(text(&version,"agent_version_id")).bind(&actor.user_id).bind(text(payload,"reason")).bind(validation).bind(&evaluation_id).bind(&request_hash).fetch_one(&mut *tx).await?;
        let request:Value=sqlx::query_scalar("INSERT INTO agent_release_requests AS r(tenant_id,operation,idempotency_key_hash,request_hash,agent_id,evaluation_id,result_version_id,result_publication_id,result_event_id,created_by) VALUES($1,'promote',$2,$3,$4::uuid,$5::uuid,$6::uuid,$7::uuid,$8::uuid,$9) RETURNING to_jsonb(r)").bind(&actor.tenant_id).bind(hashed).bind(request_hash).bind(agent).bind(&evaluation_id).bind(text(&version,"agent_version_id")).bind(text(&publication,"publication_id")).bind(text(&event,"event_id")).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        audit(&mut tx,actor,agent,"publication_promote",json!({"evaluation_id":evaluation_id,"publication_id":publication["publication_id"],"from_version_id":from,"to_version_id":version["agent_version_id"]})).await?;
        let result = release_result(&mut tx, &actor.tenant_id, &request, false).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn rollback_publication(
        &self,
        actor: &Actor,
        publication_id: &str,
        key: &str,
        payload: &Value,
    ) -> Result<Value, AgentError> {
        let reason = text(payload, "reason");
        if reason.chars().count() > 1000
            || crate::migration_compat::eval_observation::redact_trace_text(reason, None) != reason
        {
            return Err(AgentError::code(422, "AGENT_RELEASE_REASON_INVALID"));
        }

        let p:Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p WHERE tenant_id=$1 AND publication_id=$2::uuid").bind(&actor.tenant_id).bind(super::uuid(publication_id)?).fetch_optional(&self.repository.pool).await?.ok_or_else(||AgentError::code(404,"AGENT_PUBLICATION_NOT_FOUND"))?;
        let agent = text(&p, "agent_id");
        if let Some(replay) = self
            .repository
            .replay_release(actor, agent, "rollback", key, payload, Some(publication_id))
            .await?
        {
            return Ok(replay);
        }
        let version = super::uuid(text(payload, "target_version_id"))?;
        let mut resolution = self
            .repository
            .resolve_version(actor, agent, &version)
            .await?;
        resolution["publication"] = p.clone();
        let caller = audience(actor, agent, text(&p, "channel"), text(&p, "auth_mode"));
        let snapshot = self
            .build_snapshot(&caller, &resolution, text(&p, "channel"))
            .await?;
        let model = self
            .control
            .models
            .model(
                &actor.tenant_id,
                text(&snapshot["model"], "id"),
                snapshot["model"]["provider"].as_str(),
            )
            .await?;
        let provider = self
            .control
            .providers
            .provider(&actor.tenant_id, text(&snapshot["model"], "provider"))
            .await?;
        self.control
            .providers
            .runtime_config(&actor.tenant_id, text(&snapshot["model"], "provider"))
            .await?;
        let evidence = contract::model_evidence("database", &model, &provider, true);
        let snapshot_hash = spec::hash(&snapshot);
        let spec_hash = text(&resolution["version"], "spec_hash");
        let identity = json!({"operation":"rollback","publication_id":publication_id,"target_version_id":version,"runtime_snapshot_hash":snapshot_hash,"runtime_spec_hash":spec_hash,"model_authorization_hash":spec::hash(&evidence),"reason":text(payload,"reason")});
        let request_hash = spec::hash(&identity);
        let hashed = key_hash(key)?;
        let mut tx = self.repository.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!(
                "agent-release:{}:rollback:{hashed}",
                actor.tenant_id
            ))
            .execute(&mut *tx)
            .await?;
        let a = authorized(&mut tx, actor, agent, "owner", true).await?;
        if matches!(text(&a, "status"), "archived" | "deleted") {
            return Err(error("AGENT_ARCHIVED"));
        }
        if let Some(r)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM agent_release_requests r WHERE tenant_id=$1 AND operation='rollback' AND idempotency_key_hash=$2 FOR UPDATE").bind(&actor.tenant_id).bind(&hashed).fetch_optional(&mut *tx).await?{if r["request_hash"]!=request_hash||r["agent_id"]!=agent{return Err(error("AGENT_RELEASE_IDEMPOTENCY_CONFLICT"));}return release_result(&mut tx,&actor.tenant_id,&r,true).await;}
        let mut p:Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p WHERE tenant_id=$1 AND publication_id=$2::uuid AND agent_id=$3::uuid FOR UPDATE").bind(&actor.tenant_id).bind(publication_id).bind(agent).fetch_one(&mut *tx).await?;
        if p["expires_at"].as_str().is_some_and(|t| {
            chrono::DateTime::parse_from_rfc3339(t)
                .map(|t| t.with_timezone(&chrono::Utc) <= chrono::Utc::now())
                .unwrap_or(true)
        }) {
            return Err(error("AGENT_PUBLIC_EXPIRY_EXPIRED"));
        }
        if p["status"] != "active" {
            publication_quota(&mut tx, &actor.tenant_id, agent).await?;
        }
        let from = text(&p, "version_id").to_owned();
        if from.is_empty() {
            return Err(error("AGENT_ROLLBACK_PUBLICATION_EMPTY"));
        }
        if from == version {
            return Err(error("AGENT_ROLLBACK_TARGET_CURRENT"));
        }
        let historical:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM agent_publish_events WHERE tenant_id=$1 AND publication_id=$2::uuid AND agent_id=$3::uuid AND(from_version_id=$4::uuid OR to_version_id=$4::uuid))").bind(&actor.tenant_id).bind(publication_id).bind(agent).bind(&version).fetch_one(&mut *tx).await?;
        if !historical {
            return Err(error("AGENT_ROLLBACK_TARGET_NOT_HISTORICAL"));
        }
        let target = self
            .repository
            .version_material(&mut tx, &actor.tenant_id, &a, &version, "owner", p.clone())
            .await?;
        if target["version"]["spec_hash"] != spec_hash {
            return Err(error("AGENT_ROLLBACK_FINGERPRINT_STALE"));
        }
        self.validate_existing_version_resources(&mut tx, actor, &target["version"])
            .await?;
        let current:Value=sqlx::query_scalar("SELECT jsonb_build_object('model',to_jsonb(m),'provider',to_jsonb(p)) FROM llm_models m JOIN llm_providers p ON p.tenant_id=m.tenant_id AND p.provider_id=m.provider_id WHERE m.tenant_id=$1 AND m.provider_id=$2 AND m.model_id=$3 FOR SHARE OF m,p").bind(&actor.tenant_id).bind(text(&snapshot["model"],"provider")).bind(text(&snapshot["model"],"id")).fetch_optional(&mut *tx).await?.ok_or_else(||error("AGENT_RUNTIME_MODEL_UNAVAILABLE"))?;
        if current["model"]["is_enabled"] != true
            || current["provider"]["is_enabled"] != true
            || !super::entry::user_can_access_model(
                &caller,
                current["model"]["access_level"]
                    .as_str()
                    .unwrap_or("public"),
            )
        {
            return Err(error("AGENT_RUNTIME_MODEL_FORBIDDEN"));
        }
        if contract::model_evidence("database", &current["model"], &current["provider"], true)
            != evidence
        {
            return Err(error("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE"));
        }
        self.control
            .providers
            .runtime_config(&actor.tenant_id, text(&snapshot["model"], "provider"))
            .await?;
        let before:Value=sqlx::query_scalar("SELECT resolved_spec FROM agent_versions WHERE tenant_id=$1 AND agent_id=$2::uuid AND agent_version_id=$3::uuid").bind(&actor.tenant_id).bind(agent).bind(&from).fetch_optional(&mut *tx).await?.ok_or_else(||error("AGENT_ROLLBACK_CURRENT_VERSION_UNAVAILABLE"))?;
        p=sqlx::query_scalar("UPDATE agent_publications p SET version_id=$4::uuid,status='active',updated_by=$5,updated_at=NOW() WHERE tenant_id=$1 AND publication_id=$2::uuid AND agent_id=$3::uuid RETURNING to_jsonb(p)").bind(&actor.tenant_id).bind(publication_id).bind(agent).bind(&version).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        let validation = json!({"schema_version":"agent-rollback-validation/v1","runtime_snapshot_hash":snapshot_hash,"spec_hash":spec_hash,"resource_authorization_rechecked":true,"target_version_revoked":false,"prior_version_recoverable":true,"session_pinning":"existing_sessions_keep_version_new_sessions_use_pointer","diff":contract::structured_diff(&before,&target["spec"])});
        let event:Value=sqlx::query_scalar("INSERT INTO agent_publish_events AS e(tenant_id,publication_id,agent_id,from_version_id,to_version_id,actor_id,reason,validation_snapshot,operation,request_hash) VALUES($1,$2::uuid,$3::uuid,$4::uuid,$5::uuid,$6,$7,$8::jsonb,'rollback',$9) RETURNING to_jsonb(e)").bind(&actor.tenant_id).bind(publication_id).bind(agent).bind(&from).bind(&version).bind(&actor.user_id).bind(text(payload,"reason")).bind(validation).bind(&request_hash).fetch_one(&mut *tx).await?;
        let request:Value=sqlx::query_scalar("INSERT INTO agent_release_requests AS r(tenant_id,operation,idempotency_key_hash,request_hash,agent_id,result_version_id,result_publication_id,result_event_id,created_by) VALUES($1,'rollback',$2,$3,$4::uuid,$5::uuid,$6::uuid,$7::uuid,$8) RETURNING to_jsonb(r)").bind(&actor.tenant_id).bind(hashed).bind(request_hash).bind(agent).bind(&version).bind(publication_id).bind(text(&event,"event_id")).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        audit(&mut tx,actor,agent,"publication_rollback",json!({"publication_id":publication_id,"from_version_id":from,"to_version_id":version,"channel":p["channel"]})).await?;
        let result = release_result(&mut tx, &actor.tenant_id, &request, false).await?;
        tx.commit().await?;
        Ok(result)
    }
}
impl AgentRepository {
    pub async fn release_diff(
        &self,
        actor: &Actor,
        agent: &str,
        id: &str,
    ) -> Result<Value, AgentError> {
        let e = self.release_evaluation(actor, agent, id, "viewer").await?;
        let d = self.draft(actor, agent).await?;
        if d["revision"] != e["draft_revision"] || d["spec_hash"] != e["spec_hash"] {
            return Err(stale(d["revision"].as_i64().unwrap_or(0)));
        }
        let p:Value=sqlx::query_scalar("SELECT to_jsonb(p)||jsonb_build_object('current_resolved_spec',v.resolved_spec,'current_version_number',v.version_number) FROM agent_publications p LEFT JOIN agent_versions v ON v.tenant_id=p.tenant_id AND v.agent_id=p.agent_id AND v.agent_version_id=p.version_id WHERE p.tenant_id=$1 AND p.agent_id=$2::uuid AND p.channel=$3").bind(&actor.tenant_id).bind(agent).bind(text(&e,"channel")).fetch_optional(&self.pool).await?.unwrap_or(Value::Null);
        Ok(
            json!({"evaluation_id":e["evaluation_id"],"draft_revision":e["draft_revision"],"publication_id":p["publication_id"],"current_version_id":p["version_id"],"current_version_number":p["current_version_number"],"diff":contract::structured_diff(&p["current_resolved_spec"],&d["spec"])}),
        )
    }
}
