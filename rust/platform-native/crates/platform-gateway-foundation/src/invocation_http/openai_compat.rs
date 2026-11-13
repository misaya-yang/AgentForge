//! OpenAI-compatible HTTP protocol projection over the existing Gateway
//! invocation dispatcher. This module owns no provider transport, credential,
//! rate-limit, quota, or usage implementation.
use super::*;
use crate::{
    capacity_admission::AdmissionCancellation,
    gateway_dispatcher::{GatewayDispatchError, GatewayStream},
    protocol::{
        ContentData, ContentItem, InvocationMode, ProtocolTimestamp, ServiceType, StreamChunk,
        StreamEventType, UnifiedRequest, UnifiedResponse,
    },
    request_validation::RequestValidationError,
    service_ownership::ServiceHandle,
};
use axum::{
    body::{Body, Bytes, to_bytes},
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_core::Stream;
use serde_json::{Map, Value, json};
use std::{
    collections::VecDeque,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    task::{Context, Poll},
};

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_MESSAGES: usize = 512;
const MAX_EMBEDDING_INPUTS: usize = 2048;
const MAX_TEXT_CHARS: usize = 1_000_000;

pub(super) fn router(state: InvocationState) -> Router {
    Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/embeddings", post(embeddings))
        .with_state(state)
}

struct ResolvedRequest {
    identity: InvocationIdentity,
    headers: HeaderMap,
    client_ip: Option<String>,
    cancellation: AdmissionCancellation,
}

struct RequestAuthInput {
    headers: HeaderMap,
    path: String,
    client_ip: Option<String>,
    cancellation: AdmissionCancellation,
}

fn request_auth_input(request: &Request) -> RequestAuthInput {
    RequestAuthInput {
        headers: request.headers().clone(),
        path: request.uri().path().to_owned(),
        client_ip: request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|peer| peer.0.ip().to_string()),
        cancellation: request
            .extensions()
            .get::<crate::request_governance::RequestCancellation>()
            .cloned()
            .unwrap_or_default(),
    }
}

async fn resolve_request(
    state: &InvocationState,
    mut input: RequestAuthInput,
) -> Result<ResolvedRequest, Response> {
    let api_key_header = state
        .identity
        .as_deref()
        .and_then(|identity| identity.api_key_header());
    normalize_openai_credentials(&mut input.headers, &input.path, api_key_header.as_ref())?;
    let identity = map_identity_result(
        super::resolve_identity(
            &state.auth,
            state.identity.as_deref(),
            &input.headers,
            &input.path,
        )
        .await,
    )?;
    let identity = require_authenticated_identity(identity)?;
    Ok(ResolvedRequest {
        identity,
        headers: input.headers,
        client_ip: input.client_ip,
        cancellation: input.cancellation,
    })
}

fn normalize_openai_credentials(
    headers: &mut HeaderMap,
    path: &str,
    api_key_header: Option<&axum::http::HeaderName>,
) -> Result<(), Response> {
    if !matches!(
        path,
        "/v1/models" | "/v1/chat/completions" | "/v1/embeddings"
    ) {
        return Ok(());
    }

    let standard_key_header = axum::http::HeaderName::from_static("x-api-key");
    let mut key_headers = vec![standard_key_header.clone()];
    if let Some(configured) = api_key_header
        && !key_headers.contains(configured)
    {
        key_headers.push(configured.clone());
    }
    let key_header_count: usize = key_headers
        .iter()
        .map(|name| headers.get_all(name).iter().count())
        .sum();
    let authorization = headers.get_all(axum::http::header::AUTHORIZATION);
    let authorization_count = authorization.iter().count();
    if authorization_count > 1 || key_header_count > 1 {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Conflicting credentials.",
            "authentication_error",
            "conflicting_credentials",
            None,
        ));
    }

    let Some(value) = authorization.iter().next() else {
        return Ok(());
    };
    if key_header_count != 0 {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Conflicting credentials.",
            "authentication_error",
            "conflicting_credentials",
            None,
        ));
    }
    let Ok(value) = value.to_str() else {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_error",
            "authentication_required",
            None,
        ));
    };
    let mut parts = value.split_whitespace();
    let bearer = parts
        .next()
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("bearer"));
    let Some(token) = parts.next() else {
        return Ok(());
    };
    if parts.next().is_some() || !bearer || !token.starts_with("gw_") {
        return Ok(());
    }
    let Some(api_key_header) = api_key_header else {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_error",
            "authentication_required",
            None,
        ));
    };
    let value = HeaderValue::from_bytes(token.as_bytes()).map_err(|_| {
        openai_error(
            StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_error",
            "authentication_required",
            None,
        )
    })?;
    headers.remove(axum::http::header::AUTHORIZATION);
    headers.insert(api_key_header.clone(), value);
    Ok(())
}

fn map_identity_result(
    result: Result<InvocationIdentity, Box<Response>>,
) -> Result<InvocationIdentity, Response> {
    result.map_err(|_| {
        openai_error(
            StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_error",
            "authentication_required",
            None,
        )
    })
}

fn require_authenticated_identity(
    identity: InvocationIdentity,
) -> Result<InvocationIdentity, Response> {
    if !identity.is_authenticated
        || identity.user_id.trim().is_empty()
        || identity.tenant_id.trim().is_empty()
        || identity.user_id.trim().eq_ignore_ascii_case("anonymous")
        || identity.tenant_id.trim().eq_ignore_ascii_case("public")
    {
        return Err(openai_error(
            StatusCode::UNAUTHORIZED,
            "Authentication required.",
            "authentication_error",
            "authentication_required",
            None,
        ));
    }
    Ok(identity)
}

async fn parse_json_body(request: Request) -> Result<Value, Response> {
    if !crate::http_content_type::is_json_content_type(request.headers()) {
        return Err(openai_error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json.",
            "invalid_request_error",
            "unsupported_media_type",
            None,
        ));
    }
    let bytes = to_bytes(request.into_body(), MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            openai_error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Request body is too large.",
                "invalid_request_error",
                "request_too_large",
                None,
            )
        })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        openai_error(
            StatusCode::BAD_REQUEST,
            "Invalid JSON request body.",
            "invalid_request_error",
            "invalid_json",
            None,
        )
    })?;
    if !value.is_object() {
        return Err(openai_error(
            StatusCode::BAD_REQUEST,
            "Request body must be a JSON object.",
            "invalid_request_error",
            "invalid_json_object",
            None,
        ));
    }
    Ok(value)
}

fn model_id(payload: &Value) -> Result<&str, Response> {
    payload["model"]
        .as_str()
        .map(str::trim)
        .filter(|model| !model.is_empty() && model.chars().count() <= 255)
        .ok_or_else(|| {
            openai_error(
                StatusCode::BAD_REQUEST,
                "model must be a non-empty string.",
                "invalid_request_error",
                "invalid_model",
                Some("model"),
            )
        })
}

