//! Gateway scheduling policy. Agent Runtime remains the only execution owner.
//! A committed dispatch marker is never replayed automatically after uncertainty.
use super::{Actor, AgentError, routes::{self, AgentState}, text};
use axum::{Json, Router, body::to_bytes, extract::{Query, Request, State}, response::{IntoResponse, Response}, routing::any};
use chrono::{DateTime, Utc};
use ai_platform_gateway_foundation::native_contract::{CreateTask, TASK_BASE, TITLE_MAX, MESSAGE_MAX, MODEL_MAX_BYTES, INTERVAL_MIN, INTERVAL_MAX, RETRIES_MAX, SchedulePageQuery};
use serde_json::{Value, json};
use ai_platform_gateway_foundation::native_contract::responses;
use sqlx::PgPool;
use std::time::Duration;
use tokio::sync::watch;

fn invalid() -> AgentError { AgentError::code(422, "SCHEDULE_INVALID") }
fn validate(v: &CreateTask) -> Result<(), AgentError> {
    if v.title.trim().is_empty() || v.title.chars().count() > TITLE_MAX
        || v.message.trim().is_empty() || v.message.chars().count() > MESSAGE_MAX
        || v.model_id.trim().is_empty() || v.model_id.len() > MODEL_MAX_BYTES
        || v.run_at <= Utc::now()
        || v.interval_seconds.is_some_and(|n| !(INTERVAL_MIN..=INTERVAL_MAX).contains(&n))
        || !(0..=RETRIES_MAX).contains(&v.max_retries) { return Err(invalid()); }
    Ok(())
}
pub fn router(state: AgentState) -> Router {
    Router::new()
        .route(TASK_BASE, any(handle))
        .route("/api/v1/scheduled-tasks/{*rest}", any(handle))
        .with_state(state)
}
async fn handle(State(state): State<AgentState>, request: Request) -> Response {
    let mut response = match dispatch(&state, request).await {
        Ok(v) => Json(v).into_response(), Err(e) => e.into_response(),
    };
    response.headers_mut().insert(axum::http::header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
async fn dispatch(state: &AgentState, request: Request) -> Result<Value, AgentError> {
    let path = request.uri().path().to_owned();
    let query_uri = request.uri().clone();
    let method = request.method().as_str().to_owned();
    super::public::reject_forgery(request.headers(), None)?;
    let actor = Actor::resolve(&state.auth, request.headers(), &path).await?;
    let pool = &state.repository.pool;
    let tail = path.trim_start_matches(TASK_BASE).trim_matches('/');
    let parts: Vec<_> = if tail.is_empty() { vec![] } else { tail.split('/').collect() };
    match (method.as_str(), parts.as_slice()) {
        ("GET", []) => {
            let page = schedule_page_query(&query_uri, 200)?;
            let limit = page.limit.unwrap_or(200);
            let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM gateway.scheduled_tasks t WHERE tenant_id=$1 AND user_id=$2 AND ($3::timestamptz IS NULL OR (created_at,task_id)<($3::timestamptz,$4::uuid)) ORDER BY created_at DESC,task_id DESC LIMIT $5")
                .bind(&actor.tenant_id).bind(&actor.user_id).bind(page.before_at).bind(page.before_id).bind((limit + 1) as i64).fetch_all(pool).await?;
            Ok(responses::schedule_page(rows, limit, "tasks", "task_id"))
        }
        ("POST", []) => {
            if !state.runtime_enabled { return Err(AgentError::code(503, "AGENT_RUNTIME_DISABLED")); }
            state.edge_rate.check(&actor, "assistant_chat").await?;
            let bytes = to_bytes(request.into_body(), 1024 * 1024).await.map_err(|_| invalid())?;
            let v: CreateTask = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            validate(&v)?;
            let id = uuid::Uuid::new_v4().to_string();
            let row: Value = sqlx::query_scalar("INSERT INTO gateway.scheduled_tasks(task_id,tenant_id,user_id,title,message,model_id,next_run_at,interval_seconds,max_retries) VALUES($1::uuid,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING to_jsonb(scheduled_tasks)")
                .bind(id).bind(&actor.tenant_id).bind(&actor.user_id).bind(v.title.trim()).bind(v.message).bind(v.model_id).bind(v.run_at).bind(v.interval_seconds).bind(v.max_retries).fetch_one(pool).await?;
            Ok(responses::task::value(row))
        }
        ("GET", [id, "executions"]) => {
            let id = super::uuid(id)?;
            owned(pool, &actor, &id).await?;
            let page = schedule_page_query(&query_uri, 100)?;
            let limit = page.limit.unwrap_or(100);
            let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(e) FROM gateway.scheduled_task_executions e WHERE task_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND ($4::timestamptz IS NULL OR (created_at,execution_id)<($4::timestamptz,$5::uuid)) ORDER BY created_at DESC,execution_id DESC LIMIT $6")
                .bind(id).bind(&actor.tenant_id).bind(&actor.user_id).bind(page.before_at).bind(page.before_id).bind((limit + 1) as i64).fetch_all(pool).await?;
            Ok(responses::schedule_page(rows, limit, "executions", "execution_id"))
        }
        ("GET", [id]) => {
            let id = super::uuid(id)?;
            let row: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM gateway.scheduled_tasks t WHERE task_id=$1::uuid AND tenant_id=$2 AND user_id=$3")
                .bind(id).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(pool).await?;
            Ok(responses::task::value(row.ok_or_else(|| AgentError::code(404, "SCHEDULE_NOT_FOUND"))?))
        }
        ("GET", [id, "executions", execution_id]) => {
            let id = super::uuid(id)?;
            let execution_id = super::uuid(execution_id)?;
            owned(pool, &actor, &id).await?;
            let row: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(e) FROM gateway.scheduled_task_executions e WHERE task_id=$1::uuid AND execution_id=$2::uuid AND tenant_id=$3 AND user_id=$4")
                .bind(id).bind(execution_id).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(pool).await?;
            Ok(responses::execution::value(row.ok_or_else(|| AgentError::code(404, "SCHEDULE_NOT_FOUND"))?))
        }
        ("POST", [id, action]) if matches!(*action, "pause" | "resume" | "cancel") => {
            let id = super::uuid(id)?;
            let mut tx = pool.begin().await?;
            let current: Option<String> = sqlx::query_scalar("SELECT status FROM gateway.scheduled_tasks WHERE task_id=$1::uuid AND tenant_id=$2 AND user_id=$3 FOR UPDATE")
                .bind(&id).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(&mut *tx).await?;
            let current = current.ok_or_else(|| AgentError::code(404, "SCHEDULE_NOT_FOUND"))?;
            if matches!(current.as_str(), "completed" | "cancelled") { return Err(AgentError::code(409, "SCHEDULE_TERMINAL")); }
            let status = match *action { "resume" => "active", "cancel" => "cancelled", _ => "paused" };
            sqlx::query("UPDATE gateway.scheduled_tasks SET status=$2,retry_count=CASE WHEN $2='active' THEN 0 ELSE retry_count END,updated_at=NOW() WHERE task_id=$1::uuid")
                .bind(&id).bind(status).execute(&mut *tx).await?;
            if *action == "cancel" {
                sqlx::query("UPDATE gateway.scheduled_task_executions SET cancel_requested=true,updated_at=NOW() WHERE task_id=$1::uuid AND status IN ('preparing','dispatching','running','uncertain')")
                    .bind(&id).execute(&mut *tx).await?;
            }
            tx.commit().await?;
            Ok(responses::task_action::value(id, status, *action=="cancel"))
        }
        _ => Err(AgentError::code(404, "NOT_FOUND")),
    }
}
fn schedule_page_query(uri: &axum::http::Uri, maximum: usize) -> Result<SchedulePageQuery, AgentError> {
    let Query(query) = Query::<SchedulePageQuery>::try_from_uri(uri).map_err(|_| invalid())?;
    let mut query = query.validate(maximum).map_err(|_| invalid())?;
    query.before_id = query.before_id.as_deref().map(super::uuid).transpose()?;
    Ok(query)
}
async fn owned(pool: &PgPool, actor: &Actor, id: &str) -> Result<(), AgentError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.scheduled_tasks WHERE task_id=$1::uuid AND tenant_id=$2 AND user_id=$3)")
        .bind(id).bind(&actor.tenant_id).bind(&actor.user_id).fetch_one(pool).await?;
    if exists { Ok(()) } else { Err(AgentError::code(404, "SCHEDULE_NOT_FOUND")) }
}
/// Construction is inert; the host owns this loop and awaits shutdown before DB close.
pub async fn run(state: AgentState, mut stop: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            _ = interval.tick() => {
                if *stop.borrow() { break; }
                if let Err(error) = tick(&state).await {
                    eprintln!("scheduled_task_tick_failed code={error}");
                }
            }
        }
    }
}
async fn tick(state: &AgentState) -> Result<(), AgentError> {
    if !state.runtime_enabled { return Ok(()); }
    // Advisory transaction lock is held on this connection. Row locks for claim
    // and cancel protect rows; the global guard also bounds dispatch across hosts.
    let mut guard = state.repository.pool.begin().await?;
    let leader: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(742011, 1)").fetch_one(&mut *guard).await?;
    if !leader { return Ok(()); }
    reconcile(state).await?;
    for _ in 0..4 {
        let Some(job) = claim(&state.repository.pool).await? else { break; };
        launch(state, job).await?;
    }
    guard.commit().await?;
    Ok(())
}
async fn claim(pool: &PgPool) -> Result<Option<Value>, AgentError> {
    let mut tx = pool.begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM gateway.scheduled_task_executions WHERE status IN ('preparing','dispatching','running','uncertain')")
        .fetch_one(&mut *tx).await?;
    if count >= 4 { return Ok(None); }
    let task: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM gateway.scheduled_tasks t WHERE status='active' AND next_run_at<=NOW() AND NOT EXISTS(SELECT 1 FROM gateway.scheduled_task_executions e WHERE e.task_id=t.task_id AND e.status IN ('preparing','dispatching','running','uncertain')) ORDER BY next_run_at FOR UPDATE SKIP LOCKED LIMIT 1")
        .fetch_optional(&mut *tx).await?;
    let Some(task) = task else { return Ok(None); };
    let execution_id = uuid::Uuid::new_v4().to_string();
    let session_id = uuid::Uuid::new_v4().to_string();
    let row: Value = sqlx::query_scalar("INSERT INTO gateway.scheduled_task_executions(execution_id,task_id,tenant_id,user_id,session_id,scheduled_at,attempt,status) VALUES($1::uuid,$2::uuid,$3,$4,$5,$6::timestamptz,$7,'preparing') RETURNING to_jsonb(scheduled_task_executions)")
        .bind(execution_id).bind(text(&task,"task_id")).bind(text(&task,"tenant_id")).bind(text(&task,"user_id")).bind(session_id).bind(text(&task,"next_run_at")).bind(task["retry_count"].as_i64().unwrap_or(0) as i32).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(json!({"execution":row,"task":task})))
}
async fn launch(state: &AgentState, job: Value) -> Result<(), AgentError> {
    let e = &job["execution"];
    let task = &job["task"];
    let pool = &state.repository.pool;
    let id = text(e,"execution_id");
    // Only pre-dispatch failures can be retried. Never replay tool effects.
    let prepared = async {
        let current = crate::migration_knowledge_query::actor::CurrentActorStore { pool: pool.clone() }
            .resolve(text(e,"tenant_id"), text(e,"user_id")).await
            .map_err(|error| match error {
                crate::migration_knowledge_query::types::QueryError::Forbidden => AgentError::code(403,"SCHEDULE_CREATOR_REVOKED"),
                _ => AgentError::code(503,"SCHEDULE_AUTHORITY_UNAVAILABLE"),
            })?;
        let actor = Actor { tenant_id: current.tenant_id, user_id: current.user_id, roles: current.roles, tier: current.user_tier };
        actor.require_tenant()?;
        state.edge_rate.check(&actor, "assistant_chat").await?;
        routes::ensure_builtin_session(state, &actor, text(e,"session_id")).await?;
        Ok::<Actor,AgentError>(actor)
    }.await;
    let actor = match prepared {
        Ok(actor) => actor,
        Err(error) => { return preflight_failure(pool, &job, &error).await; }
    };
    let mut tx = pool.begin().await?;
    // Lock the schedule exactly as cancel does, so no pre-cancel dispatch can be claimed later.
    let status: String = sqlx::query_scalar("SELECT status FROM gateway.scheduled_tasks WHERE task_id=$1::uuid FOR UPDATE")
        .bind(text(task,"task_id")).fetch_one(&mut *tx).await?;
    if status != "active" {
        sqlx::query("UPDATE gateway.scheduled_task_executions SET status='cancelled',finished_at=NOW(),updated_at=NOW() WHERE execution_id=$1::uuid")
            .bind(id).execute(&mut *tx).await?;
        tx.commit().await?; return Ok(());
    }
    sqlx::query("UPDATE gateway.scheduled_task_executions SET status='dispatching',updated_at=NOW() WHERE execution_id=$1::uuid")
        .bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    let payload = json!({"message":task["message"],"model_id":task["model_id"],"memory_mode":"off"});
    match routes::start_builtin(state, actor, text(e,"session_id").into(), payload).await {
        Ok(turn) => {
            sqlx::query("UPDATE gateway.scheduled_task_executions SET status='running',run_id=$2::uuid,runtime_thread_id=$3::uuid,updated_at=NOW() WHERE execution_id=$1::uuid")
                .bind(id).bind(turn.run_id).bind(turn.runtime_thread_id).execute(pool).await?;
        }
        Err(error) => {
            sqlx::query("UPDATE gateway.scheduled_task_executions SET status='uncertain',error_code=$2,updated_at=NOW() WHERE execution_id=$1::uuid")
                .bind(id).bind(text(&error.detail,"code")).execute(pool).await?;
        }
    }
    Ok(())
}
async fn preflight_failure(pool: &PgPool, job: &Value, error: &AgentError) -> Result<(),AgentError> {
    let task = &job["task"];
    let retry = error.status >= 500 || error.status == 429;
    let attempt = task["retry_count"].as_i64().unwrap_or(0);
    let retry = retry && attempt < task["max_retries"].as_i64().unwrap_or(0);
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE gateway.scheduled_task_executions SET status='failed',error_code=$2,finished_at=NOW(),updated_at=NOW() WHERE execution_id=$1::uuid")
        .bind(text(&job["execution"],"execution_id")).bind(text(&error.detail,"code")).execute(&mut *tx).await?;
    sqlx::query("UPDATE gateway.scheduled_tasks SET status=CASE WHEN $2 THEN status ELSE 'paused' END,retry_count=retry_count+1,next_run_at=NOW()+make_interval(secs=>$3),updated_at=NOW() WHERE task_id=$1::uuid AND status='active'")
        .bind(text(task,"task_id")).bind(retry).bind((30 * (1_i64 << attempt.min(3))) as f64).execute(&mut *tx).await?;
    tx.commit().await?; Ok(())
}
async fn reconcile(state: &AgentState) -> Result<(), AgentError> {
    let pool = &state.repository.pool;
    let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(e) FROM gateway.scheduled_task_executions e WHERE status IN ('preparing','dispatching','running','uncertain') ORDER BY created_at LIMIT 4")
        .fetch_all(pool).await?;
    for e in rows {
        let id = text(&e,"execution_id");
        let run: Option<Value> = sqlx::query_scalar("SELECT jsonb_build_object('run_id',run_id,'status',status,'thread_id',harness_thread_id) FROM assistant.assistant_runs WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND engine='agent_runtime' ORDER BY started_at DESC LIMIT 1")
            .bind(text(&e,"tenant_id")).bind(text(&e,"user_id")).bind(text(&e,"session_id")).fetch_optional(pool).await?;
        let Some(run) = run else {
            if e["status"] == "preparing" {
                // No runtime call follows preparing without a committed dispatch marker.
                sqlx::query("UPDATE gateway.scheduled_task_executions SET status='failed',error_code='SCHEDULE_PREFLIGHT_INTERRUPTED',finished_at=NOW(),updated_at=NOW() WHERE execution_id=$1::uuid")
                    .bind(id).execute(pool).await?;
                sqlx::query("UPDATE gateway.scheduled_tasks SET status='paused',updated_at=NOW() WHERE task_id=$1::uuid AND status='active'")
                    .bind(text(&e,"task_id")).execute(pool).await?;
            } else {
                sqlx::query("UPDATE gateway.scheduled_task_executions SET status='uncertain',error_code=COALESCE(error_code,'SCHEDULE_DISPATCH_UNCERTAIN'),updated_at=NOW() WHERE execution_id=$1::uuid")
                    .bind(id).execute(pool).await?;
            }
            continue;
        };
        let status = text(&run,"status");
        if matches!(status, "succeeded" | "failed" | "cancelled") {
            let mut tx = pool.begin().await?;
            sqlx::query("UPDATE gateway.scheduled_task_executions SET status=$2,run_id=$3::uuid,runtime_thread_id=$4::uuid,finished_at=NOW(),updated_at=NOW() WHERE execution_id=$1::uuid")
                .bind(id).bind(status).bind(text(&run,"run_id")).bind(text(&run,"thread_id")).execute(&mut *tx).await?;
            // Fixed delay, skip missed ticks: no catch-up storm and no overlapping runs.
            sqlx::query("UPDATE gateway.scheduled_tasks SET status=CASE WHEN status='cancelled' THEN status WHEN interval_seconds IS NULL THEN 'completed' ELSE status END,next_run_at=CASE WHEN interval_seconds IS NULL THEN next_run_at ELSE NOW()+make_interval(secs=>interval_seconds::double precision) END,retry_count=0,updated_at=NOW() WHERE task_id=$1::uuid")
                .bind(text(&e,"task_id")).execute(&mut *tx).await?;
            tx.commit().await?;
            continue;
        }
        let scope = super::Scope { tenant_id: text(&e,"tenant_id").into(), user_id: text(&e,"user_id").into(), session_id: text(&e,"session_id").into() };
        if e["cancel_requested"] == true {
            // An acknowledged interrupt is not a fabricated terminal result.
            let _ = state.control.interrupt_turn(&scope, text(&run,"thread_id"), text(&run,"run_id"), "scheduled_task_cancelled").await;
        }
        // Existing runtime event projection owns completion/accounting. Disconnect
        // or timeout is never interpreted as success or permission to redispatch.
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            let mut events = state.control.stream_thread_events(scope, text(&run,"thread_id").into(), e["after_sequence"].as_i64().unwrap_or(0), 1000, Some(text(&run,"run_id").into())).await?;
            while let Some(event) = events.recv().await {
                let event = event?;
                if let Some(sequence) = event["sequence"].as_i64() {
                    sqlx::query("UPDATE gateway.scheduled_task_executions SET after_sequence=GREATEST(after_sequence,$2),updated_at=NOW() WHERE execution_id=$1::uuid")
                        .bind(id).bind(sequence).execute(pool).await?;
                }
            }
            Ok::<(),AgentError>(())
        }).await;
    }
    Ok(())
}

