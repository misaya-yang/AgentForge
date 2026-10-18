//! Composed offline proof using the two real Rust executables, never shell doubles.
//! Run only with integration-fixtures and an explicitly built engine path.
#![cfg(feature = "integration-fixtures")]
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    convert::Infallible,
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::process::{Child, Command};
const TOKEN: &str = "offline-worker-fixture-token-1234567890";
#[path = "../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/sdk_model_plane.rs"]
mod model_plane;
const RUN_ALLOW: &str = "00000000-0000-0000-0000-000000000001";
const RUN_DENY: &str = "00000000-0000-0000-0000-000000000002";
const REFUSAL: &str = "I cannot help with that request.";
#[derive(Clone, Default)]
struct Fixture {
    requests: Arc<Mutex<Vec<Value>>>,
    launches: Arc<Mutex<HashMap<String, Value>>>,
    polls: Arc<Mutex<HashMap<String, usize>>>,
}
fn sse(mut events: Vec<Value>, receipt: Value) -> Response {
    events.push(receipt);
    let body = events
        .iter()
        .map(|v| format!("data: {v}\n\n"))
        .collect::<String>();
    ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
}
fn completed(text: &str) -> Vec<Value> {
    vec![
        json!({"type":"response.output_text.delta","delta":text}),
        json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],"usage":{"input_tokens":3,"output_tokens":2,"input_tokens_details":{"cached_tokens":0,"cache_write_tokens":0}}}}),
    ]
}
async fn model_execute(
    State(f): State<Fixture>,
    headers: axum::http::HeaderMap,
    Json(envelope): Json<Value>,
) -> Response {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", "a".repeat(64))
    );
    assert!(!headers.contains_key("x-sdk-provider-key"));
    assert_eq!(envelope["wire"], "responses_v1");
    let run = envelope["run_id"].as_str().unwrap();
    assert!(f.launches.lock().unwrap().contains_key(run));
    let call = envelope["call_id"].as_str().unwrap();
    assert!(call.starts_with(&format!("{run}:")));
    let receipt = json!({"type":"sdk.model_receipt","settled":true,"run_id":run,"call_id":call,
        "cost_nanos":7,"receipt_id":format!("fixture-{call}")});
    let body = envelope["body"].clone();
    f.requests.lock().unwrap().push(body.clone());
    let model = body["model"].as_str().unwrap_or("");
    if model.starts_with("refusal") {
        return sse(
            vec![
                json!({"type":"response.refusal.delta","delta":REFUSAL}),
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"refusal","refusal":REFUSAL}]}],"usage":{"input_tokens":3,"output_tokens":2}}}),
            ],
            receipt,
        );
    }
    if model == "cancel" {
        let stream = futures_util::stream::once(async {
            Ok::<_, Infallible>(format!(
                "data: {}\n\n",
                json!({"type":"response.output_text.delta","delta":"started"})
            ))
        })
        .chain(futures_util::stream::pending());
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap();
    }
    if matches!(model, "allow" | "deny") {
        let returned = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["type"] == "function_call_output");
        if let Some(output) = returned {
            let result: Value = serde_json::from_str(output["output"].as_str().unwrap()).unwrap();
            let expected = if model == "allow" {
                "succeeded"
            } else {
                "failed"
            };
            assert_eq!(result["status"], expected);
            return sse(
                completed(if model == "allow" {
                    "approved result"
                } else {
                    "denied result"
                }),
                receipt,
            );
        }
        return sse(
            vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","call_id":"provider-call-1","name":"platform_capability_0","arguments":"{\"value\":\"fixture\"}"}],"usage":{"input_tokens":2,"output_tokens":1}}}),
            ],
            receipt,
        );
    }
    sse(completed("native answer"), receipt)
}
async fn model_lease(
    State(f): State<Fixture>,
    headers: axum::http::HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", "a".repeat(64))
    );
    assert!(!headers.contains_key("x-sdk-provider-key"));
    let run = body["run_id"].as_str().unwrap();
    let launches = f.launches.lock().unwrap();
    let launch = launches.get(run).expect("authenticated fixture launch");
    Json(
        json!({"schema":"sdk-model-plane/v1","expires_at_epoch_ms":model_plane::now_ms()+60_000,
        "binding_sha256":model_plane::launch_binding(launch)}),
    )
}
async fn model_revocations(headers: axum::http::HeaderMap, Json(body): Json<Value>) -> Response {
    assert_eq!(
        headers["authorization"],
        format!("Bearer {}", "a".repeat(64))
    );
    let run = body["run_id"].as_str().unwrap().to_owned();
    let stream = futures_util::stream::unfold(run, |run| async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        Some((
            Ok::<_, Infallible>(format!(
                "data: {}\n\n",
                json!({"type":"sdk.lease_heartbeat","run_id":run})
            )),
            run,
        ))
    });
    Response::builder()
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
async fn admit(Json(body): Json<Value>) -> Json<Value> {
    let _ = body;
    Json(json!({"authorized":true}))
}
async fn execute(State(f): State<Fixture>, Json(body): Json<Value>) -> Json<Value> {
    let run = body["run_id"].as_str().unwrap();
    assert!(matches!(run, RUN_ALLOW | RUN_DENY));
    assert_eq!(body["tool"], "fixture_tool");
    assert_eq!(body["arguments"], json!({"value":"fixture"}));
    assert_eq!(body["tool_call_id"], "provider-call-1");
    let mut polls = f.polls.lock().unwrap();
    let count = polls.entry(run.into()).or_default();
    *count += 1;
    let mut value = json!({"run_id":run,"tool_call_id":"provider-call-1","tool":"fixture_tool"});
    if *count == 1 {
        value["status"] = json!("approval_required");
        value["approval_id"] = json!(format!("approval-{run}"));
        value["expires_at"] = json!("2099-01-01T00:00:00Z");
        value["preview"] = json!({"value":"fixture"});
    } else {
        value["status"] = json!(if run == RUN_ALLOW {
            "succeeded"
        } else {
            "failed"
        });
        value["approval_status"] = json!(if run == RUN_ALLOW {
            "approved"
        } else {
            "denied"
        });
        value["execution_id"] = json!("fixture-execution");
        value["receipt"] = json!({"observed":true});
        value["response"] = json!({"value":"fixture-result"});
    }
    Json(value)
}
fn launch(model: &str, session: &str, request: &str, stream: bool) -> Value {
    json!({"protocol":"ai-platform/sdk-agent/v1","framework":"openai_agents","service_id":"service","tenant_id":"tenant","user_id":"user","request_id":request,"session_id":session,"agent":{"model":model,"instructions":"Fixture agent","max_turns":3},"input":"fixture question","stream":stream,
        "capability_bridge":{"grant":"a".repeat(64),"run_id":uuid::Uuid::new_v4().to_string(),"timeout_seconds":300,"tools":[]}})
}
fn capability(body: &mut Value, run: &str) {
    body["capability_bridge"] = json!({"grant":"a".repeat(64),"run_id":run,"timeout_seconds":1800,"tools":[{"name":"platform_capability_0","tool":"fixture_tool","namespace":null,"description":"fixture tool","input_schema":{"type":"object","properties":{"value":{"type":"string"}},"required":["value"],"additionalProperties":false},"id":"fixture-tool-id","schema_hash":format!("sha256:{}","b".repeat(64)),"effect":"write"}]});
}
async fn send(fixture: &Fixture, client: &reqwest::Client, body: &Value) -> reqwest::Response {
    fixture.launches.lock().unwrap().insert(
        body["capability_bridge"]["run_id"].as_str().unwrap().into(),
        body.clone(),
    );
    client
        .post("http://127.0.0.1:8096/v1/runs")
        .bearer_auth(TOKEN)
        .json(body)
        .send()
        .await
        .unwrap()
}
fn parse_sse(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
struct Service {
    child: Child,
    root: PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
async fn start_service(origin: &str) -> Service {
    let root = std::env::temp_dir().join(format!(
        "sdk-native-stack-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let engine = PathBuf::from(
        std::env::var_os("SDK_AGENT_INTEGRATION_ENGINE")
            .expect("build native engine and set SDK_AGENT_INTEGRATION_ENGINE"),
    );
    assert!(engine.is_file());
    let service = std::env::var_os("SDK_AGENT_INTEGRATION_SERVICE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_sdk-agent-worker")));
    let child = Command::new(service)
        .arg("serve")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SDK_AGENT_WORKER_TOKEN", TOKEN)
        .env("SDK_AGENT_STATE_ROOT", &root)
        .env("SDK_AGENT_RETENTION_SECONDS", "3600")
        .env("SDK_AGENT_ENGINE_PATH", engine)
        .env("SDK_AGENT_GATEWAY_URL", origin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    Service { child, root }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn actual_service_engine_provider_approval_cancel_and_history() {
    // Fixed production listener is deliberate. Refuse conflicting service state.
    let free = std::net::TcpListener::bind("127.0.0.1:8096")
        .expect("SDK worker port must be unused for composed fixture");
    drop(free);
    let fixture = Fixture::default();
    let router = Router::new()
        .route("/internal/v1/sdk-tools/models/execute", post(model_execute))
        .route("/internal/v1/sdk-tools/models/lease", post(model_lease))
        .route(
            "/internal/v1/sdk-tools/models/revocations",
            post(model_revocations),
        )
        .route("/internal/v1/sdk-tools/capabilities/admit", post(admit))
        .route("/internal/v1/sdk-tools/capabilities/execute", post(execute))
        .with_state(fixture.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut service = start_service(&origin).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(r) = client
            .get("http://127.0.0.1:8096/health/ready")
            .bearer_auth(TOKEN)
            .send()
            .await
        {
            if r.status().is_success() {
                let v: Value = r.json().await.unwrap();
                assert_eq!(v["implementation"], "native-provider-protocol");
                break;
            }
        }
        assert!(Instant::now() < deadline, "native worker failed readiness");
        assert!(
            service.child.try_wait().unwrap().is_none(),
            "native worker exited"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    // Actual nonstream child execution, durable receipt replay and history reload.
    let first = launch("text", "history-session", "request-1", false);
    let r = send(&fixture, &client, &first).await;
    assert_eq!(r.status(), StatusCode::OK);
    let value: Value = r.json().await.unwrap();
    assert_eq!(value["text"], "native answer");
    assert_eq!(value["usage"]["input_tokens"], 3);
    let before = fixture.requests.lock().unwrap().len();
    assert_eq!(
        send(&fixture, &client, &first).await.status(),
        StatusCode::OK
    );
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        before,
        "receipt replay must not call provider"
    );
    let mut second = launch("text", "history-session", "request-2", true);
    second["input"] = json!("continued question");
    let r = send(&fixture, &client, &second).await;
    assert_eq!(r.status(), StatusCode::OK);
    let events = parse_sse(&r.text().await.unwrap());
    assert!(events.iter().any(|v| v["event"] == "delta"));
    assert_eq!(events.last().unwrap()["event"], "completed");
    let requests = fixture.requests.lock().unwrap().clone();
    let input = requests.last().unwrap()["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert_eq!(input[0]["content"], "fixture question");
    assert_eq!(input[1]["content"][0]["text"], "native answer");
    assert_eq!(input[2]["content"], "continued question");
    // Provider destination is operator configuration; request JSON cannot override it.
    let before = fixture.requests.lock().unwrap().len();
    let mut destination_override =
        launch("text", "blocked-destination", "request-destination", false);
    destination_override["provider_base_url"] = json!("https://attacker.example/v1");
    let rejected = send(&fixture, &client, &destination_override).await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert_eq!(fixture.requests.lock().unwrap().len(), before);

    // Refusal deltas and final refusal content survive streaming, nonstreaming,
    // and native-history reload into the next provider turn.
    let mut refusal_stream = launch("refusal-stream", "refusal-history", "refusal-1", true);
    refusal_stream["input"] = json!("please answer");
    let response = send(&fixture, &client, &refusal_stream).await;
    assert_eq!(response.status(), StatusCode::OK);
    let events = parse_sse(&response.text().await.unwrap());
    assert!(
        events
            .iter()
            .any(|v| v["event"] == "delta" && v["text"] == REFUSAL)
    );
    assert_eq!(events.last().unwrap()["event"], "completed");
    assert_eq!(events.last().unwrap()["text"], REFUSAL);
    assert_eq!(events.last().unwrap()["streamed_text"], REFUSAL);

    let mut refusal_nonstream =
        launch("refusal-nonstream", "refusal-nonstream", "refusal-2", false);
    refusal_nonstream["input"] = json!("please answer");
    let response = send(&fixture, &client, &refusal_nonstream).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["text"], REFUSAL);

    let mut continued = launch("text", "refusal-history", "refusal-3", false);
    continued["input"] = json!("follow up");
    let response = send(&fixture, &client, &continued).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap()["text"],
        "native answer"
    );
    let requests = fixture.requests.lock().unwrap().clone();
    let refusal_turn = requests
        .iter()
        .find(|r| r["model"] == "refusal-stream")
        .unwrap();
    assert_eq!(refusal_turn["input"].as_array().unwrap().len(), 1);
    let continued_turn = requests
        .iter()
        .find(|r| {
            r["model"] == "text"
                && r["input"]
                    .as_array()
                    .is_some_and(|a| a.len() == 3 && a[2]["content"] == "follow up")
        })
        .unwrap();
    let history = continued_turn["input"].as_array().unwrap();
    assert_eq!(history[1]["content"][0]["type"], "refusal");
    assert_eq!(history[1]["content"][0]["refusal"], REFUSAL);
    assert_eq!(history[2]["content"], "follow up");

    // Authority is the sole approval decision source; both outcomes traverse child.
    for (model, run, status, approval) in [
        ("allow", RUN_ALLOW, "succeeded", "approved"),
        ("deny", RUN_DENY, "failed", "denied"),
    ] {
        let mut body = launch(model, model, "tool-request", true);
        capability(&mut body, run);
        let r = send(&fixture, &client, &body).await;
        assert_eq!(r.status(), StatusCode::OK);
        let events = parse_sse(&r.text().await.unwrap());
        assert!(
            events
                .iter()
                .any(|v| v["event"] == "approval_required" && v["run_id"] == run)
        );
        assert!(events.iter().any(|v| v["event"] == "approval_result"
            && v["status"] == status
            && v["approval_status"] == approval));
        assert!(
            events
                .iter()
                .any(|v| v["event"] == "tool_result" && v["status"] == status)
        );
        assert_eq!(events.last().unwrap()["event"], "completed");
        assert_eq!(fixture.polls.lock().unwrap()[run], 2);
    }
    // Drop a real TCP SSE response while the native engine is consuming provider SSE.
    let cancelled = launch("cancel", "cancel-session", "cancel-request", true);
    let mut response = send(&fixture, &client, &cancelled).await;
    assert_eq!(response.status(), StatusCode::OK);
    let chunk = response.chunk().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&chunk).contains("started"));
    drop(response);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let response = send(&fixture, &client, &cancelled).await;
        let status = response.status();
        let v: Value = response.json().await.unwrap();
        if status == StatusCode::CONFLICT && v["code"] == "sdk_session_reconciliation_required" {
            break;
        }
        assert_eq!(v["code"], "sdk_session_busy");
        assert!(
            Instant::now() < deadline,
            "cancelled native child retained live session lock"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let delete = json!({"tenant_id":"tenant","user_id":"user","service_id":"service","framework":"openai_agents","session_id":"cancel-session"});
    let r = client
        .delete("http://127.0.0.1:8096/v1/sessions")
        .bearer_auth(TOKEN)
        .json(&delete)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::OK);
    assert_eq!(
        send(&fixture, &client, &cancelled).await.status(),
        StatusCode::GONE
    );
    // Graceful OS shutdown must complete without outstanding uncertain owners.
    unsafe {
        libc::kill(service.child.id().unwrap() as i32, libc::SIGTERM);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(15), service.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    fs::remove_dir_all(&service.root).unwrap();
    server.abort();
}
