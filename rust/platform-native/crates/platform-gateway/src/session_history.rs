//! Assistant session CRUD and read-only Runtime history projection.
//!
//! Canonical contracts: `_assistant_routes/sessions.py`, the core database
//! session manager, `agent_runtime/thread_store.py`, and `images/history.py`.
//! This module never starts a turn, appends Runtime items, or rewrites history.
use ai_platform_gateway_foundation::sessions_api::{SessionsApiError, SessionsRuntimeCleanup};
use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::{AuthService, Principal},
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use ai_platform_gateway_foundation::native_contract::sessions::{self as contract, SessionResponse, HistoryLimit, HistoryMessage, create_metadata};
use serde_json::{Map, Value, json};
use sqlx::{FromRow, PgPool};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
};

pub type DocumentKey = (String, String);
pub type SourceVersionKey = (String, String, i64, String);
#[derive(Clone, Debug, Default)]
pub struct SourceReferences {
    pub datasets: HashSet<String>,
    pub documents: HashSet<DocumentKey>,
    pub versions: HashSet<SourceVersionKey>,
}
impl SourceReferences {
    fn extend(&mut self, other: &Self) {
        self.datasets.extend(other.datasets.iter().cloned());
        self.documents.extend(other.documents.iter().cloned());
        self.versions.extend(other.versions.iter().cloned());
    }
    fn allowed(&self, visible: &VisibleSources) -> bool {
        self.datasets
            .iter()
            .all(|id| visible.datasets.contains_key(id))
            && self.documents.is_subset(&visible.documents)
            && self.versions.is_subset(&visible.versions)
    }
}
#[derive(Clone, Debug, Default)]
pub struct VisibleSources {
    pub datasets: BTreeMap<String, String>,
    pub documents: HashSet<DocumentKey>,
    pub versions: HashSet<SourceVersionKey>,
}
/// Adapter to the existing Knowledge authorization service, never a second ACL.
/// Missing/unavailable authorizers must return empty sets. Every call is scoped
/// to the authenticated owner, not client-supplied identity headers.
pub trait HistorySourceAccess: Send + Sync {
    fn visible_sources<'a>(
        &'a self,
        principal: &'a Principal,
        references: &'a SourceReferences,
    ) -> Pin<Box<dyn Future<Output = VisibleSources> + Send + 'a>>;
}
#[derive(Clone)]
pub struct SessionConfig {
    pub default_ttl_seconds: i64,
    pub kernel_revision: Option<String>,
    pub source_access: Option<Arc<dyn HistorySourceAccess>>,
    /// The production host shares its durable generic-session cleanup owner.
    pub cleanup: Option<Arc<dyn SessionsRuntimeCleanup>>,
}
impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            default_ttl_seconds: 7 * 86400,
            kernel_revision: None,
            source_access: None,
            cleanup: None,
        }
    }
}
#[derive(Clone)]
struct Sessions {
    auth: Arc<AuthService>,
    pool: PgPool,
    config: SessionConfig,
}
/// Merge with the Gateway's existing GET /sessions route. DELETE fails closed
/// unless the host injects the durable session cleanup collaborator.
pub(super) fn router(auth: Arc<AuthService>, pool: PgPool, config: SessionConfig) -> Router {
    Router::new()
        .route(contract::COLLECTION, post(create))
        .route(contract::ITEM, get(detail).delete(delete))
        .route(
            contract::HISTORY,
            get(history),
        )
        .with_state(Sessions { auth, pool, config })
}
fn error(status: StatusCode, detail: &'static str) -> Response {
    super::error(status, detail)
}
#[allow(
    clippy::result_large_err,
    reason = "Axum response is returned directly on the cold authentication error path"
)]
async fn identity(state: &Sessions, headers: &HeaderMap) -> Result<Principal, Response> {
    let token = match select_credential(headers, &HeaderName::from_static("x-api-key")) {
        Ok(SelectedCredential::Jwt(token)) => token,
        _ => return Err(error(StatusCode::UNAUTHORIZED, "Authentication required")),
    };
    let principal = state.auth.authenticate(token).await.map_err(|e| {
        error(
            StatusCode::from_u16(e.status()).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            "Authentication failed",
        )
    })?;
    if principal.account.user_id.is_empty() || principal.account.tenant_id.is_empty() {
        return Err(error(StatusCode::UNAUTHORIZED, "Authentication required"));
    }
    Ok(principal)
}
#[derive(FromRow)]
struct SessionRow {
    session_id: String,
    user_id: String,
    tenant_id: String,
    service_id: Option<String>,
    created_at: Option<DateTime<Utc>>,
    updated_at: Option<DateTime<Utc>>,
    metadata: Value,
    history: Value,
}
fn timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(
        if value.timestamp_subsec_micros() == 0 {
            chrono::SecondsFormat::Secs
        } else {
            chrono::SecondsFormat::Micros
        },
        false,
    )
}
fn session_response(row: SessionRow) -> SessionResponse {
    let message_count = legacy_array(&row.history)
        .iter()
        .filter(|m| m.is_object())
        .count();
    SessionResponse {
        session_id: row.session_id,
        user_id: row.user_id,
        tenant_id: row.tenant_id,
        service_id: row.service_id,
        created_at: row.created_at.map(timestamp),
        updated_at: row.updated_at.map(timestamp),
        metadata: row.metadata,
        message_count,
    }
}
const SESSION_SELECT: &str = "SELECT session_id,user_id,tenant_id,service_id,created_at,updated_at,COALESCE(metadata,'{}'::jsonb) AS metadata,COALESCE(history,'[]'::jsonb) AS history FROM assistant.sessions WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND status <> 'deleted'";
async fn session(
    state: &Sessions,
    session_id: &str,
    tenant: &str,
    user: &str,
) -> Result<Option<SessionRow>, sqlx::Error> {
    let row = sqlx::query_as(SESSION_SELECT)
        .bind(session_id)
        .bind(tenant)
        .bind(user)
        .fetch_optional(&state.pool)
        .await?;
    if row.is_some() {
        // The canonical database manager renews an expired session on access;
        // this bounded update never reorders the sidebar or rewrites history.
        sqlx::query("UPDATE assistant.sessions SET expires_at=NOW()+($4::bigint * INTERVAL '1 second') WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND status <> 'deleted' AND expires_at < NOW()")
            .bind(session_id).bind(tenant).bind(user).bind(state.config.default_ttl_seconds).execute(&state.pool).await?;
    }
    Ok(row)
}
async fn detail(
    State(state): State<Sessions>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let principal = match identity(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (tenant, user) = (&principal.account.tenant_id, &principal.account.user_id);
    match session(&state, &session_id, tenant, user).await {
        Ok(Some(row)) => Json(session_response(row)).into_response(),
        Ok(None) => error(StatusCode::NOT_FOUND, "Session not found"),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to get session"),
    }
}
async fn delete(
    State(state): State<Sessions>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let principal = match identity(&state, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    delete_owned_response(
        state.config.cleanup.as_deref(),
        &principal.account.tenant_id,
        &principal.account.user_id,
        &session_id,
    )
    .await
}

async fn delete_owned_response(
    cleanup: Option<&dyn SessionsRuntimeCleanup>,
    tenant: &str,
    user: &str,
    session_id: &str,
) -> Response {
    let Some(cleanup) = cleanup else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "Session deletion was not completed");
    };
    // The shared collaborator checks canonical ownership before any mutation and
    // again under its lifecycle lock. Never use a stale cached row as authority,
    // or fall back to an unscoped manager delete when durable cleanup is absent.
    match cleanup.delete_owned_session(tenant, user, session_id).await {
        Ok(true) => {}
        Ok(false) => {
            return error(StatusCode::SERVICE_UNAVAILABLE, "Session deletion was not completed");
        }
        Err(SessionsApiError::Http { status: 404, .. }) => {
            return error(StatusCode::NOT_FOUND, "Session not found");
        }
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete session"),
    }
    // Python verifies the session is gone before acknowledging. Native durable
    // deletion can retain an FK parent tombstone, so verify logical deletion via
    // its permanent barrier, not physical row absence or a cached manager read.
    match cleanup.session_deleted(tenant, user, session_id).await {
        Ok(true) => Json(contract::delete_response(session_id)).into_response(),
        Ok(false) => error(StatusCode::SERVICE_UNAVAILABLE, "Session deletion was not completed"),
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "Failed to delete session"),
    }
}

#[cfg(test)]
#[path = "session_history_delete_tests.rs"]
mod delete_tests;

