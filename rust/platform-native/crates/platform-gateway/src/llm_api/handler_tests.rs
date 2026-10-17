use super::*;
use crate::capacity_admission::AdmissionCancellation;
use crate::dispatcher::{DispatchFuture, DispatchSettlement};
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;
#[derive(Default)]
struct Seen {
    bodies: Mutex<Vec<Bytes>>,
    route_id: Mutex<Option<axum::http::HeaderValue>>,
    guardrails: Mutex<Option<guardrails::Context>>,
    units: Mutex<Vec<(i64, i64, u16)>>,
    outcomes: Mutex<Vec<DispatchSettlement>>,
    released: AtomicUsize,
    failed_settlement: AtomicBool,
    cleanup: tokio::sync::Notify,
}
struct Lease(Arc<Seen>);
impl DispatchLease for Lease {
    fn bind_owner(&self, _: AdmissionCancellation) -> Result<(), crate::dispatcher::DispatchError> {
        Ok(())
    }
    fn wait_lost(&self) -> DispatchFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
    fn mark_dispatched(&self) -> DispatchFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn mark_completion(&self, outcome: DispatchSettlement) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            self.0.outcomes.lock().unwrap().push(outcome);
            Ok(())
        })
    }
    fn release(self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            self.0.released.fetch_add(1, Ordering::SeqCst);
            self.0.cleanup.notify_one();
            Ok(())
        })
    }
}
struct TestBackend(Arc<Seen>);
impl Backend for TestBackend {
    fn authenticate<'a>(&'a self, headers: &'a HeaderMap, _: &'a str) -> LlmFuture<'a, Actor> {
        Box::pin(async move {
            if headers.get("x-test-auth").is_none_or(|h| h != "verified") {
                return Err(LlmError::new(401, "test_unauthorized"));
            }
            Ok(tests::actor())
        })
    }
    fn models<'a>(&'a self, _: &'a Actor) -> LlmFuture<'a, Value> {
        Box::pin(async {
            Ok(
                json!({"object":"list","data":[{"id":"p/m","object":"model","created":1,"owned_by":"p"}]}),
            )
        })
    }
    fn prepare<'a>(
        &'a self,
        actor: &'a Actor,
        invocation: Invocation,
        _: &'a HeaderMap,
    ) -> LlmFuture<'a, Prepared> {
        Box::pin(async move {
            let mut record = UsageRecord::new(&actor.tenant, &actor.user, "m", chrono::Utc::now());
            record.provider = "p".into();
            Ok(Prepared {
                body: invocation.body.clone(),
                invocation,
                url: "http://upstream.invalid/v1/chat/completions"
                    .parse()
                    .unwrap(),
                headers: HeaderMap::new(),
                timeout: Duration::from_secs(2),
                record,
                pricing: json!({}),
                route_decision: self.0.route_id.lock().unwrap().clone(),
                guardrails: self.0.guardrails.lock().unwrap().clone(),
                lease: Box::new(Lease(self.0.clone())),
            })
        })
    }
    fn settle<'a>(
        &'a self,
        _: &'a Prepared,
        observed: meter::Observation,
        status: u16,
    ) -> LlmFuture<'a, ()> {
        Box::pin(async move {
            if self.0.failed_settlement.load(Ordering::SeqCst) {
                return Err(LlmError::new(503, "test_checkpoint_unavailable"));
            }
            self.0
                .units
                .lock()
                .unwrap()
                .push((observed.input, observed.output, status));
            Ok(())
        })
    }
}
struct Transport {
    seen: Arc<Seen>,
    chunks: Vec<LlmResult<Bytes>>,
    status: u16,
    sse: bool,
    pending: bool,
}
impl transport::Transport for Transport {
    fn send<'a>(&'a self, prepared: &'a Prepared) -> LlmFuture<'a, transport::Upstream> {
        Box::pin(async move {
            use futures_util::StreamExt;
            self.seen.bodies.lock().unwrap().push(prepared.body.clone());
            let mut headers = HeaderMap::new();
            headers.insert("x-route-decision", "forged-upstream".parse().unwrap());
            headers.insert(
                "content-type",
                if self.sse {
                    "text/event-stream"
                } else {
                    "application/json"
                }
                .parse()
                .unwrap(),
            );
            let stream = futures_util::stream::iter(self.chunks.clone());
            let body: Pin<Box<dyn futures_core::Stream<Item = LlmResult<Bytes>> + Send>> =
                if self.pending {
                    Box::pin(stream.chain(futures_util::stream::pending()))
                } else {
                    Box::pin(stream)
                };
            Ok(transport::Upstream {
                status: self.status,
                headers,
                body,
            })
        })
    }
}
fn api(
    chunks: Vec<LlmResult<Bytes>>,
    status: u16,
    sse: bool,
    pending: bool,
) -> (LlmApi, Arc<Seen>) {
    let seen = Arc::new(Seen::default());
    (
        LlmApi {
            backend: Arc::new(TestBackend(seen.clone())),
            transport: Arc::new(Transport {
                seen: seen.clone(),
                chunks,
                status,
                sse,
                pending,
            }),
        },
        seen,
    )
}
fn request(path: &str, body: &str) -> Request<Body> {
    Request::post(path)
        .header("x-test-auth", "verified")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}
