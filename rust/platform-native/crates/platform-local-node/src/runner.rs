//! Durable inbound claim replay guard and terminal receipt orchestration.
use crate::{
    Error, Result, digest,
    envelope::{Action, PlatformVerifier},
    outbox::{Outbox, Receipt},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;
use std::sync::{
    Arc, Mutex, MutexGuard,
    atomic::{AtomicBool, Ordering},
};
use std::{collections::HashMap, future::Future, pin::Pin};
pub const MAX_FILE_WRITE_BYTES: usize = 512 * 1024;
const MAX_FILE_WRITE_BASE64_BYTES: usize = 699_052;
const MAX_HEARTBEAT_REQUEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_QUEUED_CLAIMS: usize = 8;
const MAX_CANCELLED_ACTION_SIGNALS: usize = 4096;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    kind: String,
    command_id: String,
    action: Action,
    normalized_arguments: Value,
}
fn validate_claim(device_id: &str, raw: Value, verifier: &dyn PlatformVerifier) -> Result<Claim> {
    let c: Claim = serde_json::from_value(raw).map_err(|_| Error::Protocol)?;
    if c.kind != "claim"
        || c.command_id.is_empty()
        || c.command_id.len() > 512
        || c.command_id.chars().any(|c| c < ' ')
        || !c.normalized_arguments.is_object()
        || c.action.device_id != device_id
    {
        return Err(Error::Protocol);
    }
    c.action.verify(verifier, crate::now())?;
    let arguments_digest = if c.action.operation == "file.write" {
        let (intent, _) = file_write_intent(&c.normalized_arguments)?;
        digest(&intent)
    } else {
        digest(&c.normalized_arguments)
    };
    if arguments_digest != c.action.arguments_digest {
        return Err(Error::Denied("claim arguments changed"));
    }
    Ok(c)
}
pub struct ReplayGuard {
    db: Connection,
}
impl ReplayGuard {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS consumed_commands(command_id TEXT PRIMARY KEY,nonce TEXT UNIQUE NOT NULL,payload_digest TEXT NOT NULL,consumed_at REAL NOT NULL); CREATE TABLE IF NOT EXISTS cancelled_actions(action_id TEXT PRIMARY KEY,cancelled_at REAL NOT NULL); CREATE TABLE IF NOT EXISTS control_stream_cursor(device_id TEXT PRIMARY KEY,cursor TEXT NOT NULL);")?;
        Ok(Self { db })
    }
    pub fn stream_cursor(&self, device: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT cursor FROM control_stream_cursor WHERE device_id=?",
                [device],
                |r| r.get(0),
            )
            .optional()?)
    }
    fn checkpoint_stream(&mut self, device: &str, cursor: &str) -> Result<()> {
        self.db.execute("INSERT INTO control_stream_cursor(device_id,cursor) VALUES(?,?) ON CONFLICT(device_id) DO UPDATE SET cursor=excluded.cursor", params![device,cursor])?;
        Ok(())
    }
    pub fn reserve(&mut self, command: &str, nonce: &str, digest: &str) -> Result<bool> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing:Option<(String,String,String)>=tx.query_row("SELECT command_id,nonce,payload_digest FROM consumed_commands WHERE command_id=? OR nonce=?",params![command,nonce],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if let Some((c, n, d)) = existing {
            if c != command || n != nonce || d != digest {
                return Err(Error::Conflict);
            }
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO consumed_commands VALUES(?,?,?,?)",
            params![command, nonce, digest, crate::now()],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn cancel_action(&mut self, action_id: &str) -> Result<()> {
        if action_id.is_empty() || action_id.len() > 512 || action_id.chars().any(|c| c < ' ') {
            return Err(Error::Protocol);
        }
        self.db.execute(
            "INSERT OR IGNORE INTO cancelled_actions(action_id,cancelled_at) VALUES(?,?)",
            params![action_id, crate::now()],
        )?;
        Ok(())
    }
    pub fn action_cancelled(&self, action_id: &str) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM cancelled_actions WHERE action_id=?)",
            [action_id],
            |r| r.get(0),
        )?)
    }
}
#[derive(Clone)]
pub struct Outcome {
    pub status: String,
    pub result: Value,
    pub error_code: Option<String>,
}
/// Implementations must verify grants and approval at the point of local I/O.
pub trait Dispatcher {
    fn dispatch(&mut self, action: &Action, arguments: &Value) -> Result<Outcome>;
}
#[derive(Clone, Default)]
pub struct ActionSignals {
    inner: Arc<ActionSignalState>,
}
#[derive(Default)]
struct ActionSignalState {
    cancelled: Mutex<HashSet<String>>,
    emergency: AtomicBool,
    effect_gate: Mutex<()>,
}
impl ActionSignals {
    pub fn cancel(&self, action_id: &str) {
        if let Ok(mut cancelled) = self.inner.cancelled.lock() {
            if !cancelled.contains(action_id) && cancelled.len() >= MAX_CANCELLED_ACTION_SIGNALS {
                self.inner.emergency.store(true, Ordering::Release);
                return;
            }
            cancelled.insert(action_id.to_owned());
        }
    }
    pub fn finish_action(&self, action_id: &str) {
        if let Ok(mut cancelled) = self.inner.cancelled.lock() {
            cancelled.remove(action_id);
        }
    }
    pub fn emergency(&self) {
        self.inner.emergency.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self, action_id: &str) -> bool {
        self.inner.emergency.load(Ordering::Acquire)
            || self
                .inner
                .cancelled
                .lock()
                .map(|set| set.contains(action_id))
                .unwrap_or(true)
    }
    pub fn lock_effect(&self) -> Result<MutexGuard<'_, ()>> {
        self.inner.effect_gate.lock().map_err(|_| Error::Storage)
    }
}
pub trait RemoteControl: Send + Sync {
    fn cancel(&self, action_id: &str) -> Result<Outcome>;
    fn emergency_stop(&self) -> Result<Vec<(String, Outcome)>>;
    fn is_cancelled(&self, action_id: &str) -> bool;
    fn completion_override(&self, action_id: &str) -> Result<Option<Outcome>>;
    fn finish_action(&self, action_id: &str);
}
pub trait RunnerTransport: Send + Sync {
    fn open_control_stream<'a>(
        &'a self,
        _credential: &'a crate::transport::Credential,
        _after_cursor: Option<&'a str>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<Box<dyn crate::control_stream::ControlStream>>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async { Ok(None) })
    }
    fn exchange<'a>(
        &'a self,
        credential: &'a crate::transport::Credential,
        request: &'a crate::transport::ExchangeRequest,
    ) -> Pin<Box<dyn Future<Output = Result<crate::transport::ExchangeResponse>> + Send + 'a>>;
}
impl RunnerTransport for crate::transport::HttpsTransport {
    fn open_control_stream<'a>(
        &'a self,
        credential: &'a crate::transport::Credential,
        after_cursor: Option<&'a str>,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<Box<dyn crate::control_stream::ControlStream>>>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(async move {
            match self.open_control_stream(credential, after_cursor).await {
                Err(Error::Denied("control stream unsupported")) => Ok(None),
                result => result.map(Some),
            }
        })
    }
    fn exchange<'a>(
        &'a self,
        credential: &'a crate::transport::Credential,
        request: &'a crate::transport::ExchangeRequest,
    ) -> Pin<Box<dyn Future<Output = Result<crate::transport::ExchangeResponse>> + Send + 'a>> {
        Box::pin(crate::transport::HttpsTransport::exchange(
            self, credential, request,
        ))
    }
}
/// A write carries bytes inside the existing signed claim object. The action's
/// intent digest excludes only that payload field and includes its SHA-256.
pub fn file_write_intent(arguments: &Value) -> Result<(Value, Vec<u8>)> {
    let object = arguments.as_object().ok_or(Error::Protocol)?;
    let fields = [
        "grant_id",
        "relative_path",
        "content_sha256",
        "expected_hash",
        "content_base64",
    ];
    if object.len() != fields.len() || fields.iter().any(|field| !object.contains_key(*field)) {
        return Err(Error::Protocol);
    }
    let grant = arguments["grant_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 200)
        .ok_or(Error::Protocol)?;
    let path = arguments["relative_path"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 4096)
        .ok_or(Error::Protocol)?;
    let content_hash = arguments["content_sha256"]
        .as_str()
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or(Error::Protocol)?;
    let expected_hash = match &arguments["expected_hash"] {
        Value::Null => Value::Null,
        Value::String(value)
            if value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Value::String(value.clone())
        }
        _ => return Err(Error::Protocol),
    };
    let encoded = arguments["content_base64"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= MAX_FILE_WRITE_BASE64_BYTES && s.is_ascii())
        .ok_or(Error::Protocol)?;
    let content = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| Error::Protocol)?;
    if content.len() > MAX_FILE_WRITE_BYTES || crate::sha256(&content) != content_hash {
        return Err(Error::Denied("write content digest mismatch"));
    }
    let intent = json!({"grant_id":grant,"relative_path":path,"content_sha256":content_hash,"expected_hash":expected_hash});
    Ok((intent, content))
}
fn bounded_exchange_request(
    device: &str,
    doctor: Value,
    outbox: &Outbox,
    budget: usize,
) -> Result<(crate::transport::ExchangeRequest, u64, u64)> {
    let pending = outbox.pending(200)?;
    let low = pending
        .first()
        .map_or(outbox.acked_through()?, |receipt| receipt.sequence - 1);
    let sent_at = crate::now();
    let mut selected = Vec::new();
    for receipt in pending {
        let mut candidate = selected.clone();
        candidate.push(receipt);
        let request = crate::transport::ExchangeRequest {
            protocol_version: crate::transport::PROTOCOL_VERSION.into(),
            kind: "heartbeat".into(),
            device_id: device.into(),
            receipts: candidate.clone(),
            doctor: doctor.clone(),
            sent_at,
        };
        let encoded = serde_json::to_vec(&request).map_err(|_| Error::Protocol)?;
        if encoded.len() > budget {
            if selected.is_empty() {
                return Err(Error::Protocol);
            }
            break;
        }
        selected = candidate;
    }
    let high = selected.last().map_or(low, |receipt| receipt.sequence);
    let request = crate::transport::ExchangeRequest {
        protocol_version: crate::transport::PROTOCOL_VERSION.into(),
        kind: "heartbeat".into(),
        device_id: device.into(),
        receipts: selected,
        doctor,
        sent_at,
    };
    if serde_json::to_vec(&request)
        .map_err(|_| Error::Protocol)?
        .len()
        > budget
    {
        return Err(Error::Protocol);
    }
    Ok((request, low, high))
}
pub fn claim(
    device_id: &str,
    raw: Value,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    dispatcher: &mut dyn Dispatcher,
) -> Result<bool> {
    let c = validate_claim(device_id, raw, verifier)?;
    let event_id = format!("command.{}.{}", c.command_id, c.action.action_id);
    let action_digest = digest(&c.action.signed_payload());
    if replay.action_cancelled(&c.action.action_id)? {
        let reserved = replay.reserve(&c.command_id, &c.action.nonce, &action_digest)?;
        if outbox.has_terminal_action(&c.action.action_id)? {
            return Ok(reserved);
        }
        let receipt = Receipt {
            sequence: 0,
            event_id,
            action_id: Some(c.action.action_id),
            event_type: "action.cancelled".into(),
            status: Some("cancelled".into()),
            occurred_at: crate::now(),
            result_digest: None,
            error_code: None,
            summary: None,
            result: Some(json!({"cancelled":true,"replay_allowed":false})),
        };
        return match outbox.append(receipt, false) {
            Ok(_) => Ok(reserved),
            Err(Error::Conflict) => Ok(false),
            Err(error) => Err(error),
        };
    }
    if !replay.reserve(&c.command_id, &c.action.nonce, &action_digest)? {
        // A consumed reservation without its terminal receipt is never safe to
        // dispatch again. Preserve an already committed receipt or append unknown.
        if outbox.has_terminal_action(&c.action.action_id)? {
            return Ok(false);
        }
        let receipt = Receipt {
            sequence: 0,
            event_id,
            action_id: Some(c.action.action_id),
            event_type: "action.unknown".into(),
            status: Some("unknown".into()),
            occurred_at: crate::now(),
            result_digest: None,
            error_code: Some("command_outcome_missing".into()),
            summary: None,
            result: Some(json!({"replay_allowed":false})),
        };
        return match outbox.append(receipt, false) {
            Ok(_) => Ok(false),
            Err(Error::Conflict) => Ok(false),
            Err(error) => Err(error),
        };
    }
    let outcome = dispatcher.dispatch(&c.action, &c.normalized_arguments);
    let (mut status, mut result, mut error_code) = match &outcome {
        Ok(o) => (o.status.clone(), o.result.clone(), o.error_code.clone()),
        Err(_) => (
            "unknown".into(),
            json!({"replay_allowed":false}),
            Some("local_dispatch_unknown".into()),
        ),
    };
    if !crate::ledger::terminal(&status) {
        return Err(Error::Protocol);
    }
    let mut retain = status == "succeeded"
        && matches!(
            c.action.operation.as_str(),
            "file.list" | "file.read" | "file.hash" | "file.search" | "file.watch" | "file.write"
        );
    if crate::canonical(&result).len() > 2 * 1024 * 1024 {
        if status == "succeeded" {
            status = if retain { "failed" } else { "unknown" }.into();
        }
        result = json!({"replay_allowed":false});
        error_code = Some("local_result_too_large".into());
        retain = false;
    }
    outbox.append(
        Receipt {
            sequence: 0,
            event_id,
            action_id: Some(c.action.action_id),
            event_type: format!("action.{status}"),
            status: Some(status),
            occurred_at: crate::now(),
            result_digest: None,
            error_code,
            summary: None,
            result: Some(result),
        },
        retain,
    )?;
    outcome.map(|_| true)
}

