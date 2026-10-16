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
struct PumpTransport {
    calls: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
    claim: Value,
    control_command: Value,
}
impl RunnerTransport for PumpTransport {
    fn exchange<'a>(
        &'a self,
        _: &'a Credential,
        _: &'a ExchangeRequest,
    ) -> Pin<Box<dyn Future<Output = ai_platform_local_node::Result<ExchangeResponse>> + Send + 'a>>
    {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        let started = self.started.clone();
        let claim = self.claim.clone();
        let control_command = self.control_command.clone();
        Box::pin(async move {
            let commands = match call {
                0 => vec![claim],
                1 => {
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
        Duration::from_secs(3),
        run_forever(
            "device-fixture".into(),
            json!({"status":"ready"}),
            transport,
            credential,
            Arc::new(Trusted),
            replay,
            outbox,
            dispatcher,
            Duration::from_millis(250),
            stop_receiver,
        ),
    )
    .await;
    assert!(result.is_ok(), "poller remained blocked behind dispatcher");
    result.unwrap().unwrap();
    assert!(*state.cancelled.lock().unwrap());
    assert_eq!(dispatches.load(Ordering::SeqCst), 1, "claim was replayed");
    assert!(calls.load(Ordering::SeqCst) >= 2, "control was not polled");
    assert!(!state.side_effect.load(Ordering::SeqCst));
    let outbox = Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
    let receipts = outbox.pending(10).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].status.as_deref(), Some("cancelled"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_forever_polls_remote_cancel_while_fifo_dispatch_is_waiting_and_joins_it() {
    assert_runner_control_cancels_pending_dispatch(signed_cancel()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn run_forever_polls_emergency_stop_while_fifo_dispatch_is_waiting_and_joins_it() {
    assert_runner_control_cancels_pending_dispatch(signed_emergency_stop()).await;
}
