//! Production router/stream tests with an isolated in-memory repository. These
//! measure fixture delivery, not production database or physical-device acceptance.
use super::*;
use crate::migration_local_node::{
    broker::{BrokerAuthorization, BrokerError, BrokerScope},
    protocol::{LocalNodeAction, LocalNodeDeviceScope},
};
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tower::ServiceExt;
const DEVICE: &str = "dev_aaaaaaaaaaaaaaaaaaaaaaaa";
const TOKEN: &str = "lnv2.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
#[derive(Default)]
struct Repository {
    revoked: AtomicBool,
    authorized: AtomicUsize,
    reject_proof: AtomicBool,
    controls: Mutex<Vec<PendingControl>>,
}
impl LocalNodeChannelRepository for Repository {
    fn schema_ready(&self) -> ChannelFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
    fn authorize_request<'a>(
        &'a self,
        hash: &'a str,
        device: &'a str,
        target: &'a str,
        _: &'a str,
        _: Option<&'a request_proof::RequestProof>,
    ) -> ChannelFuture<'a, ()> {
        Box::pin(async move {
            if hash != sha256(TOKEN.as_bytes())
                || device != DEVICE
                || !target.starts_with("https://")
                || self.reject_proof.load(Ordering::SeqCst)
            {
                return Err(ChannelStoreError::Rejected);
            }
            self.authorized.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn heartbeat<'a>(
        &'a self,
        request: &'a HeartbeatRequest,
        hash: &'a str,
    ) -> ChannelFuture<'a, HeartbeatResult> {
        Box::pin(async move {
            if hash != sha256(TOKEN.as_bytes())
                || request.device_id != DEVICE
                || self.revoked.load(Ordering::SeqCst)
            {
                return Err(ChannelStoreError::Rejected);
            }
            assert!(request.receipts.is_empty());
            Ok(HeartbeatResult {
                accepted_through_sequence: 0,
                claims: Vec::new(),
                controls: self.controls.lock().unwrap().clone(),
            })
        })
    }
    fn request_nonce<'a>(&'a self, _: &'a str, _: &'a str) -> ChannelFuture<'a, Value> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn pair<'a>(
        &'a self,
        _: &'a PairingRedeemRequest,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: f64,
    ) -> ChannelFuture<'a, ()> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn describe<'a>(
        &'a self,
        _: &'a WorkerContext,
        _: &'a str,
    ) -> ChannelFuture<'a, WorkerDescriptor> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn dispatch<'a>(
        &'a self,
        _: &'a WorkerContext,
        _: &'a WorkerDispatchRequest,
    ) -> ChannelFuture<'a, Value> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn status<'a>(
        &'a self,
        _: &'a WorkerContext,
        _: &'a str,
        _: u64,
    ) -> ChannelFuture<'a, Vec<WorkerReceiptEvent>> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn cancel<'a>(
        &'a self,
        _: &'a WorkerContext,
        _: &'a WorkerCancelRequest,
    ) -> ChannelFuture<'a, Value> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn verify_device_credential<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> ChannelFuture<'a, LocalNodeDeviceScope> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
    fn dispatch_adapter_accepted<'a>(&'a self, _: &'a LocalNodeAction) -> ChannelFuture<'a, ()> {
        Box::pin(async { Err(ChannelStoreError::Rejected) })
    }
}
struct DenyWorker;
impl BrokerAuthorization for DenyWorker {
    fn authorize(&self, _: &HeaderMap, _: &str, _: &Value) -> Result<BrokerScope, BrokerError> {
        Err(BrokerError::new(401, "test fixture denies worker"))
    }
}
async fn state() -> (Arc<LocalNodeChannelState>, Arc<Repository>) {
    let repository = Arc::new(Repository::default());
    let state = LocalNodeChannelState::new(
        repository.clone(),
        PlatformSigner::from_config(&LocalNodeChannelConfig {
            platform_key_id: "stream-test-key".into(),
            platform_signing_seed_b64: URL_SAFE_NO_PAD.encode([7u8; 32]),
            tls_certificate_path: "unused".into(),
            tls_private_key_path: "unused".into(),
            tls_listen_address: "127.0.0.1:8443".parse().unwrap(),
        })
        .unwrap(),
        Arc::new(DenyWorker),
    )
    .await
    .unwrap()
    .unwrap();
    (Arc::new(state), repository)
}
fn open_body(id: &str, cursor: Option<&str>) -> Value {
    json!({"protocol_version":PROTOCOL_V2,"kind":"control_stream","device_id":DEVICE,"stream_id":id,"after_cursor":cursor,"sent_at":unix_now()})
}
fn request(body: Value, tls: bool) -> Request {
    let mut request = Request::builder()
        .method("POST")
        .uri(DEVICE_CONTROL_PATH)
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("authorization", format!("Device {TOKEN}"))
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    if tls {
        request
            .extensions_mut()
            .insert(super::super::local_node_tls::HttpsRequest);
    }
    request
}
async fn open(state: Arc<LocalNodeChannelState>, id: &str, cursor: Option<&str>) -> Response {
    local_node_companion_router(Some(state))
        .oneshot(request(open_body(id, cursor), true))
        .await
        .unwrap()
}
fn signed_frame(bytes: &[u8], signer: &PlatformSigner, stream_id: &str, sequence: u64) -> Value {
    let mut frame: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(frame["device_id"], DEVICE);
    assert_eq!(frame["stream_id"], stream_id);
    assert_eq!(frame["sequence"], sequence);
    assert_eq!(frame["cursor"], format!("{stream_id}:{sequence}"));
    let signature = URL_SAFE_NO_PAD
        .decode(
            frame
                .as_object_mut()
                .unwrap()
                .remove("platform_signature")
                .unwrap()
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let public = URL_SAFE_NO_PAD.decode(signer.public_key_b64()).unwrap();
    UnparsedPublicKey::new(&ED25519, &public)
        .verify(canonical_json(&frame).as_bytes(), &signature)
        .unwrap();
    for command in frame["commands"].as_array().unwrap() {
        let mut payload = command.clone();
        let signature = URL_SAFE_NO_PAD
            .decode(
                payload
                    .as_object_mut()
                    .unwrap()
                    .remove("platform_signature")
                    .unwrap()
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
        UnparsedPublicKey::new(&ED25519, &public)
            .verify(canonical_json(&payload).as_bytes(), &signature)
            .unwrap();
    }
    frame
}
fn control(index: usize) -> PendingControl {
    PendingControl {
        kind: "emergency_stop".into(),
        command_id: format!("stop-{index}"),
        action_id: None,
        device_id: DEVICE.into(),
        issued_at: unix_now(),
        expires_at: unix_now() + 60.0,
        nonce: format!("nonce-{index}"),
    }
}
#[tokio::test]
async fn production_stream_route_requires_tls_device_scope_and_request_proof() {
    let (state, repository) = state().await;
    let id = uuid::Uuid::new_v4().to_string();
    let router = local_node_companion_router(Some(state.clone()));
    let response = router
        .clone()
        .oneshot(request(open_body(&id, None), false))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let mut missing_credential = request(open_body(&id, None), true);
    missing_credential.headers_mut().remove("authorization");
    assert_eq!(
        router
            .clone()
            .oneshot(missing_credential)
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let mut body = open_body(&id, None);
    body["device_id"] = json!("dev_other");
    assert_eq!(
        router
            .clone()
            .oneshot(request(body, true))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    repository.reject_proof.store(true, Ordering::SeqCst);
    assert_eq!(
        open(state, &id, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(repository.authorized.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn reconnect_redelivers_pending_stop_with_new_cursor_and_revocation_closes_stream() {
    let (state, repository) = state().await;
    repository.controls.lock().unwrap().push(control(0));
    let first = uuid::Uuid::new_v4().to_string();
    let response = open(state.clone(), &first, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["x-accel-buffering"], "no");
    let mut body = response.into_body().into_data_stream();
    let frame = signed_frame(
        &body.next().await.unwrap().unwrap(),
        &state.signer,
        &first,
        1,
    );
    let cursor = frame["cursor"].as_str().unwrap();
    let second = uuid::Uuid::new_v4().to_string();
    let mut replacement = open(state.clone(), &second, Some(cursor))
        .await
        .into_body()
        .into_data_stream();
    assert!(
        body.next().await.is_none(),
        "new connection must replace the old stream"
    );
    let replay = signed_frame(
        &replacement.next().await.unwrap().unwrap(),
        &state.signer,
        &second,
        2,
    );
    assert_eq!(
        frame["commands"], replay["commands"],
        "cursor cannot drop unacknowledged offline controls"
    );
    repository.revoked.store(true, Ordering::SeqCst);
    assert!(replacement.next().await.is_none());
    drop(replacement);
    assert!(state.streams.lock().unwrap().is_empty());
}
#[tokio::test]
async fn idle_heartbeats_are_signed_and_duplicate_commands_are_not_reflooded() {
    let (state, repository) = state().await;
    repository.controls.lock().unwrap().push(control(0));
    let id = uuid::Uuid::new_v4().to_string();
    let mut body = open(state.clone(), &id, None)
        .await
        .into_body()
        .into_data_stream();
    assert_eq!(
        signed_frame(&body.next().await.unwrap().unwrap(), &state.signer, &id, 1)["commands"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let heartbeat = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        signed_frame(&heartbeat, &state.signer, &id, 2)["commands"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    drop(body);
    assert!(state.streams.lock().unwrap().is_empty());
}
#[tokio::test]
async fn production_router_loopback_stream_stop_p95_under_one_second() {
    // Deliberately an HTTP loopback transport fixture around the real router.
    // The marker models TLS termination here; TLS correctness has separate tests.
    let (state, repository) = state().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = local_node_companion_router(Some(state.clone())).layer(axum::middleware::from_fn(
        super::super::local_node_tls::mark_https,
    ));
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let id = uuid::Uuid::new_v4().to_string();
    let mut response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(format!("http://{address}{DEVICE_CONTROL_PATH}"))
        .header("authorization", format!("Device {TOKEN}"))
        .json(&open_body(&id, None))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut pending = Vec::new();
    let mut sequence = 0;
    let mut samples = Vec::new();
    // Consume the first authenticated readiness heartbeat before measuring.
    for sample in 0..=20 {
        let measured = Instant::now();
        if sample > 0 {
            *repository.controls.lock().unwrap() = vec![control(sample)];
        }
        loop {
            if let Some(end) = pending.iter().position(|b| *b == b'\n') {
                let bytes = pending.drain(..=end).collect::<Vec<_>>();
                sequence += 1;
                let frame = signed_frame(&bytes, &state.signer, &id, sequence);
                if sample == 0
                    || frame["commands"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|c| c["command_id"] == format!("stop-{sample}"))
                {
                    break;
                }
            } else {
                let bytes = tokio::time::timeout(Duration::from_secs(2), response.chunk())
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
                pending.extend_from_slice(&bytes);
            }
        }
        if sample > 0 {
            samples.push(measured.elapsed());
        }
    }
    samples.sort();
    let p95 = samples[18];
    println!(
        "U-CL-03 production router loopback HTTP stream n=20 p95_ms={:.3}",
        p95.as_secs_f64() * 1000.0
    );
    assert!(p95 < Duration::from_secs(1));
    drop(response);
    server.abort();
}