#[cfg(test)]
mod history_page_tests {
    use super::*;
    #[test]
    fn scheduled_history_page_query_accepts_encoded_timestamp_and_uuid() {
        let uri = "/api/v1/scheduled-tasks?limit=20&before_at=2026-10-13T02%3A00%3A00%2B00%3A00&before_id=00000000-0000-0000-0000-000000000001".parse().unwrap();
        let page = schedule_page_query(&uri, 200).unwrap();
        assert_eq!(page.limit, Some(20));
        assert_eq!(page.before_id.as_deref(), Some("00000000-0000-0000-0000-000000000001"));
        assert_eq!(page.before_at.unwrap().to_rfc3339(), "2026-10-13T02:00:00+00:00");
    }
    #[test]
    fn scheduled_history_page_query_rejects_invalid_and_duplicate_inputs() {
        for query in ["limit=0", "limit=201", "limit=-1", "limit=1.5", "limit=1&limit=2", "before_id=00000000-0000-0000-0000-000000000001", "before_at=2026-10-13T00:00:00Z", "before_at=bad&before_id=bad", "before_at=2026-10-13T00:00:00Z&before_id=bad", "offset=20"] {
            let uri = format!("/api/v1/scheduled-tasks?{query}").parse().unwrap();
            assert!(schedule_page_query(&uri,200).is_err(), "{query}");
        }
        assert!(schedule_page_query(&"/api/v1/scheduled-tasks?limit=101".parse().unwrap(),100).is_err());
        assert_eq!(schedule_page_query(&"/api/v1/scheduled-tasks".parse().unwrap(),200).unwrap().limit,None);
    }
}
