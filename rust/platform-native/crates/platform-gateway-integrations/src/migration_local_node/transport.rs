//! Outbound HTTPS command polling. Transport authentication never substitutes for
//! independently injected action signature, lease, nonce or local approval checks.
use super::{
    LocalNodeError as Error, Result,
    credentials::{CredentialStorageStatus, SecureCredentialStore},
    database,
    doctor::DoctorReport,
    identity::{DeviceCredential, DeviceIdentity},
    models::*,
    outbox::{ReceiptEvent, ReceiptOutbox},
    service::{LocalNodeRuntime, OutboundControlPlane},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub const PROTOCOL_VERSION: &str = "ai-platform.local-node.v1";
const MAX_EXCHANGE_REQUEST_BYTES: usize = 4 * 1024 * 1024;
pub type TransportFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
#[derive(Clone, Serialize, Deserialize)]
pub struct PairingRedemption {
    pub challenge_id: String,
    pub user_code: String,
    pub device_id: String,
    pub proof_algorithm: String,
    pub proof_public_key: String,
    pub device_proof: String,
    pub display_name: String,
    pub platform: String,
    pub node_version: String,
    pub capability_claims: Vec<String>,
    pub permission_snapshot_digest: String,
    pub protocol_version: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExchangeRequest {
    pub device_id: String,
    pub doctor: Value,
    pub receipts: Vec<Value>,
    pub sent_at: f64,
    pub protocol_version: String,
}
impl ExchangeRequest {
    pub fn as_value(&self) -> Result<Value> {
        let mut value = serde_json::to_value(self)?;
        value["kind"] = json!("heartbeat");
        Ok(value)
    }
}
fn bounded_receipt_prefix(
    request: &ExchangeRequest,
    candidates: Vec<ReceiptEvent>,
    max_bytes: usize,
) -> Result<Vec<ReceiptEvent>> {
    let mut empty = request.clone();
    empty.receipts.clear();
    let mut encoded_len = canonical_json(&empty.as_value()?).len();
    if encoded_len > max_bytes {
        return Err(Error::protocol("base exchange request exceeds byte budget"));
    }
    let had_candidates = !candidates.is_empty();
    let mut batch = Vec::new();
    for receipt in candidates {
        let receipt_len = canonical_json(&receipt.as_value(true)?).len();
        let increment = receipt_len.saturating_add(if batch.is_empty() { 0 } else { 1 });
        let Some(next_len) = encoded_len.checked_add(increment) else {
            break;
        };
        if next_len > max_bytes {
            break;
        }
        encoded_len = next_len;
        batch.push(receipt);
    }
    if had_candidates && batch.is_empty() {
        return Err(Error::protocol(
            "single pending receipt exceeds exchange request byte budget",
        ));
    }
    Ok(batch)
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeResponse {
    pub accepted_through_sequence: i64,
    pub commands: Vec<Value>,
    pub protocol_version: String,
}
pub trait OutboundTransport: Send + Sync {
    fn kind(&self) -> &str;
    fn redeem_pairing<'a>(
        &'a self,
        endpoint: &'a OutboundControlPlane,
        redemption: &'a PairingRedemption,
    ) -> TransportFuture<'a, DeviceCredential>;
    fn exchange<'a>(
        &'a self,
        endpoint: &'a OutboundControlPlane,
        credential: &'a DeviceCredential,
        request: &'a ExchangeRequest,
    ) -> TransportFuture<'a, ExchangeResponse>;
    fn close(&self) -> Result<()> {
        Ok(())
    }
}
pub trait PairingProofSigner: Send + Sync {
    fn algorithm(&self) -> &str;
    fn public_key(&self) -> &str;
    fn sign(&self, payload: &[u8]) -> Result<String>;
}
pub struct HttpsJsonTransport {
    http: reqwest::Client,
}
impl HttpsJsonTransport {
    pub fn new(timeout: f64) -> Result<Self> {
        if !timeout.is_finite() || timeout <= 0.0 || timeout > 120.0 {
            return Err(Error::new(
                "boundary_violation",
                "transport timeout invalid",
            ));
        }
        let http = reqwest::Client::builder()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs_f64(timeout))
            .no_proxy()
            .build()
            .map_err(|_| Error::new("transport_unavailable", "HTTPS adapter unavailable"))?;
        Ok(Self { http })
    }
    async fn post(
        &self,
        endpoint: &OutboundControlPlane,
        payload: Value,
        credential: Option<&DeviceCredential>,
    ) -> Result<Value> {
        endpoint.validate()?;
        if !endpoint.url.starts_with("https://") {
            return Err(Error::new(
                "transport_unavailable",
                "WSS endpoint requires explicitly configured WebSocket adapter",
            ));
        }
        let mut request = self
            .http
            .post(&endpoint.url)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("User-Agent", "AI-Platform-Local-Node/0.1")
            .body(canonical_json(&payload));
        if let Some(credential) = credential {
            request = request.header("Authorization", format!("Device {}", credential.credential));
        }
        let mut response = request.send().await.map_err(|_| {
            Error::new(
                "transport_unavailable",
                "outbound control-plane request failed",
            )
        })?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(Error::protocol("control plane returned non-200 status"));
        }
        if response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            != Some("application/json")
        {
            return Err(Error::protocol("control plane returned non-JSON response"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| Error::new("transport_unavailable", "outbound response failed"))?
        {
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(Error::protocol("control-plane response exceeds one MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes)?;
        if !value.is_object() {
            return Err(Error::protocol("control-plane response must be an object"));
        }
        Ok(value)
    }
}
impl OutboundTransport for HttpsJsonTransport {
    fn kind(&self) -> &str {
        "https"
    }
    fn redeem_pairing<'a>(
        &'a self,
        endpoint: &'a OutboundControlPlane,
        redemption: &'a PairingRedemption,
    ) -> TransportFuture<'a, DeviceCredential> {
        Box::pin(async move {
            let mut payload = serde_json::to_value(redemption)?;
            payload["kind"] = json!("pairing_redeem");
            let result = self.post(endpoint, payload, None).await?;
            parse_credential(&result, &redemption.device_id, false)?
                .ok_or_else(|| Error::new("pairing_error", "returned credential expired"))
        })
    }
    fn exchange<'a>(
        &'a self,
        endpoint: &'a OutboundControlPlane,
        credential: &'a DeviceCredential,
        request: &'a ExchangeRequest,
    ) -> TransportFuture<'a, ExchangeResponse> {
        Box::pin(async move {
            let payload = request.as_value()?;
            if canonical_json(&payload).len() > MAX_EXCHANGE_REQUEST_BYTES {
                return Err(Error::protocol(
                    "exchange request exceeds aggregate byte budget",
                ));
            }
            let raw = self.post(endpoint, payload, Some(credential)).await?;
            let response: ExchangeResponse = serde_json::from_value(raw)?;
            if response.protocol_version != PROTOCOL_VERSION
                || response.accepted_through_sequence < 0
                || response.commands.len() > 100
                || response.commands.iter().any(|v| !v.is_object())
            {
                return Err(Error::protocol("exchange response invalid"));
            }
            Ok(response)
        })
    }
}
#[derive(Clone, Debug)]
pub struct DispatchOutcome {
    pub action_id: String,
    pub status: ActionStatus,
    pub result: Value,
    pub error_code: Option<String>,
}
pub trait DeviceActionDispatcher: Send + Sync {
    fn connect(&self) -> Result<()>;
    fn dispatch(&self, action: &ActionContext, arguments: &Value) -> Result<DispatchOutcome>;
    fn cancel(&self, action_id: &str) -> Result<DispatchOutcome>;
    fn emergency_stop(&self) -> Result<Vec<String>>;
    fn disconnect(&self) -> Result<Vec<String>>;
}
pub type ActionHandler = dyn Fn(&ActionContext, &Value) -> Result<DispatchOutcome> + Send + Sync;
pub struct LocalActionRouter {
    pub runtime: Arc<LocalNodeRuntime>,
    pub handlers: BTreeMap<String, Arc<ActionHandler>>,
}
impl DeviceActionDispatcher for LocalActionRouter {
    fn connect(&self) -> Result<()> {
        self.runtime.connect()
    }
    fn dispatch(&self, action: &ActionContext, args: &Value) -> Result<DispatchOutcome> {
        self.runtime.assert_online()?;
        action.validate_payload(
            args,
            self.runtime.ledger.platform_signature_verifier.as_deref(),
            None,
            None,
        )?;
        {
            let outcome = self.handlers.get(&action.capability).ok_or_else(|| {
                Error::denied("claimed capability has no configured local handler")
            })?(action, args)?;
            if outcome.action_id != action.action_id {
                return Err(Error::protocol(
                    "local handler returned another action identity",
                ));
            }
            Ok(outcome)
        }
    }
    fn cancel(&self, id: &str) -> Result<DispatchOutcome> {
        let cancelled = self.runtime.process_runner.cancel(id)?;
        Ok(DispatchOutcome {
            action_id: id.into(),
            status: if cancelled {
                ActionStatus::Cancelled
            } else {
                ActionStatus::Failed
            },
            result: json!({}),
            error_code: if cancelled {
                None
            } else {
                Some("action_not_running".into())
            },
        })
    }
    fn emergency_stop(&self) -> Result<Vec<String>> {
        let cancelled = self.runtime.process_runner.cancel_all();
        let computer_stop = self.runtime.computer.emergency_stop();
        let mut cancelled = cancelled?;
        computer_stop?;
        cancelled.sort();
        cancelled.dedup();
        Ok(cancelled)
    }
    fn disconnect(&self) -> Result<Vec<String>> {
        self.runtime.disconnect()
    }
}
pub struct ClaimReplayGuard {
    db: Mutex<Connection>,
    command_locks: Mutex<HashMap<String, Weak<Mutex<()>>>>,
}
impl ClaimReplayGuard {
    pub fn open(path: &Path) -> Result<Self> {
        let db = database::open(path)?;
        database::lock(&db)?.execute_batch("CREATE TABLE IF NOT EXISTS consumed_commands(command_id TEXT PRIMARY KEY,nonce TEXT UNIQUE NOT NULL,payload_digest TEXT NOT NULL,consumed_at REAL NOT NULL)")?;
        Ok(Self {
            db,
            command_locks: Mutex::new(HashMap::new()),
        })
    }
    fn command_lock(&self, id: &str) -> Result<Arc<Mutex<()>>> {
        if !valid_identifier(id, 512) {
            return Err(Error::protocol("command id invalid"));
        }
        let mut locks = self
            .command_locks
            .lock()
            .map_err(|_| Error::new("transport_unavailable", "command lock unavailable"))?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(id).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(id.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }
    pub fn reserve(&self, id: &str, nonce: &str, digest: &str) -> Result<bool> {
        if !valid_identifier(id, 512) || !valid_identifier(nonce, 512) {
            return Err(Error::protocol("command id or nonce invalid"));
        }
        let mut db = database::lock(&self.db)?;
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing=tx.query_row("SELECT command_id,nonce,payload_digest FROM consumed_commands WHERE command_id=?1 OR nonce=?2 LIMIT 1",params![id,nonce],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?))).optional()?;
        if let Some(value) = existing {
            if value != (id.into(), nonce.into(), digest.into()) {
                return Err(Error::denied("control command id or nonce replayed"));
            }
            tx.commit()?;
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO consumed_commands VALUES(?1,?2,?3,?4)",
            params![id, nonce, digest, now()],
        )?;
        tx.commit()?;
        Ok(true)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransportDoctorReport {
    pub status: String,
    pub endpoint: String,
    pub transport: String,
    pub device_credential: String,
    pub identity_credential_storage: CredentialStorageStatus,
    pub outbound_credential_storage: CredentialStorageStatus,
    pub platform_signature_verifier: String,
    pub pairing_signer: String,
    pub runtime: Value,
    pub reason: Option<String>,
}
pub struct DeviceOutboundRunner {
    pub identity: Arc<DeviceIdentity>,
    pub endpoint: OutboundControlPlane,
    pub transport: Option<Arc<dyn OutboundTransport>>,
    pub credential: Mutex<Option<DeviceCredential>>,
    pub credential_store: Option<Arc<dyn SecureCredentialStore>>,
    pub credential_reference: Option<String>,
    pub platform_signature_verifier: Option<Arc<dyn PlatformSignatureVerifier>>,
    pub pairing_signer: Option<Arc<dyn PairingProofSigner>>,
    pub outbox: Arc<ReceiptOutbox>,
    pub replay_guard: Arc<ClaimReplayGuard>,
    pub dispatcher: Arc<dyn DeviceActionDispatcher>,
    pub doctor_provider: Arc<dyn Fn() -> Result<DoctorReport> + Send + Sync>,
    pub allow_test_credentials: bool,
    pub stopped: Arc<AtomicBool>,
}

#[derive(Clone)]
struct CommandProcessor {
    identity: Arc<DeviceIdentity>,
    replay_guard: Arc<ClaimReplayGuard>,
    outbox: Arc<ReceiptOutbox>,
    dispatcher: Arc<dyn DeviceActionDispatcher>,
    platform_signature_verifier: Option<Arc<dyn PlatformSignatureVerifier>>,
    stopped: Arc<AtomicBool>,
}

struct CommandPump {
    sender: Option<tokio::sync::mpsc::Sender<Value>>,
    queued: Arc<Mutex<HashSet<String>>>,
    errors: Arc<Mutex<std::collections::VecDeque<Error>>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl CommandPump {
    const CAPACITY: usize = 256;

    fn new(processor: CommandProcessor) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Value>(Self::CAPACITY);
        let queued = Arc::new(Mutex::new(HashSet::new()));
        let errors = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let worker_queued = queued.clone();
        let worker_errors = errors.clone();
        let worker = tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                let command_id = command["command_id"].as_str().map(str::to_owned);
                if processor.stopped.load(Ordering::Acquire) {
                    if let Some(id) = command_id {
                        if let Ok(mut queued) = worker_queued.lock() {
                            queued.remove(&id);
                        }
                    }
                    continue;
                }
                let processor = processor.clone();
                let result =
                    tokio::task::spawn_blocking(move || processor.process_command(&command)).await;
                let error = match result {
                    Ok(Ok(())) => None,
                    Ok(Err(error)) => Some(error),
                    Err(_) => Some(Error::new(
                        "transport_unavailable",
                        "local command worker failed",
                    )),
                };
                if let Some(error) = error {
                    if let Ok(mut errors) = worker_errors.lock() {
                        errors.push_back(error);
                    }
                }
                if let Some(id) = command_id {
                    if let Ok(mut queued) = worker_queued.lock() {
                        queued.remove(&id);
                    }
                }
            }
        });
        Self {
            sender: Some(sender),
            queued,
            errors,
            worker: Some(worker),
        }
    }

    fn try_enqueue(&self, command: Value) -> Result<Option<Value>> {
        let id = required(&command, "command_id", 512)?.to_owned();
        {
            let mut queued = self
                .queued
                .lock()
                .map_err(|_| Error::new("transport_unavailable", "command queue unavailable"))?;
            if !queued.insert(id.clone()) {
                return Ok(None);
            }
        }
        let sender = self
            .sender
            .as_ref()
            .ok_or_else(|| Error::new("transport_unavailable", "command queue closed"))?;
        match sender.try_send(command) {
            Ok(()) => Ok(None),
            Err(tokio::sync::mpsc::error::TrySendError::Full(command)) => {
                if let Ok(mut queued) = self.queued.lock() {
                    queued.remove(&id);
                }
                Ok(Some(command))
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                if let Ok(mut queued) = self.queued.lock() {
                    queued.remove(&id);
                }
                Err(Error::new(
                    "transport_unavailable",
                    "command worker stopped",
                ))
            }
        }
    }

    fn take_error(&self) -> Result<Option<Error>> {
        Ok(self
            .errors
            .lock()
            .map_err(|_| Error::new("transport_unavailable", "command status unavailable"))?
            .pop_front())
    }

    async fn close(mut self) -> Result<()> {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            worker.await.map_err(|_| {
                Error::new("transport_unavailable", "local command worker join failed")
            })?;
        }
        if let Some(error) = self.take_error()? {
            return Err(error);
        }
        Ok(())
    }
}