async fn authorized_openai_services(
    state: &InvocationState,
    identity: &InvocationIdentity,
) -> Result<Vec<ServiceHandle>, Response> {
    if let Some(catalog) = &state.model_catalog {
        match catalog.services_for_tenant(&identity.tenant_id).await {
            Ok(services) => state
                .dispatcher
                .replace_openai_model_services(&identity.tenant_id, services),
            Err(_) => {
                // Credentials and permissions from an older projection must not
                // remain dispatchable after the authoritative catalog fails.
                state
                    .dispatcher
                    .replace_openai_model_services(&identity.tenant_id, Vec::new());
                return Err(openai_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Model catalogue is temporarily unavailable.",
                    "server_error",
                    "model_catalogue_unavailable",
                    None,
                ));
            }
        }
    }
    let authority = super::authority(identity);
    let services = state
        .dispatcher
        .authorized_services(&authority)
        .await
        .map_err(|_| {
            openai_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Model catalogue is temporarily unavailable.",
                "server_error",
                "model_catalogue_unavailable",
                None,
            )
        })?;
    Ok(services
        .into_iter()
        .filter(|service| {
            let service = service.snapshot();
            service.metadata["adapter_type"] == "openai"
                && matches!(
                    service.service_type,
                    ServiceType::Conversational | ServiceType::Generative | ServiceType::Embedding
                )
        })
        .collect())
}

async fn list_models(State(state): State<InvocationState>, request: Request) -> Response {
    let auth_input = request_auth_input(&request);
    drop(request);
    let context = match resolve_request(&state, auth_input).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let services = match authorized_openai_services(&state, &context.identity).await {
        Ok(services) => services,
        Err(response) => return response,
    };
    let mut data = services
        .into_iter()
        .map(|service| {
            let service = service.snapshot();
            json!({
                "id": service.metadata.get("openai_model_id").and_then(Value::as_str).unwrap_or(&service.service_id),
                "object": "model",
                "created": 0,
                "owned_by": "ai-platform"
            })
        })
        .collect::<Vec<_>>();
    data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Json(json!({"object":"list","data":data})).into_response()
}

fn selected_service(
    services: &[ServiceHandle],
    model: &str,
    expected: &[ServiceType],
) -> Result<ServiceHandle, Response> {
    services
        .iter()
        .find(|service| {
            let service = service.snapshot();
            service
                .metadata
                .get("openai_model_id")
                .and_then(Value::as_str)
                .unwrap_or(&service.service_id)
                == model
                && expected.contains(&service.service_type)
        })
        .cloned()
        .ok_or_else(|| {
            openai_error(
                StatusCode::NOT_FOUND,
                "The requested model was not found.",
                "invalid_request_error",
                "model_not_found",
                Some("model"),
            )
        })
}

fn chat_text(messages: &[Value]) -> Result<String, Response> {
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(openai_error(
            StatusCode::BAD_REQUEST,
            "messages must be a non-empty array of at most 512 items.",
            "invalid_request_error",
            "invalid_messages",
            Some("messages"),
        ));
    }
    let mut text = Vec::new();
    for message in messages {
        let Some(object) = message.as_object() else {
            return Err(openai_error(
                StatusCode::BAD_REQUEST,
                "Each message must be an object.",
                "invalid_request_error",
                "invalid_messages",
                Some("messages"),
            ));
        };
        let role = object.get("role").and_then(Value::as_str).unwrap_or("");
        if !matches!(
            role,
            "system" | "developer" | "user" | "assistant" | "tool" | "function"
        ) {
            return Err(openai_error(
                StatusCode::BAD_REQUEST,
                "Message role is invalid.",
                "invalid_request_error",
                "invalid_message_role",
                Some("messages"),
            ));
        }
        match object.get("content") {
            Some(Value::String(content)) => text.push(format!("{role}: {content}")),
            Some(Value::Array(parts)) => {
                let mut content = Vec::new();
                for part in parts {
                    if !part.is_object()
                        || !matches!(part["type"].as_str(), Some("text"))
                        || !part["text"].is_string()
                    {
                        return Err(openai_error(
                            StatusCode::BAD_REQUEST,
                            "Only text message content is supported by this service.",
                            "invalid_request_error",
                            "unsupported_message_content",
                            Some("messages"),
                        ));
                    }
                    content.push(part["text"].as_str().unwrap_or_default());
                }
                text.push(format!("{role}: {}", content.join("")));
            }
            Some(Value::Null) | None => {}
            _ => {
                return Err(openai_error(
                    StatusCode::BAD_REQUEST,
                    "Message content must be text, text parts, or null.",
                    "invalid_request_error",
                    "invalid_message_content",
                    Some("messages"),
                ));
            }
        }
    }
    let joined = text.join("\n");
    if joined.chars().count() > MAX_TEXT_CHARS {
        return Err(openai_error(
            StatusCode::BAD_REQUEST,
            "Combined message text exceeds the request limit.",
            "invalid_request_error",
            "input_too_large",
            Some("messages"),
        ));
    }
    // A tool-only assistant message can have no user-facing text. Keep the
    // generic invocation validator's required text input while forwarding the
    // untouched public messages through the existing OpenAI protocol adapter.
    if joined.trim().is_empty() {
        Ok(serde_json::to_string(messages).unwrap_or_default())
    } else {
        Ok(joined)
    }
}

const CHAT_PARAMETERS: &[&str] = &[
    "temperature",
    "top_p",
    "n",
    "stream_options",
    "max_tokens",
    "max_completion_tokens",
    "stop",
    "presence_penalty",
    "frequency_penalty",
    "logit_bias",
    "logprobs",
    "top_logprobs",
    "tools",
    "tool_choice",
    "parallel_tool_calls",
    "response_format",
    "seed",
    "user",
    "metadata",
    "service_tier",
    "reasoning_effort",
    "verbosity",
    "functions",
    "function_call",
];

fn chat_parameters(payload: &Value, messages: &[Value]) -> Map<String, Value> {
    let mut parameters = Map::new();
    for key in CHAT_PARAMETERS {
        if let Some(value) = payload.get(*key) {
            parameters.insert((*key).to_owned(), value.clone());
        }
    }
    parameters.insert("messages".into(), Value::Array(messages.to_vec()));
    parameters
}

fn chat_request(
    identity: &InvocationIdentity,
    service_id: &str,
    payload: &Value,
) -> Result<(UnifiedRequest, String), Response> {
    let messages = payload["messages"].as_array().ok_or_else(|| {
        openai_error(
            StatusCode::BAD_REQUEST,
            "messages must be an array.",
            "invalid_request_error",
            "invalid_messages",
            Some("messages"),
        )
    })?;
    let input = chat_text(messages)?;
    let parameters = chat_parameters(payload, messages);
    let stream_id = format!("chatcmpl-{}", uuid::Uuid::new_v4());
    Ok((
        UnifiedRequest {
            request_id: stream_id.clone(),
            service_id: service_id.to_owned(),
            inputs: vec![ContentItem::text(input)],
            session_id: None,
            context: None,
            parameters: Some(parameters),
            callback_url: None,
            priority: 0,
            user_id: identity.user_id.clone(),
            tenant_id: identity.tenant_id.clone(),
            timestamp: ProtocolTimestamp::now(),
        },
        stream_id,
    ))
}

