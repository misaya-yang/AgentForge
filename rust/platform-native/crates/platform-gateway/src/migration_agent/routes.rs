//! Reachable V1 Studio/Assistant and V2 Thread/Turn/approval/SSE route factory.
use super::{
    Actor, AgentError, AgentFuture, Scope, approval,
    control::{AgentTurn, EventReceiver, RuntimeControlPlane, TurnRequest},
    entry::{PgSourceAccess, SourceAccess},
    spec,
    studio::{AgentRepository, ListAgents},
    text,
};
use crate::{
    session_manager::{CreateSession, SessionError},
    sessions_api::SessionsApiManager,
};
use ai_platform_auth::admission::AdmissionService;
use axum::{
    Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_core::Stream;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
pub trait AgentAttachments: Send + Sync {
    fn bind<'a>(
        &'a self,
        actor: &'a Actor,
        scope: &'a Scope,
        model: &'a str,
        provider: Option<&'a str>,
        refs: &'a [String],
    ) -> AgentFuture<'a, (Vec<String>, Vec<String>)>;
}
#[derive(Clone)]
pub struct AgentState {
    pub auth: Arc<AdmissionService>,
    pub repository: Arc<AgentRepository>,
    pub control: Arc<RuntimeControlPlane>,
    pub sessions: Arc<dyn SessionsApiManager>,
    pub sources: Arc<PgSourceAccess>,
    pub attachments: Arc<dyn AgentAttachments>,
    pub traces: Arc<crate::migration_trace::TraceRepository>,
    pub edge_rate: Arc<dyn super::entry_request::AgentRateLimit>,
    pub cleanup: Option<Arc<super::deletion::AgentDataCleanup>>,
    pub runtime_resources: Arc<dyn super::resources::RuntimeResources>,
    pub runtime_knowledge: Arc<dyn super::resources::RuntimeKnowledge>,
    pub default_model: String,
    pub management_enabled: bool,
    pub publish_enabled: bool,
    pub runtime_enabled: bool,
    pub public: Option<Arc<super::public::PublicRuntime>>,
}
pub fn router(state: AgentState) -> Router {
    Router::new()
        .route("/api/v1/agents", any(studio))
        .route("/api/v1/agents/{*rest}", any(studio))
        .route("/api/v2/agent/threads", any(v2))
        .route("/api/v2/agent/threads/{*rest}", any(v2))
        .route("/api/v1/assistant/chat/stream", any(assistant))
        .route("/api/v1/assistant/chat", any(assistant))
        .route("/api/v1/assistant/runs/{*rest}", any(assistant))
        .route("/api/v1/assistant/approvals/{*rest}", any(assistant))
        .route("/api/v1/assistant/tasks/{*rest}", any(assistant))
        .route("/api/v1/agent-runtime/{*rest}", any(public_runtime))
        .route("/api/v1/public/agents/{*rest}", any(public_runtime))
        .route("/api/v1/publications/{*rest}", any(public_runtime))
        .route("/embed/agents/{public_id}", any(public_runtime))
        .with_state(state.clone())
        .merge(super::scheduled::router(state.clone()))
        .merge(super::scheduled_notifications::router(state))
}
fn json_response(status: u16, value: Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::OK),
        Json(value),
    )
        .into_response()
}
fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty() && s.len() <= 255)
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}
fn etag(headers: &HeaderMap) -> Result<i64, AgentError> {
    let s = headers
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| AgentError::code(428, "AGENT_DRAFT_PRECONDITION_REQUIRED"))?
        .trim();
    let value = s
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .filter(|s| !s.is_empty() && !s.starts_with('0') && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| AgentError::code(400, "AGENT_DRAFT_ETAG_INVALID"))?;
    Ok(value)
}
async fn body(request: Request) -> Result<Value, AgentError> {
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .map_err(|_| AgentError::code(413, "REQUEST_TOO_LARGE"))?;
    if bytes.is_empty() {
        return Ok(json!({}));
    }
    let v: Value =
        serde_json::from_slice(&bytes).map_err(|_| AgentError::code(422, "INVALID_JSON"))?;
    if !v.is_object() {
        return Err(AgentError::code(422, "INVALID_BODY"));
    }
    Ok(v)
}
pub(crate) fn closed(v: &Value, allowed: &[&str]) -> Result<(), AgentError> {
    if v.as_object()
        .is_none_or(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
    {
        return Err(AgentError::code(422, "EXTRA_FIELDS_FORBIDDEN"));
    }
    Ok(())
}
pub(crate) fn bounded(
    v: &Value,
    key: &str,
    min: usize,
    max: usize,
    required: bool,
) -> Result<(), AgentError> {
    match v.get(key) {
        None | Some(Value::Null) if !required => Ok(()),
        Some(Value::String(s)) if (min..=max).contains(&s.chars().count()) => Ok(()),
        _ => Err(AgentError::code(422, "INVALID_REQUEST_FIELD")),
    }
}
fn normalized_spec(raw: &Value, default_model: &str) -> Result<Value, AgentError> {
    let mut value = json!({"schema_version":"agent-spec/v1","identity":{"icon_url":null,"theme_color":null,"welcome_message":"","suggested_prompts":[]},"outcome":{"goal":"","triggers":[],"inputs":[],"human_boundaries":[],"success_criteria":[]},"instructions":"","model":{"model_id":"","provider_id":null,"temperature":null,"max_tokens":null,"thinking_mode":null},"capabilities":[],"knowledge":[],"memory":{}});
    if !raw.is_null() {
        let fields = raw
            .as_object()
            .ok_or_else(|| AgentError::code(422, "AGENT_SPEC_INVALID"))?;
        for (k, v) in fields {
            if ["identity", "outcome", "model"].contains(&k.as_str()) && v.is_object() {
                for (a, b) in v.as_object().expect("object") {
                    value[k][a] = b.clone();
                }
            } else {
                value[k] = v.clone();
            }
        }
    }
    let errors = spec::safety_errors(&value);
    if !errors.is_empty() {
        return Err(AgentError::validation(errors));
    }
    if text(&value["model"], "model_id").trim().is_empty() {
        if default_model.trim().is_empty() {
            return Err(AgentError::code(503, "AGENT_RUNTIME_MODEL_UNAVAILABLE"));
        }
        value["model"]["model_id"] = default_model.trim().into();
        value["model"]["provider_id"] = Value::Null;
    }
    Ok(value)
}
fn version_public(mut value: Value) -> Value {
    value["spec"] = spec::redact_for_read(&value["resolved_spec"]);
    value
        .as_object_mut()
        .expect("version")
        .remove("resolved_spec");
    value
}
async fn studio(
    State(state): State<AgentState>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    let id = request_id(&headers);
    let result=async{let actor=Actor::resolve(&state.auth,&headers,&path).await?;if !matches!(method.as_str(),"GET"|"HEAD"|"OPTIONS")&&!state.management_enabled{return Err(AgentError::code(503,"AGENT_STUDIO_MUTATIONS_DISABLED"));}let parts:Vec<_>=path.trim_start_matches("/api/v1/agents").trim_matches('/').split('/').filter(|s|!s.is_empty()).collect();let payload=body(request).await?;if parts.is_empty(){match method.as_str(){"GET"=>{let limit=query.get("limit").map(|v|v.parse::<i64>()).transpose().map_err(|_|AgentError::code(422,"INVALID_LIMIT"))?.unwrap_or(25);if !(1..=100).contains(&limit){return Err(AgentError::code(422,"INVALID_LIMIT"));}return Ok(json_response(200,state.repository.list(&actor,ListAgents{limit,cursor:query.get("cursor").cloned(),status:query.get("status").cloned(),owner_id:query.get("owner_id").cloned(),search:query.get("search").cloned(),channel:query.get("channel").cloned()}).await?));},"POST"=>{closed(&payload,&["name","slug","description","spec"])?;bounded(&payload,"name",1,255,true)?;bounded(&payload,"slug",1,128,false)?;bounded(&payload,"description",0,4000,false)?;let value=normalized_spec(&payload["spec"],&state.default_model)?;let agent=state.repository.create(&actor,text(&payload,"name"),payload["slug"].as_str(),text(&payload,"description"),&value).await?;return Ok(json_response(201,json!({"request_id":id,"agent":agent})));},_=>return Err(AgentError::code(405,"METHOD_NOT_ALLOWED"))}}
    let agent=super::uuid(parts[0])?;if parts.iter().any(|p|*p=="preview"){return preview_route(&state,&actor,&agent,&parts[1..],&method,&headers,payload).await;}
    let suffix=&parts[1..];let response=match(method.as_str(),suffix){("GET",[])=>json_response(200,state.repository.get(&actor,&agent).await?),("PATCH",[])=>{closed(&payload,&["name","slug","description"])?;if payload.as_object().expect("object").is_empty(){return Err(AgentError::code(422,"INVALID_BODY"));}json_response(200,json!({"request_id":id,"agent":state.repository.update(&actor,&agent,&payload).await?}))},("DELETE",[])=>{state.repository.soft_delete(&actor,&agent).await?;json_response(200,json!({"request_id":id,"status":"deleted"}))},("POST",["archive"])=>{closed(&payload,&["disable_publications"])?;json_response(200,json!({"request_id":id,"agent":state.repository.archive(&actor,&agent,payload["disable_publications"].as_bool().unwrap_or(false)).await?}))},("POST",["copy"])=>{closed(&payload,&["name","slug"])?;let value=state.repository.copy(&actor,&agent,payload["name"].as_str(),payload["slug"].as_str()).await?;json_response(201,json!({"request_id":id,"agent":value}))},("GET",["draft"])=>{let draft=state.repository.draft(&actor,&agent).await?;let tag=format!("\"{}\"",draft["revision"]);let mut response=json_response(200,draft);response.headers_mut().insert("etag",tag.parse().expect("ETag"));response},("PUT",["draft"])=>{closed(&payload,&["spec","name","description"])?;let revision=etag(&headers)?;let value=normalized_spec(&payload["spec"],&state.default_model)?;let draft=state.repository.update_draft(&actor,&agent,revision,&value,&payload).await?;let mut response=json_response(200,json!({"request_id":id,"draft":draft}));response.headers_mut().insert("etag",format!("\"{}\"",draft["revision"]).parse().expect("etag"));response},("POST",["validate"])=>json_response(200,state.repository.validate_draft(&actor,&agent).await?),("GET",["versions"])=>json_response(200,json!(state.repository.versions(&actor,&agent).await?.into_iter().map(version_public).collect::<Vec<_>>())),("POST",["versions"])=>json_response(201,json!({"request_id":id,"version":version_public(state.repository.create_version(&actor,&agent,etag(&headers)?).await?)})),("GET",["members"])=>json_response(200,json!(state.repository.members(&actor,&agent).await?)),("PUT",["members",kind,principal])=>{closed(&payload,&["role"])?;json_response(200,json!({"request_id":id,"member":state.repository.upsert_member(&actor,&agent,kind,principal,text(&payload,"role")).await?}))},("DELETE",["members",kind,principal])=>{state.repository.remove_member(&actor,&agent,kind,principal).await?;json_response(200,json!({"request_id":id,"status":"deleted"}))},("GET",["publications"])=>json_response(200,json!(state.repository.publications(&actor,&agent).await?)),("POST",["evals"])=>{if !state.publish_enabled{return Err(AgentError::code(503,"AGENT_STUDIO_PUBLISH_DISABLED"));}closed(&payload,&["draft_revision","dataset_id","channel","auth_mode","channel_policy"])?;let profile=super::release_contract::profile(&std::env::var("AGENT_RELEASE_PROFILE").unwrap_or_default());if profile["available"]!=true{return Err(AgentError::code(503,"AGENT_RELEASE_PROFILE_UNAVAILABLE"));}let candidate=state.resolve_release_candidate(&actor,&agent,payload["draft_revision"].as_i64().unwrap_or(0),payload["channel"].as_str().unwrap_or("hosted"),payload["auth_mode"].as_str().unwrap_or("private"),&payload["channel_policy"],payload["dataset_id"].as_str()).await?;json_response(201,state.create_release_evaluation(&actor,&agent,&candidate,&profile).await?)},("GET",["evals"])=>{let mut page=state.repository.release_evaluations(&actor,&agent,100).await?;let rows=page["evaluations"].as_array().cloned().unwrap_or_default();let mut decorated=Vec::new();for row in rows{decorated.push(state.decorate_evaluation(&actor,row).await);}page["evaluations"]=json!(decorated);json_response(200,page)},("GET",["evals",evaluation])=>{let row=state.repository.release_evaluation(&actor,&agent,evaluation,"viewer").await?;json_response(200,state.decorate_evaluation(&actor,row).await)},("POST",["evals",evaluation,"execute"])=>{if !state.publish_enabled{return Err(AgentError::code(503,"AGENT_STUDIO_PUBLISH_DISABLED"));}let profile=super::release_contract::profile(&std::env::var("AGENT_RELEASE_PROFILE").unwrap_or_default());json_response(200,state.execute_release_evaluation(&actor,&agent,evaluation,&profile).await?)},("POST",["evals",evaluation,"cancel"])=>{if !state.publish_enabled{return Err(AgentError::code(503,"AGENT_STUDIO_PUBLISH_DISABLED"));}json_response(200,state.repository.transition_evaluation(&actor,&agent,evaluation,true).await?)},("GET",["evals",evaluation,"diff"])=>json_response(200,state.repository.release_diff(&actor,&agent,evaluation).await?),("POST",["publish"])=>{if !state.publish_enabled{return Err(AgentError::code(503,"AGENT_STUDIO_PUBLISH_DISABLED"));}closed(&payload,&["evaluation_id","experiment_run_id","reason"])?;bounded(&payload,"reason",0,1000,false)?;let key=headers.get("idempotency-key").and_then(|v|v.to_str().ok()).ok_or_else(||AgentError::code(428, "AGENT_RELEASE_IDEMPOTENCY_KEY_REQUIRED"))?;let mut value=state.publish_agent(&actor,&agent,key,&payload).await?;value["request_id"]=id.clone().into();json_response(200,value)},("POST",["governance","data-deletions"])=>{closed(&payload,&["scope","subject_user_id","idempotency_key"])?;let cleanup=state.cleanup.as_ref().ok_or_else(||AgentError::code(503,"AGENT_RUNTIME_CLEANUP_STORAGE_UNAVAILABLE"))?;json_response(200,cleanup.execute(&actor,&agent,text(&payload,"scope"),payload["subject_user_id"].as_str(),text(&payload,"idempotency_key")).await?)},("GET",["governance"])=>json_response(200,state.repository.governance(&actor,&agent).await?),("PUT",["governance"])=>json_response(200,state.repository.update_governance(&actor,&agent,&payload).await?),("POST",["governance","cache:invalidate"])=>{let row=state.repository.invalidate_caches(&actor,&agent).await?;json_response(200,json!({"request_id":id,"cache_epoch":row["cache_epoch"],"deleted_cache_rows":row["deleted_cache_rows"]}))},("POST",["governance","credentials:revoke"])=>json_response(200,json!({"request_id":id,"revoked":state.repository.revoke_credentials(&actor,&agent).await?})),("GET",["analytics"])=>json_response(200,state.analytics(&actor,&agent,&query).await?),("GET",["audit-events"])=>json_response(200,state.repository.audit_events(&actor,&agent,&serde_json::to_value(&query).unwrap_or(json!({})),query.get("limit").and_then(|s|s.parse().ok()).unwrap_or(50),query.get("offset").and_then(|s|s.parse().ok()).unwrap_or(0)).await?),("GET",["publish-events"])=>{state.repository.get(&actor,&agent).await?;let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(e) FROM agent_publish_events e WHERE tenant_id=$1 AND agent_id=$2::uuid ORDER BY created_at DESC").bind(&actor.tenant_id).bind(&agent).fetch_all(&state.repository.pool).await?;json_response(200,json!(rows))},_=>return Err(AgentError::code(404,"NOT_FOUND"))};Ok(response)}.await;
    match result {
        Ok(response) => response,
        Err(mut error) => {
            error.detail["request_id"] = id.into();
            error.into_response()
        }
    }
}

impl AgentState {
    pub async fn build_snapshot(
        &self,
        actor: &Actor,
        resolution: &Value,
        channel: &str,
    ) -> Result<Value, AgentError> {
        let spec_value = &resolution["spec"];
        let model_spec = &spec_value["model"];
        let model_id = text(model_spec, "model_id");
        let row = self
            .control
            .models
            .model(
                &actor.tenant_id,
                model_id,
                model_spec["provider_id"].as_str(),
            )
            .await?;
        if row["is_enabled"] == false
            || !super::entry::user_can_access_model(
                actor,
                row["access_level"].as_str().unwrap_or("public"),
            )
        {
            return Err(AgentError::code(403, "AGENT_RUNTIME_MODEL_FORBIDDEN"));
        }
        let provider = text(&row, "provider_id");
        let p = self
            .control
            .providers
            .provider(&actor.tenant_id, provider)
            .await?;
        if p["is_enabled"] != true {
            return Err(AgentError::code(503, "AGENT_RUNTIME_PROVIDER_UNAVAILABLE"));
        }
        let mut parameters = json!({});
        for k in ["temperature", "max_tokens", "thinking_mode"] {
            if !model_spec[k].is_null() {
                parameters[k] = model_spec[k].clone();
            }
        }
        let model = json!({"id":model_id,"provider":provider,"parameters":parameters});
        let publication = &resolution["publication"];
        let raw_policy = &publication["policy"];
        let mut policy = json!({"attachments":raw_policy["attachments"].as_bool().unwrap_or(channel=="preview"),"high_risk_tools":raw_policy["high_risk_tools"].as_bool().unwrap_or(channel=="preview"),"allowed_origins":raw_policy["allowed_origins"].as_array().cloned().unwrap_or_default()});
        if let Some(expiry) = raw_policy["expires_at"].as_str() {
            policy["expires_at"] = expiry.into();
        }
        let bindings = resolution["capabilities"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let effective = self
            .runtime_resources
            .effective(actor, &bindings, channel, &policy)
            .await
            .unwrap_or_default();
        let skill_keys = |values: &[Value]| {
            values
                .iter()
                .filter(|v| v["capability_type"] == "skill" || v["type"] == "skill")
                .map(|v| {
                    (
                        text(v, "resource_id").to_owned(),
                        text(v, "resource_version").to_owned(),
                    )
                })
                .collect::<std::collections::BTreeSet<_>>()
        };
        if skill_keys(&bindings) != skill_keys(&effective) {
            return Err(AgentError::code(
                if channel == "preview" { 422 } else { 409 },
                "AGENT_SKILL_UNAVAILABLE",
            ));
        }
        let mut capabilities = Vec::new();
        for binding in effective {
            let kind = text(&binding, "capability_type");
            let kind = if matches!(kind, "native" | "model_native") {
                "platform"
            } else {
                kind
            };
            if !matches!(kind, "platform" | "mcp" | "skill" | "connector") {
                continue;
            }
            let risk = binding["risk"]
                .as_str()
                .or_else(|| binding["config"]["risk"].as_str())
                .unwrap_or(if kind == "platform" { "low" } else { "high" });
            if !matches!(risk, "low" | "medium" | "high" | "critical")
                || (matches!(risk, "high" | "critical") && policy["high_risk_tools"] != true)
            {
                continue;
            }
            let config = super::object(&binding["config"]);
            if !super::resources::authenticated(actor)
                && matches!(channel, "hosted" | "embed")
                && (matches!(risk, "high" | "critical")
                    || config["write"] == true
                    || config["mutating"] == true
                    || matches!(
                        text(&config, "side_effects"),
                        "write" | "external" | "destructive"
                    ))
            {
                continue;
            }
            let hash = binding["schema_hash"]
                .as_str()
                .map(|s| format!("sha256:{}", s.trim_start_matches("sha256:")));
            capabilities.push(json!({"type":kind,"id":binding["resource_id"],"version":binding["resource_version"],"schema_hash":hash,"risk":risk,"config":config}));
        }
        capabilities.sort_by_key(|v| (text(v, "type").to_owned(), text(v, "id").to_owned()));
        let knowledge = resolution["knowledge"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut ids: Vec<String> = knowledge
            .iter()
            .filter_map(|v| v["dataset_id"].as_str())
            .map(str::to_owned)
            .collect();
        ids.sort();
        ids.dedup();
        if self
            .runtime_knowledge
            .effective(
                actor,
                &knowledge,
                channel,
                text(&resolution["agent"], "agent_id"),
            )
            .await
            .unwrap_or_default()
            != ids.iter().cloned().collect()
        {
            return Err(AgentError::code(
                if channel == "preview" { 422 } else { 409 },
                "AGENT_KNOWLEDGE_UNAVAILABLE",
            ));
        }
        let mut by_dataset = json!({});
        let mut modes = Vec::new();
        let mut top_k = 0;
        let mut threshold = 1.0_f64;
        let mut images = false;
        for b in &knowledge {
            let c = &b["retrieval_config"];
            let mode = c["mode"].as_str().unwrap_or("auto");
            let top = c["top_k"].as_i64().unwrap_or(5);
            let score = c["threshold"]
                .as_f64()
                .or_else(|| c["score_threshold"].as_f64())
                .unwrap_or(0.4);
            if !matches!(mode, "auto" | "tool" | "off")
                || !(1..=20).contains(&top)
                || !(0.0..=1.0).contains(&score)
            {
                return Err(AgentError::code(422, "AGENT_KNOWLEDGE_CONFIG_INVALID"));
            }
            modes.push(mode);
            top_k = top_k.max(top);
            threshold = threshold.min(score);
            images |= c["include_images"] == true;
            by_dataset[text(b, "dataset_id")] = json!({"mode":mode,"top_k":top,"threshold":score,"include_images":c["include_images"].as_bool().unwrap_or(false)});
        }
        let retrieval = json!({"mode":if modes.contains(&"auto"){"auto"}else if modes.contains(&"tool"){"tool"}else if modes.is_empty(){"auto"}else{"off"},"top_k":if top_k==0{5}else{top_k},"threshold":if modes.is_empty(){0.4}else{threshold},"include_images":images,"config_scope":"per_dataset","by_dataset":by_dataset,"provenance":ids.iter().map(|id|json!({"dataset_id":id,"content_mode":"live_latest","historical_replayable":false,"revision_source":"assistant_run_catalog"})).collect::<Vec<_>>(),"replayability":"live_content_provenance_only"});
        let kb = json!({"datasets":ids,"retrieval":retrieval});
        let outcome = spec::outcome_contract(spec_value);
        let instructions = if outcome.is_empty() {
            text(spec_value, "instructions").to_owned()
        } else {
            format!(
                "{outcome}\n\n[Detailed instructions]\n{}",
                text(spec_value, "instructions")
            )
        };
        let memory =
            if !super::resources::authenticated(actor) && matches!(channel, "hosted" | "embed") {
                "session"
            } else {
                match text(&spec_value["memory"], "mode") {
                    "off" => "off",
                    "user" => "user",
                    _ => "session",
                }
            };
        let version = &resolution["version"];
        let draft = &resolution["draft"];
        let agent_spec = json!({"agentId":resolution["agent"]["agent_id"],"agentVersionId":version["agent_version_id"],"channel":channel,"developerInstructions":instructions,"model":model,"knowledge":kb,"capabilities":capabilities,"memory":{"mode":memory}});
        let hash = version["spec_hash"]
            .as_str()
            .or_else(|| draft["spec_hash"].as_str())
            .unwrap_or("");
        Ok(
            json!({"schema_version":"agent-runtime/v1","tenant_id":resolution["agent"]["tenant_id"],"agent_id":resolution["agent"]["agent_id"],"agent_version_id":version["agent_version_id"],"publication":{"id":publication["publication_id"],"channel":channel,"auth_mode":publication["auth_mode"].as_str().unwrap_or("private")},"model":model,"instructions":{"agent":instructions,"developerInstructions":instructions,"prompt_hash":format!("sha256:{}",spec::hash(&json!(instructions)))},"agent_spec":agent_spec,"capabilities":capabilities,"knowledge":kb,"memory":{"mode":memory},"channel_policy":policy,"fingerprints":{"spec":format!("sha256:{}",hash.trim_start_matches("sha256:")),"tool_schema":format!("sha256:{}",spec::hash(&json!(capabilities))),"skills":format!("sha256:{}",spec::hash(&json!(capabilities.iter().filter(|v|v["type"]=="skill").collect::<Vec<_>>()))),"knowledge_revision":format!("sha256:{}",spec::hash(&kb))}}),
        )
    }
    pub async fn bind_pin(
        &self,
        scope: &Scope,
        snapshot: &Value,
        draft: Option<i64>,
    ) -> Result<Value, AgentError> {
        let expires = chrono::Utc::now() + chrono::Duration::days(7);
        let row:Option<Value>=sqlx::query_scalar("INSERT INTO assistant.sessions AS existing_session(session_id,service_id,user_id,tenant_id,state,history,metadata,config,status,expires_at,agent_id,agent_version_id,agent_draft_revision,publication_id,channel,runtime_fingerprint,agent_spec_hash) VALUES($1,'__builtin_assistant__',$2,$3,'{}'::jsonb,'[]'::jsonb,'{}'::jsonb,'{}'::jsonb,'active',$4,$5::uuid,$6::uuid,$7,$8::uuid,$9,$10,$11) ON CONFLICT(session_id) DO UPDATE SET expires_at=GREATEST(existing_session.expires_at,EXCLUDED.expires_at),updated_at=NOW() WHERE existing_session.user_id=EXCLUDED.user_id AND existing_session.tenant_id=EXCLUDED.tenant_id AND existing_session.service_id=EXCLUDED.service_id AND existing_session.status='active' AND existing_session.agent_id=EXCLUDED.agent_id AND existing_session.agent_version_id IS NOT DISTINCT FROM EXCLUDED.agent_version_id AND existing_session.agent_draft_revision IS NOT DISTINCT FROM EXCLUDED.agent_draft_revision AND existing_session.publication_id IS NOT DISTINCT FROM EXCLUDED.publication_id AND existing_session.channel=EXCLUDED.channel AND existing_session.runtime_fingerprint=EXCLUDED.runtime_fingerprint AND existing_session.agent_spec_hash=EXCLUDED.agent_spec_hash RETURNING to_jsonb(existing_session)").bind(&scope.session_id).bind(&scope.user_id).bind(&scope.tenant_id).bind(expires).bind(text(snapshot,"agent_id")).bind(snapshot["agent_version_id"].as_str()).bind(draft).bind(snapshot["publication"]["id"].as_str()).bind(text(&snapshot["publication"],"channel")).bind(format!("sha256:{}",spec::hash(snapshot))).bind(text(&snapshot["fingerprints"],"spec")).fetch_optional(&self.repository.pool).await?;
        let row = row.ok_or_else(|| AgentError::code(409, "AGENT_RUNTIME_PIN_CONFLICT"))?;
        self.control
            .threads
            .bind_new_assignment(scope, &self.control.options.kernel_revision)
            .await?;
        Ok(row)
    }
    pub async fn pinned_session(
        &self,
        scope: &Scope,
        allow_channel: bool,
    ) -> Result<Option<Value>, AgentError> {
        let s = self.control.threads.owned_session(scope).await?;
        if [
            "agent_id",
            "agent_version_id",
            "channel",
            "publication_id",
            "agent_draft_revision",
        ]
        .iter()
        .all(|k| s[*k].is_null() || s[*k] == "")
        {
            return Ok(None);
        }
        let channel = text(&s, "channel");
        if allow_channel
            && !text(&s, "agent_id").is_empty()
            && ((channel == "preview"
                && s["agent_version_id"].is_null()
                && s["publication_id"].is_null()
                && s["agent_draft_revision"].as_i64().is_some_and(|r| r > 0))
                || (matches!(channel, "hosted" | "embed" | "api")
                    && s["agent_version_id"].is_string()
                    && s["publication_id"].is_string()
                    && s["agent_draft_revision"].is_null()))
        {
            return Ok(Some(s));
        }
        if text(&s, "agent_id").is_empty()
            || text(&s, "agent_version_id").is_empty()
            || channel != "preview"
            || !s["publication_id"].is_null()
            || !s["agent_draft_revision"].is_null()
        {
            return Err(AgentError::code(409, "AGENT_RUNTIME_PIN_INVALID"));
        }
        Ok(Some(s))
    }
    pub async fn pinned_snapshot(
        &self,
        actor: &Actor,
        session: &Value,
    ) -> Result<Value, AgentError> {
        if !session["agent_draft_revision"].is_null() {
            let agent = self
                .repository
                .get(actor, text(session, "agent_id"))
                .await?;
            if matches!(text(&agent, "status"), "archived" | "deleted") {
                return Err(AgentError::code(409, "AGENT_RUNTIME_AGENT_UNAVAILABLE"));
            }
            return Ok(json!({}));
        }
        let resolution = if session["publication_id"].is_string() {
            self.repository
                .resolve_publication(
                    actor,
                    text(session, "publication_id"),
                    session["agent_version_id"].as_str(),
                )
                .await?
        } else {
            self.repository
                .resolve_version(
                    actor,
                    text(session, "agent_id"),
                    text(session, "agent_version_id"),
                )
                .await?
        };
        let snapshot = self
            .build_snapshot(actor, &resolution, text(session, "channel"))
            .await?;
        if session["agent_spec_hash"] != snapshot["fingerprints"]["spec"]
            || text(session, "runtime_fingerprint") != format!("sha256:{}", spec::hash(&snapshot))
        {
            return Err(AgentError::code(409, "AGENT_RUNTIME_PIN_STALE"));
        }
        Ok(snapshot)
    }
    pub async fn start_snapshot_turn(
        &self,
        actor: Actor,
        session: String,
        message: String,
        snapshot: Value,
    ) -> Result<AgentTurn, AgentError> {
        self.start_snapshot_turn_with_refs(actor, session, message, snapshot, vec![])
            .await
    }
    pub async fn start_snapshot_turn_with_refs(
        &self,
        actor: Actor,
        session: String,
        message: String,
        snapshot: Value,
        refs: Vec<String>,
    ) -> Result<AgentTurn, AgentError> {
        let model = &snapshot["model"];
        let params = &model["parameters"];
        let knowledge = &snapshot["knowledge"];
        let readonly = json!({"knowledge":{"dataset_ids":knowledge["datasets"],"mode":knowledge["retrieval"]["mode"].as_str().unwrap_or("off"),"top_k":knowledge["retrieval"]["top_k"].as_i64().unwrap_or(5),"score_threshold":knowledge["retrieval"]["threshold"].as_f64().unwrap_or(0.4)},"attachments":{"refs":refs}});
        self.control
            .start_turn(TurnRequest {
                actor,
                session_id: session,
                message,
                model_id: text(model, "id").into(),
                legacy_thinking_level: params["thinking_mode"].as_str().map(str::to_owned),
                max_tokens: params["max_tokens"].as_i64(),
                temperature: params["temperature"].as_f64(),
                memory_mode: snapshot["memory"]["mode"]
                    .as_str()
                    .unwrap_or("session")
                    .into(),
                readonly_capabilities: readonly,
                resolved_snapshot: Some(snapshot),
                enable_dynamic_tools: true,
                ..Default::default()
            })
            .await
    }
}
async fn v2(
    State(state): State<AgentState>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    let headers = request.headers().clone();
    let result=async{let actor=Actor::resolve(&state.auth,&headers,&path).await?;let parts:Vec<_>=path.trim_start_matches("/api/v2/agent/threads").trim_matches('/').split('/').filter(|s|!s.is_empty()).collect();let payload=body(request).await?;if parts.is_empty(){if method!="POST"{return Err(AgentError::code(405,"METHOD_NOT_ALLOWED"));}validate_thread_create_body(&payload)?;if let Some(expected)=payload["expected_tenant_id"].as_str(){if expected!=actor.tenant_id{return Err(AgentError::code(409,"AGENT_RUNTIME_TENANT_PRECONDITION_FAILED"));}}
    let target=if let(Some(id),Some(version))=(payload["agent_id"].as_str(),payload["agent_version_id"].as_str()){let resolution=state.repository.resolve_version(&actor,id,version).await?;Some(state.build_snapshot(&actor,&resolution,"preview").await?)}else{None};let session=payload["session_id"].as_str().map(str::to_owned).unwrap_or_else(||uuid::Uuid::new_v4().to_string());let scope=actor.scope(&session);if let Some(target)=&target{state.bind_pin(&scope,target,None).await?;}else{ensure_builtin_session(&state,&actor,&session).await?;}
    let model=target.as_ref().and_then(|s|s["model"]["id"].as_str()).or_else(||payload["model_id"].as_str()).unwrap_or(&state.default_model);if model.is_empty(){return Err(AgentError::code(503,"AGENT_RUNTIME_MODEL_UNAVAILABLE"));}let thread=if let Some(t)=state.control.threads.get_for_session(&scope).await?{state.control.verify_thread(&scope,&t.runtime_thread_id,model).await?;t}else{let allow=target.as_ref().map(super::runtime_contract::snapshot_allowlist).transpose()?.flatten();let runtime=state.control.ensure_thread(&scope,model,target.as_ref().and_then(|s|s["model"]["provider"].as_str()).or_else(||payload["provider_id"].as_str()),None,allow,false,target.as_ref().and_then(|s|s["agent_spec"]["developerInstructions"].as_str())).await?;let stored=state.control.threads.owned_session(&scope).await?;if stored["history"].as_array().is_some_and(|a|!a.is_empty()){state.control.threads.import_legacy(&scope,&runtime.runtime_thread_id).await?}else{state.control.threads.ensure_native(&scope,&runtime.runtime_thread_id).await?}};let mut value=thread.payload();if let Some(target)=&target{value["agent_version_target"]=json!({"agent_id":target["agent_id"],"agent_version_id":target["agent_version_id"],"agent_spec_hash":text(&target["fingerprints"],"spec").trim_start_matches("sha256:"),"runtime_snapshot_hash":format!("sha256:{}",spec::hash(target))});}return Ok(json_response(201,json!({"thread":value})));}
    let thread=state.control.threads.get(&actor,parts[0]).await?.ok_or_else(||AgentError::code(404,"THREAD_NOT_FOUND"))?;let scope=thread.scope();state.control.threads.require_assignment(&scope).await?;let suffix=&parts[1..];match(method.as_str(),suffix){
    ("GET",[])=>{let pin=state.pinned_session(&scope,true).await?;let sources=state.sources.conversation_sources(&scope).await?;let mut restricted=Vec::new();for(run,refs)in &sources.by_run{if !state.sources.allowed(&scope,refs).await?{restricted.push(run.clone());}}let mut value=thread.payload();value["restricted_source_run_ids"]=json!(restricted);if let Some(pin)=pin.filter(|p|p["agent_version_id"].is_string()&&p["publication_id"].is_null()){value["agent_version_target"]=json!({"agent_id":pin["agent_id"],"agent_version_id":pin["agent_version_id"],"agent_spec_hash":text(&pin,"agent_spec_hash").trim_start_matches("sha256:"),"runtime_snapshot_hash":pin["runtime_fingerprint"]});}Ok(json_response(200,json!({"thread":value})))},
    ("POST",["turns"])=>{validate_turn_body(&payload)?;let pin=state.pinned_session(&scope,false).await?;state.sources.require_conversation(&scope,true).await?;let turn=if let Some(pin)=pin{if payload.as_object().expect("object").keys().any(|k|k!="message"){return Err(AgentError::code(422,"AGENT_RUNTIME_VERSION_OVERRIDES_FORBIDDEN"));}let snapshot=state.pinned_snapshot(&actor,&pin).await?;state.start_snapshot_turn(actor.clone(),scope.session_id.clone(),text(&payload,"message").into(),snapshot).await?}else{start_builtin(&state,actor.clone(),scope.session_id.clone(),payload).await?};Ok(json_response(202,turn_payload(&turn)))},
    ("POST",["turns",turn])if turn.ends_with(":interrupt")||turn.ends_with(":recover")=>{let recover=turn.ends_with(":recover");let turn=turn.split(':').next().unwrap_or("");let pin=state.pinned_session(&scope,true).await?;if recover{if let Some(pin)=pin{state.pinned_snapshot(&actor,&pin).await?;}state.sources.require_conversation(&scope,true).await?;let result=state.control.recover_turn(&scope,&thread.runtime_thread_id,turn).await?;Ok(json_response(200,json!({"schema_version":"agent-turn/v2","turn_id":turn,"status":result["status"].as_str().unwrap_or("recovery_requested")})))}else{closed(&payload,&["reason"])?;state.control.interrupt_turn(&scope,&thread.runtime_thread_id,turn,payload["reason"].as_str().unwrap_or("client_interrupt")).await?;Ok(json_response(200,json!({"schema_version":"agent-turn/v2","turn_id":turn,"status":"interrupt_requested"}))) }},
    ("GET",["approvals",approval_id])=>{let mut revoked=false;if let Some(pin)=state.pinned_session(&scope,true).await?{revoked=state.pinned_snapshot(&actor,&pin).await.is_err();}match state.sources.require_conversation(&scope,false).await{Err(e)if e.detail["code"]=="ASSISTANT_SOURCE_ACCESS_REVOKED"=>revoked=true,Err(e)=>return Err(e),_=>{}}let approval=state.control.get_approval(&scope,approval_id).await?.ok_or_else(||AgentError::code(404,"APPROVAL_NOT_FOUND"))?;let preview=if revoked{json!({"can_approve":false,"reason":"Knowledge source access is unavailable. Reject this action or stop the task."})}else{approval::owner_approval_preview(&state.repository.pool,&scope,&thread.runtime_thread_id,approval_id,&approval).await};let approval=if revoked{let mut out=json!({});for k in ["approval_id","status","tool_name","expires_at","run_id"]{if let Some(v)=approval.get(k){out[k]=v.clone();}}out}else{approval};Ok(json_response(200,json!({"schema_version":"agent-approval/v2","approval":approval,"preview":preview})))},
    ("POST",["approvals",approval_id,"decision"])=>{closed(&payload,&["approved","reason"])?;let approved=payload["approved"].as_bool().ok_or_else(||AgentError::code(422,"INVALID_APPROVAL_DECISION"))?;bounded(&payload,"reason",0,500,false)?;let mut receipt_only=false;if approved{if let Some(pin)=state.pinned_session(&scope,true).await?{state.pinned_snapshot(&actor,&pin).await?;}state.sources.require_conversation(&scope,true).await?;let approval=state.control.get_approval(&scope,approval_id).await?.ok_or_else(||AgentError::code(404,"APPROVAL_NOT_FOUND"))?;receipt_only=approval::requires_receipt_readback(&approval);if !receipt_only{let preview=approval::owner_approval_preview(&state.repository.pool,&scope,&thread.runtime_thread_id,approval_id,&approval).await;if preview["can_approve"]!=true{return Err(AgentError::code(409,"APPROVAL_ACTION_UNVERIFIED"));}}}let mut result=if receipt_only{state.control.read_approval_decision(&scope,&thread.runtime_thread_id,approval_id,approved,payload["reason"].as_str()).await?}else{state.control.decide_approval(&scope,approval_id,approved,payload["reason"].as_str()).await?};if result.get("approval_id").is_none(){result["approval_id"]=(*approval_id).into();}if result.get("status").is_none(){result["status"]=if approved{"approved"}else{"rejected"}.into();}result["approved"]=approved.into();result["reason"]=payload["reason"].clone();Ok(json_response(200,json!({"schema_version":"agent-approval/v2","approval":result})))},
    ("GET",["events"])=>{let after=query.get("after_sequence").map(|v|v.parse::<i64>()).transpose().map_err(|_|AgentError::code(422,"INVALID_CURSOR"))?.unwrap_or(0);let limit=query.get("limit").map(|v|v.parse::<i64>()).transpose().map_err(|_|AgentError::code(422,"INVALID_LIMIT"))?.unwrap_or(100);if after<0||!(1..=1000).contains(&limit){return Err(AgentError::code(422,"INVALID_CURSOR"));}let turn=query.get("turn_id").map(|v|super::uuid(v)).transpose()?;let metadata=if let Some(turn)=&turn{state.control.threads.turn_metadata(&scope,&thread.runtime_thread_id,turn).await?}else{None};let events=state.control.stream_thread_events(scope.clone(),thread.runtime_thread_id.clone(),after,limit,turn.clone()).await?;let response=event_response(events,thread.runtime_thread_id.clone(),turn.clone(),after,true,metadata.clone(),None);Ok(super::capture::capture_response(&state,response,&scope,&thread.runtime_thread_id,turn.as_deref(),after,limit,true,metadata,&request_id(&headers),None).await)},_=>Err(AgentError::code(404,"NOT_FOUND"))}
 }.await;
    match result {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
fn turn_payload(turn: &AgentTurn) -> Value {
    json!({"schema_version":"agent-turn/v2","turn":{"id":turn.run_id,"thread_id":turn.runtime_thread_id,"status":"in_progress","requested_reasoning_option":turn.requested_reasoning_option,"effective_reasoning_option":turn.effective_reasoning_option,"events_url":format!("/api/v2/agent/threads/{}/events?after_sequence={}&turn_id={}",turn.runtime_thread_id,turn.after_sequence,turn.run_id)}})
}
pub(super) async fn ensure_builtin_session(
    state: &AgentState,
    actor: &Actor,
    id: &str,
) -> Result<(), AgentError> {
    let existing = state
        .sessions
        .get(id)
        .await
        .map_err(|_| AgentError::code(503, "SESSION_STORAGE_UNAVAILABLE"))?;
    let mut created = false;
    if let Some(s) = existing {
        if s.user_id != actor.user_id || s.tenant_id != actor.tenant_id {
            return Err(AgentError::code(404, "SESSION_NOT_FOUND"));
        }
    } else {
        match state
            .sessions
            .create(CreateSession {
                user_id: actor.user_id.clone(),
                tenant_id: actor.tenant_id.clone(),
                service_id: Some("__builtin_assistant__".into()),
                session_id: Some(id.into()),
                fail_if_exists: true,
                ..Default::default()
            })
            .await
        {
            Ok(_) => created = true,
            Err(SessionError::AlreadyExists) => {
                state
                    .control
                    .threads
                    .owned_session(&actor.scope(id))
                    .await?;
            }
            Err(_) => return Err(AgentError::code(503, "SESSION_STORAGE_UNAVAILABLE")),
        }
    }
    if let Err(error) = state
        .control
        .threads
        .bind_new_assignment(&actor.scope(id), &state.control.options.kernel_revision)
        .await
    {
        if created {
            let _ = state.sessions.delete(id).await;
        }
        return Err(error);
    }
    Ok(())
}
fn validate_thread_create_body(v: &Value) -> Result<(), AgentError> {
    closed(v, &["session_id", "model_id", "provider_id", "agent_id", "agent_version_id", "expected_tenant_id"])?;
    bounded(v, "provider_id", 1, 255, false)?;
    if v["agent_id"].is_null() != v["agent_version_id"].is_null()
        || (!v["agent_id"].is_null() && (!v["model_id"].is_null() || !v["provider_id"].is_null()))
    {
        return Err(AgentError::code(422, "AGENT_RUNTIME_VERSION_TARGET_INVALID"));
    }
    Ok(())
}

fn validate_turn_body(v: &Value) -> Result<(), AgentError> {
    closed(
        v,
        &[
            "message",
            "model_id",
            "provider_id",
            "expected_model_ref",
            "reasoning_option",
            "thinking_level",
            "temperature",
            "execution_profile",
            "memory_mode",
            "system_prompt",
            "os_agent_enabled",
            "local_node_device_id",
            "local_node_grant_ids",
            "resume_run_id",
            "resume_approval_id",
            "max_tokens",
            "kb_dataset_ids",
            "kb_mode",
            "kb_top_k",
            "kb_score_threshold",
            "web_search_enabled",
            "web_search_max_results",
            "file_paths",
        ],
    )?;
    bounded(v, "message", 1, 200000, true)?;
    for k in ["model_id", "provider_id"] {
        bounded(v, k, 1, 255, false)?;
    }
    for k in ["reasoning_option", "thinking_level"] {
        bounded(v, k, 0, 100, false)?;
    }
    bounded(v, "system_prompt", 0, 100000, false)?;
    if !v["reasoning_option"].is_null() && !v["thinking_level"].is_null() {
        return Err(AgentError::code(422, "AMBIGUOUS_REASONING"));
    }
    if v["execution_profile"].as_str().is_some_and(|s| s != "safe")
        || v["memory_mode"]
            .as_str()
            .is_some_and(|s| !matches!(s, "auto" | "strict" | "off"))
        || !v["resume_run_id"].is_null()
        || !v["resume_approval_id"].is_null()
    {
        return Err(AgentError::code(
            409,
            "AGENT_RUNTIME_CAPABILITY_NOT_MIGRATED",
        ));
    }
    if v["kb_mode"]
        .as_str()
        .is_some_and(|s| !matches!(s, "auto" | "tool" | "off"))
    {
        return Err(AgentError::code(422, "INVALID_KNOWLEDGE_MODE"));
    }
    for (k, min, max) in [
        ("max_tokens", 1, 1000000),
        ("kb_top_k", 1, 20),
        ("web_search_max_results", 1, 20),
    ] {
        if v.get(k)
            .is_some_and(|v| !v.is_null() && v.as_i64().is_none_or(|n| !(min..=max).contains(&n)))
        {
            return Err(AgentError::code(422, "INVALID_REQUEST_FIELD"));
        }
    }
    for (k, max) in [("temperature", 2.0), ("kb_score_threshold", 1.0)] {
        if v.get(k)
            .is_some_and(|v| !v.is_null() && v.as_f64().is_none_or(|n| !(0.0..=max).contains(&n)))
        {
            return Err(AgentError::code(422, "INVALID_REQUEST_FIELD"));
        }
    }
    for (k, max) in [
        ("file_paths", 20),
        ("kb_dataset_ids", 100),
        ("local_node_grant_ids", 100),
    ] {
        if v.get(k).is_some_and(|v| {
            v.as_array()
                .is_none_or(|a| a.len() > max || a.iter().any(|v| !v.is_string()))
        }) {
            return Err(AgentError::code(422, "INVALID_REQUEST_FIELD"));
        }
    }
    Ok(())
}
pub(super) async fn start_builtin(
    state: &AgentState,
    actor: Actor,
    session: String,
    payload: Value,
) -> Result<AgentTurn, AgentError> {
    start_builtin_profile(state, actor, session, payload, "basic".into()).await
}
async fn start_builtin_profile(
    state: &AgentState,
    actor: Actor,
    session: String,
    payload: Value,
    memory_profile: String,
) -> Result<AgentTurn, AgentError> {
    let model = payload["model_id"]
        .as_str()
        .unwrap_or(&state.default_model)
        .to_owned();
    if model.is_empty() {
        return Err(AgentError::code(503, "AGENT_RUNTIME_MODEL_UNAVAILABLE"));
    }
    let scope = actor.scope(&session);
    let refs: Vec<String> = payload["file_paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    let (bound, images) = if refs.is_empty() {
        (vec![], vec![])
    } else {
        state
            .attachments
            .bind(&actor, &scope, &model, payload["provider_id"].as_str(), &refs)
            .await?
    };
    let memory = super::entry::memory_mode(
        &state.repository.pool,
        &scope,
        payload["memory_mode"].as_str().unwrap_or("auto"),
    )
    .await?;
    let readonly = json!({"knowledge":{"dataset_ids":payload["kb_dataset_ids"].as_array().cloned().unwrap_or_default(),"mode":payload["kb_mode"].as_str().unwrap_or("off"),"top_k":payload["kb_top_k"].as_i64().unwrap_or(5),"score_threshold":payload["kb_score_threshold"].as_f64().unwrap_or(0.4)},"attachments":{"refs":bound},"web_search":{"enabled":payload["web_search_enabled"].as_bool().unwrap_or(false),"max_results":payload["web_search_max_results"].as_i64().unwrap_or(5)}});
    state
        .control
        .start_turn(TurnRequest {
            actor,
            session_id: session,
            message: text(&payload, "message").into(),
            model_id: model,
            provider_id: payload["provider_id"].as_str().map(str::to_owned),
            expected_model_ref: payload
                .get("expected_model_ref")
                .filter(|v| !v.is_null())
                .cloned(),
            reasoning_option: payload["reasoning_option"].as_str().map(str::to_owned),
            legacy_thinking_level: payload["thinking_level"].as_str().map(str::to_owned),
            max_tokens: payload["max_tokens"].as_i64(),
            temperature: payload["temperature"].as_f64(),
            style_guidance: payload["system_prompt"].as_str().map(str::to_owned),
            readonly_capabilities: readonly,
            memory_mode: memory,
            memory_profile: Some(memory_profile),
            enable_dynamic_tools: true,
            image_inputs: images,
            ..Default::default()
        })
        .await
}
struct ApiEvents {
    events: EventReceiver,
    thread: String,
    turn: Option<String>,
    after: i64,
    v2: bool,
    metadata: Option<Value>,
}
impl Stream for ApiEvents {
    type Item = Result<Bytes, AgentError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.events).poll_next(cx) {
            Poll::Ready(Some(Ok(mut raw))) => {
                if raw["event_type"] == "run_started"
                    && raw["data"]["source_access_revoked"] != true
                {
                    if let Some(metadata) = &self.metadata {
                        if let (Some(data), Some(meta)) =
                            (raw["data"].as_object_mut(), metadata.as_object())
                        {
                            data.extend(meta.clone());
                        }
                    }
                }
                let sequence = raw["sequence"].as_i64().unwrap_or(self.after + 1);
                let event = if self.v2 {
                    let data = super::object(&raw["data"]);
                    let timestamp = if let Some(n) = raw["timestamp"].as_f64() {
                        chrono::DateTime::from_timestamp(n as i64, ((n.fract().abs()) * 1e9) as u32)
                            .map(|v| v.to_rfc3339())
                            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
                    } else {
                        raw["timestamp"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339())
                    };
                    json!({"schema_version":"agent-event/v2","thread_id":self.thread,"sequence":sequence,"event":{"id":raw["event_id"].as_str().map(str::to_owned).unwrap_or_else(||format!("runtime:{sequence}")),"key":raw["event_key"].as_str().map(str::to_owned).unwrap_or_else(||format!("runtime:{sequence}")),"type":raw["event_type"].as_str().unwrap_or("item"),"item_id":data["item_id"],"turn_id":data["run_id"].as_str().map(str::to_owned).or(self.turn.clone()),"status":data["status"],"payload":raw},"timestamp":timestamp})
                } else {
                    raw
                };
                let kind = if self.v2 {
                    "item"
                } else {
                    text(&event, "event_type")
                };
                Poll::Ready(Some(Ok(Bytes::from(format!(
                    "id: {sequence}\nevent: {kind}\ndata: {}\n\n",
                    serde_json::to_string(&event).expect("SSE JSON")
                )))))
            }
            other => other.map(|value| value.map(|result| result.map(|_| Bytes::new()))),
        }
    }
}
pub fn event_response(
    events: EventReceiver,
    thread: String,
    turn: Option<String>,
    after: i64,
    v2: bool,
    metadata: Option<Value>,
    session: Option<&str>,
) -> Response {
    let mut builder = Response::builder()
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .header("x-ai-agent-kernel", "agent_runtime");
    if let Some(run) = turn.as_deref() {
        builder = builder.header("x-run-id", run);
    }
    if let Some(session) = session {
        builder = builder.header("x-session-id", session);
    }
    builder
        .body(Body::from_stream(ApiEvents {
            events,
            thread,
            turn,
            after,
            v2,
            metadata,
        }))
        .expect("SSE response")
}
pub fn public_capabilities(snapshot: &Value) -> Vec<Value> {
    let mut values = Vec::new();
    for cap in snapshot["capabilities"].as_array().into_iter().flatten() {
        if cap["type"] != "platform" {
            continue;
        }
        let name = text(cap, "id");
        let hash = text(cap, "schema_hash");
        let risk = text(cap, "risk");
        let confirmation = cap["config"]["requires_confirmation"]
            .as_bool()
            .or_else(|| matches!(risk, "high" | "critical").then_some(true));
        if name.is_empty()
            || name.len() > 200
            || !name.as_bytes()[0].is_ascii_alphanumeric()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
            || !hash.strip_prefix("sha256:").is_some_and(|h| {
                h.len() == 64
                    && h.bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            || !matches!(risk, "low" | "medium" | "high" | "critical")
            || confirmation.is_none()
        {
            continue;
        }
        values.push(json!({"name":name,"schema_hash":hash,"risk":risk,"requires_confirmation":confirmation}));
    }
    values.sort_by_key(|v| text(v, "name").to_owned());
    values
}
pub(crate) fn runtime_attachments(payload: &Value) -> Result<Vec<String>, AgentError> {
    let Some(value) = payload.get("attachments") else {
        return Ok(vec![]);
    };
    let values = value
        .as_array()
        .filter(|a| a.len() <= 20)
        .ok_or_else(|| AgentError::code(422, "INVALID_ATTACHMENTS"))?;
    values
        .iter()
        .map(|v| {
            closed(v, &["artifact_id", "filename", "mime_type"])?;
            bounded(v, "artifact_id", 1, 255, true)?;
            bounded(v, "filename", 0, 255, false)?;
            bounded(v, "mime_type", 0, 255, false)?;
            Ok(text(v, "artifact_id").to_owned())
        })
        .collect()
}
async fn preview_route(
    state: &AgentState,
    actor: &Actor,
    agent: &str,
    parts: &[&str],
    method: &str,
    headers: &HeaderMap,
    payload: Value,
) -> Result<Response, AgentError> {
    if !state.runtime_enabled {
        return Err(AgentError::code(503, "AGENT_RUNTIME_DISABLED"));
    }
    let (version, tail) = match parts {
        ["versions", version, "preview", rest @ ..] => (Some(*version), rest),
        ["preview", rest @ ..] => (None, rest),
        _ => return Err(AgentError::code(404, "NOT_FOUND")),
    };
    super::public::reject_forgery(headers, None)?;
    if method == "GET" {
        if let ["sessions", session] = tail {
            let row = state
                .control
                .threads
                .owned_session(&actor.scope(*session))
                .await?;
            if row["agent_id"] != agent
                || row["channel"] != "preview"
                || !row["publication_id"].is_null()
                || (row["agent_version_id"].is_null() == row["agent_draft_revision"].is_null())
            {
                return Err(AgentError::code(404, "AGENT_RUNTIME_SESSION_NOT_FOUND"));
            }
            return Ok(json_response(
                200,
                json!({"session_id":session,"agent_id":agent,"agent_version_id":row["agent_version_id"],"draft_revision":row["agent_draft_revision"],"channel":"preview","request_id":request_id(headers)}),
            ));
        }
    }
    match (method, tail) {
        ("POST", ["sessions"]) => closed(
            &payload,
            if version.is_some() {
                &[]
            } else {
                &["draft_revision"]
            },
        )?,
        ("POST", ["chat", "stream"]) => {
            let mut fields = vec![
                "message",
                "session_id",
                "attachments",
                "resume_run_id",
                "resume_approval_id",
            ];
            if version.is_none() {
                fields.push("draft_revision");
            }
            closed(&payload, &fields)?;
            bounded(&payload, "message", 1, 200000, true)?;
            bounded(&payload, "session_id", 1, 255, false)?;
            for key in ["resume_run_id", "resume_approval_id"] {
                bounded(&payload, key, 1, 255, false)?;
            }
            if payload["resume_run_id"].is_null() != payload["resume_approval_id"].is_null() {
                return Err(AgentError::code(422, "INVALID_RESUME_IDENTITY"));
            }
        }
        _ => return Err(AgentError::code(404, "NOT_FOUND")),
    }
    if !payload["resume_run_id"].is_null() || !payload["resume_approval_id"].is_null() {
        return Err(AgentError::code(409, "AGENT_RUNTIME_RESUME_NOT_AVAILABLE"));
    }
    let resolution = if let Some(version) = version {
        state
            .repository
            .resolve_version(actor, agent, version)
            .await?
    } else {
        state
            .repository
            .resolve_preview(
                actor,
                agent,
                payload["draft_revision"]
                    .as_i64()
                    .ok_or_else(|| AgentError::code(422, "AGENT_PREVIEW_REVISION_REQUIRED"))?,
            )
            .await?
    };
    let snapshot = state.build_snapshot(actor, &resolution, "preview").await?;
    let attachments = runtime_attachments(&payload)?;
    if !attachments.is_empty() && snapshot["channel_policy"]["attachments"] != true {
        return Err(AgentError::code(422, "AGENT_RUNTIME_ATTACHMENTS_FORBIDDEN"));
    }
    let session = payload["session_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let scope = actor.scope(&session);
    match (method, tail) {
        ("POST", ["sessions"]) => {
            state
                .bind_pin(
                    &scope,
                    &snapshot,
                    if version.is_none() {
                        resolution["draft"]["revision"].as_i64()
                    } else {
                        None
                    },
                )
                .await?;
            Ok(json_response(
                201,
                json!({"session_id":session,"agent_id":snapshot["agent_id"],"agent_version_id":snapshot["agent_version_id"],"publication_id":null,"draft_revision":if version.is_none(){resolution["draft"]["revision"].clone()}else{Value::Null},"channel":"preview","runtime_fingerprint":format!("sha256:{}",spec::hash(&snapshot)),"effective_capabilities":public_capabilities(&snapshot),"request_id":request_id(headers)}),
            ))
        }
        ("POST", ["chat", "stream"]) => {
            bounded(&payload, "message", 1, 200000, true)?;
            state
                .bind_pin(
                    &scope,
                    &snapshot,
                    if version.is_none() {
                        resolution["draft"]["revision"].as_i64()
                    } else {
                        None
                    },
                )
                .await?;
            let turn = state
                .start_snapshot_turn_with_refs(
                    actor.clone(),
                    session.clone(),
                    text(&payload, "message").into(),
                    snapshot.clone(),
                    attachments,
                )
                .await?;
            let metadata = serde_json::to_value(&turn).ok();
            let events = state
                .control
                .stream_thread_events(
                    scope.clone(),
                    turn.runtime_thread_id.clone(),
                    turn.after_sequence,
                    1000,
                    Some(turn.run_id.clone()),
                )
                .await?;
            let response = event_response(
                events,
                turn.runtime_thread_id.clone(),
                Some(turn.run_id.clone()),
                turn.after_sequence,
                false,
                metadata.clone(),
                Some(&session),
            );
            Ok(super::capture::capture_response(
                state,
                response,
                &scope,
                &turn.runtime_thread_id,
                Some(&turn.run_id),
                turn.after_sequence,
                1000,
                false,
                metadata,
                &request_id(headers),
                Some(snapshot),
            )
            .await)
        }
        _ => Err(AgentError::code(404, "NOT_FOUND")),
    }
}
async fn assistant(State(state): State<AgentState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().as_str().to_owned();
    let headers = request.headers().clone();
    let result=async{
 super::public::reject_forgery(&headers,None)?;
 let actor=Actor::resolve(&state.auth,&headers,&path).await?;let mut payload=body(request).await?;
 if (path.ends_with("/assistant/chat/stream")||path.ends_with("/assistant/chat"))&&method=="POST"{state.edge_rate.check(&actor,"assistant_chat").await?;super::public::reject_forgery(&headers,Some(&payload))?;let started=std::time::Instant::now();let model=payload["model_id"].as_str().unwrap_or(&state.default_model).to_owned();let session=payload["session_id"].as_str().map(str::to_owned).unwrap_or_else(||uuid::Uuid::new_v4().to_string());let memory_profile=payload["memory_profile"].as_str().unwrap_or("basic").to_owned();payload=super::entry_request::normalize_v1(&payload)?;validate_turn_body(&payload)?;ensure_builtin_session(&state,&actor,&session).await?;if state.pinned_session(&actor.scope(&session),false).await?.is_some(){return Err(AgentError::code(409,"AGENT_RUNTIME_PIN_INVALID"));}let turn=start_builtin_profile(&state,actor.clone(),session.clone(),payload,memory_profile).await?;let metadata=json!({"requested_reasoning_option":turn.requested_reasoning_option,"effective_reasoning_option":turn.effective_reasoning_option,"reasoning_adapter_id":turn.reasoning_adapter_id,"capability_revision":turn.capability_revision,"reasoning_fallback_reason":turn.fallback_reason,"kernel":"agent"});let events=state.control.stream_thread_events(actor.scope(&session),turn.runtime_thread_id.clone(),turn.after_sequence,1000,Some(turn.run_id.clone())).await?;if path.ends_with("/assistant/chat"){let mut events=events;let mut content=String::new();while let Some(event)=events.recv().await{let event=event?;if event["data"]["source_access_revoked"]==true{content.clear();}if event["event_type"]=="text_delta"{if let Some(s)=event["data"]["content"].as_str(){content.push_str(s);}}}return Ok(json_response(200,json!({"content":content,"usage":{},"contexts":[],"duration_ms":started.elapsed().as_secs_f64()*1000.0,"model_id":model,"session_id":session,"run_id":turn.run_id})));}let response=event_response(events,turn.runtime_thread_id.clone(),Some(turn.run_id.clone()),turn.after_sequence,false,Some(metadata.clone()),Some(&session));return Ok(super::capture::capture_response(&state,response,&actor.scope(&session),&turn.runtime_thread_id,Some(&turn.run_id),turn.after_sequence,1000,false,Some(metadata),&request_id(&headers),None).await);}
 let parts:Vec<_>=path.trim_start_matches("/api/v1/assistant/").split('/').collect();match(method.as_str(),parts.as_slice()){
 ("GET",["runs",run])=>{if let Some(row)=super::sdk_approvals::run_status(&state.repository.pool,&actor,run).await?{return Ok(json_response(200,json!({"run":row})));}let id=uuid::Uuid::parse_str(run).map_err(|_|AgentError::code(404,"RUN_NOT_FOUND"))?.to_string();let mut row:Value=sqlx::query_scalar("SELECT jsonb_build_object('run_id',run_id,'tenant_id',tenant_id,'user_id',user_id,'session_id',session_id,'status',status,'engine',engine,'usage',usage,'error',error,'started_at',started_at,'finished_at',finished_at,'updated_at',updated_at,'harness_thread_id',harness_thread_id,'harness_turn_id',harness_turn_id,'kernel_revision',kernel_revision,'capability_revision',capability_revision) FROM assistant_runs WHERE run_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND engine='agent_runtime'").bind(id).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(&state.repository.pool).await?.ok_or_else(||AgentError::code(404,"RUN_NOT_FOUND"))?;row["terminal_reason"]=if row["status"]=="cancelled"&&row["error"]=="AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED"{json!("runtime_restart_interrupted")}else{Value::Null};row["error"]=Value::Null;if let Some(raw)=row["usage"].as_str(){if let Ok(usage)=serde_json::from_str(raw){row["usage"]=usage;}}Ok(json_response(200,json!({"run":row})))},
 ("POST",["runs",_,"resume"])=>{state.edge_rate.check(&actor,"assistant_resume").await?;Err(AgentError::code(409,"AGENT_RUNTIME_ONLY"))},
 ("POST",["tasks",task,"cancel"])=>{if let Some(row)=super::sdk_approvals::cancel_run(&state.repository.pool,&actor,task).await?{return Ok(json_response(200,row));}let uuid=uuid::Uuid::parse_str(task).ok().map(|v|v.to_string());let row:Value=sqlx::query_scalar("SELECT jsonb_build_object('run_id',run_id,'session_id',session_id,'harness_thread_id',harness_thread_id,'harness_turn_id',harness_turn_id,'status',status) FROM assistant_runs WHERE tenant_id=$1 AND user_id=$2 AND engine='agent_runtime' AND(($3::uuid IS NOT NULL AND run_id=$3::uuid) OR harness_turn_id=$4) ORDER BY started_at DESC LIMIT 1").bind(&actor.tenant_id).bind(&actor.user_id).bind(uuid).bind(*task).fetch_optional(&state.repository.pool).await?.ok_or_else(||AgentError::code(404,"TASK_NOT_FOUND"))?;let status=text(&row,"status");let session=text(&row,"session_id");if !matches!(status,"running"|"pending"|"queued"|"awaiting_approval"){return Ok(json_response(200,json!({"task_id":task,"session_id":session,"cancelled":status=="cancelled","message":"Task is already complete"})));}let turn=row["harness_turn_id"].as_str().filter(|s|!s.is_empty()).unwrap_or(text(&row,"run_id"));if text(&row,"harness_thread_id").is_empty()||turn.is_empty()||session.is_empty(){return Err(AgentError::code(503,"AGENT_RUNTIME_TASK_MAPPING_UNAVAILABLE"));}state.control.interrupt_turn(&actor.scope(session),text(&row,"harness_thread_id"),turn,payload["reason"].as_str().unwrap_or("client_interrupt")).await?;Ok(json_response(200,json!({"task_id":task,"session_id":session,"cancelled":true,"message":"Cancellation requested"})))},
 ("GET",["approvals",id])=>{Ok(json_response(200,super::sdk_approvals::get(&state.repository.pool,&actor,id).await?))},
 ("POST",["approvals",id])=>{let approved=payload["approved"].as_bool().ok_or_else(||AgentError::code(422,"INVALID_APPROVAL_DECISION"))?;let row:Value=sqlx::query_scalar("SELECT jsonb_build_object('engine',r.engine,'session_id',r.session_id,'harness_thread_id',r.harness_thread_id) FROM assistant_tool_approvals a JOIN assistant_runs r ON r.run_id=a.run_id WHERE a.approval_id=$1::uuid AND a.tenant_id=$2 AND a.user_id=$3").bind(super::uuid(id)?).bind(&actor.tenant_id).bind(&actor.user_id).fetch_optional(&state.repository.pool).await?.ok_or_else(||AgentError::code(404,"APPROVAL_NOT_FOUND"))?;if matches!(row["engine"].as_str(),Some("openai_agents"|"claude_agent_sdk")){return Ok(json_response(200,super::sdk_approvals::decide(&state.repository.pool,&actor,id,approved,payload["reason"].as_str(),&payload).await?));}if ["expected_run_id","expected_tool_call_id","expected_arguments_hash"].iter().any(|key|payload.get(*key).is_some()){return Err(AgentError::code(422,"SDK_APPROVAL_BINDING_ONLY"));}if row["engine"]!="agent_runtime"{return Err(AgentError::code(409,"AGENT_RUNTIME_ONLY"));}bounded(&payload,"reason",0,usize::MAX,false)?;let scope=actor.scope(text(&row,"session_id"));let mut summary=state.control.get_approval(&scope,id).await?.ok_or_else(||AgentError::code(404,"APPROVAL_NOT_FOUND"))?;let receipt_only=approved&&approval::requires_receipt_readback(&summary);if approved{state.sources.require_conversation(&scope,true).await?;if !receipt_only{let preview=approval::owner_approval_preview(&state.repository.pool,&scope,text(&row,"harness_thread_id"),id,&summary).await;if preview["can_approve"]!=true{return Err(AgentError::code(409,"APPROVAL_ACTION_UNVERIFIED"));}}}if receipt_only{state.control.read_approval_decision(&scope,text(&row,"harness_thread_id"),id,approved,payload["reason"].as_str()).await?;}else{state.control.decide_approval(&scope,id,approved,payload["reason"].as_str()).await?;}summary["status"]=if approved{"approved"}else{"rejected"}.into();summary["approved"]=approved.into();summary["reason"]=payload["reason"].clone();Ok(json_response(200,json!({"approval":summary})))},_=>Err(AgentError::code(404,"NOT_FOUND"))}
 }.await;
    match result {
        Ok(response) => response,
        Err(e) => e.into_response(),
    }
}
async fn public_runtime(
    State(state): State<AgentState>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let Some(public) = state.public.clone() else {
        return AgentError::code(503, "AGENT_RUNTIME_UNAVAILABLE").into_response();
    };
    public.dispatch(&state, query, request).await
}

#[cfg(test)]
mod provider_selection_tests {
    use super::*;

    #[test]
    fn thread_and_turn_accept_the_same_optional_provider() {
        assert!(validate_thread_create_body(&json!({"model_id":"same","provider_id":"second"})).is_ok());
        assert!(validate_turn_body(&json!({"message":"hello","model_id":"same","provider_id":"second"})).is_ok());
        assert!(validate_thread_create_body(&json!({"model_id":"unique"})).is_ok());
    }

    #[test]
    fn provider_cannot_override_a_pinned_version() {
        let value = json!({"agent_id":"agent","agent_version_id":"version","provider_id":"other"});
        let error = validate_thread_create_body(&value).unwrap_err();
        assert_eq!(error.status, 422);
        assert_eq!(error.detail["code"], "AGENT_RUNTIME_VERSION_TARGET_INVALID");
    }

    #[test]
    fn invalid_provider_identity_is_rejected_before_thread_work() {
        for provider in [json!(""), json!(123), json!("x".repeat(256))] {
            assert!(validate_thread_create_body(&json!({"provider_id":provider})).is_err());
        }
    }
}

#[cfg(test)]
mod approval_reason_tests {
    use super::*;

    #[test]
    fn runtime_v1_reason_preserves_null_empty_and_existing_long_strings() {
        for payload in [
            json!({}),
            json!({"reason":null}),
            json!({"reason":""}),
            json!({"reason":"original"}),
            json!({"reason":"x".repeat(501)}),
        ] {
            assert!(bounded(&payload, "reason", 0, usize::MAX, false).is_ok());
        }
        assert_ne!(
            json!({"reason":null})["reason"].as_str(),
            json!({"reason":""})["reason"].as_str()
        );
    }

    #[test]
    fn runtime_v1_malformed_reason_cannot_alias_a_null_receipt() {
        for reason in [json!(true), json!(42), json!([]), json!({})] {
            let error =
                bounded(&json!({"reason":reason}), "reason", 0, usize::MAX, false).unwrap_err();
            assert_eq!(error.status, 422);
            assert_eq!(error.detail["code"], "INVALID_REQUEST_FIELD");
        }
    }
}
