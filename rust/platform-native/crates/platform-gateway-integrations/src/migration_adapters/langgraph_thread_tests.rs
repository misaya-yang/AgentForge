//! Production admission/cache logic with real reqwest requests and in-memory bodies.
//! No socket, service, model or Redis process is used.
use super::*;
use futures_util::FutureExt;
use std::{future::Future, pin::Pin};
use tokio::sync::Semaphore;

fn adapter() -> LangGraphAdapter {
    let mut service = crate::protocol::ServiceDefinition::new("thread-fixture", "Thread fixture");
    service.connector_type = TransportType::Http;
    service.connector_config = json!({"base_url":"http://fixture.invalid/","forward_auth":true});
    service.session_enabled = true;
    LangGraphAdapter::new(ServiceHandle::new(service)).unwrap()
}
fn request() -> UnifiedRequest {
    let mut request =
        UnifiedRequest::new("r", "thread-fixture", vec![ContentItem::text("fixture")]);
    request.user_id = "user".into();
    request.tenant_id = "tenant".into();
    request.session_id = Some(uuid::Uuid::new_v4().to_string());
    request
}
fn cached(session: &str) -> Option<String> {
    THREADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .get(session)
        .cloned()
}
fn remove(session: &str) {
    THREADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .remove(session);
}
enum Reply {
    Echo,
    Json(u16, Value),
    Raw(u16, &'static str, &'static str),
    Failure,
    PendingSend,
    PendingBody,
}
struct Script {
    replies: Mutex<VecDeque<Reply>>,
    requests: Mutex<Vec<reqwest::Request>>,
    slots: Arc<Semaphore>,
}
impl Script {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(vec![]),
            slots: Arc::new(Semaphore::new(1)),
        }
    }
    fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, AdapterError>> + Send>> {
        let request = builder.build().unwrap();
        let id = if request.method() == reqwest::Method::POST {
            serde_json::from_slice::<Value>(request.body().unwrap().as_bytes().unwrap()).unwrap()["thread_id"].clone()
        } else {
            json!(request.url().path_segments().unwrap().next_back().unwrap())
        };
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request/replay");
        let slots = self.slots.clone();
        Box::pin(async move {
            if matches!(reply, Reply::Failure) {
                return Err(AdapterFailure::ConnectError.into());
            }
            let permit = slots
                .try_acquire_owned()
                .expect("previous response permit not released");
            if matches!(reply, Reply::PendingSend) {
                let _permit = permit;
                return std::future::pending().await;
            }
            let (status, content_type, body) = match reply {
                Reply::Echo => (
                    200,
                    "application/json",
                    reqwest::Body::from(json!({"thread_id":id}).to_string()),
                ),
                Reply::Json(status, value) => (
                    status,
                    "application/json",
                    reqwest::Body::from(value.to_string()),
                ),
                Reply::Raw(status, kind, body) => (status, kind, reqwest::Body::from(body)),
                Reply::PendingBody => (
                    200,
                    "application/json",
                    reqwest::Body::wrap_stream(futures_util::stream::pending::<
                        Result<Vec<u8>, std::io::Error>,
                    >()),
                ),
                _ => unreachable!(),
            };
            Ok(HttpResponse {
                response: axum::http::Response::builder()
                    .status(status)
                    .header("content-type", content_type)
                    .body(body)
                    .unwrap()
                    .into(),
                permit: Some(permit),
            })
        })
    }
}

#[tokio::test]
async fn thread_acknowledgement_publishes_only_matching_identity_and_reuses_cache() {
    for original in [true, false] {
        let adapter = adapter();
        let mut request = request();
        if !original {
            request.session_id = Some(format!("legacy-{}", uuid::Uuid::new_v4()));
        }
        let session = request.session_id.as_deref().unwrap();
        let io = Script::new(vec![Reply::Echo]);
        let id = adapter
            .ensure_thread_with(session, &request, |b| io.send(b))
            .await
            .unwrap();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
        assert_eq!(cached(session), Some(id.clone()));
        assert_eq!(
            adapter
                .ensure_thread_with(session, &request, |b| io.send(b))
                .await
                .unwrap(),
            id
        );
        let requests = io.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let body: Value =
            serde_json::from_slice(requests[0].body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"thread_id":id,"if_exists":"do_nothing","metadata":{"user_id":"user","tenant_id":"tenant","original_session_id":session}})
        );
        assert_eq!(io.slots.available_permits(), 1);
        remove(session);
    }
}

#[tokio::test]
async fn thread_equivalent_uuid_text_is_accepted_without_rewriting_requested_identity() {
    let adapter = adapter();
    let request = request();
    let session = request.session_id.as_deref().unwrap();
    let io = Script::new(vec![Reply::Json(
        200,
        json!({"thread_id":session.to_uppercase()}),
    )]);
    assert_eq!(
        adapter
            .ensure_thread_with(session, &request, |b| io.send(b))
            .await
            .unwrap(),
        session
    );
    remove(session);
}