fn append_disconnect_receipts(outbox: &mut Outbox, ids: Vec<String>) -> Result<()> {
    for id in ids {
        if outbox.has_terminal_action(&id)? {
            continue;
        }
        let receipt = Receipt {
            sequence: 0,
            event_id: format!("command.disconnect.{id}"),
            action_id: Some(id),
            event_type: "action.unknown".into(),
            status: Some("unknown".into()),
            occurred_at: crate::now(),
            result_digest: None,
            error_code: Some("transport_disconnected".into()),
            summary: None,
            result: Some(json!({"replay_allowed":false})),
        };
        let _ = outbox.append(receipt, false)?;
    }
    Ok(())
}
fn stop_dispatcher(outbox: &mut Outbox, dispatcher: &mut dyn ControlledDispatcher) -> Result<()> {
    let emergency = dispatcher.remote_control().emergency_stop();
    let disconnected = dispatcher.disconnect();
    let emergency_receipts = match &emergency {
        Ok(actions) => actions.iter().try_for_each(|(id, outcome)| {
            append_control_outcome(
                outbox,
                format!("command.shutdown.{id}"),
                id.clone(),
                outcome.clone(),
            )
        }),
        Err(_) => Ok(()),
    };
    let disconnect_receipts = match &disconnected {
        Ok(ids) => append_disconnect_receipts(outbox, ids.clone()),
        Err(_) => Ok(()),
    };
    emergency?;
    disconnected?;
    emergency_receipts?;
    disconnect_receipts
}
struct DispatchCompletion {
    receipt: Receipt,
    retain_result: bool,
}
fn dispatch_claim(
    claim: Claim,
    dispatcher: &mut dyn ControlledDispatcher,
    control: &dyn RemoteControl,
) -> DispatchCompletion {
    let action_id = claim.action.action_id.clone();
    let initial = if control.is_cancelled(&action_id) {
        Ok(Outcome {
            status: "cancelled".into(),
            result: json!({"cancelled":true,"replay_allowed":false}),
            error_code: None,
        })
    } else {
        dispatcher.dispatch(&claim.action, &claim.normalized_arguments)
    };
    let outcome = match control.completion_override(&action_id) {
        Ok(Some(outcome)) => outcome,
        Ok(None) => initial.unwrap_or(Outcome {
            status: "unknown".into(),
            result: json!({"replay_allowed":false}),
            error_code: Some("local_dispatch_unknown".into()),
        }),
        Err(_) => Outcome {
            status: "unknown".into(),
            result: json!({"replay_allowed":false}),
            error_code: Some("local_control_state_unknown".into()),
        },
    };
    let (mut status, mut result, mut error_code) =
        (outcome.status, outcome.result, outcome.error_code);
    if !crate::ledger::terminal(&status) {
        status = "unknown".into();
        result = json!({"replay_allowed":false});
        error_code = Some("local_dispatch_nonterminal".into());
    }
    let mut retain_result = status == "succeeded"
        && matches!(
            claim.action.operation.as_str(),
            "file.list" | "file.read" | "file.hash" | "file.search" | "file.watch" | "file.write"
        );
    if crate::canonical(&result).len() > 2 * 1024 * 1024 {
        if status == "succeeded" {
            status = if retain_result { "failed" } else { "unknown" }.into();
        }
        result = json!({"replay_allowed":false});
        error_code = Some("local_result_too_large".into());
        retain_result = false;
    }
    let completion = DispatchCompletion {
        receipt: Receipt {
            sequence: 0,
            event_id: format!("command.{}.{}", claim.command_id, action_id),
            action_id: Some(action_id.clone()),
            event_type: format!("action.{status}"),
            status: Some(status),
            occurred_at: crate::now(),
            result_digest: None,
            error_code,
            summary: None,
            result: Some(result),
        },
        retain_result,
    };
    control.finish_action(&action_id);
    completion
}
fn accept_claim(
    device: &str,
    raw: Value,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    control: &dyn RemoteControl,
) -> Result<Option<Claim>> {
    let claim = validate_claim(device, raw, verifier)?;
    let action_id = claim.action.action_id.clone();
    let action_digest = digest(&claim.action.signed_payload());
    if replay.action_cancelled(&action_id)? || control.is_cancelled(&action_id) {
        let reserved = replay.reserve(&claim.command_id, &claim.action.nonce, &action_digest)?;
        if !outbox.has_terminal_action(&action_id)? {
            outbox.append(
                Receipt {
                    sequence: 0,
                    event_id: format!("command.{}.{}", claim.command_id, action_id),
                    action_id: Some(action_id.clone()),
                    event_type: "action.cancelled".into(),
                    status: Some("cancelled".into()),
                    occurred_at: crate::now(),
                    result_digest: None,
                    error_code: None,
                    summary: None,
                    result: Some(json!({"cancelled":true,"replay_allowed":false})),
                },
                false,
            )?;
        }
        control.finish_action(&action_id);
        let _ = reserved;
        return Ok(None);
    }
    if !replay.reserve(&claim.command_id, &claim.action.nonce, &action_digest)? {
        if !outbox.has_terminal_action(&action_id)? {
            outbox.append(
                Receipt {
                    sequence: 0,
                    event_id: format!("command.{}.{}", claim.command_id, action_id),
                    action_id: Some(action_id),
                    event_type: "action.unknown".into(),
                    status: Some("unknown".into()),
                    occurred_at: crate::now(),
                    result_digest: None,
                    error_code: Some("command_outcome_missing".into()),
                    summary: None,
                    result: Some(json!({"replay_allowed":false})),
                },
                false,
            )?;
        }
        return Ok(None);
    }
    if outbox.has_terminal_action(&action_id)? {
        return Ok(None);
    }
    Ok(Some(claim))
}
fn persist_completion(outbox: &mut Outbox, completion: DispatchCompletion) -> Result<()> {
    if let Some(action_id) = completion.receipt.action_id.as_deref() {
        if outbox.has_terminal_action(action_id)? {
            return Ok(());
        }
    }
    match outbox.append(completion.receipt, completion.retain_result) {
        Ok(_) => Ok(()),
        Err(Error::Conflict) => Ok(()),
        Err(error) => Err(error),
    }
}
fn append_control_outcome(
    outbox: &mut Outbox,
    event_id: String,
    action_id: String,
    outcome: Outcome,
) -> Result<()> {
    if outbox.has_terminal_action(&action_id)? {
        return Ok(());
    }
    if outcome.status == "failed" && outcome.error_code.as_deref() == Some("action_not_active") {
        return Ok(());
    }
    if !crate::ledger::terminal(&outcome.status) {
        return Err(Error::Protocol);
    }
    outbox.append(
        Receipt {
            sequence: 0,
            event_id,
            action_id: Some(action_id),
            event_type: format!("action.{}", outcome.status),
            status: Some(outcome.status),
            occurred_at: crate::now(),
            result_digest: None,
            error_code: outcome.error_code,
            summary: None,
            result: Some(outcome.result),
        },
        false,
    )?;
    Ok(())
}
fn control_with_remote(
    device: &str,
    mut raw: Value,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    control: &dyn RemoteControl,
    inflight: &HashMap<String, String>,
) -> Result<bool> {
    let object = raw.as_object().ok_or(Error::Protocol)?;
    let kind = object
        .get("kind")
        .and_then(Value::as_str)
        .ok_or(Error::Protocol)?
        .to_owned();
    let mut fields = vec![
        "kind",
        "command_id",
        "device_id",
        "nonce",
        "issued_at",
        "expires_at",
        "platform_key_id",
        "platform_signature",
    ];
    if kind == "cancel" {
        fields.push("action_id");
    } else if kind != "emergency_stop" {
        return Err(Error::Protocol);
    }
    if object.len() != fields.len() || fields.iter().any(|k| !object.contains_key(*k)) {
        return Err(Error::Protocol);
    }
    let field = |key: &str| {
        object
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 4096 && !s.chars().any(|c| c < ' '))
            .map(str::to_owned)
            .ok_or(Error::Protocol)
    };
    let command = field("command_id")?;
    let nonce = field("nonce")?;
    let key = field("platform_key_id")?;
    let signature = field("platform_signature")?;
    if field("device_id")? != device {
        return Err(Error::Denied("control device binding"));
    }
    let issued = raw["issued_at"].as_f64().ok_or(Error::Protocol)?;
    let expires = raw["expires_at"].as_f64().ok_or(Error::Protocol)?;
    if !issued.is_finite()
        || !expires.is_finite()
        || issued <= 0.
        || issued > crate::now() + 30.
        || expires <= issued
        || expires - issued > 600.
        || crate::now() >= expires
    {
        return Err(Error::Denied("control lifetime"));
    }
    let action = if kind == "cancel" {
        Some(field("action_id")?)
    } else {
        None
    };
    raw.as_object_mut().unwrap().remove("platform_signature");
    let payload = crate::canonical(&raw);
    if !verifier.verify(&key, payload.as_bytes(), &signature) {
        return Err(Error::Denied("control signature"));
    }
    let fresh = replay.reserve(&command, &nonce, &crate::sha256(payload.as_bytes()))?;
    if !fresh && kind != "emergency_stop" {
        return Ok(false);
    }
    // Reapplying an authenticated stop is safe and repairs a crash after nonce
    // reservation but before the stop/outbox commit. Claims remain one-use.

    if let Some(id) = action {
        replay.cancel_action(&id)?;
        let active = inflight.contains_key(&id);
        let outcome = control.cancel(&id)?;
        append_control_outcome(
            outbox,
            format!("command.{command}.{id}"),
            id.clone(),
            outcome,
        )?;
        if !active {
            control.finish_action(&id);
        }
    } else {
        let mut outcomes = control.emergency_stop()?;
        let mut handled = outcomes
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<HashSet<_>>();
        for id in inflight.keys() {
            if handled.insert(id.clone()) {
                outcomes.push((id.clone(), control.cancel(id)?));
            }
        }
        outcomes.sort_by(|left, right| left.0.cmp(&right.0));
        for (id, outcome) in outcomes {
            append_control_outcome(
                outbox,
                format!("command.{command}.{id}"),
                id.clone(),
                outcome,
            )?;
            if !inflight.contains_key(&id) {
                control.finish_action(&id);
            }
        }
    }
    let delivery_parts: Vec<_> = command.split('.').collect();
    let durable_owner_stop = delivery_parts.len() == 3
        && delivery_parts[0] == "emergency"
        && uuid::Uuid::parse_str(delivery_parts[1]).is_ok()
        && delivery_parts[2].len() == 64
        && delivery_parts[2]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    // Only the durable-owner extension emits the new action-less receipt.
    // Existing signed v2 controls keep their previous receipt contract.
    if kind == "emergency_stop" && durable_owner_stop {
        outbox.append(
            crate::outbox::Receipt {
                sequence: 0,
                event_id: format!("control.ack.{command}"),
                action_id: None,
                event_type: "control.acknowledged".into(),
                status: None,
                occurred_at: crate::now(),
                result_digest: None,
                error_code: None,
                summary: None,
                result: None,
            },
            false,
        )?;
    }
    Ok(true)
}
enum StreamEvent {
    Frame(crate::control_stream::ControlFrame),
    Unavailable,
}
// This task owns only outbound I/O. It cannot execute commands, advance durable
// cursors or approve work. Dropping the task abort handle cancels a blocked read.
async fn read_control_stream<T: RunnerTransport + 'static>(
    transport: Arc<T>,
    credential: Arc<crate::transport::Credential>,
    verifier: Arc<dyn PlatformVerifier>,
    mut cursor: tokio::sync::watch::Receiver<Option<String>>,
    events: tokio::sync::mpsc::Sender<StreamEvent>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    use crate::control_stream::{FRAME_TIMEOUT, STREAM_RETRY};
    loop {
        if *stop.borrow() {
            return;
        }
        let after = cursor.borrow_and_update().clone();
        let opened = tokio::select! {
            _ = stop.changed() => return,
            opened = tokio::time::timeout(FRAME_TIMEOUT, transport.open_control_stream(&credential, after.as_deref())) => opened,
        };
        match opened {
            Ok(Ok(None)) => return, // Existing custom transports retain polling.
            Ok(Ok(Some(mut stream))) => {
                let Ok(mut sequence) = crate::control_stream::resume_sequence(after.as_deref())
                else {
                    return;
                };
                let mut stream_id = None;
                loop {
                    let next = tokio::select! {
                        _ = stop.changed() => return,
                        frame = tokio::time::timeout(FRAME_TIMEOUT, stream.next()) => frame,
                    };
                    let frame = match next {
                        Ok(Ok(frame))
                            if frame.device_id == credential.device_id
                                && frame.verify(verifier.as_ref()).is_ok()
                                && sequence.checked_add(1) == Some(frame.sequence)
                                && stream_id.as_ref().is_none_or(|id| id == &frame.stream_id) =>
                        {
                            sequence = frame.sequence;
                            stream_id = Some(frame.stream_id.clone());
                            frame
                        }
                        _ => break,
                    };
                    tokio::select! {
                        _ = stop.changed() => return,
                        sent = events.send(StreamEvent::Frame(frame)) => if sent.is_err() { return; },
                    }
                }
            }
            _ => {}
        }
        tokio::select! {
            _ = stop.changed() => return,
            sent = events.send(StreamEvent::Unavailable) => if sent.is_err() { return; },
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(STREAM_RETRY) => {},
        }
    }
}
struct StreamTask(tokio::task::JoinHandle<()>);
impl StreamTask {
    async fn cancel(&mut self) {
        self.0.abort();
        let _ = (&mut self.0).await;
    }
}
impl Drop for StreamTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub async fn run_forever<T: RunnerTransport + 'static, D: ControlledDispatcher + 'static>(
    device: String,
    doctor: Value,
    transport: T,
    credential: crate::transport::Credential,
    verifier: Arc<dyn PlatformVerifier>,
    mut replay: ReplayGuard,
    mut outbox: Outbox,
    dispatcher: D,
    heartbeat: std::time::Duration,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    if heartbeat < std::time::Duration::from_millis(250)
        || heartbeat > std::time::Duration::from_secs(300)
    {
        return Err(Error::Denied("heartbeat interval"));
    }
    let transport = Arc::new(transport);
    let credential = Arc::new(credential);
    let (cursor_tx, cursor_rx) = tokio::sync::watch::channel(replay.stream_cursor(&device)?);
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::channel(8);
    let mut stream_task = StreamTask(tokio::spawn(read_control_stream(
        transport.clone(),
        credential.clone(),
        verifier.clone(),
        cursor_rx,
        stream_tx,
        stop.clone(),
    )));
    let control = dispatcher.remote_control();
    let (work_tx, mut work_rx) = tokio::sync::mpsc::channel::<Claim>(MAX_QUEUED_CLAIMS);
    let (completion_tx, mut completion_rx) =
        tokio::sync::mpsc::channel::<DispatchCompletion>(MAX_QUEUED_CLAIMS);
    let worker_control = control.clone();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let mut worker = tokio::task::spawn_blocking(move || -> Result<Vec<String>> {
        let mut dispatcher = dispatcher;
        if let Err(error) = dispatcher.connect() {
            let _ = ready_tx.send(false);
            let _ = dispatcher.emergency_stop();
            let _ = dispatcher.disconnect();
            return Err(error);
        }
        let _ = ready_tx.send(true);
        while let Some(claim) = work_rx.blocking_recv() {
            let completion = dispatch_claim(claim, &mut dispatcher, worker_control.as_ref());
            if completion_tx.blocking_send(completion).is_err() {
                break;
            }
        }
        dispatcher.disconnect()
    });
    let ready = ready_rx.await.unwrap_or(false);
    let mut inflight = HashMap::<String, String>::new();
    let run_result = if ready {
        run_poller(
            &device,
            doctor,
            transport.as_ref(),
            &credential,
            verifier.as_ref(),
            &mut replay,
            &mut outbox,
            control.as_ref(),
            &work_tx,
            &mut completion_rx,
            &mut inflight,
            heartbeat,
            &mut stream_rx,
            &cursor_tx,
            &mut stop,
        )
        .await
    } else {
        Err(Error::Transport)
    };
    stream_task.cancel().await;
    let cleanup = shutdown_worker(
        control,
        work_tx,
        worker,
        &mut completion_rx,
        &mut outbox,
        &inflight,
    )
    .await;
    match run_result {
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
        Ok(()) => cleanup,
    }
}
#[allow(clippy::too_many_arguments)]
async fn run_poller<T: RunnerTransport>(
    device: &str,
    doctor: Value,
    transport: &T,
    credential: &crate::transport::Credential,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    control: &dyn RemoteControl,
    work_tx: &tokio::sync::mpsc::Sender<Claim>,
    completion_rx: &mut tokio::sync::mpsc::Receiver<DispatchCompletion>,
    inflight: &mut HashMap<String, String>,
    heartbeat: std::time::Duration,
    stream_rx: &mut tokio::sync::mpsc::Receiver<StreamEvent>,
    cursor_tx: &tokio::sync::watch::Sender<Option<String>>,
    stop: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let mut stream_open = true;
    let mut streaming = false;
    let mut interval = tokio::time::interval(heartbeat);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        tokio::select! {
         event=stream_rx.recv(), if stream_open => {
          apply_stream_event(event, device, verifier, replay, outbox, control, inflight, cursor_tx, &mut interval, heartbeat, &mut streaming, &mut stream_open)?;
         },
         changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){return Ok(())}},
         completion=completion_rx.recv()=>match completion{Some(completion)=>{if let Some(id)=completion.receipt.action_id.as_deref(){inflight.remove(id);}persist_completion(outbox,completion)?;},None=>return Err(Error::Storage)},
         _=interval.tick()=>{
          let(request,low,high)=bounded_exchange_request(device,doctor.clone(),outbox,MAX_HEARTBEAT_REQUEST_BYTES)?;let mut exchange=Box::pin(transport.exchange(credential,&request));
          let response=loop{tokio::select!{
           result=&mut exchange=>break result?,
           completion=completion_rx.recv()=>match completion{Some(completion)=>{if let Some(id)=completion.receipt.action_id.as_deref(){inflight.remove(id);}persist_completion(outbox,completion)?;},None=>return Err(Error::Storage)},
         event=stream_rx.recv(), if stream_open => {
          apply_stream_event(event, device, verifier, replay, outbox, control, inflight, cursor_tx, &mut interval, heartbeat, &mut streaming, &mut stream_open)?;
         },
           changed=stop.changed()=>{if changed.is_err()||*stop.borrow(){return Ok(())}}
          }};
          if response.accepted_through_sequence<low||response.accepted_through_sequence>high{return Err(Error::Protocol)}
          outbox.acknowledge(response.accepted_through_sequence)?;
          let mut controls=Vec::new();let mut claims=Vec::new();for command in response.commands{match command["kind"].as_str(){Some("claim")=>claims.push(command),Some("cancel"|"emergency_stop")=>controls.push(command),_=>return Err(Error::Protocol)}}
          for command in controls{control_with_remote(device,command,verifier,replay,outbox,control,inflight)?;}
          for command in claims{
           if inflight.len()>=MAX_QUEUED_CLAIMS||work_tx.capacity()==0{break}
           let Some(claim)=accept_claim(device,command,verifier,replay,outbox,control)?else{continue};let action_id=claim.action.action_id.clone();let command_id=claim.command_id.clone();
           match work_tx.try_send(claim){Ok(())=>{inflight.insert(action_id,command_id);},Err(tokio::sync::mpsc::error::TrySendError::Full(claim))=>{let id=claim.action.action_id.clone();let receipt=Receipt{sequence:0,event_id:format!("command.{}.{}",claim.command_id,id),action_id:Some(id),event_type:"action.unknown".into(),status:Some("unknown".into()),occurred_at:crate::now(),result_digest:None,error_code:Some("local_dispatch_queue_full".into()),summary:None,result:Some(json!({"replay_allowed":false}))};outbox.append(receipt,false)?;break},Err(tokio::sync::mpsc::error::TrySendError::Closed(_))=>return Err(Error::Storage)}
          }
         }
        }
    }
}
#[allow(clippy::too_many_arguments)]
fn apply_stream_event(
    event: Option<StreamEvent>,
    device: &str,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    control: &dyn RemoteControl,
    inflight: &HashMap<String, String>,
    cursor_tx: &tokio::sync::watch::Sender<Option<String>>,
    interval: &mut tokio::time::Interval,
    heartbeat: std::time::Duration,
    streaming: &mut bool,
    stream_open: &mut bool,
) -> Result<()> {
    match event {
        Some(StreamEvent::Frame(frame)) => {
            frame.verify(verifier)?;
            // Heartbeats are signed too; only a verified complete frame marks a
            // proxy path healthy. Commands still pass the original verifier and
            // durable replay guard, shared with polls.
            for command in frame.commands {
                control_with_remote(device, command, verifier, replay, outbox, control, inflight)?;
            }
            replay.checkpoint_stream(device, &frame.cursor)?;
            cursor_tx.send_replace(Some(frame.cursor));
            if !*streaming {
                *interval = tokio::time::interval(heartbeat);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                *streaming = true;
            }
        }
        Some(StreamEvent::Unavailable) => {
            *interval = tokio::time::interval(heartbeat.min(crate::control_stream::FALLBACK_POLL));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            *streaming = false;
        }
        None => *stream_open = false,
    }
    Ok(())
}