async fn create(State(state): State<Sessions>, headers: HeaderMap, body: Bytes) -> Response {
    let principal = match identity(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (tenant, user) = (&principal.account.tenant_id, &principal.account.user_id);
    let metadata = match create_metadata(&body) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid request body"),
    };
    match create_owned(&state, tenant, user, metadata).await {
        Ok(row) => {
            // Python returns the newly-created datetime.utcnow() values without
            // a timezone, whereas subsequent database reads use aware UTC.
            let mut response = session_response(row);
            response.created_at = response
                .created_at
                .map(|v| v.trim_end_matches("+00:00").to_owned());
            response.updated_at = response
                .updated_at
                .map(|v| v.trim_end_matches("+00:00").to_owned());
            Json(response).into_response()
        }
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to create session",
        ),
    }
}
async fn create_owned(
    state: &Sessions,
    tenant: &str,
    user: &str,
    metadata: Value,
) -> Result<SessionRow, sqlx::Error> {
    let mut tx = state.pool.begin().await?;
    let row: SessionRow = sqlx::query_as("INSERT INTO assistant.sessions(session_id,user_id,tenant_id,service_id,state,history,metadata,config,status,expires_at) VALUES(gen_random_uuid()::text,$1,$2,'__builtin_assistant__','{}','[]',$3,'{}','active',CASE WHEN $4::bigint=0 THEN NULL ELSE NOW()+($4::bigint * INTERVAL '1 second') END) RETURNING session_id,user_id,tenant_id,service_id,created_at,updated_at,metadata,history")
        .bind(user).bind(tenant).bind(metadata).bind(state.config.default_ttl_seconds).fetch_one(&mut *tx).await?;
    // Session and immutable single-kernel assignment are committed together.
    // Failure cannot leave an unbound session, matching Python's compensating delete.
    let revision = state
        .config
        .kernel_revision
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("legacy-runtime");
    sqlx::query("INSERT INTO assistant.assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner,kernel_revision,assignment_reason) VALUES($1,$2,$3,'agent_runtime',$4,'single_kernel')")
        .bind(tenant).bind(user).bind(&row.session_id).bind(revision).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(row)
}
#[derive(FromRow)]
struct RuntimeMessage {
    role: String,
    content: String,
    sequence: Option<i64>,
    run_id: Option<String>,
    created_at: Option<DateTime<Utc>>,
    thinking_content: Option<String>,
    tool_calls: Option<Value>,
    tool_results: Option<Value>,
    runtime_events: Option<Value>,
    total: i64,
}
#[derive(FromRow)]
struct Snapshot {
    run_id: String,
    snapshot: Value,
    created_at: Option<DateTime<Utc>>,
}
#[derive(FromRow)]
struct ContextEvent {
    run_id: Option<String>,
    chunks: Option<Value>,
    created_at: Option<DateTime<Utc>>,
}
#[derive(Default)]
struct ConversationSources {
    all: SourceReferences,
    legacy: SourceReferences,
    by_run: HashMap<String, SourceReferences>,
    dated_runs: Vec<(DateTime<Utc>, SourceReferences)>,
    dated_contexts: Vec<(DateTime<Utc>, SourceReferences)>,
    dates_complete: bool,
}
fn legacy_array(history: &Value) -> &[Value] {
    history
        .as_array()
        .or_else(|| history.get("messages").and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or(&[])
}
fn string(value: &Value) -> &str {
    value.as_str().unwrap_or("")
}
fn invalid_reference(references: &mut SourceReferences) {
    references.documents.insert((String::new(), String::new()));
    references
        .versions
        .insert((String::new(), String::new(), 0, String::new()));
}
fn context_references(chunks: &Value, default_dataset: &str) -> SourceReferences {
    let mut references = SourceReferences::default();
    let Some(chunks) = chunks.as_array() else {
        invalid_reference(&mut references);
        return references;
    };
    for chunk in chunks {
        if !chunk.is_object() {
            invalid_reference(&mut references);
            continue;
        }
        let metadata = &chunk["metadata"];
        let dataset = [
            string(&chunk["dataset_id"]),
            string(&metadata["dataset_id"]),
            default_dataset,
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .trim()
        .to_owned();
        let document = [
            string(&chunk["document_id"]),
            string(&metadata["document_id"]),
        ]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .trim()
        .to_owned();
        if ["dataset_id", "document_id"].iter().any(|key| {
            !chunk[*key].is_null() && !metadata[*key].is_null() && chunk[*key] != metadata[*key]
        }) {
            invalid_reference(&mut references);
            continue;
        }
        if !dataset.is_empty() {
            references.datasets.insert(dataset.clone());
        }
        references
            .documents
            .insert((dataset.clone(), document.clone()));
        let (top_version, top_hash) = (&chunk["source_version"], &chunk["source_hash"]);
        let (meta_version, meta_hash) = (&metadata["source_version"], &metadata["source_hash"]);
        let malformed = |version: &Value, hash: &Value| {
            version.is_null() != hash.is_null()
                || (!version.is_null() && version.as_i64().is_none())
                || (!hash.is_null() && hash.as_str().is_none_or(|h| h != h.trim()))
        };
        if malformed(top_version, top_hash) || malformed(meta_version, meta_hash) {
            references
                .versions
                .insert((String::new(), String::new(), 0, String::new()));
            continue;
        }
        if top_version.is_null() && meta_version.is_null() {
            continue;
        }
        let version = if !top_version.is_null() {
            top_version
        } else {
            meta_version
        }
        .as_i64()
        .unwrap_or(0);
        let hash = string(if !top_hash.is_null() {
            top_hash
        } else {
            meta_hash
        })
        .trim()
        .to_ascii_lowercase();
        if version <= 0
            || dataset.is_empty()
            || document.is_empty()
            || hash.len() != 64
            || !hash.bytes().all(|c| c.is_ascii_hexdigit())
            || (!top_version.is_null() && !meta_version.is_null() && top_version != meta_version)
            || (!top_hash.is_null()
                && !meta_hash.is_null()
                && !string(top_hash).eq_ignore_ascii_case(string(meta_hash)))
        {
            references
                .versions
                .insert((String::new(), String::new(), 0, String::new()));
        } else {
            references
                .versions
                .insert((dataset, document, version, hash));
        }
    }
    references
}
fn legacy_references(history: &Value) -> SourceReferences {
    let mut references = SourceReferences::default();
    for message in legacy_array(history) {
        let Some(contexts) = message.get("metadata").and_then(|m| m.get("contexts")) else {
            continue;
        };
        let Some(contexts) = contexts.as_array() else {
            invalid_reference(&mut references);
            continue;
        };
        for context in contexts {
            if !context.is_object() {
                invalid_reference(&mut references);
                continue;
            }
            let dataset = string(&context["dataset_id"]);
            if !dataset.is_empty() {
                references.datasets.insert(dataset.to_owned());
            }
            references.extend(&context_references(&context["chunks"], dataset));
        }
    }
    references
}
pub(crate) type SharingSourceSnapshot = (String, Value, Option<DateTime<Utc>>);
pub(crate) type SharingSourceContext = (Option<String>, Option<Value>, Option<DateTime<Utc>>);
pub(crate) type SharingSourceMap = (SourceReferences, HashMap<String, SourceReferences>);
#[derive(Debug)]
pub(crate) struct SharingSourceError;
/// Reuse admitted-history provenance for quiz sharing. The caller supplies
/// tenant/owner-scoped, decoded rows in canonical database order.
pub(crate) fn sharing_source_map(
    history: &Value,
    snapshots: Vec<SharingSourceSnapshot>,
    contexts: Vec<SharingSourceContext>,
) -> Result<SharingSourceMap, SharingSourceError> {
    let snapshots = snapshots
        .into_iter()
        .map(|(run_id, snapshot, created_at)| Snapshot {
            run_id,
            snapshot,
            created_at,
        })
        .collect::<Vec<_>>();
    let contexts = contexts
        .into_iter()
        .map(|(run_id, chunks, created_at)| ContextEvent {
            run_id,
            chunks,
            created_at,
        })
        .collect::<Vec<_>>();
    let sources = source_map(history, &snapshots, &contexts).map_err(|_| SharingSourceError)?;
    Ok((sources.all, sources.by_run))
}

fn source_map(
    history: &Value,
    snapshots: &[Snapshot],
    contexts: &[ContextEvent],
) -> Result<ConversationSources, ()> {
    let legacy = legacy_references(history);
    let mut result = ConversationSources {
        legacy: legacy.clone(),
        all: legacy.clone(),
        dates_complete: true,
        ..Default::default()
    };
    let mut baseline = legacy.clone();
    let mut events: HashMap<String, SourceReferences> = HashMap::new();
    let mut timed: Vec<(DateTime<Utc>, String, SourceReferences)> = Vec::new();
    for event in contexts {
        let references = context_references(event.chunks.as_ref().unwrap_or(&Value::Null), "");
        let run_id = event.run_id.clone().unwrap_or_default();
        events
            .entry(run_id.clone())
            .or_default()
            .extend(&references);
        result.all.extend(&references);
        if run_id.is_empty() || event.created_at.is_none() {
            baseline.extend(&references);
        }
        if let Some(at) = event.created_at {
            result.dated_contexts.push((at, references.clone()));
            timed.push((at, run_id, references));
        } else {
            result.dates_complete = false;
        }
    }
    // Unknown-time/owner source events restrict legacy and every inherited run.
    result.legacy.documents = baseline.documents.clone();
    result.legacy.versions = baseline.versions.clone();
    let mut inherited_datasets = legacy.datasets.clone();
    for snapshot in snapshots {
        let Some(items) = snapshot
            .snapshot
            .pointer("/readonly_capabilities/items")
            .and_then(Value::as_array)
        else {
            return Err(());
        };
        for item in items.iter().filter(|item| item["kind"] == "knowledge") {
            let dataset = string(&item["payload"]["dataset_id"]);
            if dataset.is_empty() {
                return Err(());
            }
            inherited_datasets.insert(dataset.to_owned());
        }
        let mut references = baseline.clone();
        references
            .datasets
            .extend(inherited_datasets.iter().cloned());
        for (at, _, event_refs) in &timed {
            if snapshot.created_at.is_none_or(|created| *at <= created) {
                references.extend(event_refs);
            }
        }
        if let Some(own) = events.get(&snapshot.run_id) {
            references.extend(own);
        }
        result.all.extend(&references);
        result
            .by_run
            .insert(snapshot.run_id.clone(), references.clone());
        if let Some(at) = snapshot.created_at {
            result.dated_runs.push((at, references));
        } else {
            result.dates_complete = false;
        }
    }
    let missing: Vec<_> = events
        .keys()
        .filter(|id| !id.is_empty() && !result.by_run.contains_key(*id))
        .cloned()
        .collect();
    for run_id in missing {
        let cutoff = timed
            .iter()
            .filter(|(_, id, _)| id == &run_id)
            .map(|(at, _, _)| *at)
            .max();
        let prior =
            cutoff.and_then(|cutoff| result.dated_runs.iter().rev().find(|(at, _)| *at <= cutoff));
        let mut references = baseline.clone();
        references.datasets.extend(if let Some((_, prior)) = prior {
            prior.datasets.clone()
        } else if cutoff.is_some() {
            legacy.datasets.clone()
        } else {
            inherited_datasets.clone()
        });
        for (at, _, event_refs) in &timed {
            if cutoff.is_none_or(|cutoff| *at <= cutoff) {
                references.extend(event_refs);
            }
        }
        references.extend(&events[&run_id]);
        result.by_run.insert(run_id, references);
    }
    Ok(result)
}

const RUNTIME_HISTORY_SQL: &str = r#"
            SELECT role, content, sequence, run_id, created_at, thinking_content,
                   tool_calls, tool_results, runtime_events,
                   COUNT(*) OVER() AS total
              FROM (
                    SELECT 'user'::text AS role,
                           COALESCE(
                               NULLIF(snapshot.snapshot #>> '{input,message}', ''),
                               run.request_preview
                           ) AS content,
                           NULL::bigint AS sequence,
                           run.run_id::text AS run_id,
                           run.started_at AS created_at,
                           NULL::text AS thinking_content,
                           NULL::jsonb AS tool_calls,
                           NULL::jsonb AS tool_results,
                           NULL::jsonb AS runtime_events
                      FROM gateway.assistant_runs AS run
                      JOIN gateway.assistant_runtime_snapshots AS snapshot
                        ON snapshot.run_id = run.run_id
                       AND snapshot.runtime_thread_id = run.harness_thread_id
                       AND snapshot.tenant_id = run.tenant_id
                       AND snapshot.user_id = run.user_id
                       AND snapshot.session_id = run.session_id
                     WHERE run.harness_thread_id = $1::uuid
                       AND run.tenant_id = $2 AND run.user_id = $3
                       AND run.engine = 'agent_runtime'
                    UNION ALL
                    SELECT 'assistant'::text AS role,
                           item.payload #>> '{payload,message}' AS content,
                           item.sequence,
                           active_turn.turn_id AS run_id,
                           item.created_at,
                           (
                               SELECT string_agg(
                                   reasoning.payload #>> '{data,content}',
                                   '' ORDER BY reasoning.sequence
                               )
                                 FROM gateway.assistant_runtime_items AS reasoning
                                WHERE reasoning.runtime_thread_id = item.runtime_thread_id
                                  AND reasoning.tenant_id = item.tenant_id
                                  AND reasoning.user_id = item.user_id
                                  AND reasoning.turn_id = active_turn.turn_id
                                  AND reasoning.event_type = 'compat/v1/thinking_delta'
                           ) AS thinking_content
                           ,(
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'id', tool.payload #>> '{data,tool_call_id}',
                                       'name', tool.payload #>> '{data,tool_name}',
                                       'arguments', COALESCE(
                                           tool.payload #> '{data,arguments}',
                                           '{}'::jsonb
                                       ),
                                       'status', tool.payload #>> '{data,status}'
                                   ) ORDER BY tool.sequence
                               )
                                 FROM gateway.assistant_runtime_items AS tool
                                WHERE tool.runtime_thread_id = item.runtime_thread_id
                                  AND tool.tenant_id = item.tenant_id
                                  AND tool.user_id = item.user_id
                                  AND tool.turn_id = active_turn.turn_id
                                  AND tool.event_type = 'compat/v1/tool_call_start'
                           ) AS tool_calls,
                           (
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'tool_call_id', result.payload #>> '{data,tool_call_id}',
                                       'name', result.payload #>> '{data,tool_name}',
                                       'result', result.payload #> '{data,result}',
                                       'error', CASE
                                           WHEN COALESCE(
                                               (result.payload #>> '{data,success}')::boolean,
                                               FALSE
                                           ) THEN NULL
                                           ELSE COALESCE(
                                               result.payload #>> '{data,error}',
                                               result.payload #>> '{data,status}'
                                           )
                                       END,
                                       'duration_ms', result.payload #> '{data,duration_ms}'
                                   ) ORDER BY result.sequence
                               )
                                 FROM gateway.assistant_runtime_items AS result
                                WHERE result.runtime_thread_id = item.runtime_thread_id
                                  AND result.tenant_id = item.tenant_id
                                  AND result.user_id = item.user_id
                                  AND result.turn_id = active_turn.turn_id
                                  AND result.event_type = 'compat/v1/tool_call_result'
                           ) AS tool_results
                           ,(
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'event_type', CASE WHEN runtime.event_type = 'agent-runtime/tool-lifecycle'
                                                          THEN 'side_effect_unknown'
                                                          ELSE replace(runtime.event_type, 'compat/v1/', '') END,
                                       'data', runtime.payload #> '{data}',
                                       'timestamp', EXTRACT(EPOCH FROM runtime.created_at)
                                   ) ORDER BY runtime.sequence
                               )
                                 FROM gateway.assistant_runtime_items AS runtime
                                WHERE runtime.runtime_thread_id = item.runtime_thread_id
                                  AND runtime.tenant_id = item.tenant_id
                                  AND runtime.user_id = item.user_id
                                  AND runtime.turn_id = active_turn.turn_id
                                  AND (runtime.event_type IN (
                                      'compat/v1/subagent_started',
                                      'compat/v1/subagent_step',
                                      'compat/v1/subagent_finished',
                                      'compat/v1/plan_update',
                                      'compat/v1/context_compaction',
                                      'compat/v1/context_retrieved',
                                      'compat/v1/memory_loaded',
                                      'compat/v1/side_effect_unknown',
                                      'compat/v1/run_finished',
                                      'compat/v1/run_error',
                                      'compat/v1/cancelled'
                                  ) OR (runtime.event_type = 'agent-runtime/tool-lifecycle'
                                        AND runtime.item_type = 'tool_result'
                                        AND runtime.status = 'side_effect_unknown'))
                           ) AS runtime_events
                      FROM gateway.assistant_runtime_items AS item
                      JOIN LATERAL (
                           SELECT started.turn_id
                             FROM gateway.assistant_runtime_items AS started
                            WHERE started.runtime_thread_id = item.runtime_thread_id
                              AND started.tenant_id = item.tenant_id
                              AND started.user_id = item.user_id
                              AND started.event_type = 'compat/v1/run_started'
                              AND started.sequence <= item.sequence
                            ORDER BY started.sequence DESC LIMIT 1
                      ) AS active_turn ON TRUE
                     WHERE item.runtime_thread_id = $1::uuid
                       AND item.tenant_id = $2 AND item.user_id = $3
                       AND item.event_type = 'rollout/item'
                       AND item.item_type = 'event_msg'
                       AND item.payload #>> '{payload,type}' = 'agent_message'
                    UNION ALL
                    SELECT 'assistant'::text AS role,
                           string_agg(
                               CASE
                                   WHEN delta.event_type = 'compat/v1/text_delta'
                                   THEN COALESCE(delta.payload #>> '{data,content}', '')
                                   ELSE ''
                               END,
                               '' ORDER BY delta.sequence
                           ) AS content,
                           MAX(delta.sequence) AS sequence,
                           delta.turn_id AS run_id,
                           COALESCE((
                               SELECT stopped.finished_at FROM gateway.assistant_runs AS stopped
                                WHERE stopped.run_id::text = delta.turn_id
                                  AND stopped.harness_thread_id = delta.runtime_thread_id
                                  AND stopped.tenant_id = delta.tenant_id AND stopped.user_id = delta.user_id
                                  AND stopped.status = 'cancelled'
                           ), MAX(delta.created_at)) AS created_at,
                           (
                               SELECT string_agg(
                                   reasoning.payload #>> '{data,content}',
                                   '' ORDER BY reasoning.sequence
                               )
                                 FROM gateway.assistant_runtime_items AS reasoning
                                WHERE reasoning.runtime_thread_id = delta.runtime_thread_id
                                  AND reasoning.tenant_id = delta.tenant_id
                                  AND reasoning.user_id = delta.user_id
                                  AND reasoning.turn_id = delta.turn_id
                                  AND reasoning.event_type = 'compat/v1/thinking_delta'
                           ) AS thinking_content,
                           NULL::jsonb AS tool_calls,
                           NULL::jsonb AS tool_results,
                           (
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'event_type', CASE WHEN runtime.event_type = 'agent-runtime/tool-lifecycle'
                                                          THEN 'side_effect_unknown'
                                                          ELSE replace(runtime.event_type, 'compat/v1/', '') END,
                                       'data', runtime.payload #> '{data}',
                                       'timestamp', EXTRACT(EPOCH FROM runtime.created_at)
                                   ) ORDER BY runtime.sequence
                               )
                                 FROM gateway.assistant_runtime_items AS runtime
                                WHERE runtime.runtime_thread_id = delta.runtime_thread_id
                                  AND runtime.tenant_id = delta.tenant_id
                                  AND runtime.user_id = delta.user_id
                                  AND runtime.turn_id = delta.turn_id
                                  AND (runtime.event_type IN (
                                      'compat/v1/subagent_started',
                                      'compat/v1/subagent_step',
                                      'compat/v1/subagent_finished',
                                      'compat/v1/plan_update',
                                      'compat/v1/context_compaction',
                                      'compat/v1/context_retrieved',
                                      'compat/v1/memory_loaded',
                                      'compat/v1/side_effect_unknown',
                                      'compat/v1/run_finished',
                                      'compat/v1/run_error',
                                      'compat/v1/cancelled'
                                  ) OR (runtime.event_type = 'agent-runtime/tool-lifecycle'
                                        AND runtime.item_type = 'tool_result'
                                        AND runtime.status = 'side_effect_unknown'))
                           ) AS runtime_events
                      FROM gateway.assistant_runtime_items AS delta
                      LEFT JOIN LATERAL (
                           SELECT start.sequence
                             FROM gateway.assistant_runtime_items AS start
                            WHERE start.runtime_thread_id = delta.runtime_thread_id
                              AND start.tenant_id = delta.tenant_id
                              AND start.user_id = delta.user_id
                              AND start.turn_id = delta.turn_id
                              AND start.event_type = 'compat/v1/run_started'
                            ORDER BY start.sequence ASC LIMIT 1
                      ) AS started ON TRUE
                     WHERE delta.runtime_thread_id = $1::uuid
                       AND delta.tenant_id = $2 AND delta.user_id = $3
                       AND delta.event_type IN (
                           'compat/v1/text_delta',
                           'compat/v1/side_effect_unknown',
                           'compat/v1/run_finished',
                           'compat/v1/run_error',
                           'compat/v1/cancelled'
                       )
                       AND (started.sequence IS NOT NULL OR (
                           delta.event_type = 'compat/v1/cancelled' AND EXISTS (
                               SELECT 1 FROM gateway.assistant_runs AS stopped
                                JOIN gateway.assistant_runtime_snapshots AS snapshot ON snapshot.run_id = stopped.run_id
                                 AND snapshot.runtime_thread_id = stopped.harness_thread_id
                                 AND snapshot.tenant_id = stopped.tenant_id AND snapshot.user_id = stopped.user_id
                                 AND snapshot.session_id = stopped.session_id
                                WHERE stopped.run_id::text = delta.turn_id
                                  AND stopped.harness_thread_id = delta.runtime_thread_id
                                  AND stopped.tenant_id = delta.tenant_id AND stopped.user_id = delta.user_id
                                  AND stopped.status = 'cancelled' AND stopped.engine = 'agent_runtime'
                           )
                       ))
                       AND NOT EXISTS (
                           SELECT 1
                             FROM gateway.assistant_runtime_items AS completed
                            WHERE completed.runtime_thread_id = delta.runtime_thread_id
                              AND completed.tenant_id = delta.tenant_id
                              AND completed.user_id = delta.user_id
                              AND completed.event_type = 'rollout/item'
                              AND completed.item_type = 'event_msg'
                              AND completed.payload #>> '{payload,type}' = 'agent_message'
                              AND completed.sequence > started.sequence
                              AND NOT EXISTS (
                                  SELECT 1
                                    FROM gateway.assistant_runtime_items AS next_started
                                   WHERE next_started.runtime_thread_id = delta.runtime_thread_id
                                     AND next_started.tenant_id = delta.tenant_id
                                     AND next_started.user_id = delta.user_id
                                     AND next_started.event_type = 'compat/v1/run_started'
                                     AND next_started.turn_id IS DISTINCT FROM delta.turn_id
                                     AND next_started.sequence > started.sequence
                                     AND next_started.sequence <= completed.sequence
                              )
                       )
                     GROUP BY delta.runtime_thread_id, delta.tenant_id,
                              delta.user_id, delta.turn_id, started.sequence
                   ) AS message
             WHERE content IS NOT NULL
               AND (
                   content <> ''
                   OR EXISTS (
                       SELECT 1
                         FROM jsonb_array_elements(
                             COALESCE(runtime_events, '[]'::jsonb)
                         ) AS terminal_event
                        WHERE terminal_event ->> 'event_type' IN (
                            'side_effect_unknown', 'run_finished', 'run_error', 'cancelled'
                        )
                   )
               )
             ORDER BY created_at DESC, sequence DESC NULLS LAST LIMIT $4
            "#;