#[tokio::test]
async fn thread_unconfirmed_create_errors_never_cache_or_replay() {
    let cases = vec![
        Reply::Failure,
        Reply::Json(401, json!({"secret":"PRIVATE"})),
        Reply::Json(503, json!({"secret":"PRIVATE"})),
        Reply::Raw(200, "application/json", "PRIVATE malformed"),
        Reply::Raw(204, "text/plain", ""),
        Reply::Raw(200, "text/plain", "{}"),
        Reply::Json(200, Value::Null),
        Reply::Json(200, json!([])),
        Reply::Json(200, json!({})),
        Reply::Json(200, json!({"thread_id":9})),
        Reply::Json(200, json!({"thread_id":"invalid"})),
        Reply::Json(200, json!({"thread_id":uuid::Uuid::new_v4().to_string()})),
    ];
    for reply in cases {
        let adapter = adapter();
        let request = request();
        let session = request.session_id.as_deref().unwrap();
        let io = Script::new(vec![reply]);
        let error = adapter
            .ensure_thread_with(session, &request, |b| io.send(b))
            .await
            .unwrap_err();
        assert!(matches!(error, AdapterError::Dispatch(DispatchError::AdapterValidationFailed(_))));
        assert!(!format!("{error:?}").contains("PRIVATE"));
        assert_eq!(cached(session), None);
        assert_eq!(io.requests.lock().unwrap().len(), 1);
        assert_eq!(io.slots.available_permits(), 1);
    }
    let adapter = adapter();
    let request = request();
    let session = request.session_id.as_deref().unwrap();
    let io = Script::new(vec![Reply::Json(
        200,
        json!({"thread_id":session,"__error__":null}),
    )]);
    assert!(
        adapter
            .ensure_thread_with(session, &request, |b| io.send(b))
            .await
            .is_err()
    );
    assert_eq!(cached(session), None);
}

#[tokio::test]
async fn thread_conflict_reads_exact_id_with_same_authority_once() {
    let adapter = adapter();
    let request = request();
    let session = request.session_id.as_deref().unwrap();
    let io = Script::new(vec![
        Reply::Json(409, json!({"secret":"PRIVATE"})),
        Reply::Echo,
    ]);
    assert_eq!(
        adapter
            .ensure_thread_with(session, &request, |b| io.send(b))
            .await
            .unwrap(),
        session
    );
    let requests = io.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method(), reqwest::Method::GET);
    assert_eq!(requests[1].url().path(), format!("/threads/{session}"));
    assert_eq!(
        requests[0].headers().get("authorization"),
        requests[1].headers().get("authorization")
    );
    assert_eq!(requests[1].headers().get("x-tenant-id").unwrap(), "tenant");
    assert!(requests[1].body().is_none());
    assert_eq!(io.slots.available_permits(), 1);
    remove(session);
}

#[tokio::test]
async fn thread_conflict_unconfirmed_readback_never_caches_or_retries() {
    for reply in [
        Reply::Failure,
        Reply::Json(404, json!({"secret":"PRIVATE"})),
        Reply::Json(409, json!({})),
        Reply::Raw(200, "application/json", "PRIVATE"),
        Reply::Json(200, json!({})),
    ] {
        let adapter = adapter();
        let request = request();
        let session = request.session_id.as_deref().unwrap();
        let io = Script::new(vec![Reply::Json(409, json!({})), reply]);
        let error = adapter
            .ensure_thread_with(session, &request, |b| io.send(b))
            .await
            .unwrap_err();
        assert!(matches!(error, AdapterError::Dispatch(DispatchError::AdapterValidationFailed(_))));
        assert!(!format!("{error:?}").contains("PRIVATE"));
        assert_eq!(cached(session), None);
        assert_eq!(io.requests.lock().unwrap().len(), 2);
        assert_eq!(io.slots.available_permits(), 1);
    }
}

#[tokio::test]
async fn thread_cancellation_before_ack_drops_owned_body_and_keeps_cache_empty() {
    for conflict in [false, true] {
        for body in [false, true] {
            let adapter = adapter();
            let request = request();
            let session = request.session_id.as_deref().unwrap();
            let mut replies = vec![];
            if conflict {
                replies.push(Reply::Json(409, json!({})));
            }
            replies.push(if body {
                Reply::PendingBody
            } else {
                Reply::PendingSend
            });
            let io = Script::new(replies);
            let mut future =
                Box::pin(adapter.ensure_thread_with(session, &request, |b| io.send(b)));
            assert!(future.as_mut().now_or_never().is_none());
            assert_eq!(io.slots.available_permits(), 0);
            drop(future);
            assert_eq!(io.slots.available_permits(), 1);
            assert_eq!(cached(session), None);
            assert_eq!(
                io.requests.lock().unwrap().len(),
                if conflict { 2 } else { 1 }
            );
        }
    }
}

#[tokio::test]
async fn thread_legacy_cache_hit_retains_existing_behavior_without_remote_revalidation() {
    let adapter = adapter();
    let mut request = request();
    let session = request.session_id.clone().unwrap();
    THREADS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap()
        .insert(session.clone(), "historical-thread".into());
    request.user_id = "invalid\nheader".into();
    let io = Script::new(vec![]);
    assert_eq!(
        adapter
            .ensure_thread_with(&session, &request, |b| io.send(b))
            .await
            .unwrap(),
        "historical-thread"
    );
    assert!(io.requests.lock().unwrap().is_empty());
    remove(&session);
}

#[tokio::test]
async fn thread_invalid_authority_stops_real_invoke_and_stream_before_transport() {
    for stream in [false, true] {
        let adapter = adapter();
        let mut request = request();
        let session = request.session_id.clone().unwrap();
        request.user_id = "invalid\nheader".into();
        let error = if stream {
            adapter.stream(request).await.err().unwrap()
        } else {
            adapter.invoke(&request).await.unwrap_err()
        };
        assert_eq!(error, validation("langgraph_thread_creation_failed"));
        assert_eq!(cached(&session), None);
    }
}
