//! Existing registry credentials/routing/capacity; selected codec is passthrough.
//! One POST per claimed call. No redirects, fallback, retry or caller endpoint.
use super::*;
use axum::http::HeaderMap;
use std::time::Duration;
#[path = "provider_budget.rs"]
mod budget;
pub(super) struct Prepared {
    url: reqwest::Url,
    headers: HeaderMap,
    body: Value,
    request_meter: bool,
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct ProviderResult {
    pub status: u16,
    pub output: Value,
    pub usage: Option<(i64, i64)>,
    pub cache: Value,
    #[serde(default)]
    pub requests: Option<u64>,
}
impl KnowledgeModelPlane {
    pub(super) async fn acquire(
        &self,
        call: &ModelCall,
    ) -> PlaneResult<Arc<crate::capacity_admission::atomic::model_plane::ModelPlaneLease>> {
        let shared = self
            .admission
            .model_plane_admission()
            .ok_or(PlaneError::new(
                503,
                "knowledge_model_admission_unavailable",
            ))?;
        let row = self
            .lookup(call)
            .await?
            .ok_or(PlaneError::new(404, "knowledge_model_lease_not_found"))?;
        if text(&row, "request_sha256")
            != call_digest(call)
                .map_err(|_| PlaneError::new(422, "knowledge_model_payload_invalid"))?
        {
            return Err(PlaneError::new(409, "knowledge_model_body_conflict"));
        }
        let budgets = self
            .capacity
            .resolve(&crate::capacity::CapacityRequest {
                tenant_id: call.scope.actor.tenant_id.clone(),
                service_id: "knowledge-model-plane".into(),
                request_class: if call.stream { "stream" } else { "invoke" }.into(),
                resource_kind: "provider".into(),
                provider_id: json!(call.model.provider),
                upstream_group: Value::Null,
                is_admin_read: false,
                service_config: Value::Null,
            })
            .map_err(|_| PlaneError::new(503, "knowledge_capacity_unavailable"))?;
        shared
            .acquire_knowledge(
                crate::capacity_admission::atomic::model_plane::ModelCallBinding {
                    tenant: call.scope.actor.tenant_id.clone(),
                    user: call.scope.actor.user_id.clone(),
                    reservation_id: text(&row, "lease_id").into(),
                    provider: call.model.provider.clone(),
                    model: call.model.model.clone(),
                },
                &budgets,
            )
            .await
            .map_err(|_| PlaneError::new(429, "knowledge_model_admission_rejected"))
    }
    pub(super) async fn prepare(
        &self,
        call: &ModelCall,
        snapshot: &Value,
    ) -> PlaneResult<Prepared> {
        let provider = self
            .providers
            .runtime_provider(&call.scope.actor.tenant_id, &call.model.provider)
            .await
            .map_err(|_| PlaneError::new(503, "knowledge_provider_unavailable"))?;
        if provider["updated_at"] != snapshot["provider"]["updated_at"] {
            return Err(PlaneError::new(409, "knowledge_provider_revision_changed"));
        }
        let key = text(&provider, "api_key");
        let base = text(&provider, "runtime_base_url");
        let no_auth = ai_platform_gateway_foundation::request_meter::credentialless_local(
            &snapshot["model"],
            &provider,
        );
        if (key.is_empty() && !no_auth) || base.is_empty() {
            return Err(PlaneError::new(
                503,
                "knowledge_provider_credentials_missing",
            ));
        }
        let mut url = endpoint(base, call.wire, &call.model.model)?;
        let wire_key = serde_json::to_value(call.wire)
            .map_err(|_| PlaneError::new(503, "knowledge_wire_invalid"))?;
        if let Some(configured) = provider["metadata"]["knowledge_model_endpoints"]
            [wire_key.as_str().unwrap_or("")]
        .as_str()
        {
            url = registered_endpoint(base, configured)?;
        }
        let mut headers = HeaderMap::new();
        headers.insert(
            "content-type",
            "application/json".parse().expect("constant header"),
        );
        if matches!(
            call.wire,
            ModelWire::GeminiEmbedding | ModelWire::GeminiGenerateContent
        ) {
            headers.insert(
                "x-goog-api-key",
                key.parse()
                    .map_err(|_| PlaneError::new(503, "knowledge_provider_key_invalid"))?,
            );
        } else if !no_auth {
            for (k, v) in
                crate::provider_routing::provider_headers(&snapshot["model"]["capabilities"], key)
                    .into_internal_headers()
            {
                headers.insert(
                    axum::http::HeaderName::from_bytes(k.as_bytes())
                        .map_err(|_| PlaneError::new(503, "knowledge_provider_header_invalid"))?,
                    v.parse()
                        .map_err(|_| PlaneError::new(503, "knowledge_provider_header_invalid"))?,
                );
            }
        }
        url.set_query(None);
        url.set_fragment(None);
        validate_codec(call, snapshot)?;
        Ok(Prepared {
            url,
            headers,
            body: call.payload.clone(),
            request_meter: snapshot["model"]["metadata"]["billing_meter"] == "requests",
        })
    }
    pub(super) async fn dispatch(
        &self,
        call: &ModelCall,
        lease: &ModelLease,
        prepared: Prepared,
        events: Option<tokio::sync::mpsc::Sender<ModelStreamEvent>>,
    ) -> PlaneResult<ProviderResult> {
        let response = self
            .client
            .post(prepared.url)
            .headers(prepared.headers)
            .timeout(Duration::from_millis(call.budget.deadline_ms))
            .json(&prepared.body)
            .send()
            .await
            .map_err(|_| PlaneError::new(502, "knowledge_provider_dispatch_unknown"))?;
        let provider_request = response
            .headers()
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .or_else(|| {
                response
                    .headers()
                    .get("request-id")
                    .and_then(|v| v.to_str().ok())
            })
            .map(str::to_owned);
        sqlx::query("UPDATE gateway.knowledge_model_calls SET provider_request_id=$2 WHERE lease_id=$1::uuid AND state='dispatched'").bind(&lease.lease_id).bind(provider_request).execute(&self.pool).await?;
        collect_response(response, call, events, prepared.request_meter).await
    }
}
async fn collect_response(
    mut response: reqwest::Response,
    call: &ModelCall,
    events: Option<tokio::sync::mpsc::Sender<ModelStreamEvent>>,
    request_meter: bool,
) -> PlaneResult<ProviderResult> {
    let status = response.status().as_u16();
    let mut bytes = Vec::new();
    let mut line_start = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| PlaneError::new(502, "knowledge_provider_dispatch_unknown"))?
    {
        if bytes.len().saturating_add(chunk.len()) > 32 * 1024 * 1024 {
            return Err(PlaneError::new(502, "knowledge_provider_response_limit"));
        }
        bytes.extend_from_slice(&chunk);
        if call.stream && status < 400 {
            while let Some(end) = bytes[line_start..].iter().position(|b| *b == b'\n') {
                let line = std::str::from_utf8(&bytes[line_start..line_start + end])
                    .map_err(|_| PlaneError::new(502, "knowledge_stream_invalid"))?;
                if let Some(data) = line
                    .strip_prefix("data:")
                    .map(str::trim)
                    .filter(|d| *d != "[DONE]")
                {
                    let output: Value = serde_json::from_str(data)
                        .map_err(|_| PlaneError::new(502, "knowledge_stream_invalid"))?;
                    if let Some(tx) = &events {
                        let _ = tx.send(ModelStreamEvent::Delta { output }).await;
                    }
                }
                line_start += end + 1;
            }
        }
    }
    if call.stream && status < 400 {
        let chunks = match parse_stream(&bytes) {
            Ok(chunks) => chunks,
            Err(error) if request_meter => {
                return Ok(ProviderResult {
                    status: 502,
                    output: json!({"code":error.code}),
                    usage: None,
                    cache: Value::Null,
                    requests: Some(1),
                });
            }
            Err(error) => return Err(error),
        };
        let usage = chunks.iter().rev().find_map(|v| usage(v, false));
        let cache = chunks
            .iter()
            .rev()
            .find(|v| v.get("usage").is_some())
            .cloned()
            .unwrap_or(Value::Null);
        return Ok(ProviderResult {
            status,
            output: json!({"chunks":chunks}),
            usage,
            cache,
            requests: request_observation(status),
        });
    }
    let output: Value = match serde_json::from_slice(&bytes) {
        Ok(output) => output,
        Err(_) if request_meter && (200..300).contains(&status) => {
            return Ok(ProviderResult {
                status: 502,
                output: json!({"code":"knowledge_provider_response_invalid"}),
                usage: None,
                cache: Value::Null,
                requests: Some(1),
            });
        }
        Err(_) => return Err(PlaneError::new(502, "knowledge_provider_response_invalid")),
    };
    let usage = usage(
        &output,
        matches!(
            call.wire,
            ModelWire::Embeddings
                | ModelWire::GeminiEmbedding
                | ModelWire::DashscopeEmbedding
                | ModelWire::DashscopeMultimodal
                | ModelWire::Rerank
                | ModelWire::DashscopeRerank
                | ModelWire::PairScores
        ),
    );
    let cache = output.clone();
    Ok(ProviderResult {
        status,
        output,
        usage,
        cache,
        requests: request_observation(status),
    })
}