const UPLOADS_SQL: &str = r#"
            SELECT s.run_id::text AS run_id, a.artifact_id, a.filename, a.mime_type,
                   a.metadata ->> 'original_filename' AS original_filename
              FROM gateway.assistant_runtime_snapshots AS s
              JOIN gateway.assistant_runs AS r
                ON r.run_id = s.run_id AND r.harness_thread_id = s.runtime_thread_id
               AND r.tenant_id = s.tenant_id AND r.user_id = s.user_id AND r.session_id = s.session_id
             CROSS JOIN LATERAL jsonb_array_elements(
                 CASE WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,platform_config,attachments}') = 'array'
                      THEN s.snapshot #> '{readonly_capabilities,platform_config,attachments}'
                      ELSE '[]'::jsonb END
             ) WITH ORDINALITY AS selected(value, ordinality)
              JOIN assistant.artifacts AS a
                ON a.artifact_id = selected.value ->> 'ref'
               AND a.tenant_id = s.tenant_id AND a.user_id = s.user_id AND a.session_id = s.session_id
             WHERE s.runtime_thread_id = $1::uuid AND s.tenant_id = $2 AND s.user_id = $3
               AND s.run_id::text = ANY($4::text[])
               AND selected.value ->> 'descriptor' = 'read_attachment'
               AND selected.value ->> 'version' = 'v1'
               AND a.source = 'user' AND a.size_bytes > 0
             ORDER BY s.run_id, selected.ordinality
            "#;