async fn chat_completions(State(state): State<InvocationState>, request: Request) -> Response {
    let auth_input = request_auth_input(&request);
    let payload = match parse_json_body(request).await {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let context = match resolve_request(&state, auth_input).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let requested_model = match model_id(&payload) {
        Ok(model) => model.to_owned(),
        Err(response) => return response,
    };
    let streaming = match payload.get("stream") {
        None => false,
        Some(value) => match value.as_bool() {
            Some(value) => value,
            None => {
                return openai_error(
                    StatusCode::BAD_REQUEST,
                    "stream must be a boolean.",
                    "invalid_request_error",
                    "invalid_stream",
                    Some("stream"),
                );
            }
        },
    };
    let services = match authorized_openai_services(&state, &context.identity).await {
        Ok(services) => services,
        Err(response) => return response,
    };
    let service = match selected_service(
        &services,
        &requested_model,
        &[ServiceType::Conversational, ServiceType::Generative],
    ) {
        Ok(service) => service,
        Err(response) => return response,
    };
    if streaming
        && !service
            .snapshot()
            .supported_modes
            .contains(&InvocationMode::Stream)
    {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "Streaming is not supported for this model.",
            "invalid_request_error",
            "stream_not_supported",
            Some("stream"),
        );
    }
    let (unified, response_id) =
        match chat_request(&context.identity, &service.service_id(), &payload) {
            Ok(request) => request,
            Err(response) => return response,
        };
    let created = chrono::Utc::now().timestamp();
    let sdk_read_lease =
        match super::sdk_read_lease(&state, &unified, &context.identity, &context.headers).await {
            Ok(lease) => lease,
            Err(error) => return dispatch_error(error),
        };
    let authority = super::authority(&context.identity);
    if streaming {
        let gateway_stream = match state
            .dispatcher
            .stream(
                unified,
                &authority,
                context.client_ip.as_deref(),
                context.cancellation.clone(),
            )
            .await
        {
            Ok(stream) => stream,
            Err(error) => return dispatch_error(error),
        };
        let include_usage = payload["stream_options"]["include_usage"] == true;
        let body = OpenAiSse {
            stream: Some(gateway_stream),
            reading: None,
            pending: VecDeque::new(),
            cancellation: context.cancellation,
            _sdk_read_lease: sdk_read_lease,
            response_id,
            model: requested_model,
            created,
            include_usage,
            started: false,
            saw_terminal: false,
            terminal_usage: None,
            done: false,
        };
        let mut response = Response::new(Body::from_stream(body));
        response.headers_mut().insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream; charset=utf-8"),
        );
        response
            .headers_mut()
            .insert("cache-control", HeaderValue::from_static("no-cache"));
        response
            .headers_mut()
            .insert("x-accel-buffering", HeaderValue::from_static("no"));
        return response;
    }
    let _cancellation_guard = super::CancelOnDrop(context.cancellation.clone());
    let result = state
        .dispatcher
        .invoke(
            unified,
            &authority,
            context.client_ip.as_deref(),
            context.cancellation,
        )
        .await;
    match result {
        Ok(result) => chat_response(result, &response_id, &requested_model, created),
        Err(error) => dispatch_error(error),
    }
}

fn chat_response(
    response: UnifiedResponse,
    response_id: &str,
    model: &str,
    created: i64,
) -> Response {
    let content = response
        .outputs
        .iter()
        .find_map(|item| match item.data.as_ref() {
            Some(ContentData::Text(text)) => Some(text.clone()),
            Some(ContentData::Json(Value::String(text))) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let mut result = json!({
        "id": response_id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]
    });
    if let Some(usage) = response.usage.as_ref().and_then(openai_usage) {
        result["usage"] = usage;
    }
    Json(result).into_response()
}

fn embedding_request(
    identity: &InvocationIdentity,
    service_id: &str,
    payload: &Value,
) -> Result<UnifiedRequest, Response> {
    let (input, texts) = match &payload["input"] {
        Value::String(text) if !text.trim().is_empty() => {
            (Value::String(text.clone()), vec![text.clone()])
        }
        Value::Array(values)
            if !values.is_empty()
                && values.len() <= MAX_EMBEDDING_INPUTS
                && values
                    .iter()
                    .all(|item| item.as_str().is_some_and(|s| !s.trim().is_empty())) =>
        {
            let texts = values
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_owned())
                .collect::<Vec<_>>();
            (Value::Array(values.clone()), texts)
        }
        _ => {
            return Err(openai_error(
                StatusCode::BAD_REQUEST,
                "input must be a non-empty string or a non-empty array of strings.",
                "invalid_request_error",
                "invalid_embedding_input",
                Some("input"),
            ));
        }
    };
    if texts
        .iter()
        .any(|text| text.chars().count() > MAX_TEXT_CHARS)
    {
        return Err(openai_error(
            StatusCode::BAD_REQUEST,
            "Embedding input exceeds the request limit.",
            "invalid_request_error",
            "input_too_large",
            Some("input"),
        ));
    }
    let mut parameters = Map::new();
    parameters.insert("input".into(), input);
    for key in ["dimensions", "encoding_format", "user"] {
        if let Some(value) = payload.get(key) {
            parameters.insert(key.into(), value.clone());
        }
    }
    Ok(UnifiedRequest {
        request_id: format!("embed-{}", uuid::Uuid::new_v4()),
        service_id: service_id.to_owned(),
        inputs: texts.into_iter().map(ContentItem::text).collect(),
        session_id: None,
        context: None,
        parameters: Some(parameters),
        callback_url: None,
        priority: 0,
        user_id: identity.user_id.clone(),
        tenant_id: identity.tenant_id.clone(),
        timestamp: ProtocolTimestamp::now(),
    })
}

async fn embeddings(State(state): State<InvocationState>, request: Request) -> Response {
    let auth_input = request_auth_input(&request);
    let payload = match parse_json_body(request).await {
        Ok(payload) => payload,
        Err(response) => return response,
    };
    let context = match resolve_request(&state, auth_input).await {
        Ok(context) => context,
        Err(response) => return response,
    };
    let requested_model = match model_id(&payload) {
        Ok(model) => model.to_owned(),
        Err(response) => return response,
    };
    if payload.get("stream").is_some() {
        return openai_error(
            StatusCode::BAD_REQUEST,
            "stream is not supported for embeddings.",
            "invalid_request_error",
            "stream_not_supported",
            Some("stream"),
        );
    }
    let services = match authorized_openai_services(&state, &context.identity).await {
        Ok(services) => services,
        Err(response) => return response,
    };
    let service = match selected_service(&services, &requested_model, &[ServiceType::Embedding]) {
        Ok(service) => service,
        Err(response) => return response,
    };
    let unified = match embedding_request(&context.identity, &service.service_id(), &payload) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let _sdk_read_lease =
        match super::sdk_read_lease(&state, &unified, &context.identity, &context.headers).await {
            Ok(lease) => lease,
            Err(error) => return dispatch_error(error),
        };
    let _cancellation_guard = super::CancelOnDrop(context.cancellation.clone());
    match state
        .dispatcher
        .invoke(
            unified,
            &super::authority(&context.identity),
            context.client_ip.as_deref(),
            context.cancellation,
        )
        .await
    {
        Ok(response) => embedding_response(response, &requested_model),
        Err(error) => dispatch_error(error),
    }
}

