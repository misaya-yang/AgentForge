use ai_platform_local_node::{
    digest,
    envelope::{Action, PlatformVerifier},
    outbox::{Outbox, Receipt},
    runner::{
        ControlledDispatcher, Dispatcher, Outcome, RemoteControl, ReplayGuard, RunnerTransport,
        claim, file_write_intent, run_forever,
    },
    transport::{Credential, ExchangeRequest, ExchangeResponse},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

const PLATFORM_KEY: &[u8] = b"synthetic-local-runner-platform-key";

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("local-runner-recovery-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sign(payload: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(PLATFORM_KEY).unwrap();
    mac.update(payload);
    format!("{:x}", mac.finalize().into_bytes())
}

struct Trusted;
impl PlatformVerifier for Trusted {
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> bool {
        key_id == "fixture-platform-key" && sign(payload) == signature
    }
}

fn claim_fixture(arguments: Value) -> Value {
    let mut action: Action = serde_json::from_value(json!({
        "action_id":"action-fixture",
        "idempotency_key":"idempotency-fixture",
        "tenant_id":"tenant-fixture",
        "user_id":"user-fixture",
        "session_id":"session-fixture",
        "run_id":"run-fixture",
        "agent_id":"agent-fixture",
        "agent_version":"1",
        "call_id":"call-fixture",
        "device_id":"device-fixture",
        "envelope_version":1,
        "capability":"file.read",
        "tool_name":"local_file_read",
        "operation":"file.read",
        "capability_lease_id":"grant-fixture",
        "resource_refs":["grant-fixture","fixture.txt"],
        "arguments_digest":digest(&arguments),
        "target_snapshot_digest":"target-fixture",
        "policy_snapshot_digest":"policy-fixture",
        "nonce":"nonce-fixture",
        "issued_at":ai_platform_local_node::now()-1.0,
        "expires_at":ai_platform_local_node::now()+60.0,
        "platform_key_id":"fixture-platform-key",
        "platform_signature":"",
        "approval":null,
        "trace_context":null
    }))
    .unwrap();
    // Action signatures cover canonical signed payload bytes, not the digest text.
    action.platform_signature =
        sign(ai_platform_local_node::canonical(&action.signed_payload()).as_bytes());
    json!({
        "kind":"claim",
        "command_id":"command-fixture",
        "action":action,
        "normalized_arguments":arguments
    })
}

struct CountingDispatcher(usize);
impl Dispatcher for CountingDispatcher {
    fn dispatch(&mut self, _: &Action, _: &Value) -> ai_platform_local_node::Result<Outcome> {
        self.0 += 1;
        Ok(Outcome {
            status: "succeeded".into(),
            result: json!({"kind":"file_read","content":"synthetic"}),
            error_code: None,
        })
    }
}

fn opened_state(temp: &TempDir) -> (ReplayGuard, Outbox) {
    (
        ReplayGuard::open(&temp.0.join("replay.sqlite")).unwrap(),
        Outbox::open(&temp.0.join("outbox.sqlite")).unwrap(),
    )
}

#[test]
fn cancellation_tombstone_suppresses_a_later_claim_and_survives_duplicate_delivery() {
    let temp = TempDir::new();
    let (mut replay, mut outbox) = opened_state(&temp);
    replay.cancel_action("action-fixture").unwrap();
    let command = claim_fixture(json!({"grant_id":"grant-fixture","path":"fixture.txt"}));
    let mut dispatcher = CountingDispatcher(0);

    assert!(
        claim(
            "device-fixture",
            command.clone(),
            &Trusted,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .unwrap()
    );
    assert_eq!(dispatcher.0, 0);
    let receipt = outbox.pending(10).unwrap().remove(0);
    assert_eq!(receipt.status.as_deref(), Some("cancelled"));
    assert_eq!(receipt.result.unwrap()["replay_allowed"], false);

    assert!(
        !claim(
            "device-fixture",
            command,
            &Trusted,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .unwrap()
    );
    assert_eq!(dispatcher.0, 0);
    assert_eq!(outbox.pending(10).unwrap().len(), 1);
}

#[test]
fn consumed_reservation_without_receipt_becomes_unknown_and_never_dispatches() {
    let temp = TempDir::new();
    let (mut replay, mut outbox) = opened_state(&temp);
    let command = claim_fixture(json!({"grant_id":"grant-fixture","path":"fixture.txt"}));
    let action: Action = serde_json::from_value(command["action"].clone()).unwrap();
    replay
        .reserve(
            "command-fixture",
            &action.nonce,
            &digest(&action.signed_payload()),
        )
        .unwrap();
    let mut dispatcher = CountingDispatcher(0);

    assert!(
        !claim(
            "device-fixture",
            command.clone(),
            &Trusted,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .unwrap()
    );
    let receipt = outbox.pending(10).unwrap().remove(0);
    assert_eq!(receipt.status.as_deref(), Some("unknown"));
    assert_eq!(
        receipt.error_code.as_deref(),
        Some("command_outcome_missing")
    );
    assert_eq!(dispatcher.0, 0);

    assert!(
        !claim(
            "device-fixture",
            command,
            &Trusted,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .unwrap()
    );
    assert_eq!(outbox.pending(10).unwrap().len(), 1);
}

#[test]
fn unknown_recovery_does_not_replace_a_committed_terminal_receipt() {
    let temp = TempDir::new();
    let (mut replay, mut outbox) = opened_state(&temp);
    let command = claim_fixture(json!({"grant_id":"grant-fixture","path":"fixture.txt"}));
    let action: Action = serde_json::from_value(command["action"].clone()).unwrap();
    replay
        .reserve(
            "command-fixture",
            &action.nonce,
            &digest(&action.signed_payload()),
        )
        .unwrap();
    outbox
        .append(
            Receipt {
                sequence: 0,
                event_id: "command.command-fixture.action-fixture".into(),
                action_id: Some("action-fixture".into()),
                event_type: "action.succeeded".into(),
                status: Some("succeeded".into()),
                occurred_at: ai_platform_local_node::now(),
                result_digest: None,
                error_code: None,
                summary: None,
                result: Some(json!({"kind":"file_read","content":"synthetic result"})),
            },
            true,
        )
        .unwrap();
    let mut dispatcher = CountingDispatcher(0);

    assert!(
        !claim(
            "device-fixture",
            command,
            &Trusted,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .unwrap()
    );
    let receipts = outbox.pending(10).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].status.as_deref(), Some("succeeded"));
    assert_eq!(
        receipts[0].result.as_ref().unwrap()["content"],
        "synthetic result"
    );
    assert_eq!(dispatcher.0, 0);
}

#[test]
fn write_intent_decodes_only_bounded_content_bound_to_signed_hash() {
    let content = b"synthetic fixture write";
    let content_hash = ai_platform_local_node::sha256(content);
    let arguments = json!({
        "grant_id":"grant-fixture",
        "relative_path":"fixture.txt",
        "content_sha256":content_hash,
        "expected_hash":null,
        "content_base64":URL_SAFE_NO_PAD.encode(content)
    });
    let (intent, decoded) = file_write_intent(&arguments).unwrap();
    assert_eq!(decoded, content);
    assert_eq!(
        intent,
        json!({
            "grant_id":"grant-fixture",
            "relative_path":"fixture.txt",
            "content_sha256":ai_platform_local_node::sha256(content),
            "expected_hash":null
        })
    );

    let mut tampered = arguments;
    tampered["content_sha256"] = json!("0".repeat(64));
    assert!(file_write_intent(&tampered).is_err());
    tampered["extra"] = json!("forbidden");
    assert!(file_write_intent(&tampered).is_err());
}

struct PumpState {
    cancelled: Mutex<bool>,
    changed: Condvar,
    side_effect: AtomicBool,
}
struct PumpControl {
    state: Arc<PumpState>,
    stop: tokio::sync::watch::Sender<bool>,
}
impl RemoteControl for PumpControl {
    fn cancel(&self, _: &str) -> ai_platform_local_node::Result<Outcome> {
        *self.state.cancelled.lock().unwrap() = true;
        self.state.changed.notify_all();
        let _ = self.stop.send(true);
        Ok(Outcome {
            status: "cancelled".into(),
            result: json!({"cancelled":true,"replay_allowed":false}),
            error_code: None,
        })
    }
    fn emergency_stop(&self) -> ai_platform_local_node::Result<Vec<(String, Outcome)>> {
        *self.state.cancelled.lock().unwrap() = true;
        self.state.changed.notify_all();
        let _ = self.stop.send(true);
        Ok(vec![(
            "action-fixture".into(),
            Outcome {
                status: "cancelled".into(),
                result: json!({"cancelled":true,"replay_allowed":false}),
                error_code: None,
            },
        )])
    }
    fn is_cancelled(&self, _: &str) -> bool {
        *self.state.cancelled.lock().unwrap()
    }
    fn completion_override(&self, _: &str) -> ai_platform_local_node::Result<Option<Outcome>> {
        Ok(self.is_cancelled("").then(|| Outcome {
            status: "cancelled".into(),
            result: json!({"cancelled":true,"replay_allowed":false}),
            error_code: None,
        }))
    }
    fn finish_action(&self, _: &str) {}
}
struct BlockingDispatcher {
    state: Arc<PumpState>,
    started: Arc<tokio::sync::Notify>,
    control: Arc<PumpControl>,
    dispatches: Arc<AtomicUsize>,
}
impl Dispatcher for BlockingDispatcher {
    fn dispatch(&mut self, _: &Action, _: &Value) -> ai_platform_local_node::Result<Outcome> {
        self.dispatches.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        let mut cancelled = self.state.cancelled.lock().unwrap();
        while !*cancelled {
            cancelled = self.state.changed.wait(cancelled).unwrap();
        }
        Ok(Outcome {
            status: "cancelled".into(),
            result: json!({"cancelled":true,"replay_allowed":false}),
            error_code: None,
        })
    }
}
impl ControlledDispatcher for BlockingDispatcher {
    fn connect(&mut self) -> ai_platform_local_node::Result<()> {
        Ok(())
    }
    fn cancel(&mut self, action_id: &str) -> ai_platform_local_node::Result<Outcome> {
        self.control.cancel(action_id)
    }
    fn emergency_stop(&mut self) -> ai_platform_local_node::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn disconnect(&mut self) -> ai_platform_local_node::Result<Vec<String>> {
        Ok(Vec::new())
    }
    fn remote_control(&self) -> Arc<dyn RemoteControl> {
        self.control.clone()
    }
}
#[derive(Clone, Copy, PartialEq)]
enum StreamMode {
    Poll,
    Healthy,
    Buffered,
}
struct PumpTransport {
    mode: StreamMode,
    calls: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    claim: Value,
    control_command: Value,
}
impl RunnerTransport for PumpTransport {
    fn open_control_stream<'a>(
        &'a self,
        _: &'a Credential,
        _: Option<&'a str>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = ai_platform_local_node::Result<
                        Option<Box<dyn ai_platform_local_node::control_stream::ControlStream>>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            if self.mode == StreamMode::Poll {
                return Ok(None);
            }
            Ok(Some(Box::new(FixtureControlStream {
                mode: self.mode,
                started: self.started.clone(),
                command: self.control_command.clone(),
                sequence: 0,
                stream_id: uuid::Uuid::new_v4().to_string(),
            })
                as Box<
                    dyn ai_platform_local_node::control_stream::ControlStream,
                >))
        })
    }
    fn exchange<'a>(
        &'a self,
        _: &'a Credential,
        _: &'a ExchangeRequest,
    ) -> Pin<Box<dyn Future<Output = ai_platform_local_node::Result<ExchangeResponse>> + Send + 'a>>
    {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let mode = self.mode;
        let started = self.started.clone();
        let claim = self.claim.clone();
        let control_command = self.control_command.clone();
        Box::pin(async move {
            let commands = match call {
                0 => vec![claim],
                1 if mode != StreamMode::Healthy => {
                    started.notified().await;
                    vec![control_command]
                }
                _ => Vec::new(),
            };
            Ok(ExchangeResponse {
                protocol_version: ai_platform_local_node::transport::PROTOCOL_VERSION.into(),
                accepted_through_sequence: 0,
                commands,
            })
        })
    }
}
struct FixtureControlStream {
    mode: StreamMode,
    started: Arc<tokio::sync::Notify>,
    command: Value,
    sequence: u64,
    stream_id: String,
}
impl ai_platform_local_node::control_stream::ControlStream for FixtureControlStream {
    fn next(
        &mut self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = ai_platform_local_node::Result<
                        ai_platform_local_node::control_stream::ControlFrame,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            if self.mode == StreamMode::Buffered || self.sequence > 1 {
                return std::future::pending().await;
            }
            let commands = if self.sequence == 0 {
                Vec::new()
            } else {
                self.started.notified().await;
                vec![self.command.clone()]
            };
            self.sequence += 1;
            let mut frame = ai_platform_local_node::control_stream::ControlFrame {
                protocol_version: ai_platform_local_node::transport::PROTOCOL_VERSION.into(),
                device_id: "device-fixture".into(),
                stream_id: self.stream_id.clone(),
                sequence: self.sequence,
                cursor: format!("{}:{}", self.stream_id, self.sequence),
                issued_at: ai_platform_local_node::now(),
                expires_at: ai_platform_local_node::now() + 5.0,
                commands,
                platform_key_id: "fixture-platform-key".into(),
                platform_signature: String::new(),
            };
            let mut value = serde_json::to_value(&frame).unwrap();
            value.as_object_mut().unwrap().remove("platform_signature");
            frame.platform_signature = sign(ai_platform_local_node::canonical(&value).as_bytes());
            Ok(frame)
        })
    }
}
fn signed_cancel() -> Value {
    let mut value = json!({"kind":"cancel","command_id":"control-fixture","device_id":"device-fixture","nonce":"control-nonce","issued_at":ai_platform_local_node::now()-1.0,"expires_at":ai_platform_local_node::now()+60.0,"platform_key_id":"fixture-platform-key","action_id":"action-fixture","platform_signature":""});
    value.as_object_mut().unwrap().remove("platform_signature");
    let signature = sign(ai_platform_local_node::canonical(&value).as_bytes());
    value["platform_signature"] = json!(signature);
    value
}
fn signed_emergency_stop() -> Value {
    let mut value = json!({"kind":"emergency_stop","command_id":"emergency-fixture","device_id":"device-fixture","nonce":"emergency-nonce","issued_at":ai_platform_local_node::now()-1.0,"expires_at":ai_platform_local_node::now()+60.0,"platform_key_id":"fixture-platform-key","platform_signature":""});
    value.as_object_mut().unwrap().remove("platform_signature");
    let signature = sign(ai_platform_local_node::canonical(&value).as_bytes());
    value["platform_signature"] = json!(signature);
    value
}

