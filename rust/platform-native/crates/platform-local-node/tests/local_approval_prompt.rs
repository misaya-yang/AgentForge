#![cfg(unix)]
use ai_platform_local_node::{
    Error, Result,
    approvals::OneUseApproval,
    canonical, digest,
    envelope::{Action, PlatformVerifier},
    file_dispatch::{FileControlledDispatcher, LocalApprovalPrompt},
    files::FileService,
    ledger::Ledger,
    now,
    runner::{ControlledDispatcher, Dispatcher, MAX_FILE_WRITE_BYTES, ReplayGuard, claim},
    sha256,
    workspace::Grant,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

const PLATFORM_KEY: &[u8] = b"synthetic-local-approval-platform-key";
const LOCAL_KEY: &[u8] = b"synthetic-local-approval-key";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("local-approval-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sign(key: &[u8], payload: &[u8]) -> String {
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    mac.update(payload);
    format!("{:x}", mac.finalize().into_bytes())
}
fn verify_local(payload: &[u8], signature: &str) -> bool {
    sign(LOCAL_KEY, payload) == signature
}

struct Platform;
impl PlatformVerifier for Platform {
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> bool {
        key_id == "fixture-platform" && sign(PLATFORM_KEY, payload) == signature
    }
}

struct Prompt {
    allow: AtomicBool,
    calls: AtomicUsize,
    last_payload: std::sync::Mutex<Vec<u8>>,
}
impl LocalApprovalPrompt for Prompt {
    fn sign(
        &self,
        payload: &[u8],
        summary: &str,
        device_id: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<String>> {
        assert_eq!(device_id, "fixture-device");
        if cancelled() {
            return Err(Error::Denied("action cancelled"));
        }
        assert!(summary.contains("notes/result.txt"));
        assert!(!summary.contains("synthetic file contents"));
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.last_payload.lock().unwrap() = payload.to_vec();
        if self.allow.load(Ordering::SeqCst) {
            Ok(Some(sign(LOCAL_KEY, payload)))
        } else {
            Ok(None)
        }
    }
}

fn action_and_arguments(content: &[u8]) -> (Action, Value) {
    assert!(content.len() <= MAX_FILE_WRITE_BYTES);
    let content_hash = sha256(content);
    let intent = json!({
        "grant_id":"fixture-grant",
        "relative_path":"notes/result.txt",
        "content_sha256":content_hash,
        "expected_hash":null
    });
    let args = json!({
        "grant_id":"fixture-grant",
        "relative_path":"notes/result.txt",
        "content_sha256":content_hash,
        "expected_hash":null,
        "content_base64":URL_SAFE_NO_PAD.encode(content)
    });
    let mut action: Action = serde_json::from_value(json!({
        "action_id":"fixture-action",
        "idempotency_key":"fixture-idempotency",
        "tenant_id":"fixture-tenant",
        "user_id":"fixture-user",
        "session_id":"fixture-session",
        "run_id":"fixture-run",
        "agent_id":"fixture-agent",
        "agent_version":"fixture-agent-v1",
        "call_id":"fixture-call",
        "device_id":"fixture-device",
        "envelope_version":1,
        "capability":"file.write",
        "tool_name":"local_file_write",
        "operation":"file.write",
        "capability_lease_id":"fixture-grant",
        "resource_refs":["fixture-grant","notes/result.txt"],
        "arguments_digest":digest(&intent),
        "target_snapshot_digest":"missing",
        "policy_snapshot_digest":"fixture-policy",
        "nonce":"fixture-action-nonce",
        "issued_at":now()-1.0,
        "expires_at":now()+300.0,
        "platform_key_id":"fixture-platform",
        "platform_signature":"pending",
        "approval":null,
        "trace_context":null
    }))
    .unwrap();
    action.platform_signature = sign(PLATFORM_KEY, canonical(&action.signed_payload()).as_bytes());
    (action, args)
}

fn dispatcher(
    temp: &Temp,
    allow: bool,
) -> (
    FileControlledDispatcher<Platform, OneUseApproval<fn(&[u8], &str) -> bool>>,
    Arc<Prompt>,
    PathBuf,
) {
    let root = temp.0.join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("notes")).unwrap();
    let grant = Grant::issue(
        "fixture-grant".into(),
        &root,
        "fixture-tenant".into(),
        "fixture-user".into(),
        &["write"],
        None,
    )
    .unwrap();
    let grants = HashMap::from([("fixture-grant".into(), grant)]);
    let approvals = OneUseApproval::open(
        "fixture-device".into(),
        &temp.0.join("approvals.sqlite"),
        verify_local as fn(&[u8], &str) -> bool,
    )
    .unwrap();
    let prompt = Arc::new(Prompt {
        allow: AtomicBool::new(allow),
        calls: AtomicUsize::new(0),
        last_payload: std::sync::Mutex::new(Vec::new()),
    });
    let dispatcher = FileControlledDispatcher::new(
        grants,
        Platform,
        approvals,
        FileService::new(temp.0.join("rollback")).unwrap(),
        Ledger::open(&temp.0.join("ledger.sqlite")).unwrap(),
    )
    .unwrap()
    .with_approval_prompt(prompt.clone());
    (dispatcher, prompt, root.join("notes/result.txt"))
}

#[test]
fn owner_prompt_signature_is_consumed_before_write_and_replay_does_not_prompt_again() {
    let temp = Temp::new();
    let (mut dispatcher, prompt, target) = dispatcher(&temp, true);
    let content = b"synthetic file contents";
    let (action, args) = action_and_arguments(content);
    let first = dispatcher.dispatch(&action, &args).unwrap();
    assert_eq!(first.status, "succeeded");
    assert_eq!(std::fs::read(&target).unwrap(), content);
    let approval: Value = serde_json::from_slice(&prompt.last_payload.lock().unwrap()).unwrap();
    assert_eq!(approval["action_id"], action.action_id);
    assert_eq!(approval["target_snapshot_digest"], "missing");
    assert_eq!(approval["arguments_digest"], action.arguments_digest);
    assert!(approval.get("local_signature").is_none());

    // A committed action replays its durable receipt without another prompt
    // or a second filesystem publication.
    let second = dispatcher.dispatch(&action, &args).unwrap();
    assert_eq!(second.status, "succeeded");
    assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
    assert_eq!(std::fs::read(&target).unwrap(), content);
}

#[test]
fn local_owner_denial_is_terminal_without_file_effect_or_prompt_replay() {
    let temp = Temp::new();
    let (mut dispatcher, prompt, target) = dispatcher(&temp, false);
    let (action, args) = action_and_arguments(b"denied fixture");
    let outcome = dispatcher.dispatch(&action, &args).unwrap();
    assert_eq!(outcome.status, "failed");
    assert_eq!(outcome.error_code.as_deref(), Some("approval_denied"));
    assert!(!target.exists());

    // The ledger reservation makes retry fail closed; it cannot ask again and
    // silently turn the one-use remote command into a fresh local approval.
    assert!(matches!(
        dispatcher.dispatch(&action, &args),
        Err(Error::Conflict)
    ));
    assert_eq!(prompt.calls.load(Ordering::SeqCst), 1);
    assert!(!target.exists());
}

struct WaitingPrompt(std::sync::mpsc::Sender<()>);
impl LocalApprovalPrompt for WaitingPrompt {
    fn sign(
        &self,
        _: &[u8],
        _: &str,
        _: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<String>> {
        let _ = self.0.send(());
        let start = std::time::Instant::now();
        while !cancelled() && start.elapsed() < std::time::Duration::from_secs(10) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if cancelled() {
            Err(Error::Denied("action cancelled"))
        } else {
            Ok(Some(sign(LOCAL_KEY, b"unexpected prompt completion")))
        }
    }
}

#[test]
fn remote_cancel_interrupts_owner_prompt_and_tombstone_prevents_replay() {
    let temp = Temp::new();
    let root = temp.0.join("workspace");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("notes")).unwrap();
    let target = root.join("notes/result.txt");
    let grant = Grant::issue(
        "fixture-grant".into(),
        &root,
        "fixture-tenant".into(),
        "fixture-user".into(),
        &["write"],
        None,
    )
    .unwrap();
    let approvals = OneUseApproval::open(
        "fixture-device".into(),
        &temp.0.join("approvals.sqlite"),
        verify_local as fn(&[u8], &str) -> bool,
    )
    .unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let prompt = Arc::new(WaitingPrompt(started_tx));
    let mut dispatcher = FileControlledDispatcher::new(
        HashMap::from([("fixture-grant".into(), grant)]),
        Platform,
        approvals,
        FileService::new(temp.0.join("rollback")).unwrap(),
        Ledger::open(&temp.0.join("ledger.sqlite")).unwrap(),
    )
    .unwrap()
    .with_approval_prompt(prompt);
    let control = dispatcher.remote_control();
    let (action, arguments) = action_and_arguments(b"must not be written");
    let action_copy = action.clone();
    let action_id = action.action_id.clone();
    let worker = std::thread::spawn(move || {
        let outcome = dispatcher.dispatch(&action_copy, &arguments).unwrap();
        (dispatcher, outcome)
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();

    let cancelled = control.cancel(&action_id).unwrap();
    assert_eq!(cancelled.status, "cancelled");
    let (mut dispatcher, outcome) = worker.join().unwrap();
    assert_eq!(outcome.status, "cancelled");
    assert!(!target.exists());

    let mut replay = ReplayGuard::open(&temp.0.join("replay.sqlite")).unwrap();
    replay.cancel_action(&action_id).unwrap();
    let mut outbox =
        ai_platform_local_node::outbox::Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
    let command = json!({
        "kind":"claim",
        "command_id":"fixture-claim-command",
        "action":action,
        "normalized_arguments":action_and_arguments(b"must not be written").1
    });
    assert!(
        claim(
            "fixture-device",
            command,
            &Platform,
            &mut replay,
            &mut outbox,
            &mut dispatcher
        )
        .unwrap()
    );
    assert_eq!(
        outbox.pending(10).unwrap()[0].status.as_deref(),
        Some("cancelled")
    );
    assert!(!target.exists());
}