async fn shutdown_worker(
    control: Arc<dyn RemoteControl>,
    work_tx: tokio::sync::mpsc::Sender<Claim>,
    mut worker: tokio::task::JoinHandle<Result<Vec<String>>>,
    completion_rx: &mut tokio::sync::mpsc::Receiver<DispatchCompletion>,
    outbox: &mut Outbox,
    inflight: &HashMap<String, String>,
) -> Result<()> {
    let mut first_error = None;
    let mut handled = HashSet::new();
    match control.emergency_stop() {
        Ok(outcomes) => {
            for (id, outcome) in outcomes {
                handled.insert(id.clone());
                if let Err(error) =
                    append_control_outcome(outbox, format!("command.shutdown.{id}"), id, outcome)
                {
                    if first_error.is_none() {
                        first_error = Some(error)
                    }
                }
            }
        }
        Err(error) => first_error = Some(error),
    }
    for id in inflight.keys() {
        if handled.insert(id.clone()) {
            match control.cancel(id) {
                Ok(outcome) => {
                    if let Err(error) = append_control_outcome(
                        outbox,
                        format!("command.shutdown.{id}"),
                        id.clone(),
                        outcome,
                    ) {
                        if first_error.is_none() {
                            first_error = Some(error)
                        }
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error)
                    }
                }
            }
        }
    }
    drop(work_tx);
    let mut joined = None;
    loop {
        if joined.is_some() && completion_rx.is_closed() && completion_rx.is_empty() {
            break;
        }
        tokio::select! {
         result=&mut worker,if joined.is_none()=>{joined=Some(result)},
         completion=completion_rx.recv(),if !completion_rx.is_closed()||!completion_rx.is_empty()=>if let Some(completion)=completion{if let Err(error)=persist_completion(outbox,completion){if first_error.is_none(){first_error=Some(error)}}}
        }
    }
    match joined {
        Some(Ok(Ok(ids))) => {
            if let Err(error) = append_disconnect_receipts(outbox, ids) {
                if first_error.is_none() {
                    first_error = Some(error)
                }
            }
        }
        Some(Ok(Err(error))) => {
            if first_error.is_none() {
                first_error = Some(error)
            }
        }
        Some(Err(_)) => {
            if first_error.is_none() {
                first_error = Some(Error::Storage)
            }
        }
        None => {
            if first_error.is_none() {
                first_error = Some(Error::Storage)
            }
        }
    }
    if let Some(error) = first_error {
        Err(error)
    } else {
        Ok(())
    }
}

