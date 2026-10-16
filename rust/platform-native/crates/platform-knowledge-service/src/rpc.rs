//! Optional typed internal gRPC transport sharing the signed HTTP domain boundary.
//! No secondary actor resolver, synthetic identity, database bypass, or network HTTP hop.
use crate::{KnowledgeError, Result};
use ai_platform_rpc::{self as contract, ProstMessage, tonic};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use contract::knowledge::{
    RetrieveRequest, RetrieveResponse,
    knowledge_retrieval_server::{KnowledgeRetrieval, KnowledgeRetrievalServer},
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
};
use tokio_stream::wrappers::TcpListenerStream;
use tower::ServiceExt;

pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct RpcConfiguration {
    pub address: Option<SocketAddr>,
    pub max_concurrent_requests: usize,
    pub deadline: Duration,
}
impl Default for RpcConfiguration {
    fn default() -> Self {
        Self {
            address: None,
            max_concurrent_requests: 32,
            deadline: Duration::from_secs(30),
        }
    }
}
impl RpcConfiguration {
    pub fn validate(&self) -> Result<()> {
        if !(1..=256).contains(&self.max_concurrent_requests)
            || self.deadline < Duration::from_millis(100)
            || self.deadline > Duration::from_secs(120)
        {
            return Err(KnowledgeError::Configuration("internal_rpc_limits"));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct KnowledgeRpc {
    router: Router,
    permits: Arc<Semaphore>,
    deadline: Duration,
}
impl KnowledgeRpc {
    pub fn new(router: Router, config: &RpcConfiguration) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            router,
            permits: Arc::new(Semaphore::new(config.max_concurrent_requests)),
            deadline: config.deadline,
        })
    }
}
#[tonic::async_trait]
impl KnowledgeRetrieval for KnowledgeRpc {
    async fn retrieve(
        &self,
        request: tonic::Request<RetrieveRequest>,
    ) -> std::result::Result<tonic::Response<RetrieveResponse>, tonic::Status> {
        let _permit = self.permits.clone().try_acquire_owned().map_err(|_| {
            tonic::Status::resource_exhausted("Knowledge retrieval concurrency limit reached")
        })?;
        let headers = contract::metadata_to_headers(request.metadata())?;
        let input = request.into_inner();
        if input.request_json.len() > MAX_REQUEST_BYTES || input.encoded_len() > MAX_REQUEST_BYTES {
            return Err(tonic::Status::resource_exhausted(
                "Knowledge retrieval request is too large",
            ));
        }
        contract::validate_request(&input)?;
        if !headers.contains_key("x-gateway-secret") {
            return Err(tonic::Status::unauthenticated(
                "Signed Gateway authority is required",
            ));
        }
        let mut http = Request::builder()
            .method("POST")
            .uri(format!("/api/v1/knowledge/{}/retrieve", input.dataset_id))
            .body(Body::from(input.request_json))
            .map_err(|_| tonic::Status::invalid_argument("Invalid retrieval request"))?;
        *http.headers_mut() = headers;
        http.headers_mut().insert(
            "content-type",
            axum::http::HeaderValue::from_static("application/json"),
        );
        let router = self.router.clone();
        let execute = async move {
            let response = router
                .oneshot(http)
                .await
                .map_err(|_| tonic::Status::internal("Knowledge router unavailable"))?;
            let status = response.status().as_u16() as u32;
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .filter(|v| v.len() <= 128)
                .map(str::to_owned);
            let bytes = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
                .await
                .map_err(|_| {
                    tonic::Status::resource_exhausted("Knowledge retrieval response is too large")
                })?;
            let mut response = contract::build_response(status, bytes.to_vec())?;
            response.retry_after = retry_after;
            if response.encoded_len() > MAX_RESPONSE_BYTES {
                return Err(tonic::Status::resource_exhausted(
                    "Knowledge retrieval response is too large",
                ));
            }
            Ok(tonic::Response::new(response))
        };
        tokio::time::timeout(self.deadline, execute)
            .await
            .map_err(|_| {
                tonic::Status::deadline_exceeded("Knowledge retrieval deadline exceeded")
            })?
    }
}
pub async fn serve(
    listener: TcpListener,
    router: Router,
    config: RpcConfiguration,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let service = KnowledgeRetrievalServer::new(KnowledgeRpc::new(router, &config)?)
        .max_decoding_message_size(MAX_REQUEST_BYTES)
        .max_encoding_message_size(MAX_RESPONSE_BYTES);
    tonic::transport::Server::builder()
        .concurrency_limit_per_connection(config.max_concurrent_requests)
        .timeout(config.deadline)
        .add_service(service)
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), async move {
            if *shutdown.borrow() {
                return;
            }
            while shutdown.changed().await.is_ok() {
                if *shutdown.borrow() {
                    break;
                }
            }
        })
        .await
        .map_err(|_| KnowledgeError::Shutdown("internal_rpc_server"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration_knowledge_query::auth::{GatewayAuth, ReplayStore, verify_gateway};
    use axum::{Json, routing::post};
    use serde_json::json;
    use std::collections::BTreeMap;
    fn service() -> (KnowledgeRpc, Arc<GatewayAuth>) {
        let auth = Arc::new(
            GatewayAuth::new(
                "offline-rpc-test-material".into(),
                BTreeMap::new(),
                "v2".into(),
                ReplayStore::memory(),
            )
            .unwrap(),
        );
        let router = Router::new()
            .route(
                "/api/v1/knowledge/{dataset}/retrieve",
                post(|| async { Json(json!({"results":[],"metadata":{}})) }),
            )
            .layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                verify_gateway,
            ));
        (
            KnowledgeRpc::new(router, &RpcConfiguration::default()).unwrap(),
            auth,
        )
    }
    fn signed(auth: &GatewayAuth, query: &str) -> tonic::Request<RetrieveRequest> {
        let input = contract::request_from_json(
            "dataset".into(),
            serde_json::to_vec(&json!({"query":query})).unwrap(),
        )
        .unwrap();
        let mut headers = axum::http::HeaderMap::new();
        for (name, value) in [
            ("x-user-id", "user"),
            ("x-tenant-id", "tenant"),
            ("x-user-tier", "normal"),
            ("x-user-type", "user"),
            ("x-user-roles", "user"),
        ] {
            headers.insert(
                axum::http::HeaderName::from_static(name),
                value.parse().unwrap(),
            );
        }
        let signature = auth
            .sign(
                "POST",
                "/api/v1/knowledge/dataset/retrieve",
                "",
                &input.request_json,
                &headers,
            )
            .unwrap();
        headers.insert("x-gateway-secret", signature.parse().unwrap());
        let mut request = tonic::Request::new(input);
        *request.metadata_mut() = contract::metadata_from_headers(&headers).unwrap();
        request
    }
    #[tokio::test]
    async fn rpc_preserves_native_signature_and_replay_boundary() {
        let (service, auth) = service();
        let request = signed(&auth, "test");
        let mut replay = tonic::Request::new(request.get_ref().clone());
        *replay.metadata_mut() = request.metadata().clone();
        let response = service.retrieve(request).await.unwrap().into_inner();
        assert_eq!(response.status_code, 200);
        assert!(response.hits.is_empty());
        let response = service.retrieve(replay).await.unwrap().into_inner();
        assert_eq!(response.status_code, 401);
    }
    #[tokio::test]
    async fn rpc_rejects_missing_authority_and_changed_signed_body() {
        let (service, auth) = service();
        let unsigned = tonic::Request::new(
            contract::request_from_json("dataset".into(), br#"{"query":"test"}"#.to_vec()).unwrap(),
        );
        assert_eq!(
            service.retrieve(unsigned).await.unwrap_err().code(),
            tonic::Code::Unauthenticated
        );
        let mut changed = signed(&auth, "original");
        *changed.get_mut() =
            contract::request_from_json("dataset".into(), br#"{"query":"changed"}"#.to_vec())
                .unwrap();
        assert_eq!(
            service
                .retrieve(changed)
                .await
                .unwrap()
                .into_inner()
                .status_code,
            401
        );
    }
    #[tokio::test]
    async fn actual_rpc_listener_preserves_signed_router_replay_and_shutdown() {
        let (service, auth) = service();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (stop, receive) = watch::channel(false);
        let server = tokio::spawn(serve(
            listener,
            service.router,
            RpcConfiguration::default(),
            receive,
        ));
        let client = contract::RetrievalClient::connect(
            format!("http://{address}"),
            contract::RpcConfig::default(),
        )
        .await
        .unwrap();
        let request = signed(&auth, "wire test");
        let headers = contract::metadata_to_headers(request.metadata()).unwrap();
        let message = request.into_inner();
        let cancellation = contract::CancellationToken::new();
        let first = client
            .retrieve(message.clone(), &headers, &cancellation)
            .await
            .unwrap();
        let replay = client
            .retrieve(message, &headers, &cancellation)
            .await
            .unwrap();
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.status_code, 200);
        assert_eq!(replay.status_code, 401);
    }
    #[test]
    fn rpc_configuration_is_disabled_and_bounded() {
        assert!(RpcConfiguration::default().address.is_none());
        let mut config = RpcConfiguration::default();
        config.max_concurrent_requests = 0;
        assert!(config.validate().is_err());
        config.max_concurrent_requests = 32;
        config.deadline = Duration::from_secs(121);
        assert!(config.validate().is_err());
    }
}