async fn assert_runner_control_cancels_pending_dispatch(control_command: Value) {
    assert_runner_control_mode(control_command, StreamMode::Poll).await;
}
async fn assert_runner_control_mode(control_command: Value, mode: StreamMode) -> Duration {
    let measured = std::time::Instant::now();
    let emergency = control_command["command_id"]
        .as_str()
        .is_some_and(|id| id.starts_with("emergency."));
    let temp = TempDir::new();
    let started = Arc::new(tokio::sync::Notify::new());
    let (stop_sender, stop_receiver) = tokio::sync::watch::channel(false);
    let state = Arc::new(PumpState {
        cancelled: Mutex::new(false),
        changed: Condvar::new(),
        side_effect: AtomicBool::new(false),
    });
    let control = Arc::new(PumpControl {
        state: state.clone(),
        stop: stop_sender,
    });
    let dispatcher = BlockingDispatcher {
        state: state.clone(),
        started: started.clone(),
        control: control.clone(),
        dispatches: Arc::new(AtomicUsize::new(0)),
    };
    let dispatches = dispatcher.dispatches.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = PumpTransport {
        mode,
        calls: calls.clone(),
        started,
        claim: claim_fixture(json!({"grant_id":"grant-fixture","path":"fixture.txt"})),
        control_command,
    };
    let credential = Credential::new(
        "device-fixture".into(),
        "opaque-fixture-credential".into(),
        ai_platform_local_node::now() + 120.0,
    )
    .unwrap();
    let mut replay = ReplayGuard::open(&temp.0.join("replay.sqlite")).unwrap();
    let mut outbox = Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        run_forever(
            "device-fixture".into(),
            json!({"status":"ready"}),
            transport,
            credential,
            Arc::new(Trusted),
            replay,
            outbox,
            dispatcher,
            if mode == StreamMode::Poll {
                Duration::from_millis(250)
            } else {
                Duration::from_secs(5)
            },
            stop_receiver,
        ),
    )
    .await;
    assert!(result.is_ok(), "poller remained blocked behind dispatcher");
    result.unwrap().unwrap();
    assert!(*state.cancelled.lock().unwrap());
    assert_eq!(dispatches.load(Ordering::SeqCst), 1, "claim was replayed");
    if mode == StreamMode::Healthy {
        assert!(
            calls.load(Ordering::SeqCst) <= 2,
            "stream control waited for polling"
        );
        assert!(
            ReplayGuard::open(&temp.0.join("replay.sqlite"))
                .unwrap()
                .stream_cursor("device-fixture")
                .unwrap()
                .is_some()
        );
    } else {
        assert!(calls.load(Ordering::SeqCst) >= 2, "control was not polled");
    }
    assert!(!state.side_effect.load(Ordering::SeqCst));
    let outbox = Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
    let receipts = outbox.pending(10).unwrap();
    let terminals: Vec<_> = receipts
        .iter()
        .filter(|r| r.status.as_deref() == Some("cancelled"))
        .collect();
    assert_eq!(terminals.len(), 1);
    let acknowledgments: Vec<_> = receipts
        .iter()
        .filter(|r| r.event_type == "control.acknowledged")
        .collect();
    assert_eq!(acknowledgments.len(), usize::from(emergency));
    assert_eq!(receipts.len(), 1 + usize::from(emergency));
    if emergency {
        assert!(acknowledgments[0].action_id.is_none());
        assert!(acknowledgments[0].result.is_none());
    }
    measured.elapsed()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_forever_polls_remote_cancel_while_fifo_dispatch_is_waiting_and_joins_it() {
    assert_runner_control_cancels_pending_dispatch(signed_cancel()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_forever_polls_emergency_stop_while_fifo_dispatch_is_waiting_and_joins_it() {
    assert_runner_control_cancels_pending_dispatch(signed_emergency_stop()).await;
}

#[derive(Default)]
struct IdleEmergency(AtomicUsize);
impl RemoteControl for IdleEmergency {
    fn cancel(&self, _: &str) -> ai_platform_local_node::Result<Outcome> {
        unreachable!()
    }
    fn emergency_stop(&self) -> ai_platform_local_node::Result<Vec<(String, Outcome)>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![])
    }
    fn is_cancelled(&self, _: &str) -> bool {
        true
    }
    fn completion_override(&self, _: &str) -> ai_platform_local_node::Result<Option<Outcome>> {
        Ok(None)
    }
    fn finish_action(&self, _: &str) {}
}
struct IdleDispatcher(Arc<IdleEmergency>);
impl Dispatcher for IdleDispatcher {
    fn dispatch(&mut self, _: &Action, _: &Value) -> ai_platform_local_node::Result<Outcome> {
        panic!("stop must not dispatch an action")
    }
}
impl ControlledDispatcher for IdleDispatcher {
    fn connect(&mut self) -> ai_platform_local_node::Result<()> {
        Ok(())
    }
    fn cancel(&mut self, _: &str) -> ai_platform_local_node::Result<Outcome> {
        unreachable!()
    }
    fn emergency_stop(&mut self) -> ai_platform_local_node::Result<Vec<String>> {
        Ok(vec![])
    }
    fn disconnect(&mut self) -> ai_platform_local_node::Result<Vec<String>> {
        Ok(vec![])
    }
    fn remote_control(&self) -> Arc<dyn RemoteControl> {
        self.0.clone()
    }
}
#[test]
fn idle_emergency_ack_is_durable_idempotent_and_repairs_reserved_before_stop_crash() {
    let temp = TempDir::new();
    let (mut replay, mut outbox) = opened_state(&temp);
    let remote = Arc::new(IdleEmergency::default());
    let mut dispatcher = IdleDispatcher(remote.clone());
    let mut command = signed_emergency_stop();
    command["command_id"] = json!(format!(
        "emergency.{}.{}",
        uuid::Uuid::new_v4(),
        "a".repeat(64)
    ));
    command
        .as_object_mut()
        .unwrap()
        .remove("platform_signature");
    let signature = sign(ai_platform_local_node::canonical(&command).as_bytes());
    command["platform_signature"] = json!(signature);
    let mut payload = command.clone();
    payload
        .as_object_mut()
        .unwrap()
        .remove("platform_signature");
    replay
        .reserve(
            command["command_id"].as_str().unwrap(),
            command["nonce"].as_str().unwrap(),
            &ai_platform_local_node::sha256(ai_platform_local_node::canonical(&payload).as_bytes()),
        )
        .unwrap();
    for _ in 0..2 {
        assert!(
            ai_platform_local_node::runner::control(
                "device-fixture",
                command.clone(),
                &Trusted,
                &mut replay,
                &mut outbox,
                &mut dispatcher
            )
            .unwrap()
        );
    }
    assert_eq!(remote.0.load(Ordering::SeqCst), 2);
    let receipts = outbox.pending(10).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].event_type, "control.acknowledged");
    assert!(
        receipts[0].action_id.is_none()
            && receipts[0].status.is_none()
            && receipts[0].result.is_none()
    );
    drop(outbox);
    assert_eq!(
        Outbox::open(&temp.0.join("outbox.sqlite"))
            .unwrap()
            .pending(10)
            .unwrap()
            .len(),
        1
    );
    let mut changed = command.clone();
    changed["nonce"] = json!("changed");
    changed
        .as_object_mut()
        .unwrap()
        .remove("platform_signature");
    let changed_signature = sign(ai_platform_local_node::canonical(&changed).as_bytes());
    changed["platform_signature"] = json!(changed_signature);
    let mut outbox = Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
    assert!(
        ai_platform_local_node::runner::control(
            "device-fixture",
            changed,
            &Trusted,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .is_err()
    );
    assert_eq!(remote.0.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signed_stream_stop_fixture_p95_under_one_second_during_blocking_dispatch() {
    let mut samples = Vec::new();
    for _ in 0..20 {
        samples
            .push(assert_runner_control_mode(signed_emergency_stop(), StreamMode::Healthy).await);
    }
    samples.sort();
    let p95 = samples[18];
    println!(
        "U-CL-03 signed stream runner fixture n=20 p95_ms={:.3}",
        p95.as_secs_f64() * 1000.0
    );
    assert!(p95 < Duration::from_secs(1));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffering_stream_falls_back_to_poll_before_the_five_second_heartbeat() {
    let elapsed = assert_runner_control_mode(signed_cancel(), StreamMode::Buffered).await;
    assert!(elapsed >= ai_platform_local_node::control_stream::FRAME_TIMEOUT);
    assert!(
        elapsed < Duration::from_millis(4500),
        "buffered stream did not trigger immediate poll fallback: {elapsed:?}"
    );
}

struct PendingStreamTransport(Arc<AtomicBool>);
struct PendingStream(Arc<AtomicBool>);
impl Drop for PendingStream {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl ai_platform_local_node::control_stream::ControlStream for PendingStream {
    fn next(
        &mut self,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = ai_platform_local_node::Result<
                        ai_platform_local_node::control_stream::ControlFrame,
                    >,
                > + Send
                + '_,
        >,
    > {
        Box::pin(std::future::pending())
    }
}
impl RunnerTransport for PendingStreamTransport {
    fn open_control_stream<'a>(
        &'a self,
        _: &'a Credential,
        _: Option<&'a str>,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = ai_platform_local_node::Result<
                        Option<Box<dyn ai_platform_local_node::control_stream::ControlStream>>,
                    >,
                > + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            Ok(Some(Box::new(PendingStream(self.0.clone()))
                as Box<
                    dyn ai_platform_local_node::control_stream::ControlStream,
                >))
        })
    }
    fn exchange<'a>(
        &'a self,
        _: &'a Credential,
        _: &'a ExchangeRequest,
    ) -> Pin<Box<dyn Future<Output = ai_platform_local_node::Result<ExchangeResponse>> + Send + 'a>>
    {
        Box::pin(std::future::pending())
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_pending_stream_read_and_poll_without_waiting_for_network_timeouts() {
    let temp = TempDir::new();
    let (replay, outbox) = opened_state(&temp);
    let dropped = Arc::new(AtomicBool::new(false));
    let (stop, receiver) = tokio::sync::watch::channel(false);
    let runner = run_forever(
        "device-fixture".into(),
        json!({}),
        PendingStreamTransport(dropped.clone()),
        Credential::new(
            "device-fixture".into(),
            "synthetic".into(),
            ai_platform_local_node::now() + 60.0,
        )
        .unwrap(),
        Arc::new(Trusted),
        replay,
        outbox,
        IdleDispatcher(Arc::new(IdleEmergency::default())),
        Duration::from_secs(5),
        receiver,
    );
    let stop_task = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let measured = std::time::Instant::now();
        stop.send(true).unwrap();
        measured
    };
    let (result, measured) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(runner, stop_task)
    })
    .await
    .unwrap();
    result.unwrap();
    // Abort is cooperative; yield once so the outbound-only task drops its read.
    tokio::task::yield_now().await;
    assert!(measured.elapsed() < Duration::from_millis(250));
    assert!(
        dropped.load(Ordering::SeqCst),
        "pending HTTP response was retained after stop"
    );
}