impl DeviceOutboundRunner {
    fn command_processor(&self) -> CommandProcessor {
        CommandProcessor {
            identity: self.identity.clone(),
            replay_guard: self.replay_guard.clone(),
            outbox: self.outbox.clone(),
            dispatcher: self.dispatcher.clone(),
            platform_signature_verifier: self.platform_signature_verifier.clone(),
            stopped: self.stopped.clone(),
        }
    }
    fn load_credential(&self) -> Result<Option<DeviceCredential>> {
        let mut credential = self
            .credential
            .lock()
            .map_err(|_| Error::new("transport_unavailable", "credential state unavailable"))?;
        if credential.is_none() {
            if let (Some(store), Some(reference)) = (
                self.credential_store.as_ref(),
                self.credential_reference.as_deref(),
            ) {
                let status = store.status();
                let allowed = status == CredentialStorageStatus::Ready
                    || (self.allow_test_credentials
                        && status == CredentialStorageStatus::TestOnlyInsecure);
                if allowed {
                    *credential = load_persisted_credential(
                        store.as_ref(),
                        reference,
                        &self.identity.device_id,
                    )?;
                }
            }
        }
        Ok(credential.clone())
    }
    pub fn doctor(&self) -> Result<TransportDoctorReport> {
        let endpoint = if self.endpoint.validate().is_ok() {
            "ready"
        } else {
            "denied"
        };
        let transport = if self.transport.is_some() {
            "ready"
        } else {
            "unavailable"
        };
        let credential = self.load_credential()?;
        let credential_status = match credential.as_ref() {
            None => "unavailable",
            Some(c)
                if c.device_id == self.identity.device_id
                    && !c.credential.is_empty()
                    && now() < c.expires_at =>
            {
                "ready"
            }
            _ => "expired_or_invalid",
        };
        let storage = self
            .credential_store
            .as_ref()
            .map_or(CredentialStorageStatus::Unavailable, |s| s.status());
        let storage_ready = storage == CredentialStorageStatus::Ready
            || (self.allow_test_credentials
                && storage == CredentialStorageStatus::TestOnlyInsecure);
        let verifier = if self.platform_signature_verifier.is_some() {
            "ready"
        } else {
            "unavailable"
        };
        let runtime = serde_json::to_value((self.doctor_provider)()?)?;
        let reason = if endpoint != "ready" {
            Some("outbound endpoint invalid")
        } else if transport != "ready" {
            Some("outbound transport adapter unavailable")
        } else if credential_status != "ready" {
            Some("paired device credential unavailable or invalid")
        } else if !storage_ready {
            Some("native secure credential storage unavailable")
        } else if verifier != "ready" {
            Some("trusted platform signature verifier unavailable")
        } else if runtime["trusted_local_approval"] != "ready" {
            Some("trusted local approval verifier unavailable")
        } else {
            None
        };
        Ok(TransportDoctorReport {
            status: if reason.is_none() {
                "ready"
            } else {
                "unavailable"
            }
            .into(),
            endpoint: endpoint.into(),
            transport: transport.into(),
            device_credential: credential_status.into(),
            identity_credential_storage: self.identity.credential_storage,
            outbound_credential_storage: storage,
            platform_signature_verifier: verifier.into(),
            pairing_signer: if self.pairing_signer.is_some() {
                "ready"
            } else {
                "unavailable"
            }
            .into(),
            runtime,
            reason: reason.map(str::to_owned),
        })
    }
    pub async fn redeem_pairing(
        &self,
        mut redemption: PairingRedemption,
    ) -> Result<DeviceCredential> {
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Error::new("transport_unavailable", "outbound transport unavailable"))?;
        let signer = self.pairing_signer.as_ref().ok_or_else(|| {
            Error::new(
                "pairing_error",
                "native asymmetric pairing signer unavailable",
            )
        })?;
        let store = self
            .credential_store
            .as_ref()
            .filter(|s| {
                s.status() == CredentialStorageStatus::Ready
                    || (self.allow_test_credentials
                        && s.status() == CredentialStorageStatus::TestOnlyInsecure)
            })
            .ok_or_else(|| {
                Error::new(
                    "pairing_error",
                    "secure outbound credential storage unavailable",
                )
            })?;
        let reference = self
            .credential_reference
            .as_ref()
            .ok_or_else(|| Error::new("pairing_error", "outbound credential reference missing"))?;
        for value in [
            &redemption.challenge_id,
            &redemption.user_code,
            &redemption.display_name,
            &redemption.platform,
            &redemption.node_version,
        ] {
            if !valid_identifier(value, 512) {
                return Err(Error::new("pairing_error", "pairing metadata invalid"));
            }
        }
        if redemption.capability_claims.is_empty()
            || redemption.capability_claims.len() > 32
            || redemption
                .capability_claims
                .iter()
                .collect::<HashSet<_>>()
                .len()
                != redemption.capability_claims.len()
            || redemption.permission_snapshot_digest.len() != 64
            || !redemption
                .permission_snapshot_digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::new(
                "pairing_error",
                "capability claims or permission digest invalid",
            ));
        }
        if store.load(reference)?.is_some() {
            return Err(Error::new(
                "pairing_error",
                "outbound credential reference already occupied",
            ));
        }
        redemption.capability_claims.sort();
        redemption.device_id = self.identity.device_id.clone();
        redemption.proof_algorithm = signer.algorithm().into();
        redemption.proof_public_key = signer.public_key().into();
        redemption.protocol_version = PROTOCOL_VERSION.into();
        redemption.device_proof = signer.sign(
            canonical_json(&json!({"redemption_digest":pairing_redemption_digest(&redemption)}))
                .as_bytes(),
        )?;
        let credential = transport
            .redeem_pairing(&self.endpoint, &redemption)
            .await?;
        if credential.device_id != self.identity.device_id {
            return Err(Error::new(
                "pairing_error",
                "paired credential belongs to another device",
            ));
        }
        store.store(reference,canonical_json(&json!({"protocol_version":PROTOCOL_VERSION,"device_id":credential.device_id,"credential":credential.credential,"expires_at":credential.expires_at})).as_bytes())?;
        *self
            .credential
            .lock()
            .map_err(|_| Error::new("pairing_error", "credential state unavailable"))? =
            Some(credential.clone());
        Ok(credential)
    }
    pub async fn run_once(&self) -> Result<usize> {
        // One-shot calls own and join their dispatcher queue; run_forever keeps it
        // across polls so network controls remain independent of a long dispatch.
        let pump = CommandPump::new(self.command_processor());
        let result = self.run_once_with_pump(&pump).await;
        let worker_result = pump.close().await;
        match result {
            Err(error) => {
                let _ = worker_result;
                Err(error)
            }
            Ok(count) => match worker_result {
                Ok(()) => Ok(count),
                Err(error) => {
                    let _ = self.record_disconnect();
                    Err(error)
                }
            },
        }
    }

    async fn run_once_with_pump(&self, pump: &CommandPump) -> Result<usize> {
        let report = self.doctor()?;
        if report.status != "ready" {
            return Err(Error::new(
                "transport_unavailable",
                report
                    .reason
                    .unwrap_or_else(|| "outbound runner unavailable".into()),
            ));
        }
        let credential = self
            .load_credential()?
            .ok_or_else(|| Error::new("transport_unavailable", "credential missing"))?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Error::new("transport_unavailable", "outbound transport unavailable"))?;
        let candidates = self.outbox.pending(200)?;
        let mut request = ExchangeRequest {
            device_id: self.identity.device_id.clone(),
            doctor: report.runtime,
            receipts: Vec::new(),
            sent_at: now(),
            protocol_version: PROTOCOL_VERSION.into(),
        };
        let pending = bounded_receipt_prefix(&request, candidates, MAX_EXCHANGE_REQUEST_BYTES)?;
        request.receipts = pending
            .iter()
            .map(|event| event.as_value(true))
            .collect::<Result<_>>()?;
        self.dispatcher.connect()?;
        let result = async {
            if let Some(error) = pump.take_error()? {
                return Err(error);
            }
            let response = transport
                .exchange(&self.endpoint, &credential, &request)
                .await?;
            if response.protocol_version != PROTOCOL_VERSION || response.commands.len() > 100 {
                return Err(Error::protocol("control-channel protocol invalid"));
            }
            let (first, last) = match (pending.first(), pending.last()) {
                (Some(first), Some(last)) => (first.sequence - 1, last.sequence),
                _ => {
                    let ack = self.outbox.acked_through()?;
                    (ack, ack)
                }
            };
            if response.accepted_through_sequence < first
                || response.accepted_through_sequence > last
            {
                return Err(Error::protocol(
                    "control plane acknowledged unsent receipts",
                ));
            }
            self.outbox
                .acknowledge(response.accepted_through_sequence)?;
            let count = response.commands.len();
            let processor = self.command_processor();
            for command in response.commands {
                if self.stopped.load(Ordering::Acquire) {
                    break;
                }
                match command["kind"].as_str() {
                    Some("claim") => {
                        if let Some(command) = pump.try_enqueue(command)? {
                            processor
                                .reject_claim_unknown(&command, "local_dispatch_queue_full")?;
                        }
                    }
                    Some("cancel" | "emergency_stop") => {
                        processor.process_command(&command)?;
                    }
                    _ => return Err(Error::protocol("unsupported control command")),
                }
            }
            if let Some(error) = pump.take_error()? {
                return Err(error);
            }
            Ok(count)
        }
        .await;
        if result.is_err() {
            self.record_disconnect()?;
        }
        result
    }

    fn record_disconnect(&self) -> Result<()> {
        let processor = self.command_processor();
        for id in self.dispatcher.disconnect()? {
            processor.append_terminal(
                "disconnect",
                &DispatchOutcome {
                    action_id: id,
                    status: ActionStatus::Unknown,
                    result: json!({"replay_allowed":false}),
                    error_code: Some("transport_disconnected".into()),
                },
                false,
            )?;
        }
        Ok(())
    }

    pub async fn run_forever(&self, heartbeat: Duration) -> Result<()> {
        if heartbeat < Duration::from_millis(250) || heartbeat > Duration::from_secs(300) {
            return Err(Error::new(
                "boundary_violation",
                "heartbeat interval invalid",
            ));
        }
        self.stopped.store(false, Ordering::Release);
        let pump = CommandPump::new(self.command_processor());
        let loop_result = async {
            while !self.stopped.load(Ordering::Acquire) {
                self.run_once_with_pump(&pump).await?;
                let deadline = tokio::time::Instant::now() + heartbeat;
                while !self.stopped.load(Ordering::Acquire) {
                    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
                }
            }
            Ok(())
        }
        .await;
        let worker_result = pump.close().await;
        match loop_result {
            Err(error) => {
                let _ = worker_result;
                Err(error)
            }
            Ok(()) => match worker_result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let _ = self.record_disconnect();
                    Err(error)
                }
            },
        }
    }
    pub fn stop(&self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        let emergency = self.dispatcher.emergency_stop();
        let transport = self.transport.as_ref().map_or(Ok(()), |t| t.close());
        if let Err(error) = emergency {
            return Err(error);
        }
        transport
    }
    pub fn process_command(&self, command: &Value) -> Result<()> {
        self.command_processor().process_command(command)
    }
}