fn embedding_response(response: UnifiedResponse, model: &str) -> Response {
    let value = response
        .outputs
        .iter()
        .find_map(|item| match item.data.as_ref() {
            Some(ContentData::Json(value)) if value.is_object() => Some(value.clone()),
            Some(ContentData::Text(text)) => serde_json::from_str::<Value>(text).ok(),
            _ => None,
        });
    let Some(mut value) = value else {
        return openai_error(
            StatusCode::BAD_GATEWAY,
            "Embedding service returned an invalid response.",
            "server_error",
            "invalid_embedding_response",
            None,
        );
    };
    if !value["data"].is_array() {
        return openai_error(
            StatusCode::BAD_GATEWAY,
            "Embedding service returned an invalid response.",
            "server_error",
            "invalid_embedding_response",
            None,
        );
    }
    value["object"] = json!("list");
    value["model"] = json!(model);
    if let Some(data) = value["data"].as_array_mut() {
        for (index, item) in data.iter_mut().enumerate() {
            if item.get("index").is_none() {
                item["index"] = json!(index);
            }
        }
    }
    if value.get("usage").is_none()
        && let Some(usage) = response.usage.as_ref().and_then(openai_usage)
    {
        value["usage"] = usage;
    } else if let Some(usage) = value.get("usage").and_then(Value::as_object) {
        let mut map = usage.clone();
        if map.get("prompt_tokens").is_none()
            && let Some(v) = map.get("input_tokens").cloned()
        {
            map.insert("prompt_tokens".into(), v);
        }
        if map.get("total_tokens").is_none()
            && let Some(v) = map.get("prompt_tokens").cloned()
        {
            map.insert("total_tokens".into(), v);
        }
        value["usage"] = Value::Object(map);
    }
    Json(value).into_response()
}

fn numeric(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_i64))
}

fn openai_usage(values: &Map<String, Value>) -> Option<Value> {
    let values = Value::Object(values.clone());
    let prompt = numeric(&values, &["prompt_tokens", "input_tokens"]);
    let completion = numeric(&values, &["completion_tokens", "output_tokens"]);
    let total = numeric(&values, &["total_tokens"])
        .or_else(|| prompt.zip(completion).map(|(p, c)| p.saturating_add(c)));
    if prompt.is_none() && completion.is_none() && total.is_none() {
        return None;
    }
    Some(json!({
        "prompt_tokens": prompt.unwrap_or(0),
        "completion_tokens": completion.unwrap_or(0),
        "total_tokens": total.unwrap_or_else(|| prompt.unwrap_or(0).saturating_add(completion.unwrap_or(0)))
    }))
}

fn stream_frame(
    response_id: &str,
    model: &str,
    created: i64,
    delta: Value,
    finish_reason: Value,
    usage: Option<Value>,
) -> Bytes {
    let mut value = json!({
        "id": response_id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{"index":0,"delta":delta,"finish_reason":finish_reason}]
    });
    if let Some(usage) = usage {
        value["choices"] = json!([]);
        value["usage"] = usage;
    }
    Bytes::from(format!("data: {}\n\n", value))
}

fn terminal_stream_frames(
    response_id: &str,
    model: &str,
    created: i64,
    include_usage: bool,
    usage: Option<Value>,
) -> Vec<Bytes> {
    let mut frames = vec![stream_frame(
        response_id,
        model,
        created,
        json!({}),
        json!("stop"),
        None,
    )];
    if include_usage {
        if let Some(usage) = usage {
            frames.push(stream_frame(
                response_id,
                model,
                created,
                json!({}),
                Value::Null,
                Some(usage),
            ));
        }
    }
    frames.push(Bytes::from_static(b"data: [DONE]\n\n"));
    frames
}

fn stream_chunk_frames(
    chunk: &StreamChunk,
    response_id: &str,
    model: &str,
    created: i64,
) -> Vec<Bytes> {
    match chunk.event_type {
        StreamEventType::TextDelta | StreamEventType::Final => {
            let text = match chunk.content.data.as_ref() {
                Some(ContentData::Text(text)) => Some(text.clone()),
                Some(ContentData::Json(Value::String(text))) => Some(text.clone()),
                _ => None,
            };
            text.filter(|text| !text.is_empty())
                .map(|text| {
                    vec![stream_frame(
                        response_id,
                        model,
                        created,
                        json!({"content":text}),
                        Value::Null,
                        None,
                    )]
                })
                .unwrap_or_default()
        }
        StreamEventType::ToolCallStart | StreamEventType::ToolCallDelta => {
            let Some(call) = &chunk.tool_call else {
                return Vec::new();
            };
            let function = if chunk.event_type == StreamEventType::ToolCallStart {
                json!({"name":call.name,"arguments":call.arguments})
            } else {
                json!({"arguments":call.arguments})
            };
            let mut tool_call = json!({"index":chunk.chunk_index,"function":function});
            if chunk.event_type == StreamEventType::ToolCallStart {
                tool_call["id"] = json!(call.tool_call_id);
                tool_call["type"] = json!("function");
            }
            vec![stream_frame(
                response_id,
                model,
                created,
                json!({"tool_calls":[tool_call]}),
                Value::Null,
                None,
            )]
        }
        _ => Vec::new(),
    }
}

fn stream_terminal_usage(chunk: &StreamChunk) -> Option<Value> {
    (chunk.event_type == StreamEventType::StreamEnd)
        .then(|| {
            chunk
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("usage"))
                .and_then(Value::as_object)
                .and_then(openai_usage)
        })
        .flatten()
}

fn stream_eof_frames(
    saw_terminal: bool,
    response_id: &str,
    model: &str,
    created: i64,
    include_usage: bool,
    usage: Option<Value>,
) -> Vec<Bytes> {
    if saw_terminal {
        terminal_stream_frames(response_id, model, created, include_usage, usage)
    } else {
        vec![stream_error_frame(
            "Gateway stream ended without a terminal event.",
            "stream_incomplete",
        )]
    }
}

fn upstream_stream_error_frame() -> Bytes {
    stream_error_frame("The upstream stream failed.", "upstream_error")
}

type StreamRead = Pin<
    Box<
        dyn Future<
                Output = (
                    GatewayStream,
                    Result<Option<StreamChunk>, GatewayDispatchError>,
                ),
            > + Send,
    >,
>;

struct OpenAiSse {
    stream: Option<GatewayStream>,
    reading: Option<StreamRead>,
    pending: VecDeque<Bytes>,
    cancellation: AdmissionCancellation,
    _sdk_read_lease: Option<crate::sdk_read_grants::SdkReadLease>,
    response_id: String,
    model: String,
    created: i64,
    include_usage: bool,
    started: bool,
    saw_terminal: bool,
    terminal_usage: Option<Value>,
    done: bool,
}

