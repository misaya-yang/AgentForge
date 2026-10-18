//! SDK model plane: existing SDK run authority, registered models, price book,
//! bounded capacity and durable quota receipts. No managed Runtime turn is fabricated.
use super::*;
use crate::llm_api::guardrails::{Context as GuardContext, GuardrailService};
use ai_platform_gateway_foundation::{
    sdk_capabilities::canonical_hash,
    usage_recorder::{UsageRecord, prepare_with_snapshot},
};
use axum::{
    Json,
    body::{Body, Bytes},
    routing::post,
};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

const NOTIFY: &str = "sdk_execution_revocations_v1";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    run_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Execute {
    run_id: String,
    call_id: String,
    wire: String,
    body: Value,
}
fn fail(code_: &'static str) -> ContentError {
    code(503, code_)
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn expiry(row: &Value) -> Result<u64> {
    chrono::DateTime::parse_from_rfc3339(text(row, "expires_at"))
        .map(|v| v.timestamp_millis().max(0) as u64)
        .map_err(|_| fail("sdk_model_lease_invalid"))
}
pub fn router(state: Arc<KnowledgeProxy>) -> Router {
    Router::new()
        .route("/internal/v1/sdk-tools/models/lease", post(lease))
        .route("/internal/v1/sdk-tools/models/execute", post(execute))
        .route(
            "/internal/v1/sdk-tools/models/revocations",
            post(revocations),
        )
        .with_state(state)
}
async fn lease(
    State(state): State<Arc<KnowledgeProxy>>,
    headers: HeaderMap,
    Json(run): Json<Run>,
) -> Result<Json<Value>> {
    let (bound, row) =
        super::sdk_capabilities::current(&state, super::sdk_reads::token(&headers)?, &run.run_id)
            .await?;
    let pool = &state
        .sdk_control
        .as_ref()
        .ok_or_else(|| fail("sdk_model_authority_unavailable"))?
        .pool;
    let ready:bool=sqlx::query_scalar("SELECT to_regclass('gateway.sdk_model_calls') IS NOT NULL AND to_regprocedure('gateway.reserve_sdk_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint)') IS NOT NULL AND to_regprocedure('gateway.settle_sdk_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb)') IS NOT NULL AND COALESCE(has_function_privilege(current_user,to_regprocedure('gateway.reserve_sdk_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint)'),'EXECUTE'),false) AND COALESCE(has_function_privilege(current_user,to_regprocedure('gateway.settle_sdk_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb)'),'EXECUTE'),false)")
        .fetch_one(pool).await.map_err(|_|fail("sdk_model_schema_unavailable"))?;
    if !ready {
        return Err(fail("sdk_model_schema_not_admitted"));
    }
    let agent = &row["snapshot"]["sdk_agent"];
    let instructions = agent["instructions"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("You are a helpful assistant.");
    let binding = canonical_hash(&json!([
        bound.tenant_id,
        bound.user_id,
        bound.service_id,
        row["engine"],
        bound.session_id,
        bound.request_id,
        agent["model"],
        instructions,
        agent["max_turns"]
    ]));
    Ok(Json(
        json!({"schema":"sdk-model-plane/v1","expires_at_epoch_ms":expiry(&row)?,"binding_sha256":binding}),
    ))
}
/// NOTIFY carries no user payload. A wake always revalidates the live grant and
/// run, and missed hints are covered by a ten-second authority check.
async fn revocations(
    State(state): State<Arc<KnowledgeProxy>>,
    headers: HeaderMap,
    Json(run): Json<Run>,
) -> Result<Response> {
    let grant = super::sdk_reads::token(&headers)?.to_owned();
    let (_, row) = super::sdk_capabilities::current(&state, &grant, &run.run_id).await?;
    let deadline = expiry(&row)?;
    let control = state
        .sdk_control
        .as_ref()
        .ok_or_else(|| fail("sdk_model_authority_unavailable"))?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy_with((*control.pool.connect_options()).clone());
    let mut listener = sqlx::postgres::PgListener::connect_with(&pool)
        .await
        .map_err(|_| fail("sdk_revocation_listener_unavailable"))?;
    listener
        .listen(NOTIFY)
        .await
        .map_err(|_| fail("sdk_revocation_listener_unavailable"))?;
    let (tx, rx) = mpsc::channel::<Bytes>(4);
    tokio::spawn(async move {
        let mut backstop = tokio::time::interval(Duration::from_secs(10));
        loop {
            // Also covers the authorization -> LISTEN race and reconnect boundaries.
            let valid = (chrono::Utc::now().timestamp_millis().max(0) as u64) < deadline
                && super::sdk_capabilities::current(&state, &grant, &run.run_id)
                    .await
                    .is_ok();
            let event = json!({"type":if valid{"sdk.lease_heartbeat"}else{"sdk.lease_revoked"},"run_id":run.run_id});
            if tx
                .send(Bytes::from(format!("data: {event}\n\n")))
                .await
                .is_err()
                || !valid
            {
                break;
            }
            tokio::select! {
                _=tx.closed()=>break,
                _=backstop.tick()=>{},
                hint=listener.try_recv()=>{ if hint.is_err(){break;} },
                _=tokio::time::sleep(Duration::from_millis(deadline.saturating_sub(chrono::Utc::now().timestamp_millis().max(0)as u64)))=>{},
            }
        }
        drop(listener);
        pool.close().await;
    });
    Ok(sse(rx))
}
fn sse(rx: mpsc::Receiver<Bytes>) -> Response {
    let body = Body::from_stream(futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|bytes| (Ok::<_, std::convert::Infallible>(bytes), rx))
    }));
    (
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-store"),
        ],
        body,
    )
        .into_response()
}
fn validate_request(input: &Execute, row: &Value) -> Result<()> {
    let prefix = format!("{}:", input.run_id);
    let index = input
        .call_id
        .strip_prefix(&prefix)
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|n| n.to_string() == input.call_id[prefix.len()..]);
    let max = row["snapshot"]["sdk_agent"]["max_turns"]
        .as_u64()
        .unwrap_or(0);
    if index.is_none_or(|n| u64::from(n) >= max)
        || max == 0
        || max > 32
        || input.body["stream"] != true
        || input.body["model"] != row["snapshot"]["sdk_agent"]["model"]
        || !input.body.is_object()
    {
        return Err(code(422, "sdk_model_call_invalid"));
    }
    let claude = row["engine"] == "claude_agent_sdk";
    let instructions = row["snapshot"]["sdk_agent"]["instructions"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("You are a helpful assistant.");
    if input.wire
        != if claude {
            "anthropic_messages"
        } else {
            "responses_v1"
        }
        || input.body[if claude { "system" } else { "instructions" }].as_str() != Some(instructions)
    {
        return Err(code(403, "sdk_model_snapshot_mismatch"));
    }
    let allowed = if claude {
        &[
            "model",
            "system",
            "messages",
            "tools",
            "max_tokens",
            "stream",
        ][..]
    } else {
        &[
            "model",
            "instructions",
            "input",
            "tools",
            "max_output_tokens",
            "stream",
            "store",
        ][..]
    };
    if input
        .body
        .as_object()
        .is_none_or(|o| o.keys().any(|key| !allowed.contains(&key.as_str())))
        || !claude && input.body["store"] != false
    {
        return Err(code(422, "sdk_model_call_invalid"));
    }
    Ok(())
}
fn endpoint(base: &str, wire: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(base).map_err(|_| fail("sdk_model_provider_invalid"))?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(fail("sdk_model_provider_invalid"));
    }
    let resource = if wire == "anthropic_messages" {
        "messages"
    } else {
        "responses"
    };
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with(&format!("/{resource}")) {
        path.to_owned()
    } else if path.ends_with("/v1") {
        format!("{path}/{resource}")
    } else {
        format!("{path}/v1/{resource}")
    };
    url.set_path(&path);
    Ok(url)
}
struct Prepared {
    context_metric: Option<crate::migration_host::context_metrics::ContextMetric>,
    record: UsageRecord,
    pricing: Value,
    token: Value,
    call: Value,
    url: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    body: Value,
    lease_id: uuid::Uuid,
    guard: Option<GuardContext>,
    _guard_permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
async fn prepare(state: &KnowledgeProxy, grant: &str, input: &Execute) -> Result<Prepared> {
    let (bound, row) = super::sdk_capabilities::current(state, grant, &input.run_id).await?;
    validate_request(input, &row)?;
    let control = state
        .sdk_control
        .as_ref()
        .ok_or_else(|| fail("sdk_model_authority_unavailable"))?;
    let model = control
        .models
        .model(&bound.tenant_id, text(&input.body, "model"), None)
        .await
        .map_err(|_| fail("sdk_model_not_registered"))?;
    let provider_id = text(&model, "provider_id");
    let provider = control
        .providers
        .runtime_config(&bound.tenant_id, provider_id)
        .await
        .map_err(|_| fail("sdk_model_provider_unavailable"))?;
    let providers = [provider.clone()];
    let models = [model.clone()];
    let catalog =
        crate::provider_routing::RoutingSnapshot::new(&providers, &models).typed_catalog();
    let selected = catalog
        .select(
            &bound.tenant_id,
            &crate::provider_routing::ModelRef {
                provider_id: provider_id.into(),
                model_id: text(&input.body, "model").into(),
            },
        )
        .map_err(|_| code(403, "sdk_model_route_not_admitted"))?;
    if (input.wire == "anthropic_messages" && selected.runtime_provider != "anthropic")
        || (input.wire == "responses_v1"
            && !matches!(selected.runtime_provider.as_str(), "openai" | "dashscope"))
    {
        return Err(code(422, "sdk_model_provider_wire_not_admitted"));
    }

    let url = endpoint(text(&provider, "runtime_base_url"), &input.wire)?;
    let key = text(&provider, "api_key");
    if key.is_empty() {
        return Err(fail("sdk_model_provider_credentials_unavailable"));
    }
    let mut headers = reqwest::header::HeaderMap::new();
    for (key, value) in crate::provider_routing::provider_headers(&model["capabilities"], key)
        .into_internal_headers()
    {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(key.as_bytes())
                .map_err(|_| fail("sdk_model_provider_invalid"))?,
            value
                .parse()
                .map_err(|_| fail("sdk_model_provider_invalid"))?,
        );
    }
    if input.wire == "anthropic_messages" {
        headers.remove("authorization");
        headers.insert(
            "x-api-key",
            key.parse()
                .map_err(|_| fail("sdk_model_provider_invalid"))?,
        );
        headers.insert("anthropic-version", "2023-06-01".parse().expect("constant"));
    }
    let mut record = UsageRecord::new(
        &bound.tenant_id,
        &bound.user_id,
        text(&input.body, "model"),
        chrono::Utc::now(),
    );
    record.provider = provider_id.into();
    record.request_id = canonical_hash(&json!([bound.tenant_id, bound.user_id, input.call_id]));
    record.service_id = bound.service_id.clone();
    record.request_type = "sdk_model".into();
    let pricing = ai_platform_gateway_foundation::price_book::service::PgPriceBookService::new(
        control.pool.clone(),
    )
    .freeze_usage(&record)
    .await
    .map_err(|_| fail("sdk_model_price_book_unavailable"))?;
    if ai_platform_gateway_foundation::request_meter::is_snapshot(&pricing) {
        return Err(code(422, "sdk_model_request_meter_not_admitted"));
    }
    let mut body = input.body.clone();
    let output = body[if input.wire == "anthropic_messages" {
        "max_tokens"
    } else {
        "max_output_tokens"
    }]
    .as_i64()
    .unwrap_or(16384);
    if !(1..=16384).contains(&output) {
        return Err(code(422, "sdk_model_output_budget_invalid"));
    }
    body[if input.wire == "anthropic_messages" {
        "max_tokens"
    } else {
        "max_output_tokens"
    }] = output.into();
    let bytes = serde_json::to_vec(&body).map_err(|_| code(422, "sdk_model_call_invalid"))?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(code(413, "sdk_model_input_limit"));
    }
    // Conservative full UTF-8 byte ceiling, not a caller token estimate.
    record.input_tokens = bytes.len() as i64;
    record.output_tokens = output;
    prepare_with_snapshot(&mut record, &pricing)
        .map_err(|_| fail("sdk_model_price_book_invalid"))?;
    let lease_id = uuid::Uuid::new_v4();
    let hash = canonical_hash(&body);
    let token = json!({"tenant_id":bound.tenant_id,"user_id":bound.user_id,"call_id":input.call_id,"request_sha256":hash,
        "lease_id":lease_id,"model":{"provider":provider_id,"model":body["model"]},"expires_at_unix":expiry(&row)?/1000});
    let call = json!({"scope":{"actor":{"tenant_id":bound.tenant_id,"user_id":bound.user_id}},"sdk_run_id":input.run_id,
        "wire":input.wire,"model":body["model"],"request_sha256":hash,"owner_id":row["owner_id"],"fence":row["fence"],"provider_revision":provider["updated_at"]});
    let guard = GuardrailService::new(control.pool.clone())
        .inspect_input(&bound.tenant_id, &bound.user_id, &input.call_id, &bytes)
        .await
        .map_err(|_| code(403, "sdk_model_guardrails_input_not_admitted"))?;
    let guard_permit = if guard.as_ref().is_some_and(|g| g.output_enabled()) {
        static BUFFERS: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
        Some(
            tokio::time::timeout(
                Duration::from_millis(500),
                BUFFERS
                    .get_or_init(|| Arc::new(Semaphore::new(4)))
                    .clone()
                    .acquire_owned(),
            )
            .await
            .map_err(|_| fail("sdk_model_guardrails_capacity"))?
            .map_err(|_| fail("sdk_model_guardrails_capacity"))?,
        )
    } else {
        None
    };
    let context_metric = if state.context_metrics.is_some() {
        Some(
            crate::migration_host::context_metrics::producer::calculate(
                &bound.tenant_id,
                &bound.session_id,
                model["context_window"]
                    .as_i64()
                    .ok_or_else(|| fail("sdk_model_context_window_missing"))?,
                &body,
                chrono::Utc::now(),
            )
            .map_err(|_| fail("sdk_model_context_observation_invalid"))?,
        )
    } else {
        None
    };
    Ok(Prepared {
        context_metric,
        record,
        pricing,
        token,
        call,
        url,
        headers,
        body,
        lease_id,
        guard,
        _guard_permit: guard_permit,
    })
}
async fn execute(State(state): State<Arc<KnowledgeProxy>>, request: Request) -> Result<Response> {
    let grant = super::sdk_reads::token(request.headers())?.to_owned();
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .map_err(|_| code(413, "sdk_model_input_limit"))?;
    let input: Execute =
        serde_json::from_slice(&bytes).map_err(|_| code(422, "sdk_model_call_invalid"))?;
    let prepared = prepare(&state, &grant, &input).await?;
    let control = state
        .sdk_control
        .as_ref()
        .ok_or_else(|| fail("sdk_model_authority_unavailable"))?;
    // Reserve against canonical user quota before provider capacity or network dispatch.
    let mut max_input = prepared.record.input_cost_cents;
    for field in ["price_book_cached_input", "price_book_cache_write"] {
        if let Some(term) = prepared.pricing.get(field) {
            let term: ai_platform_gateway_foundation::price_book::PriceReceipt =
                serde_json::from_value(term.clone())
                    .map_err(|_| fail("sdk_model_price_book_invalid"))?;
            let nanos = term
                .quote_nanos(prepared.record.input_tokens as u64)
                .map_err(|_| fail("sdk_model_budget_overflow"))?;
            let microcents = i64::try_from(nanos / 1000 + u64::from(nanos % 1000 != 0))
                .map_err(|_| fail("sdk_model_budget_overflow"))?;
            max_input = max_input.max(microcents);
        }
    }
    let cost = max_input
        .checked_add(prepared.record.output_cost_cents)
        .ok_or_else(|| fail("sdk_model_budget_overflow"))?;
    let row:Value=sqlx::query_scalar("SELECT gateway.reserve_sdk_model_call($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)")
        .bind(&prepared.record.tenant_id).bind(&prepared.record.user_id).bind(&input.call_id).bind(text(&prepared.token,"request_sha256"))
        .bind(prepared.lease_id).bind(&prepared.token).bind(&prepared.call).bind(json!({"model":prepared.body["model"],"provider_revision":prepared.call["provider_revision"]}))
        .bind(&prepared.pricing).bind(prepared.record.input_tokens).bind(prepared.record.output_tokens).bind(cost)
        .fetch_one(&control.pool).await.map_err(|_|code(409,"sdk_model_reservation_not_admitted"))?;
    // An uncertain or already observed call is never sent to the provider again.
    if row["state"] != "reserved" || row["lease_id"] != prepared.token["lease_id"] {
        return Err(code(409, "sdk_model_call_already_reserved"));
    }
    let shared = control
        .admission
        .as_ref()
        .and_then(|a| a.model_plane_admission())
        .ok_or_else(|| fail("sdk_model_capacity_unavailable"))?;
    let capacity = control
        .capacity
        .as_ref()
        .ok_or_else(|| fail("sdk_model_capacity_unavailable"))?;
    let budgets = capacity
        .resolve(&crate::capacity::CapacityRequest {
            tenant_id: prepared.record.tenant_id.clone(),
            service_id: "sdk-model-plane".into(),
            request_class: "stream".into(),
            resource_kind: "provider".into(),
            provider_id: json!(prepared.record.provider),
            upstream_group: Value::Null,
            is_admin_read: false,
            service_config: Value::Null,
        })
        .map_err(|_| fail("sdk_model_capacity_unavailable"))?;
    let permit = shared
        .acquire_assistant(
            crate::capacity_admission::atomic::model_plane::ModelCallBinding {
                tenant: prepared.record.tenant_id.clone(),
                user: prepared.record.user_id.clone(),
                reservation_id: prepared.lease_id.to_string(),
                provider: prepared.record.provider.clone(),
                model: prepared.record.model.clone(),
            },
            &budgets,
        )
        .await
        .map_err(|_| code(429, "sdk_model_capacity_exhausted"))?;
    super::sdk_capabilities::current(&state, &grant, &input.run_id).await?;
    let current_provider = control
        .providers
        .runtime_config(&prepared.record.tenant_id, &prepared.record.provider)
        .await
        .map_err(|_| fail("sdk_model_provider_unavailable"))?;
    if current_provider["updated_at"] != prepared.call["provider_revision"] {
        return Err(code(409, "sdk_model_provider_revision_changed"));
    }
    permit
        .before_post()
        .await
        .map_err(|_| fail("sdk_model_capacity_lost"))?;
    if let (Some(metrics), Some(metric)) = (&state.context_metrics, &prepared.context_metric) {
        // Owned, validated request context only. The conservative billing ceiling
        // above is deliberately never used as a context observation.
        metrics
            .observe(&format!("sdk:{}", prepared.lease_id), metric.clone())
            .await
            .map_err(|_| fail("sdk_model_context_metrics_unavailable"))?;
    }
    let changed=sqlx::query("UPDATE gateway.sdk_model_calls SET state='dispatched',dispatched_at=clock_timestamp() WHERE lease_id=$1 AND state='reserved' AND expires_at>clock_timestamp()")
        .bind(prepared.lease_id).execute(&control.pool).await.map_err(|_|fail("sdk_model_dispatch_unknown"))?;
    if changed.rows_affected() != 1 {
        return Err(code(409, "sdk_model_dispatch_not_admitted"));
    }
    let (tx, rx) = mpsc::channel::<Bytes>(16);
    tokio::spawn(async move {
        let permit = permit;
        let pool = state
            .sdk_control
            .as_ref()
            .expect("prepared control")
            .pool
            .clone();
        let id = prepared.lease_id;
        let result = tokio::select! {
            result=tokio::time::timeout(Duration::from_secs(300),dispatch(&state,&input,&grant,prepared,&tx))=>result,
            _=permit.wait_lost()=>Ok(Err(fail("sdk_model_capacity_lost"))),
        };
        if !matches!(result, Ok(Ok(()))) {
            let _=sqlx::query("UPDATE gateway.sdk_model_calls SET state='unknown',error_code='sdk_provider_outcome_unknown' WHERE lease_id=$1 AND state IN ('dispatched','observed')").bind(id).execute(&pool).await;
            let _ = tx
                .send(Bytes::from_static(
                    b"data: {\"type\":\"error\",\"code\":\"sdk_model_outcome_unknown\"}\n\n",
                ))
                .await;
        }
    });
    Ok(sse(rx))
}
async fn dispatch(
    state: &KnowledgeProxy,
    input: &Execute,
    grant: &str,
    mut p: Prepared,
    tx: &mpsc::Sender<Bytes>,
) -> Result<()> {
    let control = state
        .sdk_control
        .as_ref()
        .ok_or_else(|| fail("sdk_model_authority_unavailable"))?;
    let client = reqwest::Client::builder()
        .retry(reqwest::retry::never())
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| fail("sdk_model_transport_invalid"))?;
    let mut response = client
        .post(p.url)
        .headers(p.headers)
        .json(&p.body)
        .send()
        .await
        .map_err(|_| fail("sdk_model_dispatch_unknown"))?;
    if !response.status().is_success() {
        return Err(fail("sdk_model_provider_rejected"));
    }
    let guard = p.guard.as_ref().filter(|g| g.output_enabled());
    let started = std::time::Instant::now();
    let mut live = tokio::time::interval(Duration::from_secs(1));
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            _=tx.closed()=>return Err(fail("sdk_model_observation_lost")),
            _=live.tick()=>{super::sdk_capabilities::current(state,grant,&input.run_id).await?;continue;},
            chunk=response.chunk()=>chunk.map_err(|_|fail("sdk_model_dispatch_unknown"))?,
        };
        let Some(chunk) = chunk else {
            break;
        };
        if bytes.len() + chunk.len() > 4 * 1024 * 1024
            || guard.is_some_and(|g| bytes.len() + chunk.len() > g.output_limit())
        {
            if let Some(guard) = guard {
                let _ = guard
                    .reject_output_limit(started.elapsed().as_micros().min(u64::MAX as u128) as u64)
                    .await;
            }
            return Err(fail("sdk_model_output_limit"));
        }
        bytes.extend_from_slice(&chunk);
        if guard.is_none() && tx.send(chunk).await.is_err() {
            return Err(fail("sdk_model_observation_lost"));
        }
    }
    let screened = if let Some(guard) = guard {
        let output = guardrail_output(&bytes)?;
        guard
            .inspect_output(
                &output,
                false,
                started.elapsed().as_micros().min(u64::MAX as u128) as u64,
            )
            .await
            .map_err(|_| code(403, "sdk_model_guardrails_output_not_admitted"))
    } else {
        Ok(())
    };
    let (usage, input_tokens, output_tokens) = observed_usage(&bytes, &input.wire)?;
    let observed = json!({"usage":usage,"input_tokens":input_tokens,"output_tokens":output_tokens});
    sqlx::query("UPDATE gateway.sdk_model_calls SET observation=$2,state='observed' WHERE lease_id=$1 AND state='dispatched'").bind(p.lease_id).bind(observed).execute(&control.pool).await.map_err(|_|fail("sdk_model_observation_unknown"))?;
    p.record.input_tokens = input_tokens;
    p.record.output_tokens = output_tokens;
    let cache = crate::usage_parser::cache::CacheUsage::observe(&p.record.provider, &usage);
    p.record.metadata.insert(
        "provider_cache_usage".into(),
        serde_json::to_value(cache).map_err(|_| fail("sdk_model_usage_invalid"))?,
    );
    prepare_with_snapshot(&mut p.record, &p.pricing)
        .map_err(|_| fail("sdk_model_price_book_invalid"))?;
    let cost_microcents = p
        .record
        .input_cost_cents
        .checked_add(p.record.output_cost_cents)
        .ok_or_else(|| fail("sdk_model_cost_overflow"))?;
    let receipt = json!({"type":"sdk.model_receipt","receipt_id":p.lease_id,"call_id":input.call_id,"run_id":input.run_id,"settled":true,"cost_nanos":cost_microcents.checked_mul(1000).ok_or_else(||fail("sdk_model_cost_overflow"))?});
    let result = json!({"state":"completed","lease_id":p.lease_id,"request_sha256":p.token["request_sha256"],"receipt":{"tenant_id":p.record.tenant_id,"user_id":p.record.user_id,"settled":true,"sdk":receipt}});
    let mut transaction = control
        .pool
        .begin()
        .await
        .map_err(|_| fail("sdk_model_settlement_unavailable"))?;
    sqlx::query("SELECT gateway.settle_sdk_model_call($1,$2,$3,$4,$5,$6,$7,$8,$9)")
        .bind(p.lease_id)
        .bind(&p.record.tenant_id)
        .bind(&p.record.user_id)
        .bind(input_tokens)
        .bind(output_tokens)
        .bind(p.record.input_cost_cents)
        .bind(p.record.output_cost_cents)
        .bind(&result)
        .bind(serde_json::to_value(&p.record).map_err(|_| fail("sdk_model_usage_invalid"))?)
        .execute(&mut *transaction)
        .await
        .map_err(|_| fail("sdk_model_settlement_unknown"))?;
    ai_platform_gateway_foundation::knowledge_model_accounting::settle_in_transaction(
        &mut transaction,
        &p.record,
    )
    .await
    .map_err(|_| fail("sdk_model_accounting_unknown"))?;
    transaction
        .commit()
        .await
        .map_err(|_| fail("sdk_model_settlement_unknown"))?;
    screened?;
    if guard.is_some() {
        tx.send(Bytes::from(bytes))
            .await
            .map_err(|_| fail("sdk_model_observation_lost"))?;
    }
    tx.send(Bytes::from(format!("\ndata: {receipt}\n\n")))
        .await
        .map_err(|_| fail("sdk_model_receipt_delivery_unknown"))?;
    Ok(())
}
fn guardrail_output(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut events = Vec::new();
    let mut assembled = std::collections::BTreeMap::<String, String>::new();
    for line in std::str::from_utf8(bytes)
        .map_err(|_| fail("sdk_model_stream_invalid"))?
        .lines()
    {
        let Some(data) = line
            .strip_prefix("data:")
            .map(str::trim)
            .filter(|s| *s != "[DONE]")
        else {
            continue;
        };
        let event: Value =
            serde_json::from_str(data).map_err(|_| fail("sdk_model_stream_invalid"))?;
        let key = canonical_hash(&json!([
            event["type"],
            event["index"],
            event["item_id"],
            event["output_index"],
            event["content_index"],
            event["delta"]["type"]
        ]));
        if let Some(delta) = event["delta"].as_str() {
            assembled.entry(key.clone()).or_default().push_str(delta);
        }
        for field in ["text", "partial_json", "thinking", "signature"] {
            if let Some(delta) = event["delta"][field].as_str() {
                assembled
                    .entry(format!("{key}:{field}"))
                    .or_default()
                    .push_str(delta);
            }
        }
        events.push(event);
    }
    serde_json::to_vec(&json!({"events":events,"assembled":assembled}))
        .map_err(|_| fail("sdk_model_stream_invalid"))
}
fn observed_usage(bytes: &[u8], wire: &str) -> Result<(Value, i64, i64)> {
    let mut usage = Value::Null;
    let mut terminal = false;
    for line in std::str::from_utf8(bytes)
        .map_err(|_| fail("sdk_model_stream_invalid"))?
        .lines()
    {
        let Some(data) = line
            .strip_prefix("data:")
            .map(str::trim)
            .filter(|s| *s != "[DONE]")
        else {
            continue;
        };
        let event: Value =
            serde_json::from_str(data).map_err(|_| fail("sdk_model_stream_invalid"))?;
        match (wire, text(&event, "type")) {
            ("responses_v1", "response.completed") => {
                if terminal || event["response"]["status"] != "completed" {
                    return Err(fail("sdk_model_terminal_invalid"));
                }
                usage = event["response"]["usage"].clone();
                terminal = true;
            }
            ("anthropic_messages", "message_start") => usage = event["message"]["usage"].clone(),
            ("anthropic_messages", "message_delta") => {
                if let Some(fields) = event["usage"].as_object() {
                    for (k, v) in fields {
                        usage[k] = v.clone();
                    }
                }
            }
            ("anthropic_messages", "message_stop") => {
                if terminal {
                    return Err(fail("sdk_model_terminal_invalid"));
                }
                terminal = true;
            }
            (_, "error" | "response.failed" | "response.incomplete") => {
                return Err(fail("sdk_model_provider_failed"));
            }
            _ => {}
        }
    }
    if !terminal {
        return Err(fail("sdk_model_stream_incomplete"));
    }
    let count = |field: &str| {
        usage[field]
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or_else(|| fail("sdk_model_usage_missing"))
    };
    let mut input = count("input_tokens")?;
    let output = count("output_tokens")?;
    if wire == "anthropic_messages" {
        for field in ["cache_read_input_tokens", "cache_creation_input_tokens"] {
            if let Some(value) = usage.get(field) {
                input = input
                    .checked_add(
                        value
                            .as_i64()
                            .filter(|n| *n >= 0)
                            .ok_or_else(|| fail("sdk_model_usage_invalid"))?,
                    )
                    .ok_or_else(|| fail("sdk_model_usage_invalid"))?;
            }
        }
    }
    // Store metering facts only; arbitrary provider fields are not content capture.
    let mut metered = json!({});
    for field in [
        "input_tokens",
        "output_tokens",
        "total_tokens",
        "cache_read_input_tokens",
        "cache_creation_input_tokens",
    ] {
        if let Some(value) = usage.get(field).filter(|v| v.as_u64().is_some()) {
            metered[field] = value.clone();
        }
    }
    if let Some(details) = usage.get("input_tokens_details") {
        let mut selected = json!({});
        for field in ["cached_tokens", "cache_write_tokens"] {
            if let Some(value) = details.get(field).filter(|v| v.as_u64().is_some()) {
                selected[field] = value.clone();
            }
        }
        metered["input_tokens_details"] = selected;
    }
    Ok((metered, input, output))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_requires_one_successful_terminal_and_complete_nonnegative_counts() {
        let wire=b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3}}}\n\n";
        let (_, i, o) = observed_usage(wire, "responses_v1").unwrap();
        assert_eq!((i, o), (2, 3));
        assert!(
            observed_usage(&[wire.as_slice(), wire.as_slice()].concat(), "responses_v1").is_err()
        );
        for data in [
            b"".as_slice(),
            b"data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n",
            b"data: {\"type\":\"error\"}\n",
        ] {
            assert!(observed_usage(data, "responses_v1").is_err());
        }
    }
    #[test]
    fn split_anthropic_text_is_assembled_before_output_guardrails() {
        let bytes=b"data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"person@\"}}\n\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"example.test\"}}\n\n";
        let value: Value = serde_json::from_slice(&guardrail_output(bytes).unwrap()).unwrap();
        assert!(
            value["assembled"]
                .as_object()
                .unwrap()
                .values()
                .any(|v| v == "person@example.test")
        );
    }
    #[test]
    fn sdk_model_calls_are_bound_to_original_model_instructions_and_turn_budget() {
        let run = "00000000-0000-0000-0000-000000000001";
        let row = json!({"engine":"openai_agents","snapshot":{"sdk_agent":{"model":"original","instructions":"policy","max_turns":2}}});
        let request = || Execute {
            run_id: run.into(),
            call_id: format!("{run}:0"),
            wire: "responses_v1".into(),
            body: json!({"model":"original","instructions":"policy","stream":true,"store":false,"input":[],"tools":[]}),
        };
        assert!(validate_request(&request(), &row).is_ok());
        for kind in 0..5 {
            let mut request = request();
            match kind {
                0 => request.body["model"] = "other".into(),
                1 => request.body["instructions"] = "other".into(),
                2 => request.call_id = format!("{run}:2"),
                3 => request.wire = "anthropic_messages".into(),
                _ => request.body["base_url"] = "https://attacker".into(),
            };
            assert!(validate_request(&request, &row).is_err());
        }
    }
    #[test]
    fn provider_endpoint_is_registry_owned_and_never_contains_credentials() {
        assert_eq!(
            endpoint("https://provider/compatible/v1", "responses_v1")
                .unwrap()
                .as_str(),
            "https://provider/compatible/v1/responses"
        );
        for base in [
            "file:///tmp/x",
            "https://user:secret@provider",
            "https://provider?key=secret",
        ] {
            assert!(endpoint(base, "responses_v1").is_err());
        }
    }
}
