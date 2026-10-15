//! Shared Assistant entry provenance, model access, memory preference and launch.
use super::{Actor, AgentError, AgentFuture, Scope, spec, text};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
pub type DocumentRef = (String, String);
pub type SourceVersionRef = (String, String, i64, String);
#[derive(Clone, Default)]
pub struct SourceRefs {
    pub datasets: BTreeSet<String>,
    pub documents: BTreeSet<DocumentRef>,
    pub versions: BTreeSet<SourceVersionRef>,
}
impl SourceRefs {
    fn extend(&mut self, other: &Self) {
        self.datasets.extend(other.datasets.iter().cloned());
        self.documents.extend(other.documents.iter().cloned());
        self.versions.extend(other.versions.iter().cloned());
        self.datasets.extend(
            self.documents
                .iter()
                .filter(|(d, _)| !d.is_empty())
                .map(|(d, _)| d.clone()),
        );
    }
    fn empty(&self) -> bool {
        self.datasets.is_empty() && self.documents.is_empty() && self.versions.is_empty()
    }
}
#[derive(Clone, Default)]
pub struct ConversationSources {
    pub all: SourceRefs,
    pub legacy: SourceRefs,
    pub by_run: BTreeMap<String, SourceRefs>,
    pub dated_runs: Vec<(DateTime<Utc>, SourceRefs)>,
    pub dated_contexts: Vec<(DateTime<Utc>, SourceRefs)>,
    pub dates_complete: bool,
}
/// Current resource authorities, never inferred from old snapshots or tenant match.
pub trait SourceAuthority: Send + Sync {
    fn allowed<'a>(&'a self, scope: &'a Scope, refs: &'a SourceRefs) -> AgentFuture<'a, bool>;
}
pub trait SourceAccess: Send + Sync {
    fn require_conversation<'a>(
        &'a self,
        scope: &'a Scope,
        for_execution: bool,
    ) -> AgentFuture<'a, ()>;
    fn require_artifact<'a>(&'a self, scope: &'a Scope, artifact: &'a Value)
    -> AgentFuture<'a, ()>;
    fn project_event<'a>(&'a self, scope: &'a Scope, event: Value) -> AgentFuture<'a, Value>;
}
#[derive(Clone)]
pub struct PgSourceAccess {
    pub pool: PgPool,
    pub authority: Arc<dyn SourceAuthority>,
}
fn unknown(refs: &mut SourceRefs) {
    refs.documents.insert((String::new(), String::new()));
    refs.versions
        .insert((String::new(), String::new(), 0, String::new()));
}
fn parsed(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|v| serde_json::from_str(v).ok())
        .unwrap_or_else(|| value.clone())
}
fn date(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
}
pub fn context_references(chunks: &Value, default_dataset: &str) -> SourceRefs {
    let mut refs = SourceRefs::default();
    let Some(chunks) = chunks.as_array() else {
        unknown(&mut refs);
        return refs;
    };
    for chunk in chunks {
        if !chunk.is_object() {
            unknown(&mut refs);
            continue;
        }
        let metadata = &chunk["metadata"];
        let dataset = chunk["dataset_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| metadata["dataset_id"].as_str())
            .unwrap_or(default_dataset)
            .trim();
        let document = chunk["document_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| metadata["document_id"].as_str())
            .unwrap_or("")
            .trim();
        if ["dataset_id", "document_id"]
            .iter()
            .any(|k| !chunk[*k].is_null() && !metadata[*k].is_null() && chunk[*k] != metadata[*k])
        {
            unknown(&mut refs);
            continue;
        }
        refs.documents.insert((dataset.into(), document.into()));
        if !dataset.is_empty() {
            refs.datasets.insert(dataset.into());
        }
        let tv = &chunk["source_version"];
        let th = &chunk["source_hash"];
        let mv = &metadata["source_version"];
        let mh = &metadata["source_hash"];
        let has = |v: &Value| !v.is_null();
        if has(tv) != has(th)
            || has(mv) != has(mh)
            || [tv, mv].iter().any(|v| has(v) && v.as_i64().is_none())
            || [th, mh]
                .iter()
                .any(|v| has(v) && v.as_str().is_none_or(|s| s != s.trim()))
        {
            refs.versions
                .insert((String::new(), String::new(), 0, String::new()));
            continue;
        }
        if !has(tv) && !has(mv) {
            continue;
        }
        let version = if has(tv) { tv } else { mv };
        let hash = if has(th) { th } else { mh };
        let h = hash.as_str().unwrap_or("").trim().to_lowercase();
        let n = version.as_i64().unwrap_or(0);
        if n <= 0
            || h.len() != 64
            || !h.bytes().all(|b| b.is_ascii_hexdigit())
            || dataset.is_empty()
            || document.is_empty()
            || (has(tv) && has(mv) && tv != mv)
            || (has(th)
                && has(mh)
                && th.as_str().unwrap_or("").to_lowercase()
                    != mh.as_str().unwrap_or("").to_lowercase())
        {
            refs.versions
                .insert((String::new(), String::new(), 0, String::new()));
        } else {
            refs.versions
                .insert((dataset.into(), document.into(), n, h));
        }
    }
    refs
}
impl PgSourceAccess {
    pub fn new(pool: PgPool, authority: Arc<dyn SourceAuthority>) -> Self {
        Self { pool, authority }
    }
    pub async fn conversation_sources(
        &self,
        scope: &Scope,
    ) -> Result<ConversationSources, AgentError> {
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('run_id',run_id::text,'snapshot',snapshot,'created_at',created_at) FROM assistant_runtime_snapshots WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 ORDER BY created_at,snapshot_id").bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).fetch_all(&self.pool).await?;
        let contexts:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('run_id',turn_id::text,'chunks',payload#>'{data,chunks}','created_at',created_at) FROM assistant_runtime_items WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3 AND event_type='compat/v1/context_retrieved' ORDER BY sequence").bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).fetch_all(&self.pool).await?;
        let history:Option<Value>=sqlx::query_scalar("SELECT history FROM assistant.sessions WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3").bind(&scope.session_id).bind(&scope.tenant_id).bind(&scope.user_id).fetch_optional(&self.pool).await?;
        let mut legacy = SourceRefs::default();
        let h = parsed(&history.unwrap_or(json!([])));
        let messages = if h.is_object() {
            h["messages"].as_array()
        } else {
            h.as_array()
        };
        for message in messages.into_iter().flatten() {
            let Some(c) = message["metadata"].get("contexts") else {
                continue;
            };
            let Some(cs) = c.as_array() else {
                unknown(&mut legacy);
                continue;
            };
            for c in cs {
                if !c.is_object() {
                    unknown(&mut legacy);
                    continue;
                }
                let dataset = text(c, "dataset_id");
                if !dataset.is_empty() {
                    legacy.datasets.insert(dataset.into());
                }
                legacy.extend(&context_references(&c["chunks"], dataset));
            }
        }
        let mut by_event: BTreeMap<String, SourceRefs> = BTreeMap::new();
        let mut timed = Vec::new();
        let mut baseline = legacy.clone();
        let mut dated_contexts = Vec::new();
        for c in &contexts {
            let refs = context_references(&parsed(&c["chunks"]), "");
            let run = text(c, "run_id").to_owned();
            by_event.entry(run.clone()).or_default().extend(&refs);
            if run.is_empty() {
                baseline.extend(&refs);
            }
            if let Some(at) = date(&c["created_at"]) {
                timed.push((at, run, refs.clone()));
                dated_contexts.push((at, refs));
            } else {
                baseline.extend(&refs);
            }
        }
        let mut inherited = legacy.datasets.clone();
        let mut by_run = BTreeMap::new();
        let mut dated_runs = Vec::new();
        for row in &rows {
            let snapshot = parsed(&row["snapshot"]);
            let items = snapshot["readonly_capabilities"]["items"]
                .as_array()
                .ok_or_else(|| AgentError::code(503, "ASSISTANT_SOURCE_CHECK_UNAVAILABLE"))?;
            for item in items {
                if item["kind"] == "knowledge" {
                    let id = text(&item["payload"], "dataset_id");
                    if id.is_empty() {
                        return Err(AgentError::code(503, "ASSISTANT_SOURCE_CHECK_UNAVAILABLE"));
                    }
                    inherited.insert(id.into());
                }
            }
            let run = text(row, "run_id");
            let at = date(&row["created_at"]);
            let mut refs = baseline.clone();
            for (t, _, r) in &timed {
                if at.is_none_or(|at| *t <= at) {
                    refs.extend(r);
                }
            }
            if at.is_none() {
                for r in by_event.values() {
                    refs.extend(r);
                }
            }
            if let Some(r) = by_event.get(run) {
                refs.extend(r);
            }
            refs.datasets.extend(inherited.iter().cloned());
            if !run.is_empty() {
                by_run.insert(run.into(), refs.clone());
            }
            if let Some(at) = at {
                dated_runs.push((at, refs));
            }
        }
        for (run, own) in &by_event {
            if run.is_empty() || by_run.contains_key(run) {
                continue;
            }
            let cutoff = timed
                .iter()
                .filter(|(_, r, _)| r == run)
                .map(|(t, _, _)| *t)
                .max();
            let mut refs = baseline.clone();
            for (t, _, r) in &timed {
                if cutoff.is_none_or(|c| *t <= c) {
                    refs.extend(r);
                }
            }
            if cutoff.is_none() {
                for r in by_event.values() {
                    refs.extend(r);
                }
            }
            refs.extend(own);
            let prior = cutoff
                .and_then(|c| {
                    dated_runs
                        .iter()
                        .filter(|(t, _)| *t <= c)
                        .last()
                        .map(|(_, r)| r.datasets.clone())
                })
                .unwrap_or_else(|| {
                    if cutoff.is_some() {
                        legacy.datasets.clone()
                    } else {
                        inherited.clone()
                    }
                });
            refs.datasets.extend(prior);
            by_run.insert(run.clone(), refs);
        }
        let mut all = baseline.clone();
        all.datasets.extend(inherited);
        for refs in by_event.values() {
            all.extend(refs);
        }
        let complete = dated_runs.len() == rows.len() && dated_contexts.len() == contexts.len();
        Ok(ConversationSources {
            all,
            legacy: baseline,
            by_run,
            dated_runs,
            dated_contexts,
            dates_complete: complete,
        })
    }
    pub async fn allowed(&self, scope: &Scope, refs: &SourceRefs) -> Result<bool, AgentError> {
        if refs.empty() {
            return Ok(true);
        }
        self.authority.allowed(scope, refs).await
    }
    pub async fn guard(
        self: Arc<Self>,
        scope: Scope,
    ) -> Result<ConversationEventGuard, AgentError> {
        let sources = self.conversation_sources(&scope).await?;
        Ok(ConversationEventGuard {
            access: self,
            scope,
            sources,
            observed: BTreeMap::new(),
            last_sequence: None,
            revoked: false,
        })
    }
}
impl SourceAccess for PgSourceAccess {
    fn require_conversation<'a>(
        &'a self,
        scope: &'a Scope,
        for_execution: bool,
    ) -> AgentFuture<'a, ()> {
        Box::pin(async move {
            let sources = self.conversation_sources(scope).await?;
            if self.allowed(scope, &sources.all).await? {
                Ok(())
            } else {
                Err(AgentError::code(
                    if for_execution { 409 } else { 403 },
                    "ASSISTANT_SOURCE_ACCESS_REVOKED",
                ))
            }
        })
    }
    fn require_artifact<'a>(
        &'a self,
        scope: &'a Scope,
        artifact: &'a Value,
    ) -> AgentFuture<'a, ()> {
        Box::pin(async move {
            if artifact["source"] == "user" {
                return Ok(());
            }
            let sources = self.conversation_sources(scope).await?;
            let refs = if let Some(at) =
                date(&artifact["created_at"]).filter(|_| sources.dates_complete)
            {
                let mut r = sources.legacy.clone();
                for (t, v) in &sources.dated_runs {
                    if *t <= at {
                        r.datasets = v.datasets.clone();
                    }
                }
                for (t, v) in &sources.dated_contexts {
                    if *t <= at {
                        r.extend(v);
                    }
                }
                r
            } else {
                sources.all
            };
            if self.allowed(scope, &refs).await? {
                Ok(())
            } else {
                Err(AgentError::code(403, "ASSISTANT_SOURCE_ACCESS_REVOKED"))
            }
        })
    }
    fn project_event<'a>(&'a self, scope: &'a Scope, event: Value) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            let sources = self.conversation_sources(scope).await?;
            let mut refs = sources
                .by_run
                .get(text(&event["data"], "run_id"))
                .cloned()
                .unwrap_or(sources.all);
            if event["event_type"] == "context_retrieved" {
                refs.extend(&context_references(&event["data"]["chunks"], ""));
            }
            if self.allowed(scope, &refs).await? {
                Ok(event)
            } else {
                Ok(redact_event(&event))
            }
        })
    }
}
pub struct ConversationEventGuard {
    access: Arc<PgSourceAccess>,
    scope: Scope,
    sources: ConversationSources,
    observed: BTreeMap<String, SourceRefs>,
    last_sequence: Option<i64>,
    revoked: bool,
}
impl ConversationEventGuard {
    pub async fn project(&mut self, event: Value) -> Result<Value, AgentError> {
        let run = text(&event["data"], "run_id").to_owned();
        let sequence = event["sequence"].as_i64();
        let gap = sequence.is_none()
            || self.last_sequence.is_none()
            || sequence != self.last_sequence.and_then(|v| v.checked_add(1));
        self.last_sequence = sequence;
        if gap
            || (!run.is_empty() && !self.sources.by_run.contains_key(&run))
            || event["event_type"] == "context_retrieved"
        {
            self.sources = self.access.conversation_sources(&self.scope).await?;
        }
        if event["event_type"] == "context_retrieved" {
            self.observed
                .entry(run.clone())
                .or_default()
                .extend(&context_references(&event["data"]["chunks"], ""));
        }
        let mut refs = self
            .sources
            .by_run
            .get(&run)
            .cloned()
            .unwrap_or_else(|| self.sources.all.clone());
        if let Some(r) = self.observed.get(&run) {
            refs.extend(r);
        }
        if !refs.empty() {
            self.revoked = self.revoked || !self.access.allowed(&self.scope, &refs).await?;
        }
        Ok(if self.revoked {
            redact_event(&event)
        } else {
            event
        })
    }
}
pub fn redact_event(event: &Value) -> Value {
    let kind = text(event, "event_type");
    let data = &event["data"];
    let mut safe = if matches!(
        kind,
        "run_finished" | "run_error" | "cancelled" | "side_effect_unknown"
    ) {
        super::threads::safe_history_event(event)["data"].clone()
    } else {
        json!({})
    };
    for k in ["run_id", "tool_call_id", "approval_id"] {
        if data[k].is_string() {
            safe[k] = data[k].clone();
        }
    }
    if matches!(
        text(data, "status"),
        "pending" | "approved" | "rejected" | "expired" | "cancelled"
    ) {
        safe["status"] = data["status"].clone();
    }
    if data["approved"].is_boolean() {
        safe["approved"] = data["approved"].clone();
    }
    safe["source_access_revoked"] = true.into();
    json!({"sequence":event["sequence"],"timestamp":event["timestamp"],"event_id":event["event_id"],"event_key":event["event_key"],"event_type":if matches!(kind,"run_started"|"approval_required"|"approval_result"|"run_finished"|"run_error"|"cancelled"|"side_effect_unknown"){kind}else{"source_access_revoked"},"data":safe})
}
pub async fn memory_mode(
    pool: &PgPool,
    scope: &Scope,
    requested: &str,
) -> Result<String, AgentError> {
    let row: Option<Value> = sqlx::query_scalar(
        "SELECT value FROM user_memory WHERE tenant_id=$1 AND user_id=$2 AND key=$3",
    )
    .bind(&scope.tenant_id)
    .bind(&scope.user_id)
    .bind("__assistant_memory_control__")
    .fetch_optional(pool)
    .await
    .map_err(|_| AgentError::code(503, "Memory preference is unavailable"))?;
    // sqlx already decoded JSONB. Only an actual JSON boolean can enable
    // memory; parsing a string again would disagree with the settings API.
    Ok(if row.as_ref().is_none_or(|v| v == &Value::Bool(true)) {
        requested
    } else {
        "off"
    }
    .into())
}
pub fn user_can_access_model(actor: &Actor, access: &str) -> bool {
    match access {
        "public" => true,
        "premium" => {
            actor.is_admin()
                || matches!(actor.tier.to_lowercase().as_str(), "premium" | "enterprise")
        }
        "admin" => actor.is_admin(),
        _ => false,
    }
}
/// Every launch is resolved by server-side model/capability authorities before
/// this pure constructor. No user-provided provider/profile becomes authority.
pub fn builtin_launch(
    scope: &Scope,
    model: &Value,
    profile: &Value,
    readonly: &Value,
    policy: &Value,
    instructions: &str,
) -> Value {
    let mut datasets: BTreeSet<String> = BTreeSet::new();
    for id in readonly["knowledge"]["dataset_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !id.is_empty() {
            datasets.insert(id.into());
        }
    }
    let kb = json!({"datasets":datasets,"retrieval":{"mode":readonly["knowledge"]["mode"].as_str().unwrap_or("off"),"top_k":readonly["knowledge"]["top_k"].as_i64().unwrap_or(5),"threshold":readonly["knowledge"]["score_threshold"].as_f64().unwrap_or(0.4)}});
    let mode = policy["memory_mode"].as_str().unwrap_or("auto");
    let agent_spec = json!({"agentId":"__builtin_assistant__","agentVersionId":null,"channel":"builtin","developerInstructions":instructions,"model":model,"knowledge":kb,"capabilities":[],"memory":{"mode":match mode{"off"=>"off","user"=>"user",_=>"session"}}});
    json!({"schema_version":"resolved-agent-launch/v1","identity":{"tenant_id":scope.tenant_id,"user_id":scope.user_id,"session_id":scope.session_id,"agent_id":"__builtin_assistant__","agent_version_id":null,"draft_revision":null,"publication_id":null,"channel":"builtin","entrypoint":"assistant","auth_mode":"private"},"agent_spec":agent_spec,"model":model,"model_profile":profile,"capability_bindings":[],"knowledge_bindings":kb,"memory_policy":{"mode":mode,"profile":policy["memory_profile"].as_str().unwrap_or("basic")},"channel_policy":{"attachments":true,"high_risk_tools":true,"allowed_origins":[]},"runtime_inputs":{"readonly_capabilities":readonly},"turn_policy":policy,"fingerprints":{"spec":format!("sha256:{}",spec::hash(&agent_spec)),"tool_schema":format!("sha256:{}",spec::hash(&json!([]))),"skills":format!("sha256:{}",spec::hash(&json!([]))),"knowledge_revision":format!("sha256:{}",spec::hash(&kb))}})
}
