//! Bounded, cancellation-aware internal Knowledge RPC. No credential issuance:
//! existing signed gateway authority remains verified by the Knowledge owner.
pub use tonic;
pub use prost::Message as ProstMessage;
pub use tokio_util::sync::CancellationToken;
pub mod knowledge { tonic::include_proto!("platform.knowledge.v1"); }
use http::{HeaderMap, HeaderName, HeaderValue};
use knowledge::{RetrieveRequest, RetrieveResponse, knowledge_retrieval_client::KnowledgeRetrievalClient};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use tonic::{Request, Status, metadata::MetadataMap, transport::{Channel, Endpoint, ClientTlsConfig}};

pub const AUTHORITY_HEADERS: &[&str] = &[
    "x-gateway-secret", "x-user-id", "x-tenant-id", "x-user-tier", "x-user-type",
    "x-user-roles", "x-user-email", "x-user-name", "x-app-user-id", "x-app-tenant-id",
];
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
/// Diagnostic context has no authorization meaning and is not promoted to an actor.
pub const DIAGNOSTIC_HEADERS: &[&str] = &["traceparent", "tracestate", "x-request-id"];
pub const MAX_DIAGNOSTIC_BYTES: usize = 2048;

/// Copies signed authority only. Duplicate authority fields are rejected rather
/// than allowing HTTP and gRPC implementations to select different identities.
pub fn metadata_from_headers(headers: &HeaderMap) -> Result<MetadataMap, Status> {
    let mut selected = HeaderMap::new();
    let mut size = 0;
    for &name in AUTHORITY_HEADERS {
        let mut values = headers.get_all(name).iter();
        if let Some(value) = values.next() {
            if values.next().is_some() { return Err(Status::unauthenticated("duplicate authority metadata")); }
            if value.to_str().is_err() { return Err(Status::unauthenticated("invalid authority metadata")); }
            size += name.len() + value.as_bytes().len();
            selected.insert(HeaderName::from_static(name), value.clone());
        }
    }
    if size > MAX_METADATA_BYTES { return Err(Status::resource_exhausted("authority metadata too large")); }
    if selected.get("x-gateway-secret").is_none_or(|v| v.is_empty()) {
        return Err(Status::unauthenticated("signed gateway authority required"));
    }
    let mut diagnostic_size = 0;
    for &name in DIAGNOSTIC_HEADERS {
        let mut values = headers.get_all(name).iter();
        if let Some(value) = values.next() {
            if values.next().is_some() || value.to_str().is_err() {
                return Err(Status::invalid_argument("invalid diagnostic metadata"));
            }
            diagnostic_size += name.len() + value.as_bytes().len();
            selected.insert(HeaderName::from_static(name), value.clone());
        }
    }
    if diagnostic_size > MAX_DIAGNOSTIC_BYTES {
        return Err(Status::resource_exhausted("diagnostic metadata too large"));
    }
    Ok(MetadataMap::from_headers(selected))
}
pub fn headers_from_metadata(metadata: &MetadataMap) -> Result<HeaderMap, Status> {
    let mut headers = HeaderMap::new();
    for &name in AUTHORITY_HEADERS.iter().chain(DIAGNOSTIC_HEADERS.iter()) {
        for value in metadata.get_all(name).iter() {
            headers.append(HeaderName::from_static(name), HeaderValue::from_bytes(value.as_encoded_bytes())
                .map_err(|_| Status::unauthenticated("invalid authority metadata"))?);
        }
    }
    Ok(metadata_from_headers(&headers)?.into_headers())
}
pub fn retrieval_path(dataset_id: &str) -> Result<String, Status> {
    if dataset_id.is_empty() || dataset_id.len() > 128 || !dataset_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_') {
        return Err(Status::invalid_argument("invalid dataset identifier"));
    }
    Ok(format!("/api/v1/knowledge/{dataset_id}/retrieve"))
}
pub fn request_from_json(dataset_id: String, request_json: Vec<u8>) -> Result<RetrieveRequest, Status> {
    retrieval_path(&dataset_id)?;
    let body: serde_json::Value = serde_json::from_slice(&request_json).map_err(|_| Status::invalid_argument("invalid retrieval JSON"))?;
    let object = body.as_object().ok_or_else(|| Status::invalid_argument("retrieval body must be an object"))?;
    let query = object.get("query").and_then(|v| v.as_str()).ok_or_else(|| Status::invalid_argument("query must be a string"))?.to_owned();
    let top_k = object.get("top_k").map(|v| v.as_u64().ok_or_else(|| Status::invalid_argument("top_k must be uint64"))).transpose()?;
    let mode = object.get("mode").map(|v| v.as_str().map(str::to_owned).ok_or_else(|| Status::invalid_argument("mode must be a string"))).transpose()?;
    let optional_string = |name: &str| -> Result<Option<String>, Status> {
        object.get(name).filter(|v| !v.is_null()).map(|v| v.as_str().map(str::to_owned)
            .ok_or_else(|| Status::invalid_argument("retrieval filter must be a string"))).transpose()
    };
    let document_id = optional_string("document_id")?;
    let language_filter = optional_string("language_filter")?;
    let source_type_filter = optional_string("source_type_filter")?;
    let metadata_filter_json = object.get("metadata_filter").filter(|v| !v.is_null())
        .map(|v| serde_json::to_vec(v).map_err(|_| Status::invalid_argument("invalid metadata filter"))).transpose()?;
    Ok(RetrieveRequest { dataset_id, request_json, query, top_k, mode, document_id, language_filter, source_type_filter, metadata_filter_json })
}
pub fn validate_request(request: &RetrieveRequest) -> Result<(), Status> {
    let parsed = request_from_json(request.dataset_id.clone(), request.request_json.clone())?;
    if parsed.query != request.query || parsed.top_k != request.top_k || parsed.mode != request.mode
        || parsed.document_id != request.document_id || parsed.language_filter != request.language_filter
        || parsed.source_type_filter != request.source_type_filter || parsed.metadata_filter_json != request.metadata_filter_json {
        return Err(Status::invalid_argument("typed retrieval fields differ from signed body"));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct RpcConfig {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub max_in_flight: usize,
    pub channel_buffer: usize,
}
impl Default for RpcConfig {
    fn default() -> Self { Self {
        connect_timeout: Duration::from_secs(3), request_timeout: Duration::from_secs(30),
        max_request_bytes: 1024 * 1024, max_response_bytes: 16 * 1024 * 1024,
        max_in_flight: 32, channel_buffer: 32,
    } }
}
impl RpcConfig {
    pub fn validate(&self) -> Result<(), Status> {
        if self.connect_timeout.is_zero() || self.request_timeout.is_zero() || self.request_timeout > Duration::from_secs(300)
            || self.max_request_bytes == 0 || self.max_response_bytes == 0
            || self.max_request_bytes > 16 * 1024 * 1024 || self.max_response_bytes > 64 * 1024 * 1024
            || self.max_in_flight == 0 || self.max_in_flight > 4096 || self.channel_buffer == 0 || self.channel_buffer > 4096 {
            return Err(Status::invalid_argument("invalid RPC resource bounds"));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct RetrievalClient {
    client: KnowledgeRetrievalClient<Channel>,
    config: RpcConfig,
    capacity: Arc<Semaphore>,
}
impl RetrievalClient {
    pub async fn connect(uri: String, config: RpcConfig) -> Result<Self, Status> {
        config.validate()?;
        let mut endpoint = Endpoint::from_shared(uri).map_err(|_| Status::invalid_argument("invalid RPC endpoint"))?;
        if !matches!(endpoint.uri().scheme_str(), Some("http" | "https")) || endpoint.uri().authority().is_none()
            || endpoint.uri().authority().is_some_and(|a| a.as_str().contains('@'))
            || endpoint.uri().path_and_query().is_some_and(|p| p.as_str() != "/") {
            return Err(Status::invalid_argument("RPC endpoint must be an HTTP(S) origin"));
        }
        if endpoint.uri().scheme_str() == Some("https") {
            endpoint = endpoint.tls_config(ClientTlsConfig::new().with_webpki_roots()).map_err(|_| Status::invalid_argument("invalid RPC TLS configuration"))?;
        }
        endpoint = endpoint.connect_timeout(config.connect_timeout).timeout(config.request_timeout)
            .concurrency_limit(config.max_in_flight).buffer_size(config.channel_buffer)
            .http2_keep_alive_interval(Duration::from_secs(30)).keep_alive_timeout(Duration::from_secs(10));
        let channel = endpoint.connect().await.map_err(|_| Status::unavailable("Knowledge RPC connection unavailable"))?;
        Ok(Self { client: KnowledgeRetrievalClient::new(channel)
            .max_encoding_message_size(config.max_request_bytes).max_decoding_message_size(config.max_response_bytes),
            capacity: Arc::new(Semaphore::new(config.max_in_flight)), config })
    }
    pub async fn retrieve(&self, message: RetrieveRequest, headers: &HeaderMap, cancel: &CancellationToken) -> Result<RetrieveResponse, Status> {
        use prost::Message;
        if cancel.is_cancelled() { return Err(Status::cancelled("retrieval cancelled")); }
        if message.encoded_len() > self.config.max_request_bytes { return Err(Status::resource_exhausted("retrieval request too large")); }
        validate_request(&message)?;
        let metadata = metadata_from_headers(headers)?;
        // Load shedding bounds total work, including callers waiting for a channel.
        let _permit = self.capacity.clone().try_acquire_owned().map_err(|_| Status::resource_exhausted("Knowledge RPC capacity exhausted"))?;
        let mut request = Request::new(message);
        *request.metadata_mut() = metadata;
        request.set_timeout(self.config.request_timeout);
        let mut client = self.client.clone();
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(Status::cancelled("retrieval cancelled")),
            result = tokio::time::timeout(self.config.request_timeout, client.retrieve(request)) => {
                result.map_err(|_| Status::deadline_exceeded("Knowledge RPC deadline exceeded"))?.map(tonic::Response::into_inner)
            }
        }
    }
}

pub use headers_from_metadata as metadata_to_headers;
/// Project typed successful hits while retaining the complete external response.
/// Never fabricate a success if the owner emitted an invalid result shape.
pub fn build_response(status_code: u32, body: Vec<u8>) -> Result<RetrieveResponse, Status> {
    if !(100..=599).contains(&status_code) { return Err(Status::internal("invalid Knowledge response status")); }
    let mut hits = Vec::new();
    if (200..300).contains(&status_code) {
        let value: serde_json::Value = serde_json::from_slice(&body).map_err(|_| Status::internal("invalid Knowledge response JSON"))?;
        let segments = value.get("results").and_then(|v| v.as_array()).ok_or_else(|| Status::internal("missing Knowledge results"))?;
        for item in segments {
            let required = |name: &str| item.get(name).and_then(|v| v.as_str()).map(str::to_owned).ok_or_else(|| Status::internal("invalid Knowledge result field"));
            let optional = |name: &str| -> Result<Option<String>, Status> {
                item.get(name).filter(|v| !v.is_null()).map(|v| v.as_str().map(str::to_owned).ok_or_else(|| Status::internal("invalid Knowledge optional field"))).transpose()
            };
            let score = item.get("score").and_then(|v| v.as_f64()).filter(|n| n.is_finite()).ok_or_else(|| Status::internal("invalid Knowledge result score"))?;
            let metadata_json = serde_json::to_vec(item.get("metadata").unwrap_or(&serde_json::Value::Null)).map_err(|_| Status::internal("invalid result metadata"))?;
            let associated_images_json = match item.get("associated_images") {
                None => Vec::new(),
                Some(serde_json::Value::Array(images)) => images.iter().map(|v| serde_json::to_vec(v).map_err(|_| Status::internal("invalid image metadata"))).collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(Status::internal("invalid result images")),
            };
            hits.push(knowledge::RetrievalHit { segment_id: required("segment_id")?, document_id: required("document_id")?, score,
                text: required("text")?, metadata_json, content_type: optional("content_type")?.unwrap_or_else(|| "text".into()),
                image_url: optional("image_url")?, vlm_description: optional("vlm_description")?, associated_images_json });
        }
    }
    Ok(RetrieveResponse { status_code, response_json: body, hits, retry_after: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_bytes_and_typed_filters_agree() {
        let body = br#"{ "query":"hello","top_k":7,"mode":"hybrid","document_id":"doc","metadata_filter":{"a":1},"future_option":true }"#.to_vec();
        let mut request = request_from_json("dataset-1".into(), body.clone()).unwrap();
        assert_eq!(request.request_json, body);
        validate_request(&request).unwrap();
        request.query = "unsigned change".into();
        assert_eq!(validate_request(&request).unwrap_err().code(), tonic::Code::InvalidArgument);
        assert!(retrieval_path("../other").is_err());
        assert_eq!(retrieval_path("dataset-1").unwrap(), "/api/v1/knowledge/dataset-1/retrieve");
    }
    #[test]
    fn authority_metadata_is_allowlisted_and_duplicate_safe() {
        let mut headers = HeaderMap::new();
        headers.insert("x-gateway-secret", "synthetic-signature".parse().unwrap());
        headers.insert("x-user-id", "user1".parse().unwrap());
        headers.insert("authorization", "Bearer never-forward".parse().unwrap());
        headers.insert("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".parse().unwrap());
        headers.insert("tracestate", "vendor=opaque".parse().unwrap());
        headers.insert("x-request-id", "request-fixture".parse().unwrap());
        let metadata = metadata_from_headers(&headers).unwrap();
        assert!(metadata.get("authorization").is_none());
        assert_eq!(metadata_to_headers(&metadata).unwrap()["x-user-id"], "user1");
        assert_eq!(metadata_to_headers(&metadata).unwrap()["traceparent"], headers["traceparent"]);
        assert_eq!(metadata_to_headers(&metadata).unwrap()["tracestate"], headers["tracestate"]);
        assert_eq!(metadata_to_headers(&metadata).unwrap()["x-request-id"], "request-fixture");
        headers.append("x-user-id", "user2".parse().unwrap());
        assert_eq!(metadata_from_headers(&headers).unwrap_err().code(), tonic::Code::Unauthenticated);
        assert!(metadata_from_headers(&HeaderMap::new()).is_err());
    }
    #[test]
    fn typed_response_preserves_public_bytes_and_errors() {
        let body = br#"{"results":[{"segment_id":"s","document_id":"d","score":0.9,"text":"answer","metadata":{"page":1}}],"metadata":{"future":true}}"#.to_vec();
        let response = build_response(200, body.clone()).unwrap();
        assert_eq!(response.response_json, body);
        assert_eq!(response.hits[0].segment_id, "s");
        let error = build_response(403, br#"{"detail":"Forbidden"}"#.to_vec()).unwrap();
        assert!(error.hits.is_empty());
        assert!(build_response(200, b"{}".to_vec()).is_err());
    }
    #[test]
    fn resource_limits_cannot_be_unbounded() {
        let mut config = RpcConfig::default();
        config.validate().unwrap();
        config.max_in_flight = 0;
        assert!(config.validate().is_err());
    }
}