const PARAMETERS_SQL: &str = r#"
            SELECT s.run_id::text AS run_id,
                   s.snapshot #> '{reasoning}' AS reasoning,
                   s.snapshot #>> '{model,id}' AS model_id,
                   s.snapshot #> '{parameters,temperature}' AS temperature
              FROM gateway.assistant_runtime_snapshots AS s
              JOIN gateway.assistant_runs AS r
                ON r.run_id = s.run_id AND r.harness_thread_id = s.runtime_thread_id
               AND r.tenant_id = s.tenant_id AND r.user_id = s.user_id AND r.session_id = s.session_id
             WHERE s.runtime_thread_id = $1::uuid AND s.tenant_id = $2 AND s.user_id = $3
               AND s.run_id::text = ANY($4::text[])
            "#;

const QUIZ_SQL: &str = r#"
                SELECT run_id::text AS run_id,
                       result_summary ->> 'quiz_id' AS quiz_id
                  FROM gateway.assistant_capability_executions
                 WHERE tenant_id = $1 AND user_id = $2
                   AND capability_id = 'generate_quiz'
                   AND status = 'succeeded'
                   AND run_id::text = ANY($3::text[])
                 ORDER BY created_at
                "#;
fn visible_text(text: &str) -> String {
    const OPEN: &str = "[AI_PLATFORM_READONLY_CONTEXT_V1]";
    const CLOSE: &str = "[/AI_PLATFORM_READONLY_CONTEXT_V1]";
    let mut remaining = text;
    let mut result = String::new();
    while let Some(start) = remaining.find(OPEN) {
        let tail = &remaining[start + OPEN.len()..];
        let Some(end) = tail.find(CLOSE) else {
            break;
        };
        result.push_str(&remaining[..start]);
        remaining = &tail[end + CLOSE.len()..];
    }
    result.push_str(remaining);
    result.trim().to_owned()
}
fn terminal(kind: &str) -> bool {
    matches!(
        kind,
        "run_finished" | "run_error" | "cancelled" | "side_effect_unknown"
    )
}
fn safe_event(event: &Value) -> Value {
    let kind = string(&event["event_type"]);
    if !terminal(kind) {
        return event.clone();
    }
    let data = &event["data"];
    let mut safe = Map::new();
    let status = string(&data["status"]);
    if matches!(status, "succeeded" | "failed" | "cancelled" | "blocked") {
        safe.insert("status".into(), status.into());
    }
    if status == "cancelled" && data["error_code"] == "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED"
    {
        safe.insert(
            "terminal_reason".into(),
            "runtime_restart_interrupted".into(),
        );
    }
    if data["terminal_envelope"]["exit_reason"] == "side_effect_unknown" {
        safe.insert(
            "terminal_envelope".into(),
            json!({"exit_reason":"side_effect_unknown"}),
        );
    }
    json!({"event_type": kind, "data": safe, "timestamp": event["timestamp"]})
}
fn knowledge_contexts(events: &[Value], allowed: &BTreeMap<String, String>) -> Vec<Value> {
    let mut groups: Vec<(String, Vec<Value>)> = Vec::new();
    for event in events
        .iter()
        .filter(|event| event["event_type"] == "context_retrieved")
    {
        let Some(chunks) = event["data"]["chunks"].as_array() else {
            continue;
        };
        for raw in chunks.iter().take(50) {
            let dataset = string(&raw["dataset_id"]);
            if !allowed.contains_key(dataset) {
                continue;
            }
            let Some(content) = raw["content"].as_str() else {
                continue;
            };
            let metadata = &raw["metadata"];
            let mut citation = json!({
                "document_id": raw["document_id"].as_str().map(Value::from).unwrap_or_else(|| metadata["document_id"].clone()),
                "segment_id": raw["segment_id"].as_str().map(Value::from).unwrap_or_else(|| metadata["segment_id"].clone()),
            });
            let version = if raw["source_version"].is_null() {
                &metadata["source_version"]
            } else {
                &raw["source_version"]
            };
            let hash = if raw["source_hash"].is_null() {
                &metadata["source_hash"]
            } else {
                &raw["source_hash"]
            };
            if !version.is_null() || !hash.is_null() {
                citation["source_version"] = version.clone();
                citation["source_hash"] = hash.clone();
            }
            let index = groups
                .iter()
                .position(|(id, _)| id == dataset)
                .unwrap_or_else(|| {
                    groups.push((dataset.to_owned(), Vec::new()));
                    groups.len() - 1
                });
            groups[index].1.push(json!({"content": content.chars().take(2000).collect::<String>(), "score": raw["score"].as_f64().unwrap_or(0.0), "metadata": citation}));
        }
    }
    groups.into_iter().map(|(id, chunks)| json!({"dataset_id":id,"dataset_name":allowed[&id],"chunks":chunks.into_iter().take(20).collect::<Vec<_>>(),"query":"","took_ms":0})).collect()
}
fn process_summary(events: &[Value], run_id: &Option<String>, text: &str) -> Option<Value> {
    let mut steps = Vec::new();
    let mut status = "succeeded";
    let mut uncertain = false;
    let mut cancelled = false;
    let mut reason: Option<&str> = None;
    for event in events {
        let data = &event["data"];
        match string(&event["event_type"]) {
            "plan_update" => {
                if let Some(plan) = data["plan"].as_array() {
                    for (index, item) in plan.iter().enumerate() {
                        let title = string(&item["step"]);
                        if title.trim().is_empty() {
                            continue;
                        }
                        let state = match string(&item["status"]) {
                            "inProgress" | "in_progress" => "running",
                            "completed" => "completed",
                            _ => "pending",
                        };
                        let id = item["id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("plan-{index}"));
                        steps.push(json!({"id":id,"title":title.chars().take(500).collect::<String>(),"status":state}));
                    }
                }
            }
            "context_compaction" => steps.push(
                json!({"id":"context-compaction","title":"Context compacted","status":"completed"}),
            ),
            "memory_loaded" => steps
                .push(json!({"id":"memory-loaded","title":"Memory loaded","status":"completed"})),
            "side_effect_unknown" => {
                status = "failed";
                uncertain = true;
            }
            "run_error" | "cancelled" => {
                status = "failed";
                if event["event_type"] == "cancelled" || data["status"] == "cancelled" {
                    cancelled = true;
                    if data["terminal_reason"] == "runtime_restart_interrupted" {
                        reason = Some("runtime_restart_interrupted");
                    }
                }
                if data["terminal_envelope"]["exit_reason"] == "side_effect_unknown" {
                    uncertain = true;
                }
            }
            _ => {}
        }
    }
    if cancelled {
        status = "cancelled";
    }
    if steps.is_empty() && status == "succeeded" && !text.is_empty() {
        return None;
    }
    Some(
        json!({"collapsed":true,"run_id":run_id,"status":status,"outcome_uncertain":uncertain,"terminal_reason":reason,"steps":steps,"tools":[]}),
    )
}
fn project_runtime(
    rows: Vec<RuntimeMessage>,
    sources: &ConversationSources,
    visible: &VisibleSources,
) -> Vec<HistoryMessage> {
    let mut messages = Vec::new();
    for row in rows.into_iter().rev() {
        let raw_events = row
            .runtime_events
            .as_ref()
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let restricted = row.role == "assistant"
            && (sources
                .by_run
                .get(row.run_id.as_deref().unwrap_or(""))
                .is_some_and(|refs| !refs.allowed(visible))
                || raw_events
                    .iter()
                    .filter(|event| event["event_type"] == "context_retrieved")
                    .filter_map(|event| event["data"]["chunks"].as_array())
                    .flatten()
                    .filter_map(|chunk| chunk["dataset_id"].as_str())
                    .any(|id| !visible.datasets.contains_key(id)));
        let text = if restricted {
            String::new()
        } else {
            visible_text(&row.content)
        };
        let has_terminal = raw_events.iter().any(|event| {
            terminal(string(&event["event_type"])) || event["data"]["status"] == "cancelled"
        });
        if text.is_empty() && !(row.role == "assistant" && (has_terminal || restricted)) {
            continue;
        }
        let mut metadata = json!({"runtime_run_id":row.run_id});
        if restricted {
            metadata["source_access_revoked"] = true.into();
        }
        if let Some(sequence) = row.sequence {
            metadata["runtime_sequence"] = sequence.into();
        }
        if row.role == "assistant" {
            if !restricted {
                if let Some(thinking) = row.thinking_content.filter(|s| !s.is_empty()) {
                    metadata["thinking_content"] = thinking.into();
                }
                for (key, value) in [
                    ("tool_calls", row.tool_calls),
                    ("tool_results", row.tool_results),
                ] {
                    if let Some(value) =
                        value.filter(|v| v.as_array().is_some_and(|v| !v.is_empty()))
                    {
                        metadata[key] = value;
                    }
                }
                let contexts = knowledge_contexts(&raw_events, &visible.datasets);
                if !contexts.is_empty() {
                    metadata["contexts"] = contexts.into();
                }
            }
            let events: Vec<_> = raw_events
                .iter()
                .filter(|event| {
                    event.is_object()
                        && event["event_type"] != "context_retrieved"
                        && (!restricted || terminal(string(&event["event_type"])))
                })
                .map(safe_event)
                .collect();
            if !events.is_empty() {
                if let Some(summary) = process_summary(&events, &row.run_id, &text) {
                    metadata["process_summary"] = summary;
                }
                metadata["runtime_events"] = events.into();
            }
        }
        messages.push(HistoryMessage {
            role: row.role,
            content: text,
            timestamp: row.created_at.map(timestamp),
            metadata,
            identity: Value::Null,
        });
    }
    messages
}
fn project_legacy(
    history: &Value,
    sources: &ConversationSources,
    visible: &VisibleSources,
) -> Result<Vec<HistoryMessage>, ()> {
    let mut messages = Vec::new();
    for raw in legacy_array(history).iter().filter(|raw| raw.is_object()) {
        let role = raw
            .get("role")
            .map_or(Some("user"), Value::as_str)
            .ok_or(())?
            .to_owned();
        let content = raw
            .get("content")
            .map_or(Some(""), Value::as_str)
            .ok_or(())?;
        let mut metadata = raw["metadata"].clone();
        if let Some(object) = metadata.as_object_mut() {
            if let Some(contexts) = object.get("contexts").and_then(Value::as_array) {
                if contexts
                    .iter()
                    .filter_map(|context| context["dataset_id"].as_str())
                    .any(|id| !visible.datasets.contains_key(id))
                {
                    metadata = json!({"source_access_revoked":true});
                } else {
                    object.insert(
                        "contexts".into(),
                        contexts
                            .iter()
                            .filter(|context| {
                                context.is_object()
                                    && visible
                                        .datasets
                                        .contains_key(string(&context["dataset_id"]))
                            })
                            .cloned()
                            .collect(),
                    );
                    object.remove("rag_evaluation");
                }
            } else {
                object.remove("rag_evaluation");
            }
        }
        let restricted = role == "assistant"
            && (!sources.legacy.allowed(visible) || metadata["source_access_revoked"] == true);
        if restricted {
            metadata = json!({"source_access_revoked":true});
        }
        // Match SessionHistoryMessage validation after redaction: persisted
        // metadata must be an object or null, never an arbitrary scalar/list.
        if !metadata.is_null() && !metadata.is_object() {
            return Err(());
        }
        let time = match raw.get("timestamp") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(normalize_saved_timestamp(value).ok_or(())?),
            _ => return Err(()),
        };
        messages.push(HistoryMessage {
            role,
            content: if restricted {
                String::new()
            } else {
                content.to_owned()
            },
            timestamp: time,
            metadata,
            identity: raw["metadata"].clone(),
        });
    }
    Ok(messages)
}
fn normalize_saved_timestamp(value: &str) -> Option<String> {
    if let Ok(time) = DateTime::parse_from_rfc3339(value) {
        return Some(time.to_rfc3339_opts(
            if time.timestamp_subsec_micros() == 0 {
                chrono::SecondsFormat::Secs
            } else {
                chrono::SecondsFormat::Micros
            },
            false,
        ));
    }
    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|time| {
            if time.and_utc().timestamp_subsec_micros() == 0 {
                time.format("%Y-%m-%dT%H:%M:%S").to_string()
            } else {
                time.format("%Y-%m-%dT%H:%M:%S%.6f").to_string()
            }
        })
}
#[derive(FromRow)]
struct Upload {
    run_id: String,
    artifact_id: String,
    filename: Option<String>,
    mime_type: Option<String>,
    original_filename: Option<String>,
}
#[derive(FromRow)]
struct Parameters {
    run_id: String,
    reasoning: Option<Value>,
    model_id: Option<String>,
    temperature: Option<Value>,
}
#[derive(FromRow)]
struct Quiz {
    run_id: String,
    quiz_id: Option<String>,
}
fn artifact_id_valid(id: &str) -> bool {
    id.strip_prefix("art_").is_some_and(|suffix| {
        suffix.len() == 16
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn quiz_id_valid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}
async fn enrich_runtime(
    pool: &PgPool,
    messages: &mut [HistoryMessage],
    tenant: &str,
    user: &str,
    thread: &str,
) -> Result<(), sqlx::Error> {
    let users: Vec<String> = messages
        .iter()
        .filter(|m| m.role == "user")
        .filter_map(|m| m.metadata["runtime_run_id"].as_str().map(str::to_owned))
        .collect();
    if !users.is_empty() {
        let uploads: Vec<Upload> = sqlx::query_as(UPLOADS_SQL)
            .bind(thread)
            .bind(tenant)
            .bind(user)
            .bind(&users)
            .fetch_all(pool)
            .await?;
        let mut seen = HashSet::new();
        for upload in uploads {
            if !artifact_id_valid(&upload.artifact_id)
                || !seen.insert((upload.run_id.clone(), upload.artifact_id.clone()))
            {
                continue;
            }
            let attachment = json!({"type":if upload.mime_type.as_deref().unwrap_or("").starts_with("image/") { "image" } else { "file" },"filename":upload.original_filename.filter(|s| !s.is_empty()).or(upload.filename),"url":format!("/api/v1/assistant/artifacts/{}/download",upload.artifact_id)});
            for message in messages
                .iter_mut()
                .filter(|m| m.role == "user" && m.metadata["runtime_run_id"] == upload.run_id)
            {
                let metadata = message.metadata.as_object_mut().expect("projected object");
                metadata
                    .entry("attachments")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .expect("projected array")
                    .push(attachment.clone());
            }
        }
    }
    let assistants: Vec<String> = messages
        .iter()
        .filter(|m| m.role == "assistant" && m.metadata["source_access_revoked"] != true)
        .filter_map(|m| m.metadata["runtime_run_id"].as_str().map(str::to_owned))
        .collect();
    if assistants.is_empty() {
        return Ok(());
    }
    let parameters: Vec<Parameters> = sqlx::query_as(PARAMETERS_SQL)
        .bind(thread)
        .bind(tenant)
        .bind(user)
        .bind(&assistants)
        .fetch_all(pool)
        .await?;
    for parameters in parameters {
        let Some(message) = messages.iter_mut().rev().find(|m| {
            m.role == "assistant"
                && m.metadata["runtime_run_id"] == parameters.run_id
                && m.metadata["source_access_revoked"] != true
        }) else {
            continue;
        };
        let terminal = message.metadata["runtime_events"]
            .as_array()
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| terminal(string(&event["event_type"])))
            });
        let metadata = message.metadata.as_object_mut().expect("projected object");
        let summary = metadata.entry("process_summary").or_insert_with(|| json!({"collapsed":terminal,"run_id":parameters.run_id,"status":if terminal {"succeeded"} else {"running"},"steps":[],"tools":[]}));
        if let Some(reasoning) = parameters.reasoning.filter(Value::is_object) {
            summary["reasoning"] = [
                "requested_option",
                "effective_option",
                "canonical_effort",
                "adapter_id",
                "fallback_reason",
            ]
            .into_iter()
            .map(|key| (key.to_owned(), reasoning[key].clone()))
            .collect::<Map<_, _>>()
            .into();
        }
        if let Some(model) = parameters.model_id {
            summary["model_id"] = model.into();
        }
        if let Some(temperature) = parameters
            .temperature
            .and_then(|v| {
                v.as_f64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
            })
            .filter(|n| n.is_finite() && (0.0..=2.0).contains(n))
        {
            summary["temperature"] = temperature.into();
        }
    }
    // Quiz resolution is advisory in the canonical projection. A missing ledger
    // never prevents restoring an otherwise valid conversation.
    if let Ok(quizzes) = sqlx::query_as::<_, Quiz>(QUIZ_SQL)
        .bind(tenant)
        .bind(user)
        .bind(&assistants)
        .fetch_all(pool)
        .await
    {
        for quiz in quizzes {
            let Some(id) = quiz
                .quiz_id
                .map(|id| id.trim().to_owned())
                .filter(|id| quiz_id_valid(id))
            else {
                continue;
            };
            if let Some(message) = messages.iter_mut().rev().find(|m| {
                m.role == "assistant"
                    && m.metadata["runtime_run_id"] == quiz.run_id
                    && m.metadata["source_access_revoked"] != true
            }) {
                message.metadata["quiz_id"] = id.into();
            }
        }
    }
    Ok(())
}
#[derive(FromRow)]
struct ImageTurn {
    turn_id: String,
    task_id: String,
    prompt: Option<String>,
    status: String,
    created_at: Option<DateTime<Utc>>,
    task_status: String,
    result: Option<Value>,
}
#[derive(FromRow)]
struct ImageArtifact {
    artifact_id: String,
    turn_id: String,
    variant: String,
}
fn source_refs_at(sources: &ConversationSources, at: Option<DateTime<Utc>>) -> SourceReferences {
    let Some(at) = at.filter(|_| sources.dates_complete) else {
        return sources.all.clone();
    };
    let mut references = sources.legacy.clone();
    for (admitted, inherited) in &sources.dated_runs {
        if *admitted <= at {
            references.datasets = inherited.datasets.clone();
        }
    }
    for (created, refs) in &sources.dated_contexts {
        if *created <= at {
            references.documents.extend(refs.documents.iter().cloned());
            references.versions.extend(refs.versions.iter().cloned());
        }
    }
    references
}
fn date(value: Option<&str>) -> Option<DateTime<Utc>> {
    let value = value?;
    DateTime::parse_from_rfc3339(value)
        .map(|time| time.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|time| time.and_utc())
        })
}
fn url_component(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
fn image_matches(message: &HistoryMessage, turn: &ImageTurn, family: &HashSet<String>) -> bool {
    if message.role != "assistant" {
        return false;
    }
    let mut metadata = message.identity.as_object().cloned().unwrap_or_default();
    if let Some(current) = message.metadata.as_object() {
        metadata.extend(current.clone());
    }
    metadata
        .get("image_turn_id")
        .is_some_and(|id| id == &turn.turn_id)
        || metadata
            .get("image_task_id")
            .is_some_and(|id| id == &turn.task_id)
        || metadata
            .get("process_summary")
            .is_some_and(|summary| summary["diagnostic_id"] == turn.task_id)
        || metadata
            .get("artifact_ids")
            .and_then(Value::as_array)
            .is_some_and(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .any(|id| family.contains(id))
            })
}
fn merge_images(
    messages: &mut Vec<HistoryMessage>,
    turns: Vec<ImageTurn>,
    artifacts: &[ImageArtifact],
    sources: &ConversationSources,
    visible: &VisibleSources,
) {
    for turn in turns {
        let family: Vec<_> = artifacts
            .iter()
            .filter(|artifact| artifact.turn_id == turn.turn_id)
            .collect();
        let mut ids: HashSet<_> = family
            .iter()
            .map(|artifact| artifact.artifact_id.clone())
            .collect();
        let result = turn.result.as_ref().unwrap_or(&Value::Null);
        if let Some(images) = result["images"].as_array() {
            ids.extend(
                images
                    .iter()
                    .filter_map(|image| image["artifact_id"].as_str().map(str::to_owned)),
            );
        }
        let index = messages
            .iter()
            .position(|message| image_matches(message, &turn, &ids));
        let mut public: Vec<_> = family
            .iter()
            .filter(|artifact| artifact.variant == "display")
            .copied()
            .collect();
        if public.is_empty() {
            public = family
                .iter()
                .filter(|artifact| artifact.variant == "raw")
                .copied()
                .collect();
        }
        let mut status = if matches!(
            turn.task_status.as_str(),
            "completed" | "failed" | "cancelled" | "unknown"
        ) {
            turn.task_status.as_str()
        } else {
            turn.status.as_str()
        };
        if !public.is_empty() && turn.status == "completed" {
            status = "completed";
        }
        if status == "completed" && public.is_empty() {
            status = "unknown";
        }
        let running = matches!(status, "pending" | "queued" | "running");
        let restricted = !source_refs_at(sources, turn.created_at).allowed(visible);
        let old = index.map(|i| &messages[i]);
        let mut content = old.map(|m| m.content.clone()).unwrap_or_default();
        let mut metadata = json!({"source_kind":"image_generation","image_turn_id":turn.turn_id,"image_task_id":turn.task_id,"image_generating":running,"image_generation_prompt":if running && !restricted {turn.prompt.clone()} else {None},"process_summary":{"status":if running {"running"} else if status=="completed" {"succeeded"} else if status=="cancelled" {"cancelled"} else {"failed"},"outcome_uncertain":status=="unknown","diagnostic_id":turn.task_id,"collapsed":true,"steps":[],"tools":[]}});
        if restricted {
            content.clear();
            metadata["source_access_revoked"] = true.into();
        } else if !public.is_empty() {
            metadata["artifact_ids"] = public
                .iter()
                .map(|artifact| Value::String(artifact.artifact_id.clone()))
                .collect();
            if content.is_empty() {
                content = public
                    .iter()
                    .map(|artifact| {
                        format!(
                            "![Generated image](/api/v1/assistant/artifacts/{}/download)",
                            url_component(&artifact.artifact_id)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n");
            }
            if status == "completed" && !result["effective_model_id"].is_null() {
                metadata["model_id"] = result["effective_model_id"].clone();
            }
        }
        if !restricted {
            let mut combined = old
                .and_then(|old| old.metadata.as_object())
                .cloned()
                .unwrap_or_default();
            combined.extend(
                metadata
                    .as_object()
                    .expect("image projection object")
                    .clone(),
            );
            metadata = combined.into();
        }
        let message = HistoryMessage {
            role: "assistant".into(),
            content,
            timestamp: old
                .and_then(|m| m.timestamp.clone())
                .or_else(|| turn.created_at.map(timestamp)),
            metadata,
            identity: Value::Null,
        };
        if let Some(index) = index {
            messages[index] = message;
        } else {
            let created = date(message.timestamp.as_deref());
            let index = messages
                .iter()
                .position(|message| {
                    created
                        .zip(date(message.timestamp.as_deref()))
                        .is_some_and(|(created, existing)| existing > created)
                })
                .unwrap_or(messages.len());
            messages.insert(index, message);
        }
    }
}
#[allow(
    clippy::too_many_arguments,
    reason = "Keeps canonical history enrichment inputs explicit without adding shared mutable context"
)]
async fn enrich_images(
    pool: &PgPool,
    messages: &mut Vec<HistoryMessage>,
    tenant: &str,
    user: &str,
    session_id: &str,
    sources: &ConversationSources,
    visible: &VisibleSources,
    limit: i64,
) -> Result<(), sqlx::Error> {
    if !messages.iter().any(|m| {
        m.metadata["source_kind"] == "image_generation"
            || (m.role == "user" && m.content.starts_with('🎨'))
    }) {
        return Ok(());
    }
    let mut turns: Vec<ImageTurn> = sqlx::query_as("SELECT t.turn_id,t.task_id,t.prompt,t.status,t.created_at,task.status AS task_status,task.result FROM assistant.image_turns t JOIN assistant.image_tasks task ON task.task_id=t.task_id WHERE t.session_id=$1 AND task.tenant_id=$2 AND task.user_id=$3 ORDER BY t.created_at DESC,t.turn_id DESC LIMIT $4")
        .bind(session_id).bind(tenant).bind(user).bind(limit).fetch_all(pool).await?;
    if turns.is_empty() {
        return Ok(());
    }
    let ids: Vec<_> = turns.iter().map(|turn| turn.turn_id.clone()).collect();
    let artifacts: Vec<ImageArtifact> = sqlx::query_as("SELECT artifact_id,turn_id,variant FROM assistant.artifacts WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND turn_id=ANY($4::text[]) AND size_bytes>0 AND variant IN ('raw','display')")
        .bind(session_id).bind(tenant).bind(user).bind(ids).fetch_all(pool).await?;
    turns.reverse();
    merge_images(messages, turns, &artifacts, sources, visible);
    Ok(())
}
async fn history(
    State(state): State<Sessions>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
    query: Result<Query<HistoryLimit>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let limit = match query {
        Ok(Query(HistoryLimit { limit })) if (1..=contract::HISTORY_MAX).contains(&limit) => limit,
        _ => return error(StatusCode::UNPROCESSABLE_ENTITY, "Invalid limit"),
    };
    let principal = match identity(&state, &headers).await {
        Ok(v) => v,
        Err(e) => return e,
    };
    let (tenant, user) = (&principal.account.tenant_id, &principal.account.user_id);
    let row = match session(&state, &session_id, tenant, user).await {
        Ok(Some(row)) => row,
        Ok(None) => return error(StatusCode::NOT_FOUND, "Session not found"),
        Err(_) => {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to get session history",
            );
        }
    };
    match read_history(&state, &principal, row, limit).await {
        Ok(response) => Json(response).into_response(),
        Err(HistoryError::SourceUnavailable) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"detail":{"code":"ASSISTANT_SOURCE_CHECK_UNAVAILABLE"}})),
        )
            .into_response(),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to get session history",
        ),
    }
}
enum HistoryError {
    Database,
    Projection,
    SourceUnavailable,
}
impl From<sqlx::Error> for HistoryError {
    fn from(_: sqlx::Error) -> Self {
        Self::Database
    }
}
async fn read_history(
    state: &Sessions,
    principal: &Principal,
    row: SessionRow,
    limit: i64,
) -> Result<Value, HistoryError> {
    let (tenant, user, session_id) = (
        &principal.account.tenant_id,
        &principal.account.user_id,
        &row.session_id,
    );
    let snapshots: Vec<Snapshot> = sqlx::query_as("SELECT run_id::text AS run_id,snapshot,created_at FROM gateway.assistant_runtime_snapshots WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 ORDER BY created_at,snapshot_id")
        .bind(session_id).bind(tenant).bind(user).fetch_all(&state.pool).await?;
    let contexts: Vec<ContextEvent> = sqlx::query_as("SELECT turn_id::text AS run_id,payload #> '{data,chunks}' AS chunks,created_at FROM gateway.assistant_runtime_items WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND event_type='compat/v1/context_retrieved' ORDER BY sequence")
        .bind(session_id).bind(tenant).bind(user).fetch_all(&state.pool).await?;
    let sources = source_map(&row.history, &snapshots, &contexts)
        .map_err(|_| HistoryError::SourceUnavailable)?;
    let mut visible = match &state.config.source_access {
        Some(access) => access.visible_sources(principal, &sources.all).await,
        None => VisibleSources::default(),
    };
    // A collaborator returning references outside its request is not trusted.
    if !visible.documents.is_subset(&sources.all.documents) {
        visible.documents.clear();
    }
    if !visible.versions.is_subset(&sources.all.versions) {
        visible.versions.clear();
    }
    let mut messages =
        project_legacy(&row.history, &sources, &visible).map_err(|_| HistoryError::Projection)?;
    let thread: Option<String> = sqlx::query_scalar("SELECT runtime_thread_id::text FROM gateway.assistant_runtime_threads WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND deleted_at IS NULL")
        .bind(tenant).bind(user).bind(session_id).fetch_optional(&state.pool).await?;
    let mut runtime_total = 0;
    let mut projected_count = 0;
    if let Some(thread) = thread {
        let rows: Vec<RuntimeMessage> = sqlx::query_as(RUNTIME_HISTORY_SQL)
            .bind(&thread)
            .bind(tenant)
            .bind(user)
            .bind(limit)
            .fetch_all(&state.pool)
            .await?;
        runtime_total = rows.first().map(|row| row.total).unwrap_or(0);
        let mut runtime = project_runtime(rows, &sources, &visible);
        enrich_runtime(&state.pool, &mut runtime, tenant, user, &thread).await?;
        projected_count = runtime.len();
        messages.extend(runtime);
    }
    enrich_images(
        &state.pool,
        &mut messages,
        tenant,
        user,
        session_id,
        &sources,
        &visible,
        limit,
    )
    .await?;
    let total =
        messages.len() + runtime_total.saturating_sub(projected_count as i64).max(0) as usize;
    if messages.len() > limit as usize {
        messages.drain(..messages.len() - limit as usize);
    }
    Ok(contract::history_response(session_id, messages, total))
}
/// Reuse the signed Knowledge proxy; retain each Python authorization helper's
/// fail-closed behavior independently when its downstream call fails.
impl HistorySourceAccess for super::knowledge::KnowledgeService {
    fn visible_sources<'a>(
        &'a self,
        principal: &'a Principal,
        references: &'a SourceReferences,
    ) -> Pin<Box<dyn Future<Output = VisibleSources> + Send + 'a>> {
        Box::pin(async move {
            use super::knowledge::{DocumentSourceReference, KnowledgeActor};
            let Ok(actor) = KnowledgeActor::from_principal(principal) else {
                return VisibleSources::default();
            };
            let mut visible = VisibleSources::default();
            if let Ok(datasets) = self.list_datasets(&actor).await {
                for dataset in datasets {
                    if let Some(id) = dataset["dataset_id"].as_str().filter(|id| !id.is_empty()) {
                        let name = dataset["name"]
                            .as_str()
                            .filter(|name| !name.is_empty())
                            .unwrap_or(id);
                        visible.datasets.insert(id.to_owned(), name.to_owned());
                    }
                }
            }
            let mut documents: BTreeMap<String, Vec<String>> = BTreeMap::new();
            for (dataset, document) in &references.documents {
                if !dataset.is_empty() && !document.is_empty() {
                    documents
                        .entry(dataset.clone())
                        .or_default()
                        .push(document.clone());
                }
            }
            for (dataset, documents) in documents {
                match self.authorize_documents(&actor, &dataset, &documents).await {
                    Ok(allowed) => visible
                        .documents
                        .extend(allowed.into_iter().map(|id| (dataset.clone(), id))),
                    Err(_) => {
                        visible.documents.clear();
                        break;
                    }
                }
            }
            let mut versions: BTreeMap<String, Vec<DocumentSourceReference>> = BTreeMap::new();
            for (dataset, document, version, hash) in &references.versions {
                if !dataset.is_empty() && !document.is_empty() && *version > 0 && !hash.is_empty() {
                    versions
                        .entry(dataset.clone())
                        .or_default()
                        .push(DocumentSourceReference {
                            document_id: document.clone(),
                            source_version: *version,
                            source_hash: hash.clone(),
                        });
                }
            }
            for (dataset, versions) in versions {
                match self
                    .authorize_document_sources(&actor, &dataset, &versions)
                    .await
                {
                    Ok(allowed) => visible
                        .versions
                        .extend(allowed.into_iter().map(|reference| {
                            (
                                dataset.clone(),
                                reference.document_id,
                                reference.source_version,
                                reference.source_hash,
                            )
                        })),
                    Err(_) => {
                        visible.versions.clear();
                        break;
                    }
                }
            }
            visible
        })
    }
}