impl CommandProcessor {
    fn process_command(&self, command: &Value) -> Result<()> {
        if canonical_json(command).len() > 65536 {
            return Err(Error::protocol("control command exceeds 64 KiB"));
        }
        let command_id = required(command, "command_id", 512)?;
        let lock = self.replay_guard.command_lock(command_id)?;
        let _guard = lock
            .lock()
            .map_err(|_| Error::new("transport_unavailable", "command lock unavailable"))?;
        match command["kind"].as_str() {
            Some("claim") => self.claim(command),
            Some("cancel") => self.control(command, false),
            Some("emergency_stop") => self.control(command, true),
            _ => Err(Error::protocol("unsupported control command")),
        }
    }
    fn claim(&self, command: &Value) -> Result<()> {
        let (command_id, action) = self.validate_claim(command)?;
        if !self.replay_guard.reserve(
            &command_id,
            &action.nonce,
            &sha256(&action.canonical_signed_payload()?),
        )? {
            self.append_unknown_if_missing(
                &format!("command.{command_id}.{}", action.action_id),
                Some(action.action_id),
                "action.unknown",
                "command_outcome_missing",
            )?;
            return Ok(());
        }
        self.dispatch_claim(command, &command_id, action)
    }

    fn reject_claim_unknown(&self, command: &Value, error_code: &str) -> Result<()> {
        let (command_id, action) = self.validate_claim(command)?;
        if !self.replay_guard.reserve(
            &command_id,
            &action.nonce,
            &sha256(&action.canonical_signed_payload()?),
        )? {
            self.append_unknown_if_missing(
                &format!("command.{command_id}.{}", action.action_id),
                Some(action.action_id),
                "action.unknown",
                "command_outcome_missing",
            )?;
            return Ok(());
        }
        self.append_terminal(
            &command_id,
            &DispatchOutcome {
                action_id: action.action_id,
                status: ActionStatus::Unknown,
                result: json!({"replay_allowed":false}),
                error_code: Some(error_code.into()),
            },
            false,
        )
    }

