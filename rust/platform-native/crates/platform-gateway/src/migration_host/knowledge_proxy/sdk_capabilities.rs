//! SDK provenance admission and current-policy revalidation. The existing Rust
//! Runtime alone prepares approvals and signs the existing Worker lease contract.
use super::*;
use crate::migration_agent::control::execution_transport::WorkloadSend;
use crate::migration_agent::{Scope, control::RuntimeControlPlane};
use ai_platform_gateway_foundation::sdk_capabilities::canonical_hash;
use ai_platform_gateway_foundation::{
    protocol::{ServiceDefinition, UnifiedRequest},
    sdk_capabilities::{AdmissionFuture, SdkCapabilityAuthority, SdkCapabilityLease, selected},
    sdk_read_grants::BoundRead,
};
use axum::{Json, extract::State, http::HeaderMap};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Weak;
use tokio::sync::oneshot;

pub struct Authority {
    pub proxy: Weak<KnowledgeProxy>,
}
fn bad() -> ContentError {
    code(403, "sdk_capability_authority_unavailable")
}
fn control(state: &KnowledgeProxy) -> Result<&Arc<RuntimeControlPlane>> {
    state.sdk_control.as_ref().ok_or_else(bad)
}
fn scope(bound: &BoundRead) -> Scope {
    Scope {
        tenant_id: bound.tenant_id.clone(),
        user_id: bound.user_id.clone(),
        session_id: bound.session_id.clone(),
    }
}
async fn readonly(
    state: &KnowledgeProxy,
    bound: &BoundRead,
    service: &ServiceDefinition,
    revision: i64,
) -> Result<Value> {
    let ids = selected(service).map_err(|_| bad())?;
    if ids != bound.capability_ids {
        return Err(bad());
    }
    // Explicit empty authority is valid; never expand an empty selection through the catalog.
    if ids.is_empty() {
        return Ok(
            json!({"schema_version":"agent-readonly-capability/v1","tenant_id":bound.tenant_id,"capability_revision":revision,"items":[],"tools":[],"mcp":[],"deferred":[],"capability_allowlist":[]}),
        );
    }
    let allow = ids.iter().map(|id| json!({"id":id})).collect::<Vec<_>>();
    let plane = control(state)?;
    let resolved = plane
        .catalog
        .fetch(
            &scope(bound),
            service.metadata["sdk_agent"]["model"]
                .as_str()
                .ok_or_else(bad)?,
            revision,
            Some(&allow),
        )
        .await
        .map_err(|_| bad())?;
    let mut result = json!({"schema_version":"agent-readonly-capability/v1","tenant_id":bound.tenant_id,"capability_revision":revision,"items":[],"tools":[],"mcp":[],"deferred":[],"capability_allowlist":[]});
    let mut seen = std::collections::BTreeSet::new();
    for key in ["tools", "mcp", "deferred"] {
        for descriptor in resolved[key].as_array().ok_or_else(bad)? {
            let id = descriptor["id"].as_str().ok_or_else(bad)?;
            if !ids.iter().any(|i| i == id) {
                continue;
            }
            let name = descriptor["name"].as_str().ok_or_else(bad)?;
            // These require Codex turn context, a separate memory policy or a
            // discovery expansion; a genuine SDK run has none of those grants.
            if matches!(
                name,
                "search_knowledge_base"
                    | "read_attachment"
                    | "context_compact"
                    | "update_user_memory"
                    | "tool_search"
                    | "tool_describe"
                    | "tool_call"
                    | "spawn_subagent"
            ) || !seen.insert(id.to_owned())
                || !descriptor["schema"].is_object()
                || descriptor["read_only"] == true && descriptor["approval_policy"] != "never"
                || descriptor["read_only"] != true && !plane.options.worker_writes_enabled
            {
                return Err(bad());
            }
            result[key].as_array_mut().unwrap().push(descriptor.clone());
            let mut binding = json!({"id":id,"name":name,"type":descriptor["kind"],"version":descriptor["version"],"schema_hash":descriptor["schema_hash"]});
            if let Some(connector) = descriptor.get("connector_binding") {
                binding["connector_binding"] = connector.clone();
            }
            result["capability_allowlist"]
                .as_array_mut()
                .unwrap()
                .push(binding);
        }
    }
    if seen.len() != ids.len() {
        return Err(bad());
    }
    Ok(result)
}
fn tools(readonly: &Value) -> Result<Vec<Value>> {
    let mut descriptors = ["tools", "mcp", "deferred"]
        .iter()
        .flat_map(|k| readonly[*k].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    descriptors.sort_by_key(|v| v["id"].as_str().unwrap_or(""));
    descriptors.into_iter().enumerate().map(|(index,d)|{
        let namespace=d["namespace"].as_str().filter(|s|!s.is_empty() && *s!="functions");
        if namespace.is_some_and(|s|s.len()>64){return Err(bad());}
        Ok(json!({"name":format!("platform_capability_{index}"),"tool":d["name"],"namespace":namespace,"description":d["description"],"input_schema":d["schema"],"id":d["id"],"schema_hash":d["schema_hash"],"effect":d["effect"]}))
    }).collect()
}
impl SdkCapabilityAuthority for Authority {
    fn admit<'a>(
        &'a self,
        request: &'a UnifiedRequest,
        service: &'a ServiceDefinition,
        grant: &'a str,
    ) -> AdmissionFuture<'a> {
        Box::pin(async move {
            let state = self
                .proxy
                .upgrade()
                .ok_or("sdk_capability_authority_unavailable")?;
            let work=async {
                let reads=state.sdk_reads.as_ref().ok_or_else(bad)?;
                let bound=reads.grants.bound(grant).map_err(|_|bad())?;
                if bound.request_id!=request.request_id || bound.service_id!=request.service_id || bound.session_id!=request.session_id.as_deref().unwrap_or("") {return Err(bad());}
                let _=super::sdk_reads::actor_bound(&state,&bound).await?;
                let plane=control(&state)?;
                let run=uuid::Uuid::new_v4();let owner=uuid::Uuid::new_v4();let snapshot_id=uuid::Uuid::new_v4();
                let revision=chrono::Utc::now().timestamp_millis();
                let readonly=readonly(&state,&bound,service,revision).await?;
                let exposed=tools(&readonly)?;
                let mut snapshot=json!({"schema_version":"sdk-capability-snapshot/v1","run_id":run,"tenant_id":bound.tenant_id,"user_id":bound.user_id,"session_id":bound.session_id,"capability_revision":revision,"sdk_agent":service.metadata["sdk_agent"],"sdk_tools":service.metadata.get("sdk_tools").cloned().unwrap_or_else(||json!({})),"readonly_capabilities":readonly});
                // Gateway issues the ceiling; the SDK sees neither root private keys nor open witnesses.
                let ceiling=crate::migration_agent::lease_authority::issue_ceiling(&scope(&bound),&run.to_string(),&snapshot_id.to_string(),"",revision as u64,&readonly,(chrono::Utc::now().timestamp_millis().max(0) as u64).saturating_add(1_800_000)).map_err(|_|bad())?;
                if let Some(token)=&ceiling { snapshot["capability_ceiling"]=serde_json::to_value(&token.root).map_err(|_|bad())?; }
                let mut authority_tx=plane.pool.begin().await.map_err(|_|bad())?;
                let request_hash=canonical_hash(&json!({"request_id":request.request_id,"service_id":request.service_id,"tenant_id":request.tenant_id,"user_id":request.user_id,"session_id":request.session_id,"inputs":request.inputs,"agent":service.metadata["sdk_agent"],"tools":service.metadata["sdk_tools"]}));
                sqlx::query("SELECT assistant.issue_sdk_execution($1::uuid,$2,$3,$4,$5,$6,$7,$8::char(64),$9::uuid,$10,$11,$12::char(64),$13::uuid)")
                    .bind(run.to_string()).bind(&bound.tenant_id).bind(&bound.user_id).bind(&bound.session_id).bind(&bound.service_id).bind(&bound.request_id)
                    .bind(service.metadata["adapter_type"].as_str().ok_or_else(bad)?).bind(request_hash).bind(snapshot_id.to_string()).bind(revision).bind(&snapshot).bind(canonical_hash(&snapshot)).bind(owner.to_string())
                    .execute(&mut *authority_tx).await.map_err(|_|code(409,"sdk_run_reconciliation_required"))?;
                if let Some(token)=&ceiling {
                    sqlx::query("INSERT INTO assistant.assistant_runtime_capability_witnesses(run_id,tenant_id,user_id,session_id,snapshot_id,token) VALUES($1,$2,$3,$4,$5,$6)")
                        .bind(run).bind(&bound.tenant_id).bind(&bound.user_id).bind(&bound.session_id).bind(snapshot_id).bind(serde_json::to_value(token).map_err(|_|bad())?)
                        .execute(&mut *authority_tx).await.map_err(|_|bad())?;
                }
                authority_tx.commit().await.map_err(|_|bad())?;
                let (sender,mut receiver)=oneshot::channel();
                let weak=self.proxy.clone();let owned_grant=grant.to_owned();let pool=plane.pool.clone();
                // The renewal task is tied to the response body guard. Neither
                // SDK progress nor an approval poll can extend the hard deadline.
                tokio::spawn(async move {
                    let mut tick=tokio::time::interval(Duration::from_secs(10));
                    let terminal=loop {
                        tokio::select! {
                            biased;
                            status=&mut receiver=>break status.unwrap_or("cancelled"),
                            _=tick.tick()=>{
                                let Some(state)=weak.upgrade() else{break "cancelled"};
                                if current(&state,&owned_grant,&run.to_string()).await.is_err(){break receiver.try_recv().unwrap_or("cancelled");}
                                let renewed=sqlx::query_scalar::<_,bool>("SELECT assistant.renew_sdk_execution($1::uuid,$2::uuid,1)").bind(run.to_string()).bind(owner.to_string()).fetch_one(&pool).await;
                                if !matches!(renewed,Ok(true)){break receiver.try_recv().unwrap_or("cancelled");}
                            }
                        }
                    };
                    let _=sqlx::query("SELECT assistant.finish_sdk_execution($1::uuid,$2::uuid,1,$3)").bind(run.to_string()).bind(owner.to_string()).bind(terminal).execute(&pool).await;
                });
                Ok(SdkCapabilityLease{bridge:json!({"grant":grant,"run_id":run,"tools":exposed,"timeout_seconds":if bound.capability_ids.is_empty(){300}else{1800}}),finished:Some(sender)})
            }.await;
            work.map_err(|_| "sdk_capability_admission_failed")
        })
    }
}
pub(super) async fn current(
    state: &KnowledgeProxy,
    grant: &str,
    run: &str,
) -> Result<(BoundRead, Value)> {
    let reads = state.sdk_reads.as_ref().ok_or_else(bad)?;
    let bound = reads.grants.bound(grant).map_err(|_| bad())?;
    let _ = super::sdk_reads::actor_bound(state, &bound).await?;
    let plane = control(state)?;
    let id = uuid::Uuid::parse_str(run).map_err(|_| bad())?;
    let row:Value=sqlx::query_scalar("SELECT to_jsonb(s) FROM assistant.assistant_sdk_runs s WHERE run_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND session_id=$4 AND service_id=$5 AND request_id=$6")
        .bind(id.to_string()).bind(&bound.tenant_id).bind(&bound.user_id).bind(&bound.session_id).bind(&bound.service_id).bind(&bound.request_id)
        .fetch_optional(&plane.pool).await.map_err(|_|bad())?.ok_or_else(bad)?;
    let owner =
        uuid::Uuid::parse_str(row["owner_id"].as_str().ok_or_else(bad)?).map_err(|_| bad())?;
    let _ = sqlx::query_scalar::<_, bool>(
        "SELECT assistant.assert_sdk_execution($1::uuid,$2,$3,$4,$5::uuid,$6,TRUE)",
    )
    .bind(id.to_string())
    .bind(&bound.tenant_id)
    .bind(&bound.user_id)
    .bind(&bound.session_id)
    .bind(owner.to_string())
    .bind(row["fence"].as_i64().ok_or_else(bad)?)
    .fetch_one(&plane.pool)
    .await
    .map_err(|_| bad())?;
    let service = reads
        .registry
        .get(&bound.service_id)
        .await
        .map_err(|_| bad())?
        .ok_or_else(bad)?
        .snapshot();
    let live = readonly(
        state,
        &bound,
        &service,
        row["capability_revision"].as_i64().ok_or_else(bad)?,
    )
    .await?;
    if live != row["snapshot"]["readonly_capabilities"]
        || canonical_hash(&row["snapshot"]) != row["snapshot_sha256"].as_str().unwrap_or("").trim()
    {
        return Err(bad());
    }
    Ok((bound, row))
}
fn effect_free_snapshot(snapshot: &Value) -> bool {
    let Some(config) = snapshot["sdk_tools"].as_object() else {
        return false;
    };
    if config
        .keys()
        .any(|key| !matches!(key.as_str(), "knowledge_dataset_ids" | "capability_ids"))
    {
        return false;
    }
    for key in ["knowledge_dataset_ids", "capability_ids"] {
        if config
            .get(key)
            .is_some_and(|value| !value.as_array().is_some_and(Vec::is_empty))
        {
            return false;
        }
    }
    let readonly = &snapshot["readonly_capabilities"];
    readonly["schema_version"] == "agent-readonly-capability/v1"
        && ["items", "tools", "mcp", "deferred", "capability_allowlist"]
            .iter()
            .all(|key| {
                readonly
                    .get(*key)
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
            })
}
fn durable_cancelled_effect_free_run(prior: &Value, engine: &str) -> bool {
    prior["engine"] == engine
        && prior["status"] == "cancelled"
        && !prior["finished_at"].is_null()
        && effect_free_snapshot(&prior["snapshot"])
}
fn cancelled_usage_receipt(
    rows: &[Value],
    bound: &BoundRead,
    request_id: &str,
    run: &str,
    engine: &str,
) -> bool {
    if rows.len() != 2 {
        return false;
    }
    let admission = rows.iter().find(|row| row["action"] == "admitted");
    let cancellation = rows.iter().find(|row| row["action"] == "cancelled");
    let (Some(admission), Some(cancellation)) = (admission, cancellation) else {
        return false;
    };
    let request = &admission["request_summary"];
    if request != &cancellation["request_summary"]
        || request["schema"] != "sdk-usage-audit/v1"
        || request["assistant_run_id"] != run
        || request["tenant_id"] != bound.tenant_id
        || request["user_id"] != bound.user_id
        || request["service_id"] != bound.service_id
        || request["request_id"] != request_id
        || request["session_id"] != bound.session_id
        || request["framework"] != engine
    {
        return false;
    }
    let response = &cancellation["response_summary"];
    response["local_execution_status"] == "cancelled"
        && response["provider_outcome"] == "unknown"
        && response["usage_state"] == "unavailable"
        && response["cost_state"] == "unavailable"
        && response["platform_pricing_state"] == "reconciliation_required"
        && response["accounting_status"] == "not_asserted"
}
async fn cancelled_run_is_reconcilable(
    state: &KnowledgeProxy,
    bound: &BoundRead,
    request_id: &str,
    engine: &str,
) -> Result<bool> {
    if request_id.is_empty()
        || request_id.len() > 255
        || request_id.chars().any(char::is_control)
        || request_id == bound.request_id
    {
        return Ok(false);
    }
    let plane = control(state)?;
    let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('run_id',s.run_id,'engine',s.engine,'status',r.status,'finished_at',r.finished_at,'snapshot',s.snapshot) FROM assistant.assistant_sdk_runs s JOIN assistant.assistant_runs r ON r.run_id=s.run_id AND r.tenant_id=s.tenant_id AND r.user_id=s.user_id AND r.session_id=s.session_id AND r.engine=s.engine WHERE s.request_id=$1 AND s.tenant_id=$2 AND s.user_id=$3 AND s.session_id=$4 AND s.service_id=$5 AND s.engine=$6 ORDER BY r.started_at DESC LIMIT 2")
        .bind(request_id).bind(&bound.tenant_id).bind(&bound.user_id).bind(&bound.session_id).bind(&bound.service_id).bind(engine)
        .fetch_all(&plane.pool).await.map_err(|_|bad())?;
    if rows.len() != 1 {
        return Ok(false);
    }
    let prior = &rows[0];
    if !durable_cancelled_effect_free_run(prior, engine) {
        return Ok(false);
    }
    let run = prior["run_id"].as_str().ok_or_else(bad)?;
    let has_tool_calls: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM assistant.assistant_sdk_tool_calls WHERE run_id=$1::uuid)",
    )
    .bind(run)
    .fetch_one(&plane.pool)
    .await
    .map_err(|_| bad())?;
    if has_tool_calls {
        return Ok(false);
    }
    let identity = json!({"tenant_id":bound.tenant_id,"user_id":bound.user_id,"service_id":bound.service_id,"request_id":request_id,"session_id":bound.session_id,"framework":engine});
    let resource = canonical_hash(&identity);
    let audits:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('action',action,'request_summary',request_summary,'response_summary',response_summary) FROM gateway.audit_logs WHERE event_type='sdk_usage_observation' AND resource_type='sdk_request' AND resource_id=$1 AND tenant_id=$2 AND user_id=$3 AND action IN ('admitted','completed','cancelled','failed') ORDER BY id LIMIT 3")
        .bind(resource).bind(&bound.tenant_id).bind(&bound.user_id).fetch_all(&plane.pool).await.map_err(|_|bad())?;
    Ok(cancelled_usage_receipt(
        &audits, bound, request_id, run, engine,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Admit {
    run_id: String,
    #[serde(default)]
    reconcile_pending_request_id: Option<String>,
}
pub async fn admit(
    State(state): State<Arc<KnowledgeProxy>>,
    headers: HeaderMap,
    Json(input): Json<Admit>,
) -> Result<Json<Value>> {
    let grant = super::sdk_reads::token(&headers)?;
    let (bound, current_run) = current(&state, grant, &input.run_id).await?;
    let mut result = json!({"authorized":true});
    if let Some(request_id) = input.reconcile_pending_request_id {
        let safe = cancelled_run_is_reconcilable(
            &state,
            &bound,
            &request_id,
            current_run["engine"].as_str().ok_or_else(bad)?,
        )
        .await?;
        result["pending_reconcilable"] = json!(safe);
    }
    Ok(Json(result))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execute {
    run_id: String,
    tool_call_id: String,
    tool: String,
    namespace: Option<String>,
    arguments: Value,
}
// Correlation is separate from the admitted SDK identity and owner fence. Never
// invent a managed turn/execution ID from an SDK run or SDK tool-call ID.
fn correlate_runtime_request(
    mut request: reqwest::RequestBuilder,
    run_id: &str,
) -> Result<reqwest::RequestBuilder> {
    let correlation = crate::migration_host::context::internal_http_headers(
        Default::default(),
        Some(run_id),
        None,
        None,
    )
    .map_err(|_| bad())?;
    for (name, value) in correlation {
        request = request.header(name, value);
    }
    Ok(request)
}
pub async fn execute(
    State(state): State<Arc<KnowledgeProxy>>,
    headers: HeaderMap,
    Json(input): Json<Execute>,
) -> Result<Json<Value>> {
    let grant = super::sdk_reads::token(&headers)?;
    if input.tool_call_id.is_empty()
        || input.tool_call_id.len() > 160
        || input.tool.len() > 160
        || !input.arguments.is_object()
        || input.arguments.to_string().len() > 65536
    {
        return Err(bad());
    }
    let (bound, row) = current(&state, grant, &input.run_id).await?;
    let plane = control(&state)?;
    let request=plane.client.post(format!("{}/internal/v1/sdk-runs/{}/capabilities",plane.options.runtime_url.trim_end_matches('/'),input.run_id))
        .header("x-ai-tenant-id",&bound.tenant_id).header("x-ai-user-id",&bound.user_id).header("x-ai-session-id",&bound.session_id)
        .header("x-execution-owner-id",row["owner_id"].as_str().ok_or_else(bad)?).header("x-execution-fence",row["fence"].as_i64().ok_or_else(bad)?.to_string())
        .json(&json!({"tool_call_id":input.tool_call_id,"tool":input.tool,"namespace":input.namespace,"arguments":input.arguments}))
        .timeout(Duration::from_secs(45));
    let request = if plane.options.runtime_internal_token.is_empty() {
        request
    } else {
        let mut token = axum::http::HeaderValue::from_str(&plane.options.runtime_internal_token)
            .map_err(|_| bad())?;
        token.set_sensitive(true);
        request.header("x-ai-platform-internal-token", token)
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut response = correlate_runtime_request(request, &input.run_id)?
        .workload_send()
        .await
        .map_err(|_| code(502, "sdk_capability_runtime_unavailable"))?;
    if !response.status().is_success() {
        return Err(code(409, "sdk_capability_runtime_rejected"));
    }
    let mut bytes = vec![];
    while let Some(chunk) = tokio::time::timeout_at(deadline, response.chunk())
        .await
        .map_err(|_| bad())?
        .map_err(|_| bad())?
    {
        if bytes.len() + chunk.len() > 1048576 {
            return Err(bad());
        }
        bytes.extend_from_slice(&chunk);
    }
    let mut result: Value = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if result["run_id"] != input.run_id || result["tool_call_id"] != input.tool_call_id {
        return Err(bad());
    }
    // Never expose a cached capability result after user/service revocation.
    current(&state, grant, &input.run_id).await?;
    if result["status"] == "approval_required" {
        result["preview"] = crate::migration_agent::sdk_approvals::preview(
            &plane.pool,
            &bound.tenant_id,
            &bound.user_id,
            result["approval_id"].as_str().ok_or_else(bad)?,
        )
        .await
        .map_err(|_| bad())?;
        result.as_object_mut().ok_or_else(bad)?.remove("arguments");
    }
    Ok(Json(result))
}

#[cfg(test)]
#[path = "sdk_capability_trace_tests.rs"]
mod trace_tests;

#[cfg(test)]
mod pending_reconciliation_tests {
    use super::*;
    use axum::http::HeaderValue;

    fn bound() -> BoundRead {
        BoundRead {
            tenant_id: "tenant-a".into(),
            user_id: "user-a".into(),
            service_id: "service-a".into(),
            request_id: "next-request".into(),
            session_id: "session-a".into(),
            dataset_ids: vec![],
            capability_ids: vec![],
            authorization: HeaderValue::from_static("Bearer fixture-jwt"),
        }
    }
    fn snapshot() -> Value {
        json!({"sdk_tools":{"knowledge_dataset_ids":[],"capability_ids":[]},"readonly_capabilities":{
            "schema_version":"agent-readonly-capability/v1",
            "items":[],"tools":[],"mcp":[],"deferred":[],"capability_allowlist":[]
        }})
    }
    fn audit_rows() -> Vec<Value> {
        let request = json!({"schema":"sdk-usage-audit/v1","assistant_run_id":"10000000-0000-4000-8000-000000000002",
            "tenant_id":"tenant-a","user_id":"user-a","service_id":"service-a","request_id":"cancelled-request",
            "session_id":"session-a","framework":"openai_agents","request_sha256":"a".repeat(64)});
        let response = json!({"local_execution_status":"cancelled","provider_outcome":"unknown","usage_state":"unavailable",
            "cost_state":"unavailable","platform_pricing_state":"reconciliation_required","accounting_status":"not_asserted"});
        vec![
            json!({"action":"admitted","request_summary":request.clone(),"response_summary":{"local_execution_status":"admitted"}}),
            json!({"action":"cancelled","request_summary":request,"response_summary":response}),
        ]
    }

    #[test]
    fn only_exact_durable_effect_free_cancellation_is_reconcilable() {
        let bound = bound();
        let rows = audit_rows();
        assert!(effect_free_snapshot(&snapshot()));
        assert!(cancelled_usage_receipt(
            &rows,
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
        assert!(!cancelled_usage_receipt(
            &rows,
            &bound,
            "other-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
        assert!(!cancelled_usage_receipt(
            &rows,
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000003",
            "openai_agents"
        ));
        assert!(!cancelled_usage_receipt(
            &rows[..1],
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
        let mut cross_scope = rows.clone();
        cross_scope[1]["request_summary"]["tenant_id"] = json!("other-tenant");
        assert!(!cancelled_usage_receipt(
            &cross_scope,
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
        let mut duplicate = rows.clone();
        duplicate.push(rows[1].clone());
        assert!(!cancelled_usage_receipt(
            &duplicate,
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
        let mut completed = rows.clone();
        completed[1]["action"] = json!("completed");
        assert!(!cancelled_usage_receipt(
            &completed,
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
        let mut known_cost = rows.clone();
        known_cost[1]["response_summary"]["provider_outcome"] = json!("completed");
        known_cost[1]["response_summary"]["usage_state"] = json!("reported");
        assert!(!cancelled_usage_receipt(
            &known_cost,
            &bound,
            "cancelled-request",
            "10000000-0000-4000-8000-000000000002",
            "openai_agents"
        ));
    }

    #[test]
    fn snapshots_with_any_read_or_capability_tools_are_not_effect_free() {
        for key in ["knowledge_dataset_ids", "capability_ids"] {
            let mut value = snapshot();
            value["sdk_tools"][key] = json!(["selected"]);
            assert!(!effect_free_snapshot(&value));
        }
        let mut value = snapshot();
        value["readonly_capabilities"]["tools"] = json!([{"effect":"read"}]);
        assert!(!effect_free_snapshot(&value));
        let mut value = snapshot();
        value["readonly_capabilities"]
            .as_object_mut()
            .unwrap()
            .remove("mcp");
        assert!(!effect_free_snapshot(&value));
    }
    #[test]
    fn only_terminal_cancelled_same_engine_runs_are_eligible() {
        let base = json!({"engine":"openai_agents","status":"cancelled","finished_at":"now","snapshot":snapshot()});
        assert!(durable_cancelled_effect_free_run(&base, "openai_agents"));
        for status in ["running", "completed", "failed"] {
            let mut row = base.clone();
            row["status"] = json!(status);
            assert!(!durable_cancelled_effect_free_run(&row, "openai_agents"));
        }
        let mut wrong_engine = base.clone();
        wrong_engine["engine"] = json!("claude_agent_sdk");
        assert!(!durable_cancelled_effect_free_run(
            &wrong_engine,
            "openai_agents"
        ));
        let mut unfinished = base.clone();
        unfinished["finished_at"] = Value::Null;
        assert!(!durable_cancelled_effect_free_run(
            &unfinished,
            "openai_agents"
        ));
    }
}