pub trait ControlledDispatcher: Dispatcher + Send {
    fn connect(&mut self) -> Result<()>;
    fn cancel(&mut self, action_id: &str) -> Result<Outcome>;
    fn emergency_stop(&mut self) -> Result<Vec<String>>;
    fn disconnect(&mut self) -> Result<Vec<String>>;
    fn remote_control(&self) -> Arc<dyn RemoteControl>;
}
pub fn control(
    device: &str,
    raw: Value,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    dispatcher: &mut dyn ControlledDispatcher,
) -> Result<bool> {
    control_with_remote(
        device,
        raw,
        verifier,
        replay,
        outbox,
        dispatcher.remote_control().as_ref(),
        &HashMap::new(),
    )
}
pub async fn run_once(
    device: &str,
    doctor: Value,
    transport: &crate::transport::HttpsTransport,
    credential: &crate::transport::Credential,
    verifier: &dyn PlatformVerifier,
    replay: &mut ReplayGuard,
    outbox: &mut Outbox,
    dispatcher: &mut dyn ControlledDispatcher,
) -> Result<usize> {
    if let Err(error) = dispatcher.connect() {
        stop_dispatcher(outbox, dispatcher)?;
        return Err(error);
    }
    let result = async {
        let (request, low, high) =
            bounded_exchange_request(device, doctor, outbox, MAX_HEARTBEAT_REQUEST_BYTES)?;
        let response = transport.exchange(credential, &request).await?;
        if response.accepted_through_sequence < low || response.accepted_through_sequence > high {
            return Err(Error::Protocol);
        }
        outbox.acknowledge(response.accepted_through_sequence)?;
        let mut controls = Vec::new();
        let mut claims = Vec::new();
        for command in response.commands {
            match command["kind"].as_str() {
                Some("claim") => claims.push(command),
                Some("cancel" | "emergency_stop") => controls.push(command),
                _ => return Err(Error::Protocol),
            }
        }
        let mut processed = 0;
        // Controls are applied before ordinary FIFO work in the same signed batch.
        // A cancel tombstone therefore suppresses a claim that has not dispatched.
        for command in controls {
            control(device, command, verifier, replay, outbox, dispatcher)?;
            processed += 1;
        }
        for command in claims {
            claim(device, command, verifier, replay, outbox, dispatcher)?;
            processed += 1;
        }
        Ok(processed)
    }
    .await;
    if result.is_err() {
        stop_dispatcher(outbox, dispatcher)?;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path =
                std::env::temp_dir().join(format!("local-runner-wire-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn heartbeat_receipt_batch_uses_escaped_wire_size_and_keeps_a_contiguous_prefix() {
        let temp = Temp::new();
        let mut outbox = Outbox::open(&temp.0.join("outbox.sqlite")).unwrap();
        for index in 0..2 {
            let text = "\"".repeat(900_000);
            outbox
                .append(
                    Receipt {
                        sequence: 0,
                        event_id: format!("quoted-{index}"),
                        action_id: Some(format!("action-{index}")),
                        event_type: "action.succeeded".into(),
                        status: Some("succeeded".into()),
                        occurred_at: crate::now(),
                        result_digest: None,
                        error_code: None,
                        summary: None,
                        result: Some(json!({"content":text})),
                    },
                    true,
                )
                .unwrap();
        }
        let budget = 3 * 1024 * 1024;
        let (request, low, high) =
            bounded_exchange_request("device-fixture", json!({"status":"ready"}), &outbox, budget)
                .unwrap();
        let encoded = serde_json::to_vec(&request).unwrap();
        assert!(encoded.len() <= budget);
        assert_eq!(low, 0);
        assert_eq!(high, 1);
        assert_eq!(
            request
                .receipts
                .iter()
                .map(|receipt| receipt.sequence)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }
}