/// Reuse the canonical Runtime history projection for authorized share freezing.
/// The caller supplies current source authority; stored references never grant access.
pub async fn agent_runtime_history(
    pool: &PgPool,
    scope: &crate::migration_agent::Scope,
    access: &crate::migration_agent::entry::PgSourceAccess,
    limit: usize,
) -> Result<Option<(Vec<Value>, usize)>, crate::migration_agent::AgentError> {
    use crate::migration_agent::{AgentError, entry::SourceRefs};
    let session: Option<SessionRow> = sqlx::query_as(SESSION_SELECT)
        .bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id)
        .fetch_optional(pool).await?;
    let Some(session) = session else { return Ok(None) };
    let thread: Option<String> = sqlx::query_scalar("SELECT runtime_thread_id::text FROM gateway.assistant_runtime_threads WHERE tenant_id=$1 AND user_id=$2 AND session_id=$3 AND deleted_at IS NULL")
        .bind(&scope.tenant_id).bind(&scope.user_id).bind(&scope.session_id)
        .fetch_optional(pool).await?;
    let Some(thread) = thread else { return Ok(None) };
    let snapshots: Vec<Snapshot> = sqlx::query_as("SELECT run_id::text AS run_id,snapshot,created_at FROM gateway.assistant_runtime_snapshots WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 ORDER BY created_at,snapshot_id")
        .bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).fetch_all(pool).await?;
    let contexts: Vec<ContextEvent> = sqlx::query_as("SELECT turn_id::text AS run_id,payload #> '{data,chunks}' AS chunks,created_at FROM gateway.assistant_runtime_items WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND event_type='compat/v1/context_retrieved' ORDER BY sequence")
        .bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).fetch_all(pool).await?;
    let sources = source_map(&session.history, &snapshots, &contexts)
        .map_err(|_| AgentError::code(503,"ASSISTANT_SOURCE_CHECK_UNAVAILABLE"))?;
    let mut visible = VisibleSources::default();
    for dataset in &sources.all.datasets {
        let refs = SourceRefs { datasets: [dataset.clone()].into_iter().collect(), ..Default::default() };
        if access.allowed(scope,&refs).await? { visible.datasets.insert(dataset.clone(),dataset.clone()); }
    }
    for document in &sources.all.documents {
        let refs = SourceRefs { documents: [document.clone()].into_iter().collect(), ..Default::default() };
        if access.allowed(scope,&refs).await? { visible.documents.insert(document.clone()); }
    }
    for version in &sources.all.versions {
        let refs = SourceRefs { versions: [version.clone()].into_iter().collect(), ..Default::default() };
        if access.allowed(scope,&refs).await? { visible.versions.insert(version.clone()); }
    }
    let rows: Vec<RuntimeMessage> = sqlx::query_as(RUNTIME_HISTORY_SQL)
        .bind(&thread).bind(&scope.tenant_id).bind(&scope.user_id)
        .bind(limit.clamp(1,500) as i64).fetch_all(pool).await?;
    let total = rows.first().map(|row| row.total.max(0) as usize).unwrap_or(0);
    let mut messages = project_runtime(rows,&sources,&visible);
    enrich_runtime(pool,&mut messages,&scope.tenant_id,&scope.user_id,&thread).await
        .map_err(|_| AgentError::code(503,"AGENT_HISTORY_UNAVAILABLE"))?;
    let messages = messages.into_iter().map(serde_json::to_value).collect::<Result<Vec<_>,_>>()
        .map_err(|_| AgentError::code(503,"AGENT_HISTORY_UNAVAILABLE"))?;
    Ok(Some((messages,total)))
}