fn request_observation(status: u16) -> Option<u64> {
    match ai_platform_gateway_foundation::request_meter::observed_requests(Some(status), true) {
        Some(ai_platform_gateway_foundation::request_meter::MeasuredUnits::Requests {
            requests,
            ..
        }) => Some(requests),
        _ => None,
    }
}
fn validate_codec(call: &ModelCall, snapshot: &Value) -> PlaneResult<()> {
    let embedding = matches!(
        call.purpose,
        ModelPurpose::QueryEmbedding
            | ModelPurpose::DocumentEmbedding
            | ModelPurpose::SimilarityEmbedding
    );
    let wire_embedding = matches!(
        call.wire,
        ModelWire::Embeddings
            | ModelWire::GeminiEmbedding
            | ModelWire::DashscopeEmbedding
            | ModelWire::DashscopeMultimodal
    );
    let rerank = call.purpose == ModelPurpose::Rerank;
    if embedding != wire_embedding
        || rerank
            != matches!(
                call.wire,
                ModelWire::Rerank | ModelWire::DashscopeRerank | ModelWire::PairScores
            )
    {
        return Err(PlaneError::new(
            422,
            "knowledge_model_operation_codec_mismatch",
        ));
    }
    // Registered model kind/capability, never a client-chosen operation upgrade.
    let model = &snapshot["model"];
    let kind = model["model_type"]
        .as_str()
        .or_else(|| model["metadata"]["model_type"].as_str())
        .unwrap_or("chat");
    if (embedding && !matches!(kind, "embedding" | "multimodal_embedding"))
        || (rerank && kind != "rerank")
        || (!embedding && !rerank && !matches!(kind, "chat" | "llm" | "vision" | "multimodal"))
    {
        return Err(PlaneError::new(
            403,
            "knowledge_model_operation_not_authorized",
        ));
    }
    if let Some(model) = call.payload.get("model") {
        if model.as_str() != Some(call.model.model.as_str()) {
            return Err(PlaneError::new(403, "knowledge_payload_model_mismatch"));
        }
    }
    if call.wire == ModelWire::GeminiEmbedding {
        let expected = format!("models/{}", call.model.model);
        if call.payload["requests"].as_array().is_none_or(|requests| {
            requests
                .iter()
                .any(|request| request["model"].as_str() != Some(expected.as_str()))
        }) {
            return Err(PlaneError::new(403, "knowledge_payload_model_mismatch"));
        }
    }
    if call.wire == ModelWire::PairScores
        && call.payload["provider"].as_str() != Some(call.model.provider.as_str())
    {
        return Err(PlaneError::new(403, "knowledge_payload_provider_mismatch"));
    }
    if call.stream != call.payload["stream"].as_bool().unwrap_or(false) {
        return Err(PlaneError::new(422, "knowledge_stream_binding_invalid"));
    }
    if !embedding && !rerank {
        budget::validate(call)?;
    }
    Ok(())
}
fn endpoint(base: &str, wire: ModelWire, model: &str) -> PlaneResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base)
        .map_err(|_| PlaneError::new(503, "knowledge_provider_endpoint_invalid"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PlaneError::new(503, "knowledge_provider_endpoint_invalid"));
    }
    if model.is_empty()
        || (matches!(
            wire,
            ModelWire::GeminiEmbedding | ModelWire::GeminiGenerateContent
        ) && (model.contains(['?', '#', '/']) || model.contains("..")))
    {
        return Err(PlaneError::new(
            422,
            "knowledge_provider_model_path_invalid",
        ));
    }
    let original = url.path().trim_end_matches('/');
    let dashscope = matches!(
        wire,
        ModelWire::DashscopeEmbedding
            | ModelWire::DashscopeMultimodal
            | ModelWire::DashscopeRerank
            | ModelWire::DashscopeVision
    );
    let normalized = if dashscope && !original.contains("/api/v1/services/") {
        let prefix = original
            .trim_end_matches("/compatible-mode/v1")
            .trim_end_matches("/compatible-mode")
            .trim_end_matches("/api/v1");
        format!("{prefix}/api/v1")
    } else {
        original.to_owned()
    };
    let path = normalized.as_str();
    let suffix = match wire {
        ModelWire::Embeddings => "embeddings",
        ModelWire::ChatCompletions => "chat/completions",
        ModelWire::Rerank => "rerank",
        ModelWire::PairScores => "score",
        ModelWire::DashscopeRerank => "services/rerank/text-rerank/text-rerank",
        ModelWire::DashscopeEmbedding => "services/embeddings/text-embedding/text-embedding",
        ModelWire::DashscopeMultimodal => {
            "services/embeddings/multimodal-embedding/multimodal-embedding"
        }
        ModelWire::DashscopeVision => "services/aigc/multimodal-generation/generation",
        ModelWire::GeminiEmbedding => "batchEmbedContents",
        ModelWire::GeminiGenerateContent => "generateContent",
    };
    let target = if matches!(
        wire,
        ModelWire::GeminiEmbedding | ModelWire::GeminiGenerateContent
    ) {
        format!(
            "{}/models/{model}:{suffix}",
            path.trim_end_matches("/models")
        )
    } else if path.ends_with(suffix) {
        path.into()
    } else {
        format!("{path}/{suffix}")
    };
    url.set_path(&target);
    Ok(url)
}
fn registered_endpoint(base: &str, configured: &str) -> PlaneResult<reqwest::Url> {
    let base = reqwest::Url::parse(base)
        .map_err(|_| PlaneError::new(503, "knowledge_provider_endpoint_invalid"))?;
    let target = base
        .join(configured)
        .map_err(|_| PlaneError::new(503, "knowledge_provider_endpoint_invalid"))?;
    if target.origin() != base.origin()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.query().is_some()
        || target.fragment().is_some()
    {
        return Err(PlaneError::new(
            503,
            "knowledge_registered_endpoint_origin_mismatch",
        ));
    }
    Ok(target)
}
fn usage(v: &Value, input_only: bool) -> Option<(i64, i64)> {
    let u = v.get("usage").or_else(|| v.get("usageMetadata"))?;
    let input = u["prompt_tokens"]
        .as_i64()
        .or_else(|| u["input_tokens"].as_i64())
        .or_else(|| u["promptTokenCount"].as_i64())
        .or_else(|| {
            input_only
                .then(|| {
                    u["total_tokens"]
                        .as_i64()
                        .or_else(|| u["totalTokenCount"].as_i64())
                })
                .flatten()
        })?;
    let output = u["completion_tokens"]
        .as_i64()
        .or_else(|| u["output_tokens"].as_i64())
        .or_else(|| u["candidatesTokenCount"].as_i64())
        .or_else(|| {
            input_only
                .then_some(0)
                .filter(|_| u["total_tokens"].is_i64())
        })?;
    (input >= 0 && output >= 0).then_some((input, output))
}
fn parse_stream(bytes: &[u8]) -> PlaneResult<Vec<Value>> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| PlaneError::new(502, "knowledge_stream_invalid"))?;
    let mut out = Vec::new();
    let mut terminal = false;
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data:").map(str::trim) {
            if data == "[DONE]" {
                terminal = true;
                continue;
            }
            let value: Value = serde_json::from_str(data)
                .map_err(|_| PlaneError::new(502, "knowledge_stream_invalid"))?;
            out.push(value)
        }
    }
    if !terminal {
        return Err(PlaneError::new(502, "knowledge_stream_interrupted"));
    }
    Ok(out)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_missing_is_unknown_not_free() {
        assert_eq!(usage(&json!({"data":[]}), true), None);
        assert_eq!(
            usage(
                &json!({"usage":{"prompt_tokens":2,"completion_tokens":3}}),
                false
            ),
            Some((2, 3))
        );
    }
    #[test]
    fn endpoint_cannot_take_credentials_or_untrusted_model_path() {
        assert!(endpoint("https://u:p@host/v1", ModelWire::Embeddings, "m").is_err());
        assert!(endpoint("https://host/v1", ModelWire::GeminiEmbedding, "../m").is_err());
        assert_eq!(
            endpoint("https://host/v1", ModelWire::Embeddings, "m")
                .unwrap()
                .path(),
            "/v1/embeddings"
        );
    }
    #[test]
    fn registered_paths_preserve_vendor_codec_without_credential_redirection() {
        assert_eq!(
            endpoint(
                "https://host/compatible-mode/v1",
                ModelWire::DashscopeEmbedding,
                "m"
            )
            .unwrap()
            .path(),
            "/api/v1/services/embeddings/text-embedding/text-embedding"
        );
        assert_eq!(
            registered_endpoint("https://host/v1", "/api/v1/reranks")
                .unwrap()
                .path(),
            "/api/v1/reranks"
        );
        assert!(registered_endpoint("https://host/v1", "https://foreign/score").is_err());
        assert!(registered_endpoint("https://host/v1", "http://host/score").is_err());
        assert!(
            endpoint(
                "https://host/v1",
                ModelWire::PairScores,
                "BAAI/bge-reranker-v2-m3"
            )
            .is_ok()
        );
    }
    #[test]
    fn interrupted_stream_does_not_settle() {
        assert!(parse_stream(b"data: {}\n").is_err());
        assert!(parse_stream(b"data: {}\n\ndata: [DONE]\n").is_ok());
    }
    #[tokio::test]
    async fn actual_score_http_success_has_one_request_and_no_invented_tokens() {
        use axum::{Json, Router, routing::post};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/score",
                post(|| async { Json(json!({"scores":[0.2,0.9]})) }),
            )
            .route(
                "/reject",
                post(|| async {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"code":"limited"})),
                    )
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let mut call = super::super::tests::call();
        call.purpose = ModelPurpose::Rerank;
        call.wire = ModelWire::PairScores;
        let response = client
            .post(format!("http://{address}/score"))
            .send()
            .await
            .unwrap();
        let result = collect_response(response, &call, None, true).await.unwrap();
        assert_eq!(result.requests, Some(1));
        assert_eq!(result.usage, None);
        assert_eq!(result.output["scores"], json!([0.2, 0.9]));
        let response = client
            .post(format!("http://{address}/reject"))
            .send()
            .await
            .unwrap();
        let result = collect_response(response, &call, None, true).await.unwrap();
        assert_eq!(result.requests, None);
        assert_eq!(result.usage, None);
        server.abort();
        let _ = server.await;
    }
    #[tokio::test]
    async fn interrupted_real_http_never_confirms_a_request_fee() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 2048];
            let _ = socket.read(&mut bytes).await.unwrap();
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"scores\":[1]}").await.unwrap();
            socket.shutdown().await.unwrap();
        });
        let client = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let response = client.post(format!("http://{address}/score")).send().await;
        let outcome = match response {
            Ok(response) => {
                collect_response(response, &super::super::tests::call(), None, true).await
            }
            Err(_) => Err(PlaneError::new(502, "unknown")),
        };
        assert!(outcome.is_err());
        server.await.unwrap();
    }
}