    fn validate_claim(&self, command: &Value) -> Result<(String, ActionContext)> {
        shape(
            command,
            &["kind", "command_id", "action", "normalized_arguments"],
        )?;
        let command_id = required(command, "command_id", 512)?.to_owned();
        shape(
            &command["action"],
            &[
                "action_id",
                "idempotency_key",
                "tenant_id",
                "user_id",
                "session_id",
                "run_id",
                "agent_id",
                "agent_version",
                "call_id",
                "device_id",
                "envelope_version",
                "capability",
                "tool_name",
                "operation",
                "capability_lease_id",
                "resource_refs",
                "arguments_digest",
                "target_snapshot_digest",
                "policy_snapshot_digest",
                "nonce",
                "issued_at",
                "expires_at",
                "platform_key_id",
                "platform_signature",
                "approval",
                "trace_context",
            ],
        )?;
        let action: ActionContext = serde_json::from_value(command["action"].clone())?;
        if action.device_id != self.identity.device_id {
            return Err(Error::denied("claimed action targets another device"));
        }
        action.validate_payload(
            &command["normalized_arguments"],
            self.platform_signature_verifier.as_deref(),
            None,
            None,
        )?;
        Ok((command_id, action))
    }

    fn dispatch_claim(
        &self,
        command: &Value,
        command_id: &str,
        action: ActionContext,
    ) -> Result<()> {
        match self
            .dispatcher
            .dispatch(&action, &command["normalized_arguments"])
        {
            Ok(outcome) => self.append_terminal(
                command_id,
                &outcome,
                outcome.status == ActionStatus::Succeeded
                    && [
                        "file.list",
                        "file.read",
                        "file.hash",
                        "file.search",
                        "file.watch",
                    ]
                    .contains(&action.operation.as_str()),
            ),
            Err(e) => {
                self.append_terminal(
                    command_id,
                    &DispatchOutcome {
                        action_id: action.action_id,
                        status: ActionStatus::Unknown,
                        result: json!({"replay_allowed":false}),
                        error_code: Some(e.code.into()),
                    },
                    false,
                )?;
                Err(e)
            }
        }
    }
    fn control(&self, command: &Value, emergency: bool) -> Result<()> {
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
        if !emergency {
            fields.push("action_id");
        }
        shape(command, &fields)?;
        let (id, device, nonce, key, signature) = (
            required(command, "command_id", 512)?,
            required(command, "device_id", 512)?,
            required(command, "nonce", 512)?,
            required(command, "platform_key_id", 512)?,
            required(command, "platform_signature", 8192)?,
        );
        let action_id = if emergency {
            None
        } else {
            Some(required(command, "action_id", 512)?.to_owned())
        };
        let issued = command["issued_at"]
            .as_f64()
            .ok_or_else(|| Error::protocol("control issue time invalid"))?;
        let expiry = command["expires_at"]
            .as_f64()
            .ok_or_else(|| Error::protocol("control expiry invalid"))?;
        if device != self.identity.device_id
            || !issued.is_finite()
            || !expiry.is_finite()
            || issued > now() + 30.0
            || expiry <= issued
            || expiry - issued > 600.0
            || now() >= expiry
        {
            return Err(Error::denied("control command scope or lifetime invalid"));
        }
        let mut signed = command.clone();
        signed.as_object_mut().unwrap().remove("platform_signature");
        let payload = canonical_json(&signed);
        if !self
            .platform_signature_verifier
            .as_ref()
            .is_some_and(|v| v.verify(key, payload.as_bytes(), signature))
        {
            return Err(Error::denied(
                "platform control signature verification failed",
            ));
        }
        if !self
            .replay_guard
            .reserve(id, nonce, &sha256(payload.as_bytes()))?
        {
            if emergency {
                self.append_unknown_if_missing(
                    &format!("command.{id}"),
                    None,
                    "device.emergency_stopped",
                    "command_outcome_missing",
                )?;
            } else {
                let action_id = action_id
                    .as_deref()
                    .ok_or_else(|| Error::protocol("cancel action id missing"))?;
                self.append_unknown_if_missing(
                    &format!("command.{id}.{action_id}"),
                    Some(action_id.to_owned()),
                    "action.unknown",
                    "command_outcome_missing",
                )?;
            }
            return Ok(());
        }
        if emergency {
            match self.dispatcher.emergency_stop() {
                Ok(mut stopped) => {
                    stopped.sort();
                    self.append_bounded_receipt(
                        ReceiptEvent {
                            sequence: 0,
                            event_id: format!("command.{id}"),
                            action_id: None,
                            event_type: "device.emergency_stopped".into(),
                            status: Some(ActionStatus::Cancelled),
                            occurred_at: now(),
                            result_digest: None,
                            error_code: None,
                            summary: None,
                            result: Some(json!({"stopped_action_ids":stopped})),
                        },
                        false,
                    )
                }
                Err(error) => {
                    self.append_bounded_receipt(
                        ReceiptEvent {
                            sequence: 0,
                            event_id: format!("command.{id}"),
                            action_id: None,
                            event_type: "device.emergency_stopped".into(),
                            status: Some(ActionStatus::Unknown),
                            occurred_at: now(),
                            result_digest: None,
                            error_code: Some(error.code.into()),
                            summary: None,
                            result: Some(json!({"replay_allowed":false})),
                        },
                        false,
                    )?;
                    Err(error)
                }
            }
        } else {
            let action_id = action_id
                .as_deref()
                .ok_or_else(|| Error::protocol("cancel action id missing"))?;
            match self.dispatcher.cancel(action_id) {
                Ok(outcome) => self.append_terminal(id, &outcome, false),
                Err(error) => {
                    self.append_terminal(
                        id,
                        &DispatchOutcome {
                            action_id: action_id.to_owned(),
                            status: ActionStatus::Unknown,
                            result: json!({"replay_allowed":false}),
                            error_code: Some(error.code.into()),
                        },
                        false,
                    )?;
                    Err(error)
                }
            }
        }
    }
    fn append_terminal(
        &self,
        command: &str,
        outcome: &DispatchOutcome,
        retain: bool,
    ) -> Result<()> {
        self.append_bounded_receipt(
            ReceiptEvent {
                sequence: 0,
                event_id: format!("command.{command}.{}", outcome.action_id),
                action_id: Some(outcome.action_id.clone()),
                event_type: format!("action.{}", outcome.status.as_str()),
                status: Some(outcome.status),
                occurred_at: now(),
                result_digest: None,
                error_code: outcome.error_code.clone(),
                summary: None,
                result: Some(outcome.result.clone()),
            },
            retain,
        )
    }

