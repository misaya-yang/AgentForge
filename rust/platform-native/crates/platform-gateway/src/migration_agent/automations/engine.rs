use super::*;
use chrono::{DateTime, Utc};

async fn current(state: &AgentState, row: &Value) -> Result<Actor, AgentError> {
    let user = crate::migration_knowledge_query::actor::CurrentActorStore {
        pool: state.repository.pool.clone(),
    }
    .resolve(text(row, "tenant_id"), text(row, "user_id"))
    .await
    .map_err(|error| match error {
        crate::migration_knowledge_query::types::QueryError::Forbidden => {
            AgentError::code(403, "AUTOMATION_CREATOR_REVOKED")
        }
        _ => AgentError::code(503, "AUTOMATION_ACTOR_UNAVAILABLE"),
    })?;
    Ok(Actor {
        tenant_id: user.tenant_id,
        user_id: user.user_id,
        roles: user.roles,
        tier: user.user_tier,
    })
}
/// Called by the existing scheduled-task dispatcher. No new process or daemon.
pub async fn tick(state: &AgentState) -> Result<(), AgentError> {
    let pool = &state.repository.pool;
    let mut guard = pool.begin().await?;
    let leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(742011,2)")
        .fetch_one(&mut *guard)
        .await?;
    if !leader {
        return Ok(());
    }
    // A prior dispatcher crash leaves a committed fence. There is no retry of
    // dispatching operations, which could have crossed the HTTP/Runtime boundary.
    sqlx::query("UPDATE gateway.automation_deliveries SET status='uncertain',result=jsonb_build_object('code','AUTOMATION_DISPATCH_INTERRUPTED'),updated_at=now() WHERE status='dispatching'").execute(pool).await?;
    reconcile(state).await?;
    let rules:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(r)-'webhook_secret_ciphertext' FROM gateway.automation_rules r WHERE status='active' AND definition->'trigger'->>'kind'<>'webhook' ORDER BY updated_at,automation_id LIMIT 20").fetch_all(pool).await?;
    for row in rules {
        match tokio::time::timeout(std::time::Duration::from_secs(5), poll(state, &row)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) if error.status == 403 || error.status == 404 => {
                sqlx::query("UPDATE gateway.automation_rules SET status='paused',revision=revision+1,updated_at=now() WHERE automation_id=$1::uuid AND status='active'").bind(text(&row,"automation_id")).execute(pool).await?;
            }
            _ => {} // A transient observation failure does not invent an event.
        }
        // Round-robin observation also advances rules with no event or a transient error.
        sqlx::query("UPDATE gateway.automation_rules SET updated_at=now() WHERE automation_id=$1::uuid AND revision=$2 AND status='active'").bind(text(&row,"automation_id")).bind(row["revision"].as_i64().unwrap_or(0)).execute(pool).await?;
    }
    for _ in 0..4 {
        let mut tx = pool.begin().await?;
        let job:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('delivery',to_jsonb(d),'rule',to_jsonb(r)-'webhook_secret_ciphertext') FROM gateway.automation_deliveries d JOIN gateway.automation_rules r USING(automation_id) WHERE d.status='queued' AND r.status='active' AND NOT EXISTS(SELECT 1 FROM gateway.automation_deliveries p WHERE p.automation_id=d.automation_id AND p.event_key=d.event_key AND p.action_index<d.action_index AND p.status<>'succeeded') ORDER BY d.created_at,d.action_index FOR UPDATE OF d,r SKIP LOCKED LIMIT 1").fetch_optional(&mut *tx).await?;
        let Some(job) = job else {
            break;
        };
        let delivery = &job["delivery"];
        sqlx::query("UPDATE gateway.automation_deliveries SET status='dispatching',updated_at=now() WHERE delivery_id=$1::uuid AND status='queued'").bind(text(delivery,"delivery_id")).execute(&mut *tx).await?;
        tx.commit().await?;
        let outcome =
            tokio::time::timeout(std::time::Duration::from_secs(30), effect(state, &job)).await;
        let (status, result) = match outcome {
            Ok(Ok((status, result))) => (status, result),
            // The committed marker deliberately treats transport errors as unknown.
            Ok(Err(error)) if error.status < 500 => {
                ("failed", json!({"code":text(&error.detail,"code")}))
            }
            _ => (
                "uncertain",
                json!({"code":"AUTOMATION_EFFECT_OUTCOME_UNKNOWN"}),
            ),
        };
        sqlx::query("UPDATE gateway.automation_deliveries SET status=$2,result=$3,updated_at=now() WHERE delivery_id=$1::uuid AND status='dispatching'").bind(text(delivery,"delivery_id")).bind(status).bind(result).execute(pool).await?;
    }
    guard.commit().await?;
    Ok(())
}
async fn poll(state: &AgentState, row: &Value) -> Result<(), AgentError> {
    let d = definition(row)?;
    let pool = &state.repository.pool;
    let watermark = text(row, "watermark");
    let (event, next, mark) = match d.trigger {
        Trigger::Webhook => return Ok(()),
        Trigger::Cron {
            expression,
            timezone,
        } => {
            let at = row["next_run_at"]
                .as_str()
                .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
                .map(|v| v.with_timezone(&Utc))
                .ok_or_else(invalid)?;
            if at > Utc::now() {
                return Ok(());
            }
            let next = cron::Cron::parse(&expression)?
                .next(pool, &timezone, Utc::now())
                .await?;
            (
                format!("cron:{}", at.timestamp()),
                Some(next),
                watermark.to_owned(),
            )
        }
        Trigger::SpacePageChanged { item_id } => {
            let actor = current(state, row).await?;
            let content = state
                .automation_knowledge
                .as_ref()
                .ok_or_else(|| AgentError::code(503, "AUTOMATION_KNOWLEDGE_UNAVAILABLE"))?
                .read(&actor, &item_id)
                .await?;
            let revision = content["revision"]
                .as_i64()
                .ok_or_else(invalid)?
                .to_string();
            if revision == watermark {
                return Ok(());
            }
            (format!("space:{item_id}:{revision}"), None, revision)
        }
        Trigger::ConnectorEvent { entry_id } => {
            let _ = current(state, row).await?;
            let cursor: Value = serde_json::from_str(watermark).map_err(|_| invalid())?;
            let observed:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('id',connection_id,'at',updated_at) FROM gateway.catalog_connections WHERE tenant_id=$1 AND entry_id=$2 AND (owner_kind<>'user' OR owner_id=$3) AND (updated_at,connection_id)>($4::timestamptz,$5::text) ORDER BY updated_at,connection_id LIMIT 1").bind(text(row,"tenant_id")).bind(entry_id).bind(text(row,"user_id")).bind(text(&cursor,"at")).bind(text(&cursor,"id")).fetch_optional(pool).await?;
            let Some(observed) = observed else {
                return Ok(());
            };
            let at = text(&observed, "at").to_owned();
            (
                format!("connector:{}:{at}", text(&observed, "id")),
                None,
                observed.to_string(),
            )
        }
    };
    let mut tx = pool.begin().await?;
    let current:Option<Value>=sqlx::query_scalar("SELECT to_jsonb(r)-'webhook_secret_ciphertext' FROM gateway.automation_rules r WHERE automation_id=$1::uuid AND status='active' AND revision=$2 AND watermark IS NOT DISTINCT FROM $3::text FOR UPDATE").bind(text(row,"automation_id")).bind(row["revision"].as_i64().unwrap_or(0)).bind(row["watermark"].as_str()).fetch_optional(&mut *tx).await?;
    if let Some(current) = current {
        enqueue(&mut tx, &current, &event).await?;
        sqlx::query("UPDATE gateway.automation_rules SET next_run_at=$2,watermark=$3,updated_at=now() WHERE automation_id=$1::uuid").bind(text(row,"automation_id")).bind(next).bind(mark).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}
async fn effect(state: &AgentState, job: &Value) -> Result<(&'static str, Value), AgentError> {
    let row = &job["rule"];
    let delivery = &job["delivery"];
    let actor = current(state, row).await?;
    let action: Action =
        serde_json::from_value(delivery["action"].clone()).map_err(|_| invalid())?;
    match action {
        Action::Notify { message } => {
            sqlx::query("INSERT INTO gateway.automation_notifications(notification_id,tenant_id,user_id,message) VALUES($1::uuid,$2,$3,$4) ON CONFLICT DO NOTHING").bind(text(delivery,"delivery_id")).bind(&actor.tenant_id).bind(&actor.user_id).bind(message).execute(&state.repository.pool).await?;
            Ok((
                "succeeded",
                json!({"notification_id":delivery["delivery_id"]}),
            ))
        }
        Action::WriteSpacePage { item_id, markdown } => {
            let result = state
                .automation_knowledge
                .as_ref()
                .ok_or_else(|| AgentError::code(503, "AUTOMATION_KNOWLEDGE_UNAVAILABLE"))?
                .append(&actor, &item_id, &markdown)
                .await?;
            Ok((
                "succeeded",
                json!({"item_id":item_id,"revision":result["revision"]}),
            ))
        }
        action => {
            if !state.runtime_enabled {
                return Err(AgentError::code(503, "AGENT_RUNTIME_DISABLED"));
            }
            state.edge_rate.check(&actor, "assistant_chat").await?;
            let (prompt, instructions, skill_version) = match action {
                Action::RunAgent { prompt } => (prompt, None, Value::Null),
                Action::RunSkill { skill_id, prompt } => {
                    let repository = state
                        .automation_skills
                        .as_ref()
                        .ok_or_else(|| AgentError::code(503, "AUTOMATION_SKILLS_UNAVAILABLE"))?;
                    let available = repository
                        .list_for_actor(&actor.tenant_id, &actor.user_id, true)
                        .await
                        .map_err(|_| AgentError::code(503, "AUTOMATION_SKILLS_UNAVAILABLE"))?;
                    let selected = available
                        .iter()
                        .find(|skill| {
                            text(skill, "skill_id") == skill_id || text(skill, "name") == skill_id
                        })
                        .ok_or_else(|| AgentError::code(403, "AUTOMATION_SKILL_UNAVAILABLE"))?;
                    let skill = repository
                        .authorize_version(
                            &actor.tenant_id,
                            &actor.user_id,
                            text(selected, "version_id"),
                            false,
                        )
                        .await
                        .map_err(|_| AgentError::code(403, "AUTOMATION_SKILL_UNAVAILABLE"))?;
                    let activated = crate::migration_content::skills::execute_instructions(&skill);
                    if activated["success"] != true {
                        return Err(AgentError::code(403, "AUTOMATION_SKILL_UNAVAILABLE"));
                    }
                    (
                        prompt,
                        Some(text(&activated, "result").to_owned()),
                        json!({"version_id":skill["version_id"],"content_hash":skill["content_hash"]}),
                    )
                }
                _ => unreachable!(),
            };
            let session = text(delivery, "delivery_id").to_owned();
            routes::ensure_builtin_session(state, &actor, &session).await?;
            let turn = routes::start_builtin(
                state,
                actor,
                session,
                json!({"message":prompt,"model_id":definition(row)?.model_id,"memory_mode":"off","system_prompt":instructions}),
            )
            .await?;
            Ok((
                "running",
                json!({"run_id":turn.run_id,"session_id":delivery["delivery_id"],"skill":skill_version}),
            ))
        }
    }
}
async fn reconcile(state: &AgentState) -> Result<(), AgentError> {
    sqlx::query("UPDATE gateway.automation_deliveries d SET status=CASE WHEN r.status='succeeded' THEN 'succeeded' ELSE 'failed' END,updated_at=now() FROM assistant.assistant_runs r WHERE d.status='running' AND d.result->>'run_id'=r.run_id::text AND d.tenant_id=r.tenant_id AND d.user_id=r.user_id AND r.status IN ('succeeded','failed','cancelled')").execute(&state.repository.pool).await?;
    Ok(())
}
