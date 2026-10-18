//! Private native SDK-protocol service. Deploy behind operator TLS/mTLS.
mod bridge;
mod contract;
mod lifecycle;
#[path = "../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/sdk_model_plane.rs"]
mod model_plane;
mod supervisor;
mod transport;
#[path = "../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/workload_identity.rs"]
mod workload_identity;
use crate::bridge::Authority;
use crate::contract::{Launch, MAX_BODY, PROTOCOL, Result, hash};
use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch};

struct Config {
    token: String,
    root: PathBuf,
    engine: PathBuf,
    #[cfg(test)]
    fixture_script: Option<PathBuf>,
    retention: u64,
    gateway: Option<String>,
}
impl Config {
    fn load() -> Result<Self> {
        if [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_BASE_URL",
            "ANTHROPIC_BASE_URL",
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some())
        {
            return Err("sdk_provider_configuration_forbidden");
        }
        let token = std::env::var("SDK_AGENT_WORKER_TOKEN").unwrap_or_default();
        let token_file = std::env::var("SDK_AGENT_WORKER_TOKEN_FILE").unwrap_or_default();
        let token = if !token_file.is_empty() {
            if !token.is_empty() {
                return Err("sdk_worker_ambiguous_token_source");
            }
            std::fs::read_to_string(&token_file)
                .map_err(|_| "sdk_worker_token_invalid")?
                .trim_end_matches(['\r', '\n'])
                .to_owned()
        } else {
            token
        };
        if workload_identity::required() && (!token.is_empty() || !token_file.is_empty()) {
            return Err("sdk_worker_shared_bearer_forbidden");
        }
        if !workload_identity::required()
            && (!(32..=4096).contains(&token.len())
                || !token.bytes().all(|b| (33..=126).contains(&b)))
        {
            return Err("sdk_worker_token_invalid");
        }
        let root = PathBuf::from(
            std::env::var("SDK_AGENT_STATE_ROOT").map_err(|_| "sdk_worker_state_invalid")?,
        );
        lifecycle::validate_root(&root)?;
        let retention = std::env::var("SDK_AGENT_RETENTION_SECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| (3600..=7776000).contains(v))
            .ok_or("sdk_retention_invalid")?;
        let engine = std::env::var_os("SDK_AGENT_ENGINE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .unwrap_or_else(|_| PathBuf::from("/usr/local/bin/sdk-agent-worker"))
                    .with_file_name("ai-platform-sdk-agent-engine")
            });
        if !engine.is_absolute() {
            return Err("sdk_engine_path_invalid");
        }
        Ok(Self {
            token,
            root,
            engine,
            #[cfg(test)]
            fixture_script: None,
            retention,
            gateway: std::env::var("SDK_AGENT_GATEWAY_URL")
                .ok()
                .filter(|s| !s.is_empty()),
        })
    }
}
struct App {
    config: Config,
    authority: Authority,
    owners: Mutex<HashMap<String, Arc<supervisor::Owner>>>,
    closing: AtomicBool,
    sweep: Mutex<Option<Instant>>,
    stop: watch::Sender<bool>,
}
impl App {
    fn authorized(&self, request: &Request) -> bool {
        if workload_identity::authenticated_peer() {
            return true;
        }
        if workload_identity::required() {
            return false;
        }
        use sha2::{Digest, Sha256};
        let expected = Sha256::digest(format!("Bearer {}", self.config.token).as_bytes());
        let actual = Sha256::digest(
            request
                .headers()
                .get(header::AUTHORIZATION)
                .map(|h| h.as_bytes())
                .unwrap_or_default(),
        );
        expected
            .iter()
            .zip(actual.iter())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
    }
    fn state_ready(&self) -> bool {
        if self.closing.load(Ordering::SeqCst) {
            return false;
        }
        let owners = self.owners.lock().unwrap();
        if owners.values().any(|o| o.uncertain.load(Ordering::SeqCst)) {
            return false;
        }
        let dir = self.config.root.join(".cleanup");
        if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink() || !m.is_dir())
        {
            return false;
        }
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for (n, e) in entries.enumerate() {
                    let Ok(e) = e else {
                        return false;
                    };
                    let name = e.file_name();
                    let Some(scope) = name.to_str().and_then(|s| s.strip_suffix(".json")) else {
                        return false;
                    };
                    if n >= 100 || !owners.contains_key(scope) {
                        return false;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
        true
    }
    fn engine_ready(&self) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(&self.config.engine)
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
}
fn error(code: &'static str) -> Response {
    let status = match code {
        "sdk_worker_unauthorized" => 401,
        "sdk_request_too_large" => 413,
        "sdk_request_invalid" | "sdk_provider_credential_forbidden" => 400,
        "sdk_session_deleted" => 410,
        "sdk_session_busy"
        | "sdk_session_reconciliation_required"
        | "sdk_session_profile_changed"
        | "sdk_request_id_reused" => 409,
        "sdk_knowledge_access_denied" => 403,
        _ => 503,
    };
    (
        StatusCode::from_u16(status).unwrap(),
        Json(json!({"code":code})),
    )
        .into_response()
}
fn json_response(v: Value) -> Response {
    let mut r = Json(v).into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    r
}
async fn ready(State(app): State<Arc<App>>, request: Request) -> Response {
    if !workload_identity::required() && !app.authorized(&request) {
        return error("sdk_worker_unauthorized");
    }
    let retention = app
        .sweep
        .lock()
        .unwrap()
        .is_some_and(|t| t.elapsed() < Duration::from_secs(120));
    if !retention
        || !app.state_ready()
        || !app.engine_ready()
        || lifecycle::validate_root(&app.config.root).is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"ready":false})),
        )
            .into_response();
    }
    Json(json!({"ready":true,"protocol":PROTOCOL,"implementation":"native-provider-protocol","versions":{},"native_version":env!("CARGO_PKG_VERSION")})).into_response()
}
async fn delete_session(State(app): State<Arc<App>>, request: Request) -> Response {
    if !app.authorized(&request) {
        return error("sdk_worker_unauthorized");
    }
    let bytes = match to_bytes(request.into_body(), 8192).await {
        Ok(b) => b,
        Err(_) => return error("sdk_request_too_large"),
    };
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(_) => return error("sdk_request_invalid"),
    };
    if !contract::exact(
        &body,
        &[
            "tenant_id",
            "user_id",
            "service_id",
            "framework",
            "session_id",
        ],
    ) {
        return error("sdk_request_invalid");
    }
    let scope = match contract::scope(&body) {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    match lifecycle::remove(&app.config.root, &scope, "deleted", None) {
        Ok(_) => json_response(json!({"deleted":true})),
        Err(e) => error(e),
    }
}
fn event_response(rx: mpsc::Receiver<Value>) -> Response {
    let stream = tokio_stream::wrappers::ReceiverStream::new(rx)
        .map(|v| Ok::<_, std::convert::Infallible>(format!("data: {}\n\n", v)));
    let mut response = Response::new(Body::from_stream(stream));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
async fn runs(State(app): State<Arc<App>>, request: Request) -> Response {
    if !app.authorized(&request) {
        return error("sdk_worker_unauthorized");
    }
    if request.headers().contains_key("x-sdk-provider-key") {
        return error("sdk_provider_credential_forbidden");
    }
    let bytes = match to_bytes(request.into_body(), MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return error("sdk_request_too_large"),
    };
    let body = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(_) => return error("sdk_request_invalid"),
    };
    let mut launch = match Launch::parse(body) {
        Ok(l) => l,
        Err(e) => return error(e),
    };
    // Bind every local state path to the Gateway-authenticated actor before
    // acquiring a session lock or publishing its tool/profile data.
    let mut model_stop = app.stop.subscribe();
    let admitted = tokio::select! {result=app.authority.model_lease(&launch)=>result,_=model_stop.changed()=>Err("sdk_state_unavailable")};
    launch.model_plane = Some(match admitted {
        Ok(plane) => plane,
        Err(code) => return error(code),
    });
    let session = match lifecycle::acquire(&app.config.root, &launch.scope) {
        Ok(s) => s,
        Err(e) => return error(e),
    };
    if !app.state_ready() {
        return error("sdk_state_unavailable");
    }
    if let Err(e) = lifecycle::profile(&session, &launch) {
        return error(e);
    }
    let mut admission_stop = app.stop.subscribe();
    let admission = tokio::select! {r=app.authority.access(&launch)=>r,_=admission_stop.changed()=>Err("sdk_state_unavailable")};
    if let Err(e) = admission {
        return error(e);
    }
    if !app.state_ready() {
        return error("sdk_state_unavailable");
    }
    let fingerprint = launch.fingerprint();
    let receipt = session.path.join(format!(
        "run-{}.json",
        hash(launch.body["request_id"].as_str().unwrap().as_bytes())
    ));
    let pending = match lifecycle::read_pending(&session) {
        Ok(pending) => pending,
        Err(code) => return error(code),
    };
    if let Some(pending) = pending {
        if pending.request_id == launch.body["request_id"].as_str().unwrap()
            || !lifecycle::locally_reconcilable(&session, &pending)
        {
            return error("sdk_session_reconciliation_required");
        }
        let reconciled = matches!(
            app.authority
                .reconcile_cancelled_pending(&launch, &pending.request_id)
                .await,
            Ok(true)
        );
        if !reconciled {
            return error("sdk_session_reconciliation_required");
        }
        let prior_receipt = session
            .path
            .join(format!("run-{}.json", hash(pending.request_id.as_bytes())));
        if lifecycle::reconcile_cancelled(&session, &prior_receipt, &pending).is_err() {
            return error("sdk_session_reconciliation_required");
        }
    }
    if receipt.exists() {
        let prior = match lifecycle::read(&receipt) {
            Ok(v) => v,
            Err(e) => return error(e),
        };
        if prior["fingerprint"] != fingerprint {
            return error("sdk_request_id_reused");
        }
        if !prior["result"].is_object() {
            return error("sdk_state_unavailable");
        }
        if launch.body["stream"] == true {
            let (tx, rx) = mpsc::channel(1);
            let _ = tx.try_send(prior["result"].clone());
            return event_response(rx);
        }
        return json_response(prior["result"].clone());
    }
    if let Err(e) = lifecycle::touch(&session.path).and_then(|_| {
        lifecycle::atomic(
            &session.path.join("pending.json"),
            &json!({"request_id":launch.body["request_id"],"fingerprint":fingerprint}),
        )
    }) {
        return error(e);
    }
    let streaming = launch.body["stream"] == true;
    let (tx, mut rx) = mpsc::channel(16);
    if let Err(e) = supervisor::start(app, session, launch, receipt, fingerprint, tx) {
        return error(e);
    }
    if streaming {
        return event_response(rx);
    }
    while let Some(event) = rx.recv().await {
        match event["event"].as_str() {
            Some("completed") => return json_response(event),
            Some("error") => return (StatusCode::BAD_GATEWAY, Json(event)).into_response(),
            _ => {}
        }
    }
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"code":"sdk_stream_incomplete"})),
    )
        .into_response()
}
async fn run() -> Result<()> {
    let config = Config::load()?;
    let transport = transport::Transport::load()?;
    if std::env::args().nth(1).as_deref() == Some("health") {
        let (client, health_url) = transport.health()?;
        let mut r = client
            .get(health_url)
            .bearer_auth(config.token)
            .send()
            .await
            .map_err(|_| "sdk_worker_not_ready")?;
        if !r.status().is_success() {
            return Err("sdk_worker_not_ready");
        }
        let mut b = Vec::new();
        while let Some(c) = r.chunk().await.map_err(|_| "sdk_worker_not_ready")? {
            if b.len() + c.len() > 4096 {
                return Err("sdk_worker_not_ready");
            }
            b.extend(c);
        }
        if serde_json::from_slice::<Value>(&b).map_err(|_| "sdk_worker_not_ready")?["ready"] != true
        {
            return Err("sdk_worker_not_ready");
        }
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() != Some("serve") {
        return Err("sdk_worker_action_required");
    }
    unsafe {
        libc::umask(0o077);
    }
    let gateway_ca = std::env::var("SDK_AGENT_GATEWAY_TLS_CA_FILE")
        .ok()
        .map(PathBuf::from);
    let authority = Authority::new_with_ca(config.gateway.clone(), gateway_ca.as_deref())?;
    let (stop, _) = watch::channel(false);
    let app = Arc::new(App {
        config,
        authority,
        owners: Mutex::new(HashMap::new()),
        closing: AtomicBool::new(false),
        sweep: Mutex::new(None),
        stop,
    });
    let sweeper_app = app.clone();
    let mut stopped = app.stop.subscribe();
    let sweep = tokio::spawn(async move {
        let mut sweeper = lifecycle::Sweeper::new();
        loop {
            let result = sweeper.tick(&sweeper_app.config.root, sweeper_app.config.retention);
            *sweeper_app.sweep.lock().unwrap() = result.ok().map(|_| Instant::now());
            tokio::select! {_=tokio::time::sleep(Duration::from_secs(30))=>{},_=stopped.changed()=>break}
        }
    });
    let router = Router::new()
        .route("/v1/runs", post(runs))
        .route("/v1/sessions", delete(delete_session))
        .route("/health/ready", get(ready))
        .with_state(app.clone());
    let shutdown = app.clone();
    let mut stopped = app.stop.subscribe();
    let graceful = async move {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal initialization");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        shutdown.closing.store(true, Ordering::SeqCst);
        let _ = shutdown.stop.send(true);
    };
    let server = async move {
        if workload_identity::tls_enabled() {
            if transport.has_legacy_tls() {
                return Err("sdk_workload_tls_legacy_listener_conflict");
            }
            workload_identity::serve(transport.address(), router, graceful)
                .await
                .map_err(|_| "sdk_worker_server_failed")
        } else {
            let listener = transport.bind().await?;
            axum::serve(listener, router)
                .with_graceful_shutdown(graceful)
                .await
                .map_err(|_| "sdk_worker_server_failed")
        }
    };
    tokio::pin!(server);
    tokio::select! {
        result=&mut server=>{result.map_err(|_|"sdk_worker_server_failed")?;app.closing.store(true,Ordering::SeqCst);let _=app.stop.send(true);},
        _=stopped.changed()=>{let _=tokio::time::timeout(Duration::from_secs(10),&mut server).await;}
    }
    let _ = sweep.await;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.owners.lock().unwrap().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    if !app.owners.lock().unwrap().is_empty() {
        return Err("sdk_cleanup_unresolved");
    }
    Ok(())
}
#[tokio::main]
async fn main() {
    if let Err(code) = run().await {
        eprintln!("{code}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod http_tests {
    use super::*;
    use std::{
        collections::HashSet,
        fs,
        os::unix::fs::PermissionsExt,
        path::Path,
        sync::atomic::AtomicU64,
        time::{SystemTime, UNIX_EPOCH},
    };
    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);
    fn fixture(script: &str) -> (Arc<App>, PathBuf) {
        fixture_with_gateway(script, None)
    }
    fn fixture_with_gateway(script: &str, gateway: Option<String>) -> (Arc<App>, PathBuf) {
        let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "sdk-http-{}-{}-{fixture_id}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let engine = root.join("test-engine");
        fs::write(&engine, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&engine, fs::Permissions::from_mode(0o700)).unwrap();
        let (stop, _) = watch::channel(false);
        let app = Arc::new(App {
            config: Config {
                token: "test-worker-token-not-a-secret-123456".into(),
                root: root.clone(),
                // /tmp is intentionally noexec in the restricted build role.
                // Execute the installed interpreter, reading fixture source as data.
                engine: PathBuf::from("/bin/sh"),
                fixture_script: Some(engine),
                retention: 3600,
                gateway: gateway.clone(),
            },
            authority: Authority::new(gateway).unwrap().with_model_fixtures(),
            owners: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
            sweep: Mutex::new(Some(Instant::now())),
            stop,
        });
        (app, root)
    }
    async fn wait_for_session_release(root: &Path, scope: &str) -> lifecycle::Session {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            match lifecycle::acquire(root, scope) {
                Ok(session) => return session,
                Err("sdk_session_busy") if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err("sdk_session_busy") => {
                    panic!("session lock did not release after child cleanup")
                }
                Err(code) => panic!("session did not reach clean ownership release: {code}"),
            }
        }
    }
    fn body(stream: bool) -> Value {
        json!({"protocol":PROTOCOL,"framework":"openai_agents","service_id":"svc","tenant_id":"tenant","user_id":"user","request_id":"request","session_id":"session","agent":{"model":"fixture","instructions":"","max_turns":2},"input":"hello","stream":stream})
    }
    fn fixture_grant() -> String {
        "a".repeat(64)
    }
    fn sdk_body(stream: bool, request_id: &str, run_id: &str, input: &str, tools: Value) -> Value {
        let mut value = body(stream);
        value["request_id"] = json!(request_id);
        value["input"] = json!(input);
        let timeout = if tools.as_array().is_some_and(|items| !items.is_empty()) {
            1800
        } else {
            300
        };
        value["capability_bridge"] = json!({"grant":fixture_grant(),"run_id":run_id,"tools":tools,"timeout_seconds":timeout});
        value
    }
    #[derive(Default)]
    struct ReconcileGateway {
        cancelled: Mutex<HashSet<String>>,
        requests: Mutex<Vec<String>>,
    }
    async fn capability_admit(
        State(state): State<Arc<ReconcileGateway>>,
        headers: axum::http::HeaderMap,
        Json(body): Json<Value>,
    ) -> Response {
        let expected = format!("Bearer {}", fixture_grant());
        if headers.get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
            return (StatusCode::FORBIDDEN, Json(json!({"authorized":false}))).into_response();
        }
        if let Some(request_id) = body["reconcile_pending_request_id"].as_str() {
            state.requests.lock().unwrap().push(request_id.to_owned());
            let safe = state.cancelled.lock().unwrap().contains(request_id);
            return Json(json!({"authorized":true,"pending_reconcilable":safe})).into_response();
        }
        Json(json!({"authorized":true})).into_response()
    }
    async fn reconcile_gateway() -> (String, Arc<ReconcileGateway>, tokio::task::JoinHandle<()>) {
        let state = Arc::new(ReconcileGateway::default());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route(
                "/internal/v1/sdk-tools/capabilities/admit",
                post(capability_admit),
            )
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        (origin, state, task)
    }
    fn write_tool() -> Value {
        json!([{"name":"platform_capability_0","tool":"write_action","namespace":null,"description":"","input_schema":{},"id":"write-capability","schema_hash":format!("sha256:{}","a".repeat(64)),"effect":"write"}])
    }
    fn request(app: &App, body: Value) -> Request {
        Request::builder()
            .method("POST")
            .uri("/v1/runs")
            .header("authorization", format!("Bearer {}", app.config.token))
            .body(Body::from(body.to_string()))
            .unwrap()
    }
    async fn value(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), MAX_BODY).await.unwrap()).unwrap()
    }
    #[tokio::test]
    async fn unauthorized_and_strict_body() {
        let (app, root) = fixture("exit 1");
        let response = runs(State(app.clone()), Request::new(Body::empty())).await;
        assert_eq!(response.status(), 401);
        let mut b = body(false);
        b["extra"] = json!(true);
        assert_eq!(
            runs(State(app.clone()), request(&app, b)).await.status(),
            400
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn completed_receipt_replay_and_reuse() {
        let (app, root) = fixture(
            "read frame\nprintf '%s\\n' '{\"event\":\"delta\",\"text\":\"answer\"}' '{\"event\":\"completed\",\"text\":\"answer\",\"streamed_text\":\"answer\",\"usage\":{}}'",
        );
        let first = runs(State(app.clone()), request(&app, body(false))).await;
        assert_eq!(first.status(), 200);
        assert_eq!(value(first).await["text"], "answer");
        let scope = contract::scope(&body(false)).unwrap();
        assert!(!root.join(&scope).join("pending.json").exists());
        let second = runs(State(app.clone()), request(&app, body(false))).await;
        assert_eq!(second.status(), 200);
        assert_eq!(value(second).await["text"], "answer");
        let mut changed = body(false);
        changed["input"] = json!("different");
        assert_eq!(
            runs(State(app.clone()), request(&app, changed))
                .await
                .status(),
            409
        );
        assert!(app.owners.lock().unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn malformed_child_is_fenced_until_delete() {
        let (app, root) =
            fixture("read frame\nprintf '%s\\n' '{\"event\":\"completed\",\"text\":7}'");
        assert_eq!(
            runs(State(app.clone()), request(&app, body(false)))
                .await
                .status(),
            502
        );
        assert_eq!(
            runs(State(app.clone()), request(&app, body(false)))
                .await
                .status(),
            409
        );
        let scope = contract::scope(&body(false)).unwrap();
        let session = wait_for_session_release(&root, &scope).await;
        let pending = lifecycle::read_pending(&session)
            .unwrap()
            .expect("malformed execution remains fenced");
        assert_eq!(pending.request_id, "request");
        assert!(lifecycle::locally_reconcilable(&session, &pending));
        drop(session);
        assert!(lifecycle::remove(&root, &scope, "deleted", None).unwrap());
        assert_eq!(
            runs(State(app.clone()), request(&app, body(false)))
                .await
                .status(),
            410
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn stream_disconnect_kills_group_and_preserves_unknown() {
        let (app, root) = fixture("read frame\ntrap '' TERM\nsleep 60 &\nwait");
        let response = runs(State(app.clone()), request(&app, body(true))).await;
        assert_eq!(response.status(), 200);
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(response);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !app.owners.lock().unwrap().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(app.owners.lock().unwrap().is_empty());
        let scope = contract::scope(&body(true)).unwrap();
        assert!(root.join(&scope).join("pending.json").exists());
        assert!(root.join(&scope).join(".cleanup-complete").exists());
        assert!(lifecycle::remove(&root, &scope, "deleted", None).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cancelled_effect_free_run_reconciles_then_same_session_continues() {
        const RUN_ONE: &str = "10000000-0000-4000-8000-000000000001";
        const RUN_TWO: &str = "10000000-0000-4000-8000-000000000002";
        const RUN_CANCEL: &str = "10000000-0000-4000-8000-000000000003";
        const RUN_NEXT: &str = "10000000-0000-4000-8000-000000000004";
        const RUN_RETRY: &str = "10000000-0000-4000-8000-000000000005";
        let script = "read frame\ncase \"$frame\" in *cancel-sentinel*) printf '%s\\n' '{\"event\":\"delta\",\"text\":\"partial response\"}'; trap '' TERM; sleep 60 & wait ;; *) printf '%s\\n' '{\"event\":\"delta\",\"text\":\"answer\"}' '{\"event\":\"completed\",\"text\":\"answer\",\"streamed_text\":\"answer\",\"usage\":{}}' ;; esac";
        let (origin, gateway, server) = reconcile_gateway().await;
        let (app, root) = fixture_with_gateway(script, Some(origin));
        for (request_id, run_id, input) in [
            ("first-request", RUN_ONE, "first turn"),
            ("second-request", RUN_TWO, "second turn"),
        ] {
            let response = runs(
                State(app.clone()),
                request(&app, sdk_body(false, request_id, run_id, input, json!([]))),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert_eq!(value(response).await["text"], "answer");
        }
        let first_scope = contract::scope(&sdk_body(
            false,
            "first-request",
            RUN_ONE,
            "first turn",
            json!([]),
        ))
        .unwrap();
        let state = root.join(&first_scope);
        let prior_history = json!({"schema":"sdk-native-history/v1","framework":"openai_agents","messages":[
            {"role":"user","content":"first turn"},{"type":"message","role":"assistant","content":[]},
            {"role":"user","content":"second turn"},{"type":"message","role":"assistant","content":[]}
        ]});
        lifecycle::atomic(&state.join("native-history.json"), &prior_history).unwrap();
        let history_before = fs::read(state.join("native-history.json")).unwrap();

        let cancel_body = sdk_body(
            true,
            "cancel-request",
            RUN_CANCEL,
            "cancel-sentinel long list",
            json!([]),
        );
        let response = runs(State(app.clone()), request(&app, cancel_body)).await;
        assert_eq!(response.status(), 200);
        let mut events = response.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&first).contains("partial response"));
        drop(events);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !app.owners.lock().unwrap().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(app.owners.lock().unwrap().is_empty());
        assert!(state.join("pending.json").exists());
        let unverified = runs(
            State(app.clone()),
            request(
                &app,
                sdk_body(
                    false,
                    "unverified-next",
                    RUN_RETRY,
                    "after cancellation",
                    json!([]),
                ),
            ),
        )
        .await;
        assert_eq!(unverified.status(), 409);
        assert_eq!(
            value(unverified).await["code"],
            "sdk_session_reconciliation_required"
        );
        assert!(state.join("pending.json").exists());
        gateway
            .cancelled
            .lock()
            .unwrap()
            .insert("cancel-request".into());

        let next = runs(
            State(app.clone()),
            request(
                &app,
                sdk_body(
                    false,
                    "next-request",
                    RUN_NEXT,
                    "after cancellation",
                    json!([]),
                ),
            ),
        )
        .await;
        assert_eq!(next.status(), 200);
        assert_eq!(value(next).await["text"], "answer");
        assert!(!state.join("pending.json").exists());
        assert_eq!(
            fs::read(state.join("native-history.json")).unwrap(),
            history_before
        );
        assert_eq!(
            *gateway.requests.lock().unwrap(),
            vec!["cancel-request", "cancel-request"]
        );
        let canceled = fs::read_dir(&state)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .find(|entry| {
                entry.file_name().to_string_lossy().starts_with("run-")
                    && lifecycle::read(&entry.path()).is_ok_and(|value| {
                        value["reconciliation"]["request_id"] == "cancel-request"
                    })
            })
            .expect("durable cancellation reconciliation receipt");
        let receipt = lifecycle::read(&canceled.path()).unwrap();
        assert_eq!(receipt["reconciliation"]["provider_outcome"], "unknown");
        assert_eq!(receipt["reconciliation"]["usage_state"], "unavailable");
        assert_eq!(receipt["reconciliation"]["cost_state"], "unavailable");
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn cancelled_tool_bearing_pending_remains_fenced() {
        const RUN_CANCEL: &str = "20000000-0000-4000-8000-000000000003";
        const RUN_NEXT: &str = "20000000-0000-4000-8000-000000000004";
        let script = "read frame\ncase \"$frame\" in *cancel-sentinel*) printf '%s\\n' '{\"event\":\"delta\",\"text\":\"partial response\"}'; trap '' TERM; sleep 60 & wait ;; *) printf '%s\\n' '{\"event\":\"completed\",\"text\":\"answer\",\"streamed_text\":\"answer\",\"usage\":{}}' ;; esac";
        let (origin, gateway, server) = reconcile_gateway().await;
        let (app, root) = fixture_with_gateway(script, Some(origin));
        let tools = write_tool();
        let cancel_body = sdk_body(
            true,
            "effectful-cancel",
            RUN_CANCEL,
            "cancel-sentinel",
            tools.clone(),
        );
        let response = runs(State(app.clone()), request(&app, cancel_body)).await;
        assert_eq!(response.status(), 200);
        let mut events = response.into_body().into_data_stream();
        let _ = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        drop(events);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !app.owners.lock().unwrap().is_empty() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(app.owners.lock().unwrap().is_empty());
        let scope = contract::scope(&sdk_body(
            true,
            "effectful-cancel",
            RUN_CANCEL,
            "cancel-sentinel",
            tools.clone(),
        ))
        .unwrap();
        let state = root.join(scope);
        assert!(state.join("pending.json").exists());
        gateway
            .cancelled
            .lock()
            .unwrap()
            .insert("effectful-cancel".into());
        let next = runs(
            State(app.clone()),
            request(
                &app,
                sdk_body(true, "effectful-next", RUN_NEXT, "next", tools),
            ),
        )
        .await;
        assert_eq!(next.status(), 409);
        assert_eq!(
            value(next).await["code"],
            "sdk_session_reconciliation_required"
        );
        assert!(state.join("pending.json").exists());
        assert!(gateway.requests.lock().unwrap().is_empty());
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn orphan_cleanup_blocks_readiness() {
        let (app, root) = fixture("exit 0");
        let s = lifecycle::acquire(&root, &"d".repeat(64)).unwrap();
        lifecycle::begin_cleanup(&s).unwrap();
        drop(s);
        let response = ready(State(app.clone()), request(&app, body(false))).await;
        assert_eq!(response.status(), 503);
        assert_eq!(value(response).await["ready"], false);
        fs::remove_dir_all(root).unwrap();
    }
    #[tokio::test]
    async fn provider_key_header_is_rejected_before_launch_or_local_state() {
        let (app, root) = fixture("exit 99");
        let mut req = request(&app, body(false));
        req.headers_mut().insert(
            "x-sdk-provider-key",
            header::HeaderValue::from_static("fixture-forbidden"),
        );
        let response = runs(State(app.clone()), req).await;
        assert_eq!(response.status(), 400);
        assert_eq!(
            value(response).await["code"],
            "sdk_provider_credential_forbidden"
        );
        assert!(app.owners.lock().unwrap().is_empty());
        assert!(!root.join(contract::scope(&body(false)).unwrap()).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn real_model_revocation_cleans_the_process_group_and_fences_unknown_outcome() {
        use std::convert::Infallible;
        const RUN: &str = "30000000-0000-4000-8000-000000000001";
        #[derive(Clone)]
        struct ModelAuthority {
            body: Value,
            revoked: watch::Sender<bool>,
        }
        async fn lease(
            State(state): State<ModelAuthority>,
            headers: axum::http::HeaderMap,
            Json(request): Json<Value>,
        ) -> Json<Value> {
            assert_eq!(
                headers["authorization"],
                format!("Bearer {}", fixture_grant())
            );
            assert!(!headers.contains_key("x-sdk-provider-key"));
            assert_eq!(request, json!({"run_id":RUN}));
            Json(
                json!({"schema":"sdk-model-plane/v1","expires_at_epoch_ms":model_plane::now_ms()+30_000,
                "binding_sha256":model_plane::launch_binding(&state.body)}),
            )
        }
        async fn revocations(
            State(state): State<ModelAuthority>,
            headers: axum::http::HeaderMap,
            Json(request): Json<Value>,
        ) -> Response {
            assert_eq!(
                headers["authorization"],
                format!("Bearer {}", fixture_grant())
            );
            assert!(!headers.contains_key("x-sdk-provider-key"));
            assert_eq!(request, json!({"run_id":RUN}));
            let mut receiver = state.revoked.subscribe();
            let initial = futures_util::stream::once(async {
                Ok::<_, Infallible>(format!(
                    "data: {}\n\n",
                    json!({"type":"sdk.lease_heartbeat","run_id":RUN})
                ))
            });
            let end = futures_util::stream::once(async move {
                while !*receiver.borrow_and_update() {
                    if receiver.changed().await.is_err() {
                        break;
                    }
                }
                Ok::<_, Infallible>(format!(
                    "data: {}\n\n",
                    json!({"type":"sdk.lease_revoked","run_id":RUN})
                ))
            });
            Response::builder()
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(initial.chain(end)))
                .unwrap()
        }
        let launch = sdk_body(true, "revoked-request", RUN, "cancel-sentinel", json!([]));
        let (revoked, _) = watch::channel(false);
        let authority = ModelAuthority {
            body: launch.clone(),
            revoked,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server_router = Router::new()
            .route("/internal/v1/sdk-tools/models/lease", post(lease))
            .route(
                "/internal/v1/sdk-tools/models/revocations",
                post(revocations),
            )
            .route(
                "/internal/v1/sdk-tools/capabilities/admit",
                post(|| async { Json(json!({"authorized":true})) }),
            )
            .with_state(authority.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, server_router).await.unwrap();
        });
        let script = "read frame\n[ -z \"${OPENAI_API_KEY+x}\" ] && [ -z \"${ANTHROPIC_API_KEY+x}\" ] || exit 97\ntrap '' TERM\nsleep 60 &\nprintf '%s\\n' \"$!\" > \"$HOME/descendant.pid\"\nprintf '%s\\n' '{\"event\":\"delta\",\"text\":\"model lease active\"}'\nwait";
        let (mut app, root) = fixture_with_gateway(script, Some(origin.clone()));
        Arc::get_mut(&mut app).unwrap().authority = Authority::new(Some(origin)).unwrap();
        let response = runs(State(app.clone()), request(&app, launch.clone())).await;
        assert_eq!(response.status(), 200);
        let mut stream = response.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&first).contains("model lease active"));
        let scope = contract::scope(&launch).unwrap();
        let state = root.join(&scope);
        let descendant: i32 = fs::read_to_string(state.join("descendant.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // Persist revocation even if the HTTP watcher is still subscribing.
        authority.revoked.send_replace(true);
        let terminal = tokio::time::timeout(Duration::from_secs(8), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&terminal).contains("sdk_execution_failed"));
        assert!(!String::from_utf8_lossy(&terminal).contains("completed"));
        drop(stream);
        assert!(app.owners.lock().unwrap().is_empty());
        assert!(app.state_ready());
        assert!(state.join("pending.json").exists());
        assert!(state.join(".cleanup-complete").exists());
        assert_eq!(unsafe { libc::kill(descendant, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert_eq!(
            runs(State(app.clone()), request(&app, launch))
                .await
                .status(),
            409
        );
        server.abort();
        fs::remove_dir_all(root).unwrap();
    }
}
