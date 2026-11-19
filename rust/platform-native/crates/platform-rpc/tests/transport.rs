//! Real HTTP/2 loopback transport, with synthetic authority only. Production HMAC
//! verification belongs to Knowledge's signed ingress adapter, not this fixture.
use ai_platform_rpc::{self as rpc, knowledge::{self, knowledge_retrieval_server::{KnowledgeRetrieval, KnowledgeRetrievalServer}}, tonic::{self, Request, Response, Status}};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
#[derive(Clone)]
struct Fixture { entered: Arc<Notify> }
#[tonic::async_trait]
impl KnowledgeRetrieval for Fixture {
    async fn retrieve(&self, request: Request<knowledge::RetrieveRequest>) -> Result<Response<knowledge::RetrieveResponse>, Status> {
        let headers = rpc::metadata_to_headers(request.metadata())?;
        if headers["x-gateway-secret"] != "synthetic-test-only" { return Err(Status::unauthenticated("fixture authority mismatch")); }
        if headers.get("x-user-id").and_then(|v| v.to_str().ok()) != Some("fixture-user")
            || headers.get("x-tenant-id").and_then(|v| v.to_str().ok()) != Some("fixture-tenant")
            || headers.get("x-request-id").and_then(|v| v.to_str().ok()) != Some("fixture-request")
            || headers.get("traceparent").and_then(|v| v.to_str().ok()) != Some("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01") {
            return Err(Status::unauthenticated("fixture metadata mismatch"));
        }
        rpc::validate_request(request.get_ref())?;
        if request.get_ref().query == "wait" {
            self.entered.notify_one();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        Ok(Response::new(rpc::build_response(200, br#"{"results":[{"segment_id":"segment","document_id":"document","text":"answer","score":1,"metadata":{}}]}"#.to_vec())?))
    }
}
#[tokio::test]
async fn real_channel_preserves_contract_cancels_and_sheds_excess_work() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let stop = rpc::CancellationToken::new();
    let shutdown = stop.clone();
    let entered = Arc::new(Notify::new());
    let service = Fixture { entered: entered.clone() };
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder().add_service(KnowledgeRetrievalServer::new(service)
            .max_decoding_message_size(1024 * 1024).max_encoding_message_size(16 * 1024 * 1024))
            .serve_with_incoming_shutdown(tokio_stream::wrappers::TcpListenerStream::new(listener), shutdown.cancelled_owned()).await.unwrap();
    });
    let client = rpc::RetrievalClient::connect(origin, rpc::RpcConfig { max_in_flight: 1, request_timeout: Duration::from_secs(2), ..Default::default() }).await.unwrap();
    let mut headers = http::HeaderMap::new();
    headers.insert("x-gateway-secret", "synthetic-test-only".parse().unwrap());
    headers.insert("x-user-id", "fixture-user".parse().unwrap());
    headers.insert("x-tenant-id", "fixture-tenant".parse().unwrap());
    headers.insert("x-request-id", "fixture-request".parse().unwrap());
    headers.insert("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01".parse().unwrap());
    let message = || rpc::request_from_json("dataset".into(), br#"{"query":"answer"}"#.to_vec()).unwrap();
    let response = client.retrieve(message(), &headers, &rpc::CancellationToken::new()).await.unwrap();
    assert_eq!(response.hits[0].text, "answer");
    let cancel = rpc::CancellationToken::new();
    let child_cancel = cancel.clone();
    let child_client = client.clone();
    let child_headers = headers.clone();
    let pending = tokio::spawn(async move { child_client.retrieve(rpc::request_from_json("dataset".into(), br#"{"query":"wait"}"#.to_vec()).unwrap(), &child_headers, &child_cancel).await });
    tokio::time::timeout(Duration::from_secs(2), entered.notified()).await.unwrap();
    let overloaded = client.retrieve(message(), &headers, &rpc::CancellationToken::new()).await.unwrap_err();
    assert_eq!(overloaded.code(), tonic::Code::ResourceExhausted);
    cancel.cancel();
    assert_eq!(pending.await.unwrap().unwrap_err().code(), tonic::Code::Cancelled);
    // The permit is released when the cancelled future is dropped.
    client.retrieve(message(), &headers, &rpc::CancellationToken::new()).await.unwrap();
    stop.cancel();
    tokio::time::timeout(Duration::from_secs(3), server).await.unwrap().unwrap();
}