async fn released(seen: &Seen) {
    if seen.released.load(Ordering::SeqCst) == 0 {
        tokio::time::timeout(Duration::from_secs(2), seen.cleanup.notified())
            .await
            .expect("owned lease cleanup did not finish");
    }
    assert_eq!(seen.released.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn actual_handler_forwards_native_request_and_response_bytes_exactly() {
    let response=Bytes::from_static(b"{ \"choices\":[{\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}, \"future\":1e-3 }");
    let (api, seen) = api(vec![Ok(response.clone())], 200, false, false);
    let input =
        "{\"model\":\"m\", \"messages\":[],\"max_tokens\":10,\"unknown\":{\"cache_control\":true}}";
    let actual = router(api)
        .oneshot(request("/v1/chat/completions", input))
        .await
        .unwrap();
    assert_eq!(actual.status(), 200);
    assert_eq!(to_bytes(actual.into_body(), 4096).await.unwrap(), response);
    released(&seen).await;
    assert_eq!(
        seen.bodies.lock().unwrap().as_slice(),
        &[Bytes::from(input)]
    );
    assert_eq!(*seen.units.lock().unwrap(), vec![(3, 2, 200)]);
    assert_eq!(
        *seen.outcomes.lock().unwrap(),
        vec![DispatchSettlement::Completed]
    );
}
#[tokio::test]
async fn unsigned_requests_cannot_reach_provider_and_models_use_the_same_identity_gate() {
    let (api, seen) = api(vec![], 200, false, false);
    let router = router(api);
    let response = router
        .clone()
        .oneshot(
            Request::post("/v1/embeddings")
                .body(Body::from("{} "))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(seen.bodies.lock().unwrap().is_empty());
    let response = router
        .oneshot(
            Request::get("/v1/models/p/m")
                .header("x-test-auth", "verified")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let value: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(value["id"], "p/m");
}
#[tokio::test]
async fn actual_stream_preserves_every_frame_and_commits_terminal_usage_once() {
    let chunks=vec![Bytes::from_static(b": ping\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"future\":true},\"finish_reason\":\"stop\"}]}\n\n"),Bytes::from_static(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n")];
    let expected = chunks.concat();
    let (api, seen) = api(chunks.into_iter().map(Ok).collect(), 200, true, false);
    let response=router(api).oneshot(request("/v1/chat/completions",r#"{"model":"m","messages":[],"max_tokens":10,"stream":true,"stream_options":{"include_usage":true}}"#)).await.unwrap();
    assert_eq!(
        to_bytes(response.into_body(), 4096).await.unwrap().as_ref(),
        expected.as_slice()
    );
    released(&seen).await;
    assert_eq!(*seen.units.lock().unwrap(), vec![(3, 2, 200)]);
}
#[tokio::test]
async fn missing_terminal_or_failed_transport_errors_the_body_and_retains_unknown_hold() {
    for chunks in [
        vec![Ok(Bytes::from_static(
            b"data: {\"choices\":[{\"index\":0,\"finish_reason\":null}]}\n\n",
        ))],
        vec![Err(LlmError::new(502, "upstream_failure"))],
    ] {
        let (api, seen) = api(chunks, 200, true, false);
        let response = router(api)
            .oneshot(request(
                "/v1/chat/completions",
                r#"{"model":"m","messages":[],"max_tokens":10,"stream":true}"#,
            ))
            .await
            .unwrap();
        assert!(to_bytes(response.into_body(), 4096).await.is_err());
        released(&seen).await;
        assert!(seen.units.lock().unwrap().is_empty());
        assert!(
            seen.outcomes
                .lock()
                .unwrap()
                .contains(&DispatchSettlement::Unknown)
        );
    }
}
#[tokio::test]
async fn dropped_stream_cancels_owned_work_and_persists_unknown_without_retry() {
    let (api, seen) = api(
        vec![Ok(Bytes::from_static(b": waiting\n\n"))],
        200,
        true,
        true,
    );
    let response = router(api)
        .oneshot(request(
            "/v1/messages",
            r#"{"model":"m","messages":[],"max_tokens":10,"stream":true}"#,
        ))
        .await
        .unwrap();
    drop(response);
    released(&seen).await;
    assert_eq!(seen.bodies.lock().unwrap().len(), 1);
    assert!(seen.units.lock().unwrap().is_empty());
    assert!(
        seen.outcomes
            .lock()
            .unwrap()
            .contains(&DispatchSettlement::Unknown)
    );
}
#[tokio::test]
async fn rejected_http_without_usage_is_never_assumed_free_or_successful() {
    let error = Bytes::from_static(br#"{"error":{"type":"rate_limit_error","message":"limited"}}"#);
    let (api, seen) = api(vec![Ok(error.clone())], 429, false, false);
    let response = router(api)
        .oneshot(request(
            "/v1/messages",
            r#"{"model":"m","messages":[],"max_tokens":10}"#,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), error);
    released(&seen).await;
    assert_eq!(
        *seen.outcomes.lock().unwrap(),
        vec![DispatchSettlement::Unknown]
    );
    assert!(seen.units.lock().unwrap().is_empty());
}
#[tokio::test]
async fn embedding_and_anthropic_handlers_preserve_native_payloads_and_meter_actual_units() {
    for (path, input, output, units) in [
        (
            "/v1/embeddings",
            r#"{"model":"m","input":[1,2],"encoding_format":"base64"}"#,
            r#"{ "object":"list","data":[{"embedding":"AAAA"}],"usage":{"prompt_tokens":7,"total_tokens":7},"future":true }"#,
            (7, 0, 200),
        ),
        (
            "/v1/messages",
            r#"{"model":"m","messages":[],"max_tokens":10,"future":true}"#,
            r#"{"type":"message","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":5,"cache_read_input_tokens":80,"cache_creation_input_tokens":10}}"#,
            (110, 5, 200),
        ),
    ] {
        let (api, seen) = api(vec![Ok(Bytes::from(output))], 200, false, false);
        let response = router(api).oneshot(request(path, input)).await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            to_bytes(response.into_body(), 4096).await.unwrap().as_ref(),
            output.as_bytes()
        );
        released(&seen).await;
        assert_eq!(
            seen.bodies.lock().unwrap().as_slice(),
            &[Bytes::from(input)]
        );
        assert_eq!(*seen.units.lock().unwrap(), vec![units]);
    }
}
#[tokio::test]
async fn anthropic_handler_preserves_stream_and_uses_final_cumulative_usage() {
    let bytes=Bytes::from_static(b"event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":0}}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":21,\"output_tokens\":5}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    let (api, seen) = api(vec![Ok(bytes.clone())], 200, true, false);
    let response = router(api)
        .oneshot(request(
            "/v1/messages",
            r#"{"model":"m","messages":[],"max_tokens":10,"stream":true}"#,
        ))
        .await
        .unwrap();
    assert_eq!(to_bytes(response.into_body(), 4096).await.unwrap(), bytes);
    released(&seen).await;
    assert_eq!(*seen.units.lock().unwrap(), vec![(21, 5, 200)]);
}
#[tokio::test]
async fn failed_accounting_cannot_expose_native_stream_success_terminator() {
    let(api,seen)=api(vec![Ok(Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\ndata: [DONE]\n\n"))],200,true,false);
    seen.failed_settlement.store(true, Ordering::SeqCst);
    let response = router(api)
        .oneshot(request(
            "/v1/chat/completions",
            r#"{"model":"m","messages":[],"max_tokens":10,"stream":true}"#,
        ))
        .await
        .unwrap();
    assert!(to_bytes(response.into_body(), 4096).await.is_err());
    released(&seen).await;
    assert!(seen.units.lock().unwrap().is_empty());
    assert_eq!(
        *seen.outcomes.lock().unwrap(),
        vec![DispatchSettlement::Completed, DispatchSettlement::Unknown]
    );
}

#[tokio::test]
async fn persisted_routing_header_survives_sync_and_stream_but_provider_forgery_does_not() {
    let id = "a".repeat(64);
    for stream in [false, true] {
        let body = if stream {
            Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n")
        } else {
            Bytes::from_static(br#"{"choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#)
        };
        let (api, seen) = api(vec![Ok(body)], 200, stream, false);
        *seen.route_id.lock().unwrap() = Some(id.parse().unwrap());
        let input = json!({"model":"m","messages":[],"max_tokens":10,"stream":stream}).to_string();
        let mut req = request("/v1/chat/completions", &input);
        req.headers_mut()
            .insert("x-route-decision", "forged-client".parse().unwrap());
        let actual = router(api).oneshot(req).await.unwrap();
        assert_eq!(actual.status(), 200);
        assert_eq!(
            actual.headers()["x-route-decision"].to_str().unwrap(),
            id.as_str()
        );
        let _ = to_bytes(actual.into_body(), 4096).await.unwrap();
        released(&seen).await;
    }
    let (api,seen)=api(vec![Ok(Bytes::from_static(br#"{"choices":[{"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":0}}"#))],200,false,false);
    let actual = router(api)
        .oneshot(request(
            "/v1/chat/completions",
            r#"{"model":"m","messages":[],"max_tokens":1}"#,
        ))
        .await
        .unwrap();
    assert!(!actual.headers().contains_key("x-route-decision"));
    let _ = to_bytes(actual.into_body(), 4096).await.unwrap();
    released(&seen).await;
}

#[tokio::test]
async fn enforced_output_blocks_before_any_bytes_but_bills_completed_sync_and_stream_usage() {
    for stream in [false, true] {
        let chunks = if stream {
            vec![Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"person@\"}}]}\n\n"),Bytes::from_static(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"example.test\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n")]
        } else {
            vec![Bytes::from_static(br#"{"choices":[{"message":{"content":"person@example.test"},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#)]
        };
        let (api, seen) = api(chunks.into_iter().map(Ok).collect(), 200, stream, false);
        let context = guardrails::test_context();
        *seen.guardrails.lock().unwrap() = Some(context.clone());
        let input = json!({"model":"m","messages":[],"max_tokens":10,"stream":stream}).to_string();
        let response = router(api)
            .oneshot(request("/v1/chat/completions", &input))
            .await
            .unwrap();
        assert_eq!(response.status(), 403);
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("person@"));
        released(&seen).await;
        assert_eq!(*seen.units.lock().unwrap(), vec![(3, 2, 200)]);
        assert_eq!(
            *seen.outcomes.lock().unwrap(),
            vec![DispatchSettlement::Completed]
        );
        assert_eq!(
            context.output_report().unwrap().decision,
            guardrails::Decision::Block
        );
    }
}