impl Drop for OpenAiSse {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl Stream for OpenAiSse {
    type Item = Result<Bytes, std::convert::Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        if let Some(bytes) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(bytes)));
        }
        if !this.started {
            this.started = true;
            return Poll::Ready(Some(Ok(stream_frame(
                &this.response_id,
                &this.model,
                this.created,
                json!({"role":"assistant"}),
                Value::Null,
                None,
            ))));
        }
        if this.done {
            return Poll::Ready(None);
        }
        loop {
            if this.reading.is_none() {
                let Some(mut stream) = this.stream.take() else {
                    this.done = true;
                    return Poll::Ready(None);
                };
                this.reading = Some(Box::pin(async move {
                    let next = stream.next().await;
                    (stream, next)
                }));
            }
            let read = this.reading.as_mut().expect("stream read is present");
            match read.as_mut().poll(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready((stream, result)) => {
                    this.reading = None;
                    this.stream = Some(stream);
                    match result {
                        Ok(Some(chunk)) if chunk.event_type == StreamEventType::StreamEnd => {
                            this.saw_terminal = true;
                            this.terminal_usage = stream_terminal_usage(&chunk);
                        }
                        Ok(Some(chunk)) => {
                            this.pending.extend(stream_chunk_frames(
                                &chunk,
                                &this.response_id,
                                &this.model,
                                this.created,
                            ));
                        }
                        Ok(None) => {
                            this.pending.extend(stream_eof_frames(
                                this.saw_terminal,
                                &this.response_id,
                                &this.model,
                                this.created,
                                this.include_usage,
                                this.terminal_usage.take(),
                            ));
                            this.done = true;
                        }
                        Err(_) => {
                            this.pending.push_back(upstream_stream_error_frame());
                            this.done = true;
                        }
                    }
                    if let Some(bytes) = this.pending.pop_front() {
                        return Poll::Ready(Some(Ok(bytes)));
                    }
                }
            }
        }
    }
}

fn stream_error_frame(message: &str, code: &str) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        json!({"error":{"message":message,"type":"server_error","param":null,"code":code}})
    ))
}

fn dispatch_failure_class(error: &GatewayDispatchError) -> (&'static str, Option<u16>) {
    use crate::dispatcher::{AdapterFailure, DispatchError};
    match error {
        GatewayDispatchError::Dispatch(DispatchError::Adapter(AdapterFailure::HttpStatus(
            status,
        ))) => ("adapter_http_status", Some(*status)),
        GatewayDispatchError::Dispatch(DispatchError::Adapter(_)) => ("adapter_failure", None),
        GatewayDispatchError::Dispatch(DispatchError::AdapterValidationFailed(_))
        | GatewayDispatchError::Dispatch(DispatchError::AdapterValidationDetail(_)) => {
            ("adapter_validation", None)
        }
        GatewayDispatchError::Metadata(_) => ("usage_metadata", None),
        GatewayDispatchError::Configuration(_) => ("dispatch_configuration", None),
        GatewayDispatchError::Registry(_) => ("registry_failure", None),
        GatewayDispatchError::Preflight(_) => ("preflight_failure", None),
        GatewayDispatchError::Session(_) => ("session_failure", None),
        GatewayDispatchError::Task(_) => ("task_failure", None),
        GatewayDispatchError::StreamStatistics(_) => ("stream_statistics", None),
        GatewayDispatchError::Cleanup => ("cleanup_failure", None),
        GatewayDispatchError::Dispatch(_) => ("dispatch_failure", None),
    }
}

fn dispatch_error(error: GatewayDispatchError) -> Response {
    let expected_client_error = matches!(
        &error,
        GatewayDispatchError::Preflight(
            crate::dispatch_preflight::PreflightError::ServiceNotFound(_)
                | crate::dispatch_preflight::PreflightError::RateLimited(_)
                | crate::dispatch_preflight::PreflightError::Validation(_)
        ) | GatewayDispatchError::Session(crate::session_manager::SessionError::PermissionDenied(
            _
        )) | GatewayDispatchError::Registry(
            crate::service_registry::RegistryError::ValidationFailed(_)
        )
    );
    if !expected_client_error {
        let (failure_class, provider_status) = dispatch_failure_class(&error);
        tracing::warn!(
            target: "ai_platform_gateway::openai_dispatch",
            failure_class,
            provider_status = ?provider_status,
            "OpenAI-compatible dispatch failed"
        );
    }
    match error {
        GatewayDispatchError::Preflight(
            crate::dispatch_preflight::PreflightError::ServiceNotFound(_),
        ) => openai_error(
            StatusCode::NOT_FOUND,
            "The requested model was not found.",
            "invalid_request_error",
            "model_not_found",
            Some("model"),
        ),
        GatewayDispatchError::Preflight(
            crate::dispatch_preflight::PreflightError::RateLimited(_),
        ) => openai_error(
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded.",
            "rate_limit_error",
            "rate_limit_exceeded",
            None,
        ),
        GatewayDispatchError::Preflight(crate::dispatch_preflight::PreflightError::Validation(
            RequestValidationError::PermissionDenied(_)
            | RequestValidationError::AuthenticationRequired(_),
        ))
        | GatewayDispatchError::Session(crate::session_manager::SessionError::PermissionDenied(
            _,
        )) => openai_error(
            StatusCode::FORBIDDEN,
            "Access to the requested model is denied.",
            "permission_error",
            "model_access_denied",
            Some("model"),
        ),
        GatewayDispatchError::Preflight(crate::dispatch_preflight::PreflightError::Validation(
            _,
        ))
        | GatewayDispatchError::Registry(
            crate::service_registry::RegistryError::ValidationFailed(_),
        ) => openai_error(
            StatusCode::BAD_REQUEST,
            "The request is invalid for this model.",
            "invalid_request_error",
            "invalid_request",
            None,
        ),
        _ => openai_error(
            StatusCode::BAD_GATEWAY,
            "The model request failed.",
            "server_error",
            "upstream_error",
            None,
        ),
    }
}

