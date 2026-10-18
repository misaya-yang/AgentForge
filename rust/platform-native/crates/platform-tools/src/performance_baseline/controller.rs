use super::{
    Result,
    config::{Case, Check, Kind, ProviderScope},
    ensure, evidence,
    identity::{self, Prepared},
    mock, sha, trace,
};
use ai_platform_service_primitives::data_connection::{
    ConnectionConfig, ConnectionPurpose, DatabaseRole, TenantScope, bind_transaction,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Default)]
struct State {
    clock: Option<Instant>,
    rows: Vec<Value>,
    resources: BTreeMap<String, Vec<Value>>,
    resource_errors: Vec<String>,
    private_resources: Vec<Value>,
}
type Shared = Arc<Mutex<State>>;
struct Sampler(tokio::task::JoinHandle<()>);
impl Drop for Sampler {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn sample(p: &Prepared, state: Shared, stop: Arc<AtomicBool>) -> Sampler {
    let services = p.accepted.services.clone();
    let limits = p.profile.budgets.rss_stop_bytes.clone();
    let millis = p.profile.budgets.sample_ms;
    let max = p.profile.budgets.sample_limit;
    Sampler(tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(millis));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        for _ in 0..max {
            tick.tick().await;
            if stop.load(Ordering::Acquire) {
                break;
            }
            for s in &services {
                let point = crate::performance::ProcessIdentity {
                    pid: s.pid,
                    start_ticks: s.start_ticks,
                }
                .sample();
                let mut st = state.lock().expect("baseline state poisoned");
                match point {
                    Ok(point) => {
                        if limits.get(&s.id).is_some_and(|limit| {
                            point["rss_bytes"].as_u64().is_some_and(|rss| rss > *limit)
                        }) {
                            st.resource_errors.push("service_rss_safety_limit".into());
                            stop.store(true, Ordering::Release);
                        }
                        st.resources.entry(s.id.clone()).or_default().push(point);
                    }
                    Err(_) => {
                        st.resource_errors
                            .push("service_identity_or_sampler_failed".into());
                        stop.store(true, Ordering::Release);
                    }
                }
            }
        }
        if !stop.load(Ordering::Acquire) {
            if let Ok(mut st) = state.lock() {
                st.resource_errors
                    .push("service_sampling_budget_exhausted".into());
            }
            stop.store(true, Ordering::Release);
        }
    }))
}
pub struct Http {
    pub started: Instant,
    pub status: u16,
    pub failure: Option<&'static str>,
    pub elapsed_ms: f64,
    pub headers_ms: f64,
    pub first_body_ms: Option<f64>,
    pub bytes: usize,
    pub body: Value,
    pub request_id: Option<String>,
    pub response_trace_id: Option<String>,
}
pub fn client(request_ms: u64) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_millis(request_ms.min(5000)))
        .timeout(Duration::from_millis(request_ms))
        .pool_max_idle_per_host(1)
        .build()
        .map_err(|_| "http_client_configuration".into())
}
fn client_for(p: &Prepared, target: &str) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_millis(
            p.profile.budgets.request_ms.min(5000),
        ))
        .timeout(Duration::from_millis(p.profile.budgets.request_ms))
        .pool_max_idle_per_host(1);
    if let Some(t) = p.profile.transports.get(target) {
        if let Some(file) = &t.ca_file {
            let bytes = evidence::read(&file.path, false, 65536)?;
            ensure(sha(&bytes) == file.sha256, "transport_ca_changed")?;
            builder = builder.add_root_certificate(
                reqwest::Certificate::from_pem(&bytes).map_err(|_| "transport_ca_invalid")?,
            );
        }
        if let Some(file) = &t.identity_file {
            let bytes = evidence::read(&file.path, true, 65536)?;
            ensure(sha(&bytes) == file.sha256, "transport_identity_changed")?;
            builder = builder.identity(
                reqwest::Identity::from_pem(&bytes).map_err(|_| "transport_identity_invalid")?,
            );
        }
    }
    builder
        .build()
        .map_err(|_| "http_transport_configuration".into())
}
pub async fn http(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    method: reqwest::Method,
    token: Option<&str>,
    body: Option<&Value>,
    parent: Option<&str>,
    limit: usize,
) -> Result<Http> {
    http_observed(
        client, origin, path, method, token, body, parent, limit, None,
    )
    .await
}
async fn http_observed(
    client: &reqwest::Client,
    origin: &str,
    path: &str,
    method: reqwest::Method,
    token: Option<&str>,
    body: Option<&Value>,
    parent: Option<&str>,
    limit: usize,
    observed: Option<(&Shared, usize)>,
) -> Result<Http> {
    ensure(
        path.starts_with('/') && !path.starts_with("//") && !path.contains(['?', '#', '\\']),
        "request_path_rejected",
    )?;
    let start = Instant::now();
    let mut request = client
        .request(method, format!("{origin}{path}"))
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    if let Some(parent) = parent {
        let parts = parent.split('-').collect::<Vec<_>>();
        ensure(
            parts.len() == 4
                && parts[0] == "00"
                && parts[1].len() == 32
                && parts[2].len() == 16
                && parts[3].len() == 2
                && parts
                    .iter()
                    .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit())),
            "request_traceparent_shape",
        )?;
        request = request
            .header("traceparent", parent)
            .header("x-request-id", format!("perf-{}", &parent[3..35]));
    }
    let mut response = request
        .send()
        .await
        .map_err(|_| "http_transport_or_timeout")?;
    let headers_ms = start.elapsed().as_secs_f64() * 1000.0;
    let status = response.status().as_u16();
    let request_id = response
        .headers()
        .get("x-gateway-request-id")
        .and_then(|h| h.to_str().ok())
        .filter(|s| super::config::safe_id(s))
        .map(str::to_owned);
    let response_trace_id = response
        .headers()
        .get("traceparent")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.split('-').nth(1))
        .filter(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned);
    if let Some((state, index)) = observed {
        mutate(
            state,
            index,
            "http",
            json!({"status":status,"response_headers_ms":headers_ms,"body_state":"incomplete","body_bytes":0}),
        );
        if let Some(request_id) = &request_id {
            if let Ok(mut st) = state.lock() {
                st.private_resources.push(json!({"observation_index":index,"gateway_request_id":request_id,"state":"response_headers_observed_body_not_yet_complete"}));
            }
        }
    }
    let mut failure = if response.content_length().is_some_and(|n| n > limit as u64) {
        Some("http_response_body_budget")
    } else {
        None
    };
    let mut bytes = Vec::new();
    let mut first = None;
    let mut observed_bytes = 0usize;
    while failure.is_none() {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if !chunk.is_empty() && first.is_none() {
                    first = Some(start.elapsed().as_secs_f64() * 1000.0);
                }
                observed_bytes = observed_bytes.saturating_add(chunk.len());
                if let Some((state, index)) = observed {
                    mutate(
                        state,
                        index,
                        "http",
                        json!({"status":status,"response_headers_ms":headers_ms,"first_decoded_body_byte_ms":first,"elapsed_ms":start.elapsed().as_secs_f64()*1000.0,"body_state":"incomplete","body_bytes":observed_bytes}),
                    );
                }
                if bytes.len() + chunk.len() > limit {
                    failure = Some("http_response_body_budget");
                    break;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => break,
            Err(_) => {
                failure = Some("http_body_or_timeout");
                break;
            }
        }
    }
    let body = if failure.is_none() {
        match serde_json::from_slice(&bytes) {
            Ok(value) => value,
            Err(_) => {
                failure = Some("invalid_json_body");
                Value::Null
            }
        }
    } else {
        Value::Null
    };
    Ok(Http {
        started: start,
        status,
        failure,
        elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        headers_ms,
        first_body_ms: first,
        bytes: observed_bytes,
        body,
        request_id,
        response_trace_id,
    })
}
pub fn checks(body: &Value, checks: &[Check]) -> bool {
    !checks.is_empty()
        && checks
            .iter()
            .all(|c| body.pointer(&c.pointer) == Some(&c.equals))
}
pub fn substitute(value: &Value, correlation: &str) -> Value {
    match value {
        Value::String(s) => json!(s.replace("{{correlation}}", correlation)),
        Value::Array(a) => Value::Array(a.iter().map(|v| substitute(v, correlation)).collect()),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.clone(), substitute(v, correlation)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
fn flush(directory: &Path, state: &Shared) -> Result<()> {
    let s = state.lock().map_err(|_| "observation_state")?;
    evidence::save(directory, "observations.json", &json!(s.rows))?;
    evidence::save(directory, "resources.json", &json!(s.resources))?;
    evidence::save(
        directory,
        "private-state.json",
        &json!({"created_or_uncertain_resources":s.private_resources,"cleanup":"operator_owned_no_automatic_replay_or_delete"}),
    )
}
fn mutate(state: &Shared, index: usize, code: &str, value: Value) {
    if let Ok(mut s) = state.lock() {
        s.rows[index][code] = value;
    }
}
async fn fixtures(p: &Prepared, client: &reqwest::Client, token: &str) -> Result<()> {
    let identity = http(
        client,
        &p.profile.gateway_origin,
        "/api/v1/auth/me",
        reqwest::Method::GET,
        Some(token),
        None,
        None,
        p.profile.budgets.max_body_bytes,
    )
    .await?;
    ensure(
        identity.status == 200
            && identity.body["tenant_id"] == p.profile.tenant_id
            && identity.body["user_id"] == p.profile.user_id,
        "live_actor_tenant_mismatch",
    )?;
    for probe in &p.profile.fixture_checks {
        let response = http(
            client,
            &p.profile.gateway_origin,
            &probe.path,
            reqwest::Method::GET,
            Some(token),
            None,
            None,
            p.profile.budgets.max_body_bytes,
        )
        .await?;
        ensure(
            response.status == 200 && checks(&response.body, &probe.checks),
            "live_fixture_acl_or_identity_changed",
        )?;
    }
    Ok(())
}
async fn collect_trace(
    p: &Prepared,
    _client: &reqwest::Client,
    token: Option<&str>,
    trace_id: &str,
) -> Result<Vec<trace::Span>> {
    let client = client_for(p, "trace")?;
    let start = Instant::now();
    loop {
        let response = http(
            &client,
            &p.profile.trace_origin,
            &format!("/api/traces/{trace_id}"),
            reqwest::Method::GET,
            token,
            None,
            None,
            p.profile.budgets.max_body_bytes,
        )
        .await;
        if let Ok(r) = response {
            if r.status == 200 {
                return trace::spans(&r.body, trace_id, &p.accepted.services);
            }
            if r.status != 404 && r.status != 202 {
                return Err("trace_observation_rejected".into());
            }
        }
        if start.elapsed() >= Duration::from_millis(p.profile.budgets.trace_wait_ms) {
            return Err("correlated_trace_not_observed_before_deadline".into());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
async fn accounting_pool(p: &Prepared) -> Result<Option<sqlx::PgPool>> {
    let Some(config) = &p.profile.accounting else {
        return Ok(None);
    };
    let value = evidence::checked_json(&config.config_file, &config.config_sha256, true)?;
    let env: BTreeMap<String, String> =
        serde_json::from_value(value).map_err(|_| "accounting_connection_profile")?;
    let config = ConnectionConfig::from_environment(
        "PERFORMANCE",
        DatabaseRole::Gateway,
        ConnectionPurpose::TenantTransactions,
        &env,
    )
    .map_err(|_| "scoped_accounting_connection_rejected")?;
    ensure(
        config.max_connections == 1 && config.expected_host == "127.0.0.1",
        "one_local_scoped_accounting_connection_required",
    )?;
    config
        .connect()
        .await
        .map(Some)
        .map_err(|_| "accounting_role_or_mtls_verification_failed".into())
}
async fn accounting(
    p: &Prepared,
    pool: Option<&sqlx::PgPool>,
    request_id: Option<&str>,
    response_body: &Value,
) -> Value {
    let (Some(pool), Some(request)) = (pool, request_id) else {
        return json!({"status":"unknown","reason":"scoped_readonly_ledger_or_gateway_request_id_missing"});
    };
    let result = async {
        let mut tx = pool.begin().await.map_err(|_| "ledger_begin")?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|_| "ledger_readonly")?;
        let scope = TenantScope::new(&p.profile.tenant_id).map_err(|_| "ledger_tenant")?;
        bind_transaction(&mut tx, &scope, DatabaseRole::Gateway, 2000)
            .await
            .map_err(|_| "ledger_context")?;
        let rows:Vec<(String,Option<i32>,Option<i32>,i32,i32,Value)>=sqlx::query_as("SELECT status,input_tokens,output_tokens,input_cost_cents,output_cost_cents,metadata FROM gateway.usage_records WHERE tenant_id=$1 AND user_id=$2 AND request_id=$3")
 .bind(&p.profile.tenant_id).bind(&p.profile.user_id).bind(request).fetch_all(&mut *tx).await.map_err(|_|"ledger_read")?;
        tx.rollback().await.map_err(|_| "ledger_read_rollback")?;
        ensure(
            rows.len() == 1,
            "same_request_ledger_row_not_unique_or_not_yet_observed",
        )?;
        let row = &rows[0];
        ensure(
            row.0 == "success" && row.3 >= 0 && row.4 >= 0,
            "same_request_accounting_not_successful",
        )?;
        let tokens = response_body["usage"]["prompt_tokens"]
            .as_i64()
            .zip(response_body["usage"]["completion_tokens"].as_i64());
        let token_match = tokens.is_some_and(|(a, b)| {
            row.1.map(i64::from) == Some(a) && row.2.map(i64::from) == Some(b)
        });
        let request_match = row.1.is_none()
            && row.2.is_none()
            && row.5["measured_units"]["meter"] == "requests"
            && row.5["measured_units"]["requests"] == 1;
        ensure(
            token_match || request_match,
            "same_request_usage_does_not_match_provider_or_request_meter",
        )?;
        Ok::<Value, String>(
            json!({"status":"observed","scope":"exact_tenant_user_gateway_request_id","read_only_transaction":true,"input_tokens":row.1,"output_tokens":row.2,"input_cost_microcents":row.3,"output_cost_microcents":row.4}),
        )
    };
    match tokio::time::timeout(Duration::from_secs(5), result).await {
        Ok(Ok(v)) => v,
        _ => {
            json!({"status":"unknown","reason":"same_request_readonly_accounting_evidence_unavailable"})
        }
    }
}
async fn ingestion(
    p: &Prepared,
    c: &Case,
    client: &reqwest::Client,
    token: &str,
    body: &Value,
    response: &Http,
    state: &Shared,
    index: usize,
    dir: &Path,
) -> Value {
    let i = c.ingestion.as_ref().unwrap();
    let Some(document) = response
        .body
        .pointer(&i.document_id_pointer)
        .and_then(Value::as_str)
        .filter(|s| super::config::safe_id(s))
    else {
        return json!({"status":"unknown","reason":"accepted_document_id_missing_do_not_resubmit"});
    };
    if let Ok(mut state) = state.lock() {
        state.private_resources.push(json!({"case":c.id,"document_id":document,"dataset_id":i.dataset_id,"submission":"accepted","terminal_observed":false}));
    }
    let _ = flush(dir, state);
    let path = format!("/api/v1/knowledge/{}/documents/{document}", i.dataset_id);
    let start = Instant::now();
    let mut polls = 0usize;
    loop {
        let r = http(
            client,
            &p.profile.gateway_origin,
            &path,
            reqwest::Method::GET,
            Some(token),
            None,
            None,
            p.profile.budgets.max_body_bytes,
        )
        .await;
        polls += 1;
        if let Ok(r) = r {
            if r.status == 200
                && r.body
                    .pointer(&i.document_id_pointer)
                    .and_then(Value::as_str)
                    == Some(document)
            {
                let observed = r
                    .body
                    .pointer(&i.state_pointer)
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if observed == i.completed_state && r.body["dataset_id"] == i.dataset_id {
                    let elapsed_ms = response.started.elapsed().as_secs_f64() * 1000.0;
                    let bytes = body
                        .pointer(&i.content_pointer)
                        .and_then(Value::as_str)
                        .map(|s| s.len());
                    mutate(state, index, "ingestion_terminal", json!("completed"));
                    let terminal = state
                        .lock()
                        .ok()
                        .and_then(|s| s.clock.map(|at| at.elapsed().as_secs_f64() * 1000.0));
                    mutate(state, index, "terminal_offset_ms", json!(terminal));
                    return json!({"status":"measured","completed_documents":1,"confirmed_input_utf8_bytes":bytes,"indexed_chunks":r.body.pointer(&i.chunks_pointer).and_then(Value::as_u64),"submit_through_terminal_observation_ms":elapsed_ms,"documents_per_second":1000.0/elapsed_ms,"completed_input_utf8_bytes_per_second":bytes.map(|b|b as f64*1000.0/elapsed_ms),"poll_count":polls,"completion_scope":"same_authorized_document_terminal_state_observed; queue_and_worker_stages_not_inferred"});
                }
                if i.failed_states.iter().any(|s| s == observed) {
                    return json!({"status":"failed","reason":"ingestion_terminal_failure","poll_count":polls});
                }
            }
        }
        if start.elapsed() >= Duration::from_millis(i.timeout_ms) {
            return json!({"status":"unknown","reason":"ingestion_deadline_or_status_unavailable_effect_may_continue","poll_count":polls});
        }
        tokio::time::sleep(Duration::from_millis(p.profile.budgets.poll_ms)).await;
    }
}
pub fn relevance(c: &Case, body: &Value) -> Value {
    let r = c.retrieval.as_ref().unwrap();
    let Some(results) = body.pointer(&r.results_pointer).and_then(Value::as_array) else {
        return json!({"status":"unknown","reason":"retrieval_result_list_missing"});
    };
    let ids = results
        .iter()
        .take(r.top_k)
        .filter_map(|v| v.pointer(&r.id_pointer).and_then(Value::as_str))
        .collect::<Vec<_>>();
    let unique = ids.iter().copied().collect::<BTreeSet<_>>();
    let matched = r
        .relevant_document_ids
        .iter()
        .filter(|v| unique.contains(v.as_str()))
        .count();
    json!({"status":if ids.len()!=results.len().min(r.top_k){"unknown"}else if (matched as f64/r.relevant_document_ids.len()as f64)<r.min_recall_at_k{"failed_relevance_guard"}else{"measured"},"relevant_document_count":r.relevant_document_ids.len(),"returned_hits":ids.len(),"unique_document_hits":unique.len(),"relevant_document_hits":matched,"recall_at_k":matched as f64/r.relevant_document_ids.len()as f64,"document_precision_at_k":if unique.is_empty(){None}else{Some(matched as f64/unique.len()as f64)},"k":r.top_k,"scope":"operator_pinned_document_relevance_fixture_no_content_export"})
}
async fn one(
    p: &Prepared,
    c: &Case,
    client: &reqwest::Client,
    actor: &str,
    trace_token: Option<&str>,
    pool: Option<&sqlx::PgPool>,
    mock: Option<&mock::Handle>,
    state: &Shared,
    index: usize,
    run_id: &str,
    dir: &Path,
) -> Result<()> {
    let trace_id = uuid::Uuid::new_v4().simple().to_string();
    let span = uuid::Uuid::new_v4().simple().to_string()[..16].to_owned();
    let parent = format!("00-{trace_id}-{span}-01");
    let correlation = format!("af-perf:{run_id}:{}:{index}", c.id);
    let dispatch = state
        .lock()
        .ok()
        .and_then(|s| s.clock.map(|at| at.elapsed().as_secs_f64() * 1000.0));
    mutate(state, index, "dispatch_offset_ms", json!(dispatch));
    let body = substitute(&c.body, &correlation);
    let before = mock.map(|h| h.observations().len()).unwrap_or(0);
    mutate(
        state,
        index,
        "outcome",
        json!("dispatched_effect_unknown_until_observed"),
    );
    mutate(state, index, "trace_id", json!(trace_id));
    flush(dir, state)?;
    let response = match http_observed(
        client,
        &p.profile.gateway_origin,
        &c.path,
        reqwest::Method::POST,
        Some(actor),
        Some(&body),
        Some(&parent),
        p.profile.budgets.max_body_bytes,
        Some((state, index)),
    )
    .await
    {
        Ok(r) => r,
        Err(code) => {
            mutate(state, index, "outcome", json!(code));
            let end = state
                .lock()
                .ok()
                .and_then(|s| s.clock.map(|at| at.elapsed().as_secs_f64() * 1000.0));
            mutate(state, index, "http_finished_offset_ms", json!(end));
            flush(dir, state)?;
            return Ok(());
        }
    };
    mutate(
        state,
        index,
        "http",
        json!({"status":response.status,"elapsed_ms":response.elapsed_ms,"response_headers_ms":response.headers_ms,"first_decoded_body_byte_ms":response.first_body_ms,"body_bytes":response.bytes,"failure":response.failure}),
    );
    let end = state
        .lock()
        .ok()
        .and_then(|s| s.clock.map(|at| at.elapsed().as_secs_f64() * 1000.0));
    mutate(state, index, "http_finished_offset_ms", json!(end));
    let valid = response.failure.is_none()
        && (200..300).contains(&response.status)
        && checks(&response.body, &c.checks);
    mutate(state, index, "body_validated", json!(valid));
    mutate(
        state,
        index,
        "outcome",
        json!(if valid {
            "validated_http_success"
        } else if response.status == 429 {
            "rate_limited"
        } else {
            "http_or_correctness_failure"
        }),
    );
    if c.kind == Kind::Ingestion {
        if let Some(i) = &c.ingestion {
            if let Some(document) = response
                .body
                .pointer(&i.document_id_pointer)
                .and_then(Value::as_str)
                .filter(|s| super::config::safe_id(s))
            {
                if let Ok(mut st) = state.lock() {
                    st.private_resources.push(json!({"case":c.id,"document_id":document,"expected_dataset_id":i.dataset_id,"submission":"candidate_response_before_correctness_validation","safe_cleanup":"verify_owner_dataset_before_any_action"}));
                }
            }
        }
    }
    flush(dir, state)?;
    if !valid {
        return Ok(());
    }
    if c.kind == Kind::GatewayReal && mock.is_some_and(|m| m.observations().len() > before) {
        mutate(state, index, "body_validated", json!(false));
        mutate(
            state,
            index,
            "outcome",
            json!("real_provider_case_observed_synthetic_upstream"),
        );
        flush(dir, state)?;
        return Ok(());
    }
    if let Some(request) = &response.request_id {
        if let Ok(mut s) = state.lock() {
            s.private_resources
                .push(json!({"case":c.id,"gateway_request_id":request,"trace_id":trace_id}));
        }
    }
    mutate(
        state,
        index,
        "response_trace_correlated",
        json!(response.response_trace_id.as_deref() == Some(&trace_id)),
    );
    if c.kind == Kind::Ingestion {
        let value = ingestion(p, c, client, actor, &body, &response, state, index, dir).await;
        mutate(state, index, "ingestion", value);
    }
    if c.kind == Kind::Retrieval {
        mutate(state, index, "relevance", relevance(c, &response.body));
    }
    let spans = collect_trace(p, client, trace_token, &trace_id).await;
    mutate(
        state,
        index,
        "server_metrics",
        match spans {
            Ok(spans) => trace::metrics(c, &response.body, &spans),
            Err(code) => json!({"status":"unknown","reason":code}),
        },
    );
    if matches!(c.kind, Kind::GatewayMock | Kind::GatewayReal) {
        mutate(
            state,
            index,
            "accounting",
            accounting(p, pool, response.request_id.as_deref(), &response.body).await,
        );
    }
    if c.kind == Kind::GatewayMock {
        let observations = mock.map(|h| h.observations()).unwrap_or_default();
        let matched = observations
            .iter()
            .skip(before)
            .filter(|r| {
                r.correlation == correlation && r.outcome == "synthetic_upstream_response_written"
            })
            .collect::<Vec<_>>();
        if matched.len() == 1 {
            mutate(
                state,
                index,
                "synthetic_upstream",
                json!({"status":"observed","elapsed_ms":matched[0].elapsed_ms,"response_write_ms":matched[0].response_write_ms,"same_trace_header":matched[0].trace_id.as_deref()==Some(&trace_id),"scope":"owned_deterministic_upstream_not_real_provider_latency"}),
            );
            if let Some(control) = &c.direct_control_body {
                let control = substitute(control, &correlation);
                let m = p.profile.mock.as_ref().unwrap();
                let control_token = evidence::token(&m.token_file)?;
                let direct = http(
                    client,
                    &m.origin,
                    "/v1/chat/completions",
                    reqwest::Method::POST,
                    Some(&control_token),
                    Some(&control),
                    Some(&parent),
                    p.profile.budgets.max_body_bytes,
                )
                .await;
                mutate(
                    state,
                    index,
                    "direct_upstream_control",
                    match direct {
                        Ok(r) => {
                            json!({"status":if r.status==200&&r.failure.is_none()&&checks(&r.body,&c.checks){"measured_control_only"}else{"failed_control"},"http_status":r.status,"elapsed_ms":r.elapsed_ms,"equivalent_upstream_request_body":sha(&serde_json::to_vec(&control).map_err(|_|"control_encode")?)==matched[0].request_sha256.as_deref().unwrap_or(""),"gateway_overhead_from_this_control":null,"scope":"independent_same_body_control_no_percentile_subtraction"})
                        }
                        Err(code) => json!({"status":"unknown","reason":code}),
                    },
                );
            }
        } else {
            mutate(
                state,
                index,
                "synthetic_upstream",
                json!({"status":"unknown","reason":"owned_upstream_not_uniquely_correlated"}),
            );
        }
    }
    flush(dir, state)
}
async fn measure(
    p: &Prepared,
    state: &Shared,
    stop: &Arc<AtomicBool>,
    dir: &Path,
    allow_real: bool,
) -> Result<()> {
    identity::verify_live(p).await?;
    let actor = evidence::token(&p.profile.actor_token_file)?;
    let trace_token = p
        .profile
        .trace_token_file
        .as_deref()
        .map(evidence::token)
        .transpose()?;
    let client = client_for(p, "gateway")?;
    fixtures(p, &client, &actor).await?;
    let trace_client = client_for(p, "trace")?;
    let probe = http(
        &trace_client,
        &p.profile.trace_origin,
        &format!("/api/traces/{}", uuid::Uuid::new_v4().simple()),
        reqwest::Method::GET,
        trace_token.as_deref(),
        None,
        None,
        p.profile.budgets.max_body_bytes,
    )
    .await?;
    ensure(
        probe.status == 404 || (probe.status == 200 && probe.failure.is_none()),
        "owned_trace_reader_not_ready_or_authorized",
    )?;
    let pool = accounting_pool(p).await?;
    let run_id = uuid::Uuid::new_v4().simple().to_string();
    let upstream = if let Some(c) = p.profile.mock.clone() {
        Some(
            mock::start(
                c,
                &run_id,
                p.accepted
                    .services
                    .iter()
                    .find(|s| s.id == "gateway")
                    .unwrap()
                    .network_addresses
                    .iter()
                    .filter_map(|a| a.parse().ok())
                    .collect(),
            )
            .await?,
        )
    } else {
        None
    };
    let _sampler = sample(p, state.clone(), stop.clone());
    let mut offset = 0;
    for c in &p.profile.cases {
        identity::verify_live(p).await?;
        fixtures(p, &client, &actor).await?;
        let real = c.kind == Kind::GatewayReal || c.provider_scope == ProviderScope::AuthorizedReal;
        if real && !(allow_real && p.accepted.real_provider_authorized) {
            for i in offset..offset + c.warmup + c.samples {
                mutate(
                    state,
                    i,
                    "outcome",
                    json!("blocked_real_provider_authorization_missing"),
                );
            }
            offset += c.warmup + c.samples;
            continue;
        }
        let mut warmup_ok = true;
        for local in 0..c.warmup + c.samples {
            let index = offset + local;
            if stop.load(Ordering::Acquire) {
                return Err("resource_safety_stop".into());
            }
            if !warmup_ok {
                mutate(state, index, "outcome", json!("not_started_warmup_failed"));
                continue;
            }
            one(
                p,
                c,
                &client,
                &actor,
                trace_token.as_deref(),
                pool.as_ref(),
                upstream.as_ref(),
                state,
                index,
                &run_id,
                dir,
            )
            .await?;
            if local < c.warmup {
                let s = state.lock().map_err(|_| "state_lock")?;
                warmup_ok = row_complete(&s.rows[index]);
            }
            tokio::time::sleep(Duration::from_millis(c.gap_ms)).await;
        }
        offset += c.warmup + c.samples;
        identity::verify_live(p).await?;
        flush(dir, state)?;
        tokio::time::sleep(Duration::from_millis(p.profile.budgets.case_gap_ms)).await;
    }
    if let Some(pool) = pool {
        pool.close().await;
    }
    fixtures(p, &client, &actor).await?;
    identity::verify_live(p).await?;
    Ok(())
}
fn row_complete(r: &Value) -> bool {
    r["body_validated"] == true
        && r["response_trace_correlated"] == true
        && r["server_metrics"]["status"] == "measured"
        && (!matches!(
            r["kind"].as_str(),
            Some("deterministic_synthetic_upstream" | "real_provider")
        ) || r["accounting"]["status"] == "observed")
        && (r["kind"] != "deterministic_synthetic_upstream"
            || (r["synthetic_upstream"]["status"] == "observed"
                && r["synthetic_upstream"]["same_trace_header"] == true))
        && (r["kind"] != "resource_authorized_ingestion" || r["ingestion"]["status"] == "measured")
        && (r["kind"] != "resource_authorized_retrieval" || r["relevance"]["status"] == "measured")
        && (r.get("direct_upstream_control").is_none()
            || r["direct_upstream_control"]["status"] == "measured_control_only"
                && r["direct_upstream_control"]["equivalent_upstream_request_body"] == true)
}
pub fn summary(rows: &[Value]) -> Value {
    let mut cases = BTreeMap::<String, Vec<&Value>>::new();
    for r in rows {
        cases
            .entry(r["case"].as_str().unwrap_or("unknown").into())
            .or_default()
            .push(r);
    }
    let mut summaries = Vec::new();
    for (case, rows) in cases {
        let measured = rows
            .iter()
            .copied()
            .filter(|r| r["phase"] == "measured")
            .collect::<Vec<_>>();
        let successes = measured
            .iter()
            .filter(|r| r["body_validated"] == true)
            .collect::<Vec<_>>();
        let distribution = crate::performance::distribution(
            successes
                .iter()
                .filter_map(|r| r["http"]["elapsed_ms"].as_f64())
                .collect(),
        )
        .unwrap_or(Value::Null);
        let mut outcomes = BTreeMap::<String, usize>::new();
        for r in &measured {
            *outcomes
                .entry(r["outcome"].as_str().unwrap_or("unknown").into())
                .or_default() += 1;
        }
        let mut statuses = BTreeMap::<String, usize>::new();
        for r in &measured {
            if let Some(status) = r["http"]["status"].as_u64() {
                *statuses.entry(status.to_string()).or_default() += 1;
            }
        }
        let complete = measured.iter().all(|r| row_complete(r));
        let values = |pointer: &str| {
            crate::performance::distribution(
                measured
                    .iter()
                    .filter_map(|r| r.pointer(pointer).and_then(Value::as_f64))
                    .collect(),
            )
            .unwrap_or(Value::Null)
        };
        let first = measured
            .iter()
            .filter_map(|r| r["dispatch_offset_ms"].as_f64())
            .min_by(f64::total_cmp);
        let last = measured
            .iter()
            .filter_map(|r| {
                r["terminal_offset_ms"]
                    .as_f64()
                    .or_else(|| r["http_finished_offset_ms"].as_f64())
            })
            .max_by(f64::total_cmp);
        let wall = first
            .zip(last)
            .map(|(a, b)| (b - a) / 1000.0)
            .filter(|s| *s > 0.0);
        let completed = measured
            .iter()
            .filter(|r| r["ingestion"]["status"] == "measured")
            .count();
        let bytes = measured
            .iter()
            .filter(|r| r["ingestion"]["status"] == "measured")
            .filter_map(|r| r["ingestion"]["confirmed_input_utf8_bytes"].as_u64())
            .sum::<u64>();
        let names = measured
            .iter()
            .flat_map(|r| {
                r["server_metrics"]["retrieval_stages"]
                    .as_object()
                    .into_iter()
                    .flat_map(|m| m.keys().cloned())
            })
            .collect::<BTreeSet<_>>();
        let mut stages = BTreeMap::new();
        for name in names {
            let mut metrics = BTreeMap::new();
            for metric in ["union_active_ms", "span_duration_sum_ms", "elapsed_ms"] {
                metrics.insert(
                    metric,
                    values(&format!("/server_metrics/retrieval_stages/{name}/{metric}")),
                );
            }
            stages.insert(name, metrics);
        }
        summaries.push(json!({"case":case,"provider_scope":rows.first().map(|r|r["provider_scope"].clone()),"kind":rows.first().map(|r|r["kind"].clone()),"status":if complete{"MEASURED_BASELINE_NO_SLO_EVALUATION"}else{"PARTIAL_FAILED_OR_UNKNOWN"},"scheduled_measured_requests":measured.len(),"validated_successes":successes.len(),"outcomes":outcomes,"http_statuses":statuses,"started_requests":measured.iter().filter(|r|r["dispatch_offset_ms"].is_number()).count(),"finished_attempts":measured.iter().filter(|r|r["http_finished_offset_ms"].is_number()).count(),"successful_client_elapsed_ms":distribution,"gateway_excluding_upstream_interval_ms":values("/server_metrics/gateway/gateway_excluding_upstream_interval_ms"),"retrieval_stages":stages,"closed_loop_measured_wall_seconds":wall,"validated_successes_per_second":wall.map(|s|successes.len()as f64/s),"ingestion_submit_to_terminal_ms":values("/ingestion/submit_through_terminal_observation_ms"),"ingestion_completed_documents":completed,"ingestion_completed_documents_per_second":wall.map(|s|completed as f64/s),"ingestion_completed_input_utf8_bytes_per_second":wall.map(|s|bytes as f64/s),"warmup_excluded":rows.len()-measured.len(),"population":"per named case; serial wall throughput includes intervening observation/policy quiet time; no synthetic/real aggregation or independent-percentile subtraction"}));
    }
    json!(summaries)
}
pub async fn run(p: Prepared, dir: &Path, allow_real: bool) -> Value {
    let state = Arc::new(Mutex::new(State {
        clock: Some(Instant::now()),
        ..State::default()
    }));
    for c in &p.profile.cases {
        for i in 0..c.warmup + c.samples {
            state.lock().unwrap().rows.push(json!({"case":c.id,"kind":c.kind.label(),"provider_scope":c.provider_scope.label(),"sequence":i,"phase":if i<c.warmup{"warmup"}else{"measured"},"outcome":"not_started","http":null,"server_metrics":{"status":"unknown"}}));
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let mut findings = Vec::new();
    let intent = json!({"schema":"agentforge-performance-intent/v1","profile_sha256":p.profile_sha256,"accepted_receipt_sha256":p.receipt_sha256,"started_unix":chrono::Utc::now().timestamp(),"write_retries":false,"service_lifecycle":"unchanged; only owned bounded synthetic listener","real_provider_flag":allow_real,"acceptance":false});
    let result = if let Err(code) =
        evidence::save(dir, "intent.json", &intent).and_then(|_| flush(dir, &state))
    {
        Err(code)
    } else {
        tokio::select! {result=tokio::time::timeout(Duration::from_secs(p.profile.budgets.run_seconds),measure(&p,&state,&stop,dir,allow_real))=>match result{Ok(v)=>v,Err(_)=>Err("run_deadline_effects_may_continue_no_replay".into())},signal=tokio::signal::ctrl_c()=>{let _=signal;Err("interrupted_effects_may_continue_no_replay".into())}}
    };
    stop.store(true, Ordering::Release);
    if let Err(code) = result {
        findings.push(code);
    }
    if let Err(code) = flush(dir, &state) {
        findings.push(code);
    }
    // A failed final identity check never upgrades an earlier measured result.
    if let Err(code) = identity::verify_live(&p).await {
        findings.push(code);
    }
    let s = state.lock().unwrap();
    findings.extend(s.resource_errors.clone());
    let cases = summary(&s.rows);
    let mut resources = BTreeMap::new();
    for service in &p.accepted.services {
        let samples = s
            .resources
            .get(&service.id)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let v = crate::performance::process_summary(samples)
            .unwrap_or(json!({"status":"unknown","reason":"invalid_actual_process_samples"}));
        resources.insert(service.id.clone(),json!({"pid":service.pid,"start_ticks":service.start_ticks,"image_id":service.image_id,"binary_sha256":service.binary_sha256,"observed_runtime_limits":{"memory_limit_bytes":service.runtime_settings["memory_limit_bytes"],"nano_cpus":service.runtime_settings["nano_cpus"],"cpu_quota":service.runtime_settings["cpu_quota"],"cpu_period":service.runtime_settings["cpu_period"]},"status":v["status"],"sample_count":v["sample_count"],"window_seconds":v["window_seconds"],"cpu_seconds":v["gateway_cpu_seconds"],"cpu_percent_of_one_core":v["gateway_cpu_percent_of_one_core"],"sampled_peak_rss_bytes":v["sampled_peak_rss_bytes"],"rss_start_bytes":v["rss_start_bytes"],"rss_end_bytes":v["rss_end_bytes"],"scope":"exact accepted live PID; whole harness window includes warmups, observation probes, quiet windows and background work"}));
    }
    let measured = s.rows.iter().any(|r| r["http"].is_object());
    let complete = findings.is_empty()
        && cases.as_array().is_some_and(|a| {
            a.iter()
                .all(|c| c["status"] == "MEASURED_BASELINE_NO_SLO_EVALUATION")
        })
        && resources.values().all(|r| r["status"] == "measured");
    let e = &p.profile.environment;
    let environment = json!({"scope":"operator_declared_and_signed_profile_not_hardware_probe","topology":e.topology,"cpu_limit_millicores":e.cpu_limit_millicores,"memory_limit_bytes":e.memory_limit_bytes,"database_pool_limit":e.database_pool_limit,"corpus_sha256":e.corpus_sha256,"index_sha256":e.index_sha256,"build_flags_sha256":e.build_flags_sha256,"cache_condition":e.cache_condition,"background_load":e.background_load,"controls_required_enabled":e.controls});
    json!({"schema":"agentforge-performance-baseline/v1","environment":environment,"status":if complete{"COMPLETE_BASELINE_NO_SLO_EVALUATION"}else if measured{"PARTIAL_FAILED_OR_UNKNOWN"}else{"BLOCKED"},"profile_sha256":p.profile_sha256,"accepted_receipt_sha256":p.receipt_sha256,"elapsed_seconds":start.elapsed().as_secs_f64(),"findings":findings,"cases":cases,"services":resources,"sampling_interval_ms":p.profile.budgets.sample_ms,"load_model":"serial finite closed loop; no write retry; no capacity claim","unknown_scope":["server_pool_waiters_unless_independently_traced","streaming_tokens_or_cancellation","real_provider_acceptance_when_only_synthetic_case_ran","unobserved_stage_timings","instantaneous_memory_peaks_between_samples"],"artifacts":["observations.json","resources.json","private-state.json","intent.json"],"sensitive_data_exported":false,"acceptance":false})
}
