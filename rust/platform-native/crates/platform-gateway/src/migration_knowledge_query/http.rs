//! Native mounts consume JWT-authenticated principals; no trusted-header bypass.
use super::{
    config::RetrieveRequest,
    qa::{LlmSelector, QaService, manual_source, valid_identity},
    service::RetrievalService,
    types::{QueryError, Result, UserContext},
};
use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::AuthService,
};
use axum::{
    Extension, Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
#[derive(Clone)]
struct ApiState {
    auth: Option<Arc<AuthService>>,
    retrieval: Arc<RetrievalService>,
    qa: Option<Arc<QaService>>,
}
pub fn router(
    auth: Arc<AuthService>,
    retrieval: Arc<RetrievalService>,
    qa: Option<Arc<QaService>>,
) -> Router {
    routes(ApiState {
        auth: Some(auth),
        retrieval,
        qa,
    })
}
pub fn knowledge_service_router(
    auth: Arc<super::auth::GatewayAuth>,
    retrieval: Arc<RetrievalService>,
    qa: Option<Arc<QaService>>,
) -> Result<Router> {
    auth.validate_service_boundary()?;
    Ok(routes(ApiState {
        auth: None,
        retrieval,
        qa,
    })
    .layer(axum::middleware::from_fn_with_state(
        auth,
        super::auth::verify_gateway,
    )))
}
fn routes(state: ApiState) -> Router {
    Router::new()
        .route("/api/v1/knowledge/{dataset_id}/retrieve", post(retrieve))
        .route("/api/v1/knowledge/{dataset_id}/retrieve_batch", post(batch))
        .route("/api/v1/knowledge/{dataset_id}/hit_test", post(hit_test))
        .route("/api/v1/knowledge/{dataset_id}/qa", post(qa))
        .route("/api/v1/knowledge/{dataset_id}/qa/stream", post(qa_stream))
        .route("/api/v1/knowledge/{dataset_id}/qa/batch", post(qa_batch))
        .route(
            "/api/v1/knowledge/{dataset_id}/retrieve_evaluate",
            post(retrieve_evaluate),
        )
        .route(
            "/api/v1/knowledge/retrieval/presets",
            get(retrieval_presets),
        )
        .route("/api/v1/knowledge/{dataset_id}/queries", get(query_history))
        .route(
            "/api/v1/knowledge/{dataset_id}/feedback",
            get(feedback_list).put(feedback_upsert),
        )
        .with_state(state)
}
async fn actor(
    state: &ApiState,
    headers: &HeaderMap,
    verified: Option<Extension<UserContext>>,
) -> Result<UserContext> {
    match &state.auth {
        Some(auth) => super::auth::verified_actor(auth, headers).await,
        None => verified.map(|v| v.0).ok_or(QueryError::Forbidden),
    }
}
async fn retrieve(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(request): Json<RetrieveRequest>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    retrieve_authorized(state, user, id, request, false).await
}
async fn hit_test(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(mut request): Json<RetrieveRequest>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    request.hierarchical = false;
    request.telemetry_source = "hit_test".into();
    request.source_type_filter = None;
    request.language_filter = None;
    request.metadata_filter = None;
    retrieve_authorized(state, user, id, request, true).await
}
async fn retrieve_authorized(
    state: ApiState,
    user: UserContext,
    id: String,
    request: RetrieveRequest,
    preview: bool,
) -> Result<Json<Value>> {
    if user.anonymous() {
        return Err(QueryError::Forbidden);
    }
    let hierarchy = request.hierarchical;
    let dataset = state
        .retrieval
        .store
        .require_dataset_access(&user, &id, if preview { "editor" } else { "viewer" })
        .await?;
    let generation = dataset.generation()?;
    let (results, meta) = state.retrieval.retrieve(&user, &id, request).await?;
    state
        .retrieval
        .store
        .require_generation(&user, &id, &generation)
        .await?;
    let ids: Vec<_> = results.iter().map(|r| r.document_id.clone()).collect();
    let mut identity_scope = dataset.clone();
    identity_scope.tenant_id = user.tenant_id.clone();
    let identities = state
        .retrieval
        .store
        .published_source_identities(&identity_scope, &ids)
        .await?;
    state
        .retrieval
        .store
        .require_generation(&user, &id, &generation)
        .await?;
    let values: Vec<_> = results
        .into_iter()
        .map(|r| {
            let mut item = serde_json::to_value(&r).unwrap_or(Value::Null);
            if let Some(m) = item["metadata"].as_object_mut() {
                m.remove("source_version");
                m.remove("source_hash");
            }
            item["source_version"] = Value::Null;
            item["source_hash"] = Value::Null;
            if !manual_source(&r.metadata) {
                if let Some((version, hash)) = identities.get(&r.document_id) {
                    if valid_identity(*version, hash) {
                        item["source_version"] = json!(version);
                        item["source_hash"] = json!(hash);
                        item["metadata"]["source_version"] = json!(version);
                        item["metadata"]["source_hash"] = json!(hash)
                    }
                }
            }
            item["source_type"] = r.metadata["source_type"].clone();
            item["citation_text"] = r.metadata["citation_text"].clone();
            item["source_reference"] = r
                .metadata
                .get("source_reference")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if hierarchy {
                for k in ["level", "parent_context", "document_summary"] {
                    item[k] = r.metadata[k].clone()
                }
                if let Some(m) = item.as_object_mut() {
                    for k in [
                        "content_type",
                        "image_url",
                        "vlm_description",
                        "associated_images",
                        "source_type",
                        "citation_text",
                        "source_reference",
                    ] {
                        m.remove(k);
                    }
                }
            }
            if preview {
                if let Some(m) = item.as_object_mut() {
                    for k in [
                        "content_type",
                        "image_url",
                        "vlm_description",
                        "associated_images",
                        "source_type",
                        "citation_text",
                        "source_reference",
                    ] {
                        m.remove(k);
                    }
                }
            }
            item
        })
        .collect();
    Ok(Json(
        json!({"results":values,"metadata":meta,"trace_id":meta["trace_id"],"query_fingerprint":meta["query_fingerprint"]}),
    ))
}
#[derive(Deserialize)]
struct BatchRequest {
    #[serde(default)]
    queries: Vec<Value>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    max_parallel: Option<usize>,
    #[serde(flatten)]
    options: serde_json::Map<String, Value>,
}
async fn batch(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(mut body): Json<BatchRequest>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    if user.anonymous() {
        return Err(QueryError::Forbidden);
    }
    if body.queries.is_empty() {
        body.queries = body
            .query
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| json!(s))
            .collect()
    }
    if body.queries.is_empty() {
        return Err(QueryError::Validation("queries must not be empty".into()));
    }
    let mut specs = Vec::new();
    for query in &body.queries {
        let mut value = Value::Object(body.options.clone());
        if let Some(s) = query.as_str() {
            value["query"] = json!(s.trim())
        } else if let Some(m) = query.as_object() {
            for (k, v) in m {
                value[k] = v.clone()
            }
        } else {
            return Err(QueryError::Validation(
                "batch query must be string or object".into(),
            ));
        }
        specs.push(
            serde_json::from_value::<RetrieveRequest>(value)
                .map_err(|_| QueryError::Validation("invalid batch query".into()))?,
        )
    }
    let mut primary = specs[0].clone();
    primary.telemetry_source = "batch".into();
    let unique = specs
        .iter()
        .map(|s| s.query.trim())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    primary.top_k = body.top_k.unwrap_or(match unique {
        1 => 5,
        2 => 6,
        3 => 8,
        4 => 9,
        _ => 10,
    });
    let (results, metadata) = state
        .retrieval
        .retrieve_batch(&user, &id, primary, specs, body.max_parallel.unwrap_or(10))
        .await?;
    Ok(Json(
        json!({"batch_results":results,"total_queries":metadata["total_queries"],"total_results":metadata["total_results"],"execution_time_ms":metadata["execution_time_ms"],"trace_id":metadata["trace_id"],"query_fingerprint":metadata["query_fingerprint"]}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QaRequest {
    query: String,
    #[serde(default = "qa_top")]
    top_k: usize,
    #[serde(default = "qa_mode")]
    mode: String,
    fusion_method: Option<String>,
    dense_weight: Option<f64>,
    bm25_weight: Option<f64>,
    score_threshold: Option<f64>,
    document_id: Option<String>,
    #[serde(default)]
    rerank: bool,
    rerank_top_n: Option<usize>,
    #[serde(default)]
    mmr: bool,
    mmr_lambda: Option<f64>,
    llm_config: Option<LlmSelector>,
    #[serde(default)]
    include_raw_results: bool,
}
fn qa_top() -> usize {
    5
}
fn qa_mode() -> String {
    "hybrid".into()
}
impl QaRequest {
    fn retrieval(&self) -> Result<RetrieveRequest> {
        if self.rerank_top_n.is_some_and(|n| !(1..=200).contains(&n)) {
            return Err(QueryError::Validation(
                "QA rerank_top_n must be 1..200".into(),
            ));
        }
        Ok(RetrieveRequest {
            query: self.query.clone(),
            top_k: self.top_k,
            mode: self.mode.clone(),
            fusion_method: self.fusion_method.clone(),
            dense_weight: self.dense_weight,
            bm25_weight: self.bm25_weight,
            score_threshold: self.score_threshold,
            document_id: self.document_id.clone(),
            rerank: Some(self.rerank),
            rerank_top_n: self.rerank_top_n,
            mmr: Some(self.mmr),
            mmr_lambda: self.mmr_lambda,
            telemetry_source: "qa".into(),
            ..Default::default()
        })
    }
}
async fn qa(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(body): Json<QaRequest>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    let service = state
        .qa
        .ok_or_else(|| QueryError::Validation("DashScope QA is not configured".into()))?;
    let request = body.retrieval()?;
    Ok(Json(
        service
            .query(
                &user,
                &id,
                request,
                body.llm_config.unwrap_or_default(),
                body.include_raw_results,
            )
            .await?,
    ))
}

struct Events {
    receiver: tokio::sync::mpsc::Receiver<Value>,
}
impl futures_core::Stream for Events {
    type Item = std::result::Result<Event, Infallible>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver
            .poll_recv(cx)
            .map(|v| v.map(|v| Ok(Event::default().data(v.to_string()))))
    }
}
async fn qa_stream(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(body): Json<QaRequest>,
) -> Result<Response> {
    let user = actor(&state, &headers, verified).await?;
    let service = state
        .qa
        .ok_or_else(|| QueryError::Validation("DashScope QA is not configured".into()))?;
    let request = body.retrieval()?;
    let receiver = service
        .query_stream(
            user,
            id,
            request,
            body.llm_config.unwrap_or_default(),
            body.include_raw_results,
        )
        .await?;
    Ok((
        [("Cache-Control", "no-cache"), ("X-Accel-Buffering", "no")],
        Sse::new(Events { receiver }),
    )
        .into_response())
}
/// Gateway assembly with canonical bounded RAG capture inside verified identity.
pub fn traced_gateway_router(
    auth: Arc<AuthService>,
    retrieval: Arc<RetrievalService>,
    qa: Option<Arc<QaService>>,
    capture: super::rag_trace::RagTraceCapture,
) -> Router {
    routes(ApiState {
        auth: None,
        retrieval,
        qa,
    })
    .layer(axum::middleware::from_fn_with_state(
        capture,
        super::rag_trace::capture,
    ))
    .layer(axum::middleware::from_fn_with_state(
        auth,
        super::auth::verify_jwt,
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QaBatchRequest {
    test_cases: Vec<super::qa::QaCase>,
    #[serde(default = "qa_top")]
    top_k: usize,
    #[serde(default = "qa_mode")]
    mode: String,
    #[serde(default)]
    rerank: bool,
    #[serde(default)]
    mmr: bool,
    llm_config: Option<LlmSelector>,
}
async fn qa_batch(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(body): Json<QaBatchRequest>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    let service = state
        .qa
        .ok_or_else(|| QueryError::Validation("DashScope QA is not configured".into()))?;
    let r = RetrieveRequest {
        top_k: body.top_k,
        mode: body.mode,
        rerank: Some(body.rerank),
        mmr: Some(body.mmr),
        telemetry_source: "qa".into(),
        ..Default::default()
    };
    let results = tokio::time::timeout(
        service.batch_timeout,
        service.run_test_batch(
            &user,
            &id,
            body.test_cases,
            r,
            body.llm_config.unwrap_or_default(),
            3,
        ),
    )
    .await
    .map_err(|_| QueryError::DependenciesFailed)??;
    let summary = super::qa::aggregate_test_results(&results);
    Ok(Json(json!({"results":results,"summary":summary})))
}

async fn retrieve_evaluate(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(body): Json<Value>,
) -> Response {
    let user = match actor(&state, &headers, verified).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    let request = match super::retrieval_eval::EvaluationRequest::parse(body) {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        state.retrieval.evaluate_cases(&user, &id, request),
    )
    .await
    {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => e.into_response(),
        Err(_) => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(json!({"detail":"Retrieval evaluation timed out"})),
        )
            .into_response(),
    }
}
async fn retrieval_presets() -> Json<Value> {
    Json(super::retrieval_eval::presets())
}

async fn query_history(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Query(filter): Query<super::observability::QueryHistoryFilter>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    Ok(Json(
        state
            .retrieval
            .store
            .list_query_history(&user, &id, filter)
            .await?,
    ))
}
async fn feedback_list(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Query(filter): Query<super::observability::FeedbackFilter>,
) -> Result<Json<Value>> {
    let user = actor(&state, &headers, verified).await?;
    Ok(Json(
        state
            .retrieval
            .store
            .list_query_feedback(&user, &id, filter)
            .await?,
    ))
}
async fn feedback_upsert(
    State(state): State<ApiState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    verified: Option<Extension<UserContext>>,
    Json(body): Json<super::observability::FeedbackUpsert>,
) -> Response {
    let user = match actor(&state, &headers, verified).await {
        Ok(u) => u,
        Err(e) => return e.into_response(),
    };
    match state
        .retrieval
        .store
        .upsert_query_feedback(&user, &id, body)
        .await
    {
        Ok(v) => Json(v).into_response(),
        Err(super::observability::ObservationError::Conflict) => (
            StatusCode::CONFLICT,
            Json(json!({"detail":"query fingerprint does not match the persisted trace"})),
        )
            .into_response(),
        Err(super::observability::ObservationError::Query(e)) => e.into_response(),
    }
}