fn openai_error(
    status: StatusCode,
    message: &str,
    kind: &str,
    code: &str,
    param: Option<&str>,
) -> Response {
    (
        status,
        Json(json!({"error":{"message":message,"type":kind,"param":param,"code":code}})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> InvocationIdentity {
        InvocationIdentity {
            user_id: "resolved-user".into(),
            tenant_id: "resolved-tenant".into(),
            is_authenticated: true,
            tier: "public".into(),
            roles: vec!["user".into()],
            permissions: vec![],
            allowed_models: None,
            session_principal: None,
            session_jwt_selected: false,
        }
    }

    #[test]
    fn chat_mapping_preserves_messages_and_uses_only_resolved_identity() {
        let payload = json!({
            "model":"chat-service",
            "messages":[{"role":"system","content":"rules"},{"role":"user","content":"hello"}],
            "temperature":0.25,
            "tenant_id":"forged-tenant",
            "user_id":"forged-user",
            "service_id":"other-service"
        });
        let (request, id) = chat_request(&identity(), "chat-service", &payload).unwrap();
        assert!(id.starts_with("chatcmpl-"));
        assert_eq!(request.service_id, "chat-service");
        assert_eq!(request.user_id, "resolved-user");
        assert_eq!(request.tenant_id, "resolved-tenant");
        assert_eq!(
            request.parameters.as_ref().unwrap()["messages"],
            payload["messages"]
        );
        assert_eq!(request.parameters.as_ref().unwrap()["temperature"], 0.25);
        assert!(
            request
                .parameters
                .as_ref()
                .unwrap()
                .get("tenant_id")
                .is_none()
        );
        assert!(
            request
                .parameters
                .as_ref()
                .unwrap()
                .get("service_id")
                .is_none()
        );
    }

    #[test]
    fn chat_mapping_rejects_non_text_content_and_bad_roles() {
        let image = json!({"model":"chat","messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.test/x"}}]}]});
        assert!(chat_request(&identity(), "chat", &image).is_err());
        let role = json!({"model":"chat","messages":[{"role":"operator","content":"hello"}]});
        assert!(chat_request(&identity(), "chat", &role).is_err());
    }

    #[test]
    fn embedding_mapping_preserves_single_and_batched_inputs() {
        let single = json!({"input":"hello"});
        let request = embedding_request(&identity(), "embed", &single).unwrap();
        assert_eq!(request.service_id, "embed");
        assert_eq!(request.inputs.len(), 1);
        assert_eq!(request.parameters.as_ref().unwrap()["input"], "hello");
        let batch = json!({"input":["one","two"],"dimensions":128});
        let request = embedding_request(&identity(), "embed", &batch).unwrap();
        assert_eq!(request.inputs.len(), 2);
        assert_eq!(
            request.parameters.as_ref().unwrap()["input"],
            batch["input"]
        );
        assert_eq!(request.parameters.as_ref().unwrap()["dimensions"], 128);
    }

    #[tokio::test]
    async fn openai_compat_model_catalog_enforces_service_auth_and_invoke_permission() {
        use crate::{
            dispatch_preflight::{
                DispatchPreflight, PreflightAuthority, UnconfiguredRateLimitEventRecorder,
            },
            dispatcher_rate_limiter::{RateLimitConfig, RateLimiter},
            protocol::{ServiceAuthConfig, ServiceConfig, ServiceDefinition},
            service_registry::{MemoryRegistryStorage, ServiceRegistry},
        };
        use ai_platform_auth::rbac::Rbac;

        let registry = Arc::new(ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new())));
        registry
            .register(ServiceDefinition::new("visible", "Visible"))
            .await
            .unwrap();
        let mut restricted = ServiceDefinition::new("restricted", "Restricted");
        let mut config = ServiceConfig::default();
        config.auth = ServiceAuthConfig {
            enabled: true,
            require_auth: true,
            allowed_roles: vec!["admin".into()],
            ..Default::default()
        };
        restricted.service_config = Some(config);
        registry.register(restricted).await.unwrap();

        let preflight = DispatchPreflight::new(
            registry,
            Arc::new(Rbac::default()),
            Arc::new(RateLimiter::new(RateLimitConfig::default())),
            Arc::new(UnconfiguredRateLimitEventRecorder),
        );
        let roles = vec!["user".into(), "service:invoke".into()];
        let authority =
            PreflightAuthority::from_resolved_context("resolved-user", "resolved-tenant", &roles);
        let services = preflight.authorized_services(&authority).await.unwrap();
        assert_eq!(
            services
                .iter()
                .map(ServiceHandle::service_id)
                .collect::<Vec<_>>(),
            vec!["visible".to_owned()]
        );
    }

    #[tokio::test]
    async fn managed_model_dispatch_enforces_tenant_and_model_access_level() {
        use crate::{
            dispatch_preflight::{
                DispatchPreflight, PreflightAuthority, PreflightError,
                UnconfiguredRateLimitEventRecorder,
            },
            dispatcher_rate_limiter::{RateLimitConfig, RateLimiter},
            protocol::{
                ContentItem, ServiceAuthConfig, ServiceConfig, ServiceDefinition, ServiceType,
            },
            service_registry::{MemoryRegistryStorage, ServiceRegistry},
        };
        use ai_platform_auth::rbac::Rbac;

        let registry = Arc::new(ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new())));
        let mut definition = ServiceDefinition::new("managed-openai-tiered", "Model");
        definition.service_type = ServiceType::Generative;
        definition.metadata = json!({
            "adapter_type":"openai",
            "openai_model_id":"qwen3.8-flash",
            "openai_compat_scope":{"tenant_id":"tenant-a","access_level":"premium"}
        });
        definition.service_config = Some(ServiceConfig {
            auth: ServiceAuthConfig {
                enabled: true,
                require_auth: true,
                ..Default::default()
            },
            ..Default::default()
        });
        registry
            .replace_dispatcher_model_services("tenant-a", vec![ServiceHandle::new(definition)]);
        let preflight = DispatchPreflight::new(
            registry.clone(),
            Arc::new(Rbac::default()),
            Arc::new(RateLimiter::new(RateLimitConfig::default())),
            Arc::new(UnconfiguredRateLimitEventRecorder),
        );
        let roles = vec!["user".into(), "service:invoke".into()];
        let basic = PreflightAuthority::from_resolved_context_with_tier(
            "user-a", "tenant-a", "basic", &roles,
        );
        assert!(
            preflight
                .authorized_services(&basic)
                .await
                .unwrap()
                .is_empty()
        );
        let request = UnifiedRequest::new(
            "request",
            "managed-openai-tiered",
            vec![ContentItem::text("hello")],
        );
        assert!(matches!(
            preflight
                .prepare(
                    request.clone(),
                    &basic,
                    None,
                    crate::dispatch_preflight::PreflightMode::Invoke
                )
                .await,
            Err(PreflightError::Validation(
                RequestValidationError::PermissionDenied(_)
            ))
        ));

        let premium = PreflightAuthority::from_resolved_context_with_tier(
            "user-a", "tenant-a", "premium", &roles,
        );
        assert_eq!(
            preflight.authorized_services(&premium).await.unwrap().len(),
            1
        );
        assert!(
            preflight
                .prepare(
                    request.clone(),
                    &premium,
                    None,
                    crate::dispatch_preflight::PreflightMode::Invoke
                )
                .await
                .is_ok()
        );
        assert!(matches!(
            preflight
                .prepare(
                    request.clone(),
                    &premium,
                    None,
                    crate::dispatch_preflight::PreflightMode::Submit
                )
                .await,
            Err(PreflightError::Validation(
                RequestValidationError::ValidationFailed(_)
            ))
        ));

        let other_tenant = PreflightAuthority::from_resolved_context_with_tier(
            "user-b", "tenant-b", "admin", &roles,
        );
        assert!(matches!(
            preflight
                .prepare(
                    request,
                    &other_tenant,
                    None,
                    crate::dispatch_preflight::PreflightMode::Invoke
                )
                .await,
            Err(PreflightError::ServiceNotFound(_))
        ));
        assert!(
            preflight
                .authorized_services(&other_tenant)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn api_key_model_allowlist_filters_catalog_and_blocks_invoke_and_stream() {
        use crate::{
            dispatch_preflight::{
                DispatchPreflight, PreflightAuthority, PreflightError,
                UnconfiguredRateLimitEventRecorder,
            },
            dispatcher_rate_limiter::{RateLimitConfig, RateLimiter},
            protocol::{
                ContentItem, ServiceAuthConfig, ServiceConfig, ServiceDefinition, ServiceType,
            },
            service_registry::{MemoryRegistryStorage, ServiceRegistry},
        };
        use ai_platform_auth::rbac::Rbac;

        let registry = Arc::new(ServiceRegistry::new(Arc::new(MemoryRegistryStorage::new())));
        let mut services = Vec::new();
        for (service_id, model_id) in [
            ("managed-openai-allowed", "qwen3.8-flash"),
            ("managed-openai-denied", "qwen-plus"),
        ] {
            let mut definition = ServiceDefinition::new(service_id, model_id);
            definition.service_type = ServiceType::Conversational;
            definition.metadata = json!({
                "adapter_type":"openai",
                "openai_model_id":model_id,
                "openai_compat_scope":{"tenant_id":"tenant-a","access_level":"public"}
            });
            definition.service_config = Some(ServiceConfig {
                auth: ServiceAuthConfig {
                    enabled: true,
                    require_auth: true,
                    ..Default::default()
                },
                ..Default::default()
            });
            services.push(ServiceHandle::new(definition));
        }
        registry.replace_dispatcher_model_services("tenant-a", services);
        let preflight = DispatchPreflight::new(
            registry,
            Arc::new(Rbac::default()),
            Arc::new(RateLimiter::new(RateLimitConfig::default())),
            Arc::new(UnconfiguredRateLimitEventRecorder),
        );
        // Even a key carrying an admin role cannot exceed its explicit model list.
        let roles = vec!["admin".into(), "service:invoke".into()];
        let allowlist = vec!["qwen3.8-flash".into(), "text-embedding-v4".into()];
        let authority = PreflightAuthority::from_resolved_context_with_tier_and_model_scope(
            "key-owner",
            "tenant-a",
            "admin",
            &roles,
            Some(&allowlist),
        );
        let visible = preflight.authorized_services(&authority).await.unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible[0].snapshot().metadata["openai_model_id"],
            "qwen3.8-flash"
        );

        let denied = UnifiedRequest::new(
            "denied",
            "managed-openai-denied",
            vec![ContentItem::text("hello")],
        );
        for mode in [
            crate::dispatch_preflight::PreflightMode::Invoke,
            crate::dispatch_preflight::PreflightMode::Stream,
        ] {
            assert!(matches!(
                preflight
                    .prepare(denied.clone(), &authority, None, mode)
                    .await,
                Err(PreflightError::Validation(
                    RequestValidationError::PermissionDenied(_)
                ))
            ));
        }

        let allowed = UnifiedRequest::new(
            "allowed",
            "managed-openai-allowed",
            vec![ContentItem::text("hello")],
        );
        assert!(
            preflight
                .prepare(
                    allowed,
                    &authority,
                    None,
                    crate::dispatch_preflight::PreflightMode::Invoke
                )
                .await
                .is_ok()
        );
    }

    #[test]
    fn usage_mapping_accepts_openai_and_gateway_field_names() {
        let gateway = Map::from_iter([
            ("input_tokens".into(), json!(11)),
            ("output_tokens".into(), json!(4)),
        ]);
        assert_eq!(
            openai_usage(&gateway),
            Some(json!({"prompt_tokens":11,"completion_tokens":4,"total_tokens":15}))
        );
        let upstream = Map::from_iter([
            ("prompt_tokens".into(), json!(7)),
            ("completion_tokens".into(), json!(3)),
            ("total_tokens".into(), json!(10)),
        ]);
        assert_eq!(
            openai_usage(&upstream),
            Some(json!({
                "prompt_tokens":7,"completion_tokens":3,"total_tokens":10
            }))
        );
    }

    #[test]
    fn stream_projection_emits_openai_delta_and_final_usage_chunk() {
        let chunk = StreamChunk {
            request_id: "internal".into(),
            chunk_index: 1,
            content: ContentItem::text("hello"),
            is_final: false,
            event_type: StreamEventType::TextDelta,
            tool_call: None,
            metadata: None,
        };
        let frame = stream_chunk_frames(&chunk, "chatcmpl-1", "chat", 12);
        let value: Value = serde_json::from_slice(
            frame[0]
                .strip_prefix(b"data: ")
                .unwrap()
                .strip_suffix(b"\n\n")
                .unwrap()
                .as_ref(),
        )
        .unwrap();
        assert_eq!(value["choices"][0]["delta"]["content"], "hello");

        let usage = Map::from_iter([
            ("input_tokens".into(), json!(3)),
            ("output_tokens".into(), json!(2)),
            ("total_tokens".into(), json!(5)),
        ]);
        let final_frame = stream_frame(
            "chatcmpl-1",
            "chat",
            12,
            json!({}),
            Value::Null,
            openai_usage(&usage),
        );
        let value: Value = serde_json::from_slice(
            final_frame
                .strip_prefix(b"data: ")
                .unwrap()
                .strip_suffix(b"\n\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["choices"], json!([]));
        assert_eq!(value["usage"]["total_tokens"], 5);

        let frames = terminal_stream_frames(
            "chatcmpl-1",
            "chat",
            12,
            true,
            Some(json!({"prompt_tokens":3,"completion_tokens":2,"total_tokens":5})),
        );
        let terminal: Value = serde_json::from_slice(
            frames[0]
                .strip_prefix(b"data: ")
                .unwrap()
                .strip_suffix(b"\n\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(terminal["choices"][0]["finish_reason"], "stop");
        let usage_chunk: Value = serde_json::from_slice(
            frames[1]
                .strip_prefix(b"data: ")
                .unwrap()
                .strip_suffix(b"\n\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(usage_chunk["choices"], json!([]));
        assert_eq!(usage_chunk["usage"]["total_tokens"], 5);
        assert_eq!(frames[2].as_ref(), b"data: [DONE]\n\n");
        let without_usage = terminal_stream_frames("chatcmpl-1", "chat", 12, false, None);
        assert_eq!(without_usage.len(), 2);
        assert!(
            String::from_utf8_lossy(without_usage[0].as_ref())
                .contains("\"finish_reason\":\"stop\"")
        );
        assert_eq!(without_usage[1].as_ref(), b"data: [DONE]\n\n");
    }

    #[test]
    fn gateway_stream_end_usage_is_projected_only_when_requested() {
        let chunk = StreamChunk {
            request_id: "internal".into(),
            chunk_index: 9999,
            content: ContentItem::text(""),
            is_final: true,
            event_type: StreamEventType::StreamEnd,
            tool_call: None,
            metadata: Some(Map::from_iter([(
                "usage".into(),
                json!({"input_tokens":6,"output_tokens":4,"total_tokens":10}),
            )])),
        };
        let usage = stream_terminal_usage(&chunk).unwrap();
        let with_usage =
            stream_eof_frames(true, "chatcmpl-1", "chat", 12, true, Some(usage.clone()));
        assert_eq!(with_usage.len(), 3);
        assert!(String::from_utf8_lossy(with_usage[1].as_ref()).contains("\"total_tokens\":10"));
        let without_usage = stream_eof_frames(true, "chatcmpl-1", "chat", 12, false, Some(usage));
        assert_eq!(without_usage.len(), 2);
        assert_eq!(without_usage[1].as_ref(), b"data: [DONE]\n\n");
    }

    #[test]
    fn upstream_stream_failure_and_missing_terminal_emit_sanitized_sse_errors() {
        let incomplete = stream_eof_frames(false, "chatcmpl-1", "chat", 12, true, None);
        assert_eq!(incomplete.len(), 1);
        let incomplete: Value = serde_json::from_slice(
            incomplete[0]
                .strip_prefix(b"data: ")
                .unwrap()
                .strip_suffix(b"\n\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(incomplete["error"]["code"], "stream_incomplete");

        let failed = upstream_stream_error_frame();
        let failed: Value = serde_json::from_slice(
            failed
                .strip_prefix(b"data: ")
                .unwrap()
                .strip_suffix(b"\n\n")
                .unwrap(),
        )
        .unwrap();
        assert_eq!(failed["error"]["code"], "upstream_error");
        assert!(
            failed["error"]["message"]
                .as_str()
                .unwrap()
                .find("secret")
                .is_none()
        );
    }

    #[test]
    fn dropping_openai_sse_cancels_the_existing_admission_signal() {
        let cancellation = AdmissionCancellation::default();
        let body = OpenAiSse {
            stream: None,
            reading: None,
            pending: VecDeque::new(),
            cancellation: cancellation.clone(),
            _sdk_read_lease: None,
            response_id: "chatcmpl-1".into(),
            model: "chat".into(),
            created: 12,
            include_usage: false,
            started: false,
            saw_terminal: false,
            terminal_usage: None,
            done: false,
        };
        drop(body);
        assert!(cancellation.is_cancelled());
    }

    #[tokio::test]
    async fn route_auth_failure_is_openai_shaped_and_does_not_leak_resolver_details() {
        let secret_detail = Response::new(Body::from("private auth backend detail"));
        let response = match map_identity_result(Err(Box::new(secret_detail))) {
            Err(response) => response,
            Ok(_) => panic!("identity failure must be rejected"),
        };
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["type"], "authentication_error");
        assert_eq!(value["error"]["code"], "authentication_required");
        assert!(!String::from_utf8_lossy(&body).contains("private auth backend detail"));
    }

    #[test]
    fn dispatch_failure_diagnostic_uses_only_static_class_and_status() {
        let error = GatewayDispatchError::Dispatch(crate::dispatcher::DispatchError::Adapter(
            crate::dispatcher::AdapterFailure::HttpStatus(502),
        ));
        assert_eq!(
            dispatch_failure_class(&error),
            ("adapter_http_status", Some(502))
        );
        let error = GatewayDispatchError::Configuration("private detail");
        assert_eq!(
            dispatch_failure_class(&error),
            ("dispatch_configuration", None)
        );
    }

    #[test]
    fn openai_bearer_api_key_is_translated_to_the_canonical_key_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer gw_scoped-key"),
        );
        let key_header = axum::http::HeaderName::from_static("x-api-key");
        normalize_openai_credentials(&mut headers, "/v1/chat/completions", Some(&key_header))
            .unwrap();
        assert!(headers.get(axum::http::header::AUTHORIZATION).is_none());
        assert_eq!(headers.get(&key_header).unwrap(), "gw_scoped-key");
    }

    #[test]
    fn openai_credentials_reject_conflicts_and_duplicate_headers() {
        let key_header = axum::http::HeaderName::from_static("x-api-key");
        let mut conflicting = HeaderMap::new();
        conflicting.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer gw_scoped-key"),
        );
        conflicting.insert(key_header.clone(), HeaderValue::from_static("another-key"));
        let response =
            normalize_openai_credentials(&mut conflicting, "/v1/models", Some(&key_header))
                .unwrap_err();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let mut duplicate = HeaderMap::new();
        duplicate.append(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer gw_first"),
        );
        duplicate.append(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer gw_second"),
        );
        let response =
            normalize_openai_credentials(&mut duplicate, "/v1/embeddings", Some(&key_header))
                .unwrap_err();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn openai_normalization_preserves_jwt_and_rejects_bearer_keys_without_key_auth() {
        let mut jwt = HeaderMap::new();
        jwt.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer eyJ.jwt.token"),
        );
        let key_header = axum::http::HeaderName::from_static("x-api-key");
        normalize_openai_credentials(&mut jwt, "/v1/models", Some(&key_header)).unwrap();
        assert_eq!(
            jwt.get(axum::http::header::AUTHORIZATION).unwrap(),
            "Bearer eyJ.jwt.token"
        );

        let mut unavailable = HeaderMap::new();
        unavailable.insert(
            axum::http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer gw_unknown"),
        );
        assert!(normalize_openai_credentials(&mut unavailable, "/v1/models", None).is_err());
    }

    #[tokio::test]
    async fn canonical_anonymous_identity_is_rejected_by_openai_compat_routes() {
        let mut anonymous = identity();
        anonymous.user_id = "anonymous".into();
        anonymous.tenant_id = "public".into();
        anonymous.is_authenticated = false;
        let response = match require_authenticated_identity(anonymous) {
            Err(response) => response,
            Ok(_) => panic!("anonymous canonical identity must be rejected"),
        };
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["error"]["code"], "authentication_required");
    }

    #[tokio::test]
    async fn embedding_response_restores_openai_envelope_and_indices() {
        let response = UnifiedResponse {
            request_id: "r".into(),
            status: "success".into(),
            outputs: vec![ContentItem {
                content_type: crate::protocol::ContentType::Json,
                data: Some(ContentData::Json(json!({
                    "data":[{"embedding":[0.1,0.2]}],
                    "usage":{"prompt_tokens":4,"total_tokens":4}
                }))),
                url: None,
                mime_type: None,
                metadata: None,
            }],
            session_id: None,
            task_id: None,
            usage: None,
            error: None,
            metadata: None,
            timestamp: ProtocolTimestamp::now(),
        };
        let response = embedding_response(response, "embed-service");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["object"], "list");
        assert_eq!(value["model"], "embed-service");
        assert_eq!(value["data"][0]["index"], 0);
        assert_eq!(value["usage"]["prompt_tokens"], 4);
        assert_eq!(value["usage"]["total_tokens"], 4);
    }

    #[test]
    fn openai_compat_dispatch_error_is_safe_and_openai_shaped() {
        let response = openai_error(
            StatusCode::FORBIDDEN,
            "Access denied.",
            "permission_error",
            "model_access_denied",
            Some("model"),
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/json"
        );
    }
}