    fn append_unknown_if_missing(
        &self,
        event_id: &str,
        action_id: Option<String>,
        event_type: &str,
        error_code: &str,
    ) -> Result<()> {
        if self.outbox.contains_event(event_id)? {
            return Ok(());
        }
        self.append_bounded_receipt(
            ReceiptEvent {
                sequence: 0,
                event_id: event_id.to_owned(),
                action_id,
                event_type: event_type.to_owned(),
                status: Some(ActionStatus::Unknown),
                occurred_at: now(),
                result_digest: None,
                error_code: Some(error_code.to_owned()),
                summary: None,
                result: Some(json!({"replay_allowed":false})),
            },
            false,
        )
    }

    fn append_bounded_receipt(&self, mut event: ReceiptEvent, mut retain: bool) -> Result<()> {
        if event
            .result
            .as_ref()
            .is_some_and(|result| canonical_json(result).len() > 2 * 1024 * 1024)
        {
            event.event_type = if event.action_id.is_some() {
                "action.unknown".into()
            } else {
                event.event_type
            };
            event.status = Some(ActionStatus::Unknown);
            event.error_code = Some("local_result_too_large".into());
            event.result_digest = None;
            event.result = Some(json!({"replay_allowed":false}));
            retain = false;
        }
        self.outbox.append(event, retain)?;
        Ok(())
    }
}
pub fn shape(value: &Value, fields: &[&str]) -> Result<()> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::protocol("object required"))?;
    if obj.len() != fields.len() || fields.iter().any(|f| !obj.contains_key(*f)) {
        return Err(Error::protocol("unexpected object shape"));
    }
    Ok(())
}
pub fn required<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str> {
    value[key]
        .as_str()
        .filter(|s| valid_identifier(s, max))
        .ok_or_else(|| Error::protocol("required field invalid"))
}
pub fn pairing_redemption_digest(redemption: &PairingRedemption) -> String {
    let mut claims = redemption.capability_claims.clone();
    claims.sort();
    format!(
        "sha256:{}",
        digest_payload(
            &json!({"protocol_version":PROTOCOL_VERSION,"kind":"pairing_redeem","challenge_id":redemption.challenge_id,"user_code_digest":sha256(redemption.user_code.as_bytes()),"device_id":redemption.device_id,"proof_algorithm":redemption.proof_algorithm,"proof_public_key":redemption.proof_public_key,"display_name":redemption.display_name,"platform":redemption.platform,"node_version":redemption.node_version,"capability_claims":claims,"permission_snapshot_digest":redemption.permission_snapshot_digest})
        )
    )
}
fn parse_credential(
    value: &Value,
    expected: &str,
    allow_expired: bool,
) -> Result<Option<DeviceCredential>> {
    shape(
        value,
        &["protocol_version", "device_id", "credential", "expires_at"],
    )?;
    if value["protocol_version"] != PROTOCOL_VERSION
        || required(value, "device_id", 512)? != expected
    {
        return Err(Error::new(
            "pairing_error",
            "credential protocol or device mismatch",
        ));
    }
    let credential = DeviceCredential {
        device_id: expected.into(),
        credential: required(value, "credential", 8192)?.into(),
        expires_at: value["expires_at"]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| Error::new("pairing_error", "credential expiry invalid"))?,
    };
    if now() >= credential.expires_at {
        if allow_expired {
            return Ok(None);
        }
        return Err(Error::new(
            "pairing_error",
            "control plane returned expired credential",
        ));
    }
    Ok(Some(credential))
}
pub fn load_persisted_credential(
    store: &dyn SecureCredentialStore,
    reference: &str,
    device: &str,
) -> Result<Option<DeviceCredential>> {
    let Some(raw) = store.load(reference)? else {
        return Ok(None);
    };
    if raw.len() > 16 * 1024 {
        return Err(Error::new(
            "pairing_error",
            "stored credential record invalid",
        ));
    }
    parse_credential(&serde_json::from_slice(&raw)?, device, true)
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        fs,
        path::PathBuf,
        sync::{Condvar, atomic::AtomicUsize},
    };

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "local-node-transport-recovery-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct MemoryStore(Mutex<BTreeMap<String, Vec<u8>>>);
    impl SecureCredentialStore for MemoryStore {
        fn status(&self) -> CredentialStorageStatus {
            CredentialStorageStatus::Ready
        }
        fn load(&self, reference: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(reference).cloned())
        }
        fn store(&self, reference: &str, value: &[u8]) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(reference.to_owned(), value.to_vec());
            Ok(())
        }
    }

    struct AcceptVerifier;
    impl PlatformSignatureVerifier for AcceptVerifier {
        fn verify(&self, _: &str, _: &[u8], _: &str) -> bool {
            true
        }
    }

    struct TestDispatcher {
        cancel_calls: AtomicUsize,
        cancel_status: ActionStatus,
        cancel_result: Value,
    }
    impl DeviceActionDispatcher for TestDispatcher {
        fn connect(&self) -> Result<()> {
            Ok(())
        }
        fn dispatch(&self, _: &ActionContext, _: &Value) -> Result<DispatchOutcome> {
            Err(Error::new("test_error", "unused dispatch"))
        }
        fn cancel(&self, action_id: &str) -> Result<DispatchOutcome> {
            self.cancel_calls.fetch_add(1, Ordering::Relaxed);
            Ok(DispatchOutcome {
                action_id: action_id.to_owned(),
                status: self.cancel_status,
                result: self.cancel_result.clone(),
                error_code: None,
            })
        }
        fn emergency_stop(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
        fn disconnect(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    fn runner(
        temp: &TempDir,
        dispatcher: Arc<dyn DeviceActionDispatcher>,
    ) -> (DeviceOutboundRunner, Arc<MemoryStore>) {
        let store = Arc::new(MemoryStore::default());
        let identity = Arc::new(
            DeviceIdentity::load_or_create(&temp.0.join("identity"), store.as_ref()).unwrap(),
        );
        let credential_store: Arc<dyn SecureCredentialStore> = store.clone();
        let runner = DeviceOutboundRunner {
            identity,
            endpoint: OutboundControlPlane {
                url: "https://control.example.test/device".into(),
            },
            transport: None,
            credential: Mutex::new(None),
            credential_store: Some(credential_store),
            credential_reference: Some("outbound-credential".into()),
            platform_signature_verifier: Some(Arc::new(AcceptVerifier)),
            pairing_signer: None,
            outbox: Arc::new(ReceiptOutbox::open(&temp.0.join("outbox.sqlite")).unwrap()),
            replay_guard: Arc::new(ClaimReplayGuard::open(&temp.0.join("replay.sqlite")).unwrap()),
            dispatcher,
            doctor_provider: Arc::new(|| -> Result<DoctorReport> {
                Err(Error::new("test_error", "unused doctor"))
            }),
            allow_test_credentials: false,
            stopped: Arc::new(AtomicBool::new(false)),
        };
        (runner, store)
    }

    fn test_dispatcher(status: ActionStatus, result: Value) -> Arc<TestDispatcher> {
        Arc::new(TestDispatcher {
            cancel_calls: AtomicUsize::new(0),
            cancel_status: status,
            cancel_result: result,
        })
    }

    fn cancel_command(device: &str, command_id: &str, nonce: &str) -> Value {
        cancel_action_command(device, command_id, nonce, "action-1")
    }

    fn cancel_action_command(
        device: &str,
        command_id: &str,
        nonce: &str,
        action_id: &str,
    ) -> Value {
        json!({
            "kind":"cancel",
            "command_id":command_id,
            "device_id":device,
            "nonce":nonce,
            "issued_at":now()-1.0,
            "expires_at":now()+60.0,
            "platform_key_id":"test-key",
            "platform_signature":"test-signature",
            "action_id":action_id
        })
    }

    fn reserve_control(runner: &DeviceOutboundRunner, command: &Value) {
        let mut signed = command.clone();
        signed.as_object_mut().unwrap().remove("platform_signature");
        runner
            .replay_guard
            .reserve(
                command["command_id"].as_str().unwrap(),
                command["nonce"].as_str().unwrap(),
                &sha256(canonical_json(&signed).as_bytes()),
            )
            .unwrap();
    }

    fn claim_command(device: &str) -> Value {
        let arguments = json!({});
        let action = ActionContext {
            action_id: "long-action".into(),
            idempotency_key: "idempotency-long-action".into(),
            tenant_id: "tenant-1".into(),
            user_id: "user-1".into(),
            session_id: "session-1".into(),
            run_id: "run-1".into(),
            agent_id: "agent-1".into(),
            agent_version: "v1".into(),
            call_id: "call-1".into(),
            device_id: device.into(),
            envelope_version: 1,
            capability: "computer.click".into(),
            tool_name: "computer_click".into(),
            operation: "computer.click".into(),
            capability_lease_id: "lease-1".into(),
            resource_refs: vec!["window-1".into()],
            arguments_digest: digest_payload(&arguments),
            target_snapshot_digest: "target-digest".into(),
            policy_snapshot_digest: "policy-digest".into(),
            nonce: "claim-nonce-1".into(),
            issued_at: now() - 1.0,
            expires_at: now() + 300.0,
            platform_key_id: "test-key".into(),
            platform_signature: "test-signature".into(),
            approval: None,
            trace_context: None,
        };
        json!({
            "kind":"claim",
            "command_id":"claim-command-1",
            "action":serde_json::to_value(action).unwrap(),
            "normalized_arguments":arguments
        })
    }

    #[test]
    fn oversized_cancel_result_is_persisted_as_unknown() {
        let temp = TempDir::new();
        let dispatcher = test_dispatcher(
            ActionStatus::Succeeded,
            json!({"output":"x".repeat(3 * 1024 * 1024)}),
        );
        let (runner, _) = runner(&temp, dispatcher.clone());
        let command = cancel_command(&runner.identity.device_id, "oversized-1", "nonce-1");

        runner.process_command(&command).unwrap();

        let receipts = runner.outbox.pending(200).unwrap();
        assert_eq!(dispatcher.cancel_calls.load(Ordering::Relaxed), 1);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].event_type, "action.unknown");
        assert_eq!(receipts[0].status, Some(ActionStatus::Unknown));
        assert_eq!(
            receipts[0].error_code.as_deref(),
            Some("local_result_too_large")
        );
        assert_eq!(
            receipts[0].result_digest.as_deref(),
            Some(digest_payload(&json!({"replay_allowed":false})).as_str())
        );
    }

    #[test]
    fn consumed_command_without_receipt_recovers_unknown_without_dispatch() {
        let temp = TempDir::new();
        let dispatcher = test_dispatcher(ActionStatus::Succeeded, json!({"ok":true}));
        let (runner, _) = runner(&temp, dispatcher.clone());
        let command = cancel_command(&runner.identity.device_id, "crash-1", "nonce-1");
        reserve_control(&runner, &command);

        runner.process_command(&command).unwrap();

        let receipts = runner.outbox.pending(200).unwrap();
        assert_eq!(dispatcher.cancel_calls.load(Ordering::Relaxed), 0);
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].event_id, "command.crash-1.action-1");
        assert_eq!(receipts[0].event_type, "action.unknown");
        assert_eq!(receipts[0].status, Some(ActionStatus::Unknown));
        assert_eq!(
            receipts[0].error_code.as_deref(),
            Some("command_outcome_missing")
        );
    }

    #[test]
    fn duplicate_preserves_a_committed_terminal_receipt() {
        let temp = TempDir::new();
        let dispatcher = test_dispatcher(ActionStatus::Succeeded, json!({"ok":true}));
        let (runner, _) = runner(&temp, dispatcher.clone());
        let command = cancel_command(&runner.identity.device_id, "committed-1", "nonce-1");
        let committed = runner
            .outbox
            .append(
                ReceiptEvent {
                    sequence: 0,
                    event_id: "command.committed-1.action-1".into(),
                    action_id: Some("action-1".into()),
                    event_type: "action.succeeded".into(),
                    status: Some(ActionStatus::Succeeded),
                    occurred_at: now(),
                    result_digest: None,
                    error_code: None,
                    summary: None,
                    result: Some(json!({"committed":true})),
                },
                false,
            )
            .unwrap();
        runner.outbox.acknowledge(committed.sequence).unwrap();
        reserve_control(&runner, &command);

        runner.process_command(&command).unwrap();

        assert_eq!(dispatcher.cancel_calls.load(Ordering::Relaxed), 0);
        assert!(runner.outbox.pending(200).unwrap().is_empty());
        assert!(
            runner
                .outbox
                .contains_event("command.committed-1.action-1")
                .unwrap()
        );
    }

    #[test]
    fn paired_credential_is_reloaded_from_secure_storage() {
        let temp = TempDir::new();
        let (runner, store) = runner(&temp, test_dispatcher(ActionStatus::Succeeded, json!({})));
        let saved = json!({
            "protocol_version":PROTOCOL_VERSION,
            "device_id":runner.identity.device_id,
            "credential":"device-secret",
            "expires_at":now()+3600.0
        });
        store
            .store("outbound-credential", canonical_json(&saved).as_bytes())
            .unwrap();

        let loaded = runner.load_credential().unwrap().unwrap();

        assert_eq!(loaded.credential, "device-secret");
        assert_eq!(loaded.device_id, runner.identity.device_id);
    }

    #[test]
    fn receipt_batches_budget_escaped_json_and_keep_a_contiguous_prefix() {
        let base = ExchangeRequest {
            device_id: "device-1".into(),
            doctor: json!({"status":"ready"}),
            receipts: Vec::new(),
            sent_at: now(),
            protocol_version: PROTOCOL_VERSION.into(),
        };
        let escaped = "\0".repeat(349_500);
        let receipt = |sequence| ReceiptEvent {
            sequence,
            event_id: format!("event-{sequence}"),
            action_id: Some(format!("action-{sequence}")),
            event_type: "action.succeeded".into(),
            status: Some(ActionStatus::Succeeded),
            occurred_at: now(),
            result_digest: None,
            error_code: None,
            summary: None,
            result: Some(json!({"payload":escaped.clone()})),
        };
        let first = receipt(1);
        let second = receipt(2);
        let base_len = canonical_json(&base.as_value().unwrap()).len();
        let first_len = canonical_json(&first.as_value(true).unwrap()).len();
        assert!(canonical_json(first.result.as_ref().unwrap()).len() <= 2 * 1024 * 1024);
        assert!(first_len > 2 * 1024 * 1024);
        assert!(base_len + first_len * 2 + 1 > MAX_EXCHANGE_REQUEST_BYTES);

        let batch =
            bounded_receipt_prefix(&base, vec![first, second], MAX_EXCHANGE_REQUEST_BYTES).unwrap();
        let mut request = base.clone();
        request.receipts = batch
            .iter()
            .map(|event| event.as_value(true).unwrap())
            .collect();

        assert_eq!(
            batch.iter().map(|item| item.sequence).collect::<Vec<_>>(),
            [1]
        );
        assert!(canonical_json(&request.as_value().unwrap()).len() <= MAX_EXCHANGE_REQUEST_BYTES);
    }

    struct BlockingDispatcher {
        state: Mutex<(bool, bool)>,
        changed: Condvar,
    }
    impl BlockingDispatcher {
        fn new() -> Self {
            Self {
                state: Mutex::new((false, false)),
                changed: Condvar::new(),
            }
        }
        fn wait_until_started(&self) {
            let mut state = self.state.lock().unwrap();
            while !state.0 {
                state = self.changed.wait(state).unwrap();
            }
        }
    }
    impl DeviceActionDispatcher for BlockingDispatcher {
        fn connect(&self) -> Result<()> {
            Ok(())
        }
        fn dispatch(&self, action: &ActionContext, _: &Value) -> Result<DispatchOutcome> {
            let mut state = self.state.lock().unwrap();
            state.0 = true;
            self.changed.notify_all();
            while !state.1 {
                state = self.changed.wait(state).unwrap();
            }
            Ok(DispatchOutcome {
                action_id: action.action_id.clone(),
                status: ActionStatus::Cancelled,
                result: json!({"cancelled":true}),
                error_code: None,
            })
        }
        fn cancel(&self, action_id: &str) -> Result<DispatchOutcome> {
            if action_id != "long-action" {
                return Err(Error::new("test_error", "wrong action targeted"));
            }
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
            Ok(DispatchOutcome {
                action_id: action_id.to_owned(),
                status: ActionStatus::Cancelled,
                result: json!({}),
                error_code: None,
            })
        }
        fn emergency_stop(&self) -> Result<Vec<String>> {
            let mut state = self.state.lock().unwrap();
            state.1 = true;
            self.changed.notify_all();
            Ok(vec!["long-action".into()])
        }
        fn disconnect(&self) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn cancel_command_runs_while_fifo_dispatch_is_blocked() {
        let temp = TempDir::new();
        let dispatcher = Arc::new(BlockingDispatcher::new());
        let dispatcher_trait: Arc<dyn DeviceActionDispatcher> = dispatcher.clone();
        let (runner, _) = runner(&temp, dispatcher_trait);
        let processor = runner.command_processor();
        let pump = CommandPump::new(processor.clone());
        assert!(
            pump.try_enqueue(claim_command(&runner.identity.device_id))
                .unwrap()
                .is_none()
        );

        let wait_dispatch = dispatcher.clone();
        tokio::task::spawn_blocking(move || wait_dispatch.wait_until_started())
            .await
            .unwrap();
        processor
            .process_command(&cancel_action_command(
                &runner.identity.device_id,
                "cancel-long-action",
                "cancel-nonce-1",
                "long-action",
            ))
            .unwrap();
        pump.close().await.unwrap();

        let receipts = runner.outbox.pending(200).unwrap();
        assert_eq!(receipts.len(), 2);
        assert!(receipts.iter().any(|receipt| {
            receipt.event_id == "command.claim-command-1.long-action"
                && receipt.status == Some(ActionStatus::Cancelled)
        }));
        assert!(receipts.iter().any(|receipt| {
            receipt.event_id == "command.cancel-long-action.long-action"
                && receipt.status == Some(ActionStatus::Cancelled)
        }));
    }
}
