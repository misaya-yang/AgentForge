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
    collections::{BTreeMap, HashSet},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub const PROTOCOL_VERSION: &str = "ai-platform.local-node.v1";
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
            let raw = self
                .post(endpoint, request.as_value()?, Some(credential))
                .await?;
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
        let mut cancelled = self.runtime.process_runner.cancel_all()?;
        self.runtime.computer.emergency_stop()?;
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
}
impl ClaimReplayGuard {
    pub fn open(path: &Path) -> Result<Self> {
        let db = database::open(path)?;
        database::lock(&db)?.execute_batch("CREATE TABLE IF NOT EXISTS consumed_commands(command_id TEXT PRIMARY KEY,nonce TEXT UNIQUE NOT NULL,payload_digest TEXT NOT NULL,consumed_at REAL NOT NULL)")?;
        Ok(Self { db })
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
    pub stopped: AtomicBool,
}
impl DeviceOutboundRunner {
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
        let credential = self
            .credential
            .lock()
            .map_err(|_| Error::new("transport_unavailable", "credential state unavailable"))?;
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
            .credential
            .lock()
            .map_err(|_| Error::new("transport_unavailable", "credential state unavailable"))?
            .clone()
            .ok_or_else(|| Error::new("transport_unavailable", "credential missing"))?;
        let transport = self
            .transport
            .as_ref()
            .ok_or_else(|| Error::new("transport_unavailable", "outbound transport unavailable"))?;
        self.dispatcher.connect()?;
        let pending = self.outbox.pending(200)?;
        let request = ExchangeRequest {
            device_id: self.identity.device_id.clone(),
            doctor: report.runtime,
            receipts: pending
                .iter()
                .map(|e| e.as_value(true))
                .collect::<Result<_>>()?,
            sent_at: now(),
            protocol_version: PROTOCOL_VERSION.into(),
        };
        let result = async {
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
            for command in response.commands {
                if self.stopped.load(Ordering::Acquire) {
                    break;
                }
                self.process_command(&command)?;
            }
            Ok(count)
        }
        .await;
        if result.is_err() {
            for id in self.dispatcher.disconnect()? {
                self.append_terminal(
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
        }
        result
    }
    pub async fn run_forever(&self, heartbeat: Duration) -> Result<()> {
        if heartbeat < Duration::from_millis(250) || heartbeat > Duration::from_secs(300) {
            return Err(Error::new(
                "boundary_violation",
                "heartbeat interval invalid",
            ));
        }
        self.stopped.store(false, Ordering::Release);
        while !self.stopped.load(Ordering::Acquire) {
            self.run_once().await?;
            tokio::time::sleep(heartbeat).await;
        }
        Ok(())
    }
    pub fn stop(&self) -> Result<()> {
        self.stopped.store(true, Ordering::Release);
        self.dispatcher.emergency_stop()?;
        if let Some(transport) = &self.transport {
            transport.close()?;
        }
        Ok(())
    }
    pub fn process_command(&self, command: &Value) -> Result<()> {
        if canonical_json(command).len() > 65536 {
            return Err(Error::protocol("control command exceeds 64 KiB"));
        }
        match command["kind"].as_str() {
            Some("claim") => self.claim(command),
            Some("cancel") => self.control(command, false),
            Some("emergency_stop") => self.control(command, true),
            _ => Err(Error::protocol("unsupported control command")),
        }
    }
    fn claim(&self, command: &Value) -> Result<()> {
        shape(
            command,
            &["kind", "command_id", "action", "normalized_arguments"],
        )?;
        let command_id = required(command, "command_id", 512)?;
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
        if !self.replay_guard.reserve(
            command_id,
            &action.nonce,
            &sha256(&action.canonical_signed_payload()?),
        )? {
            return Ok(());
        }
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
            return Ok(());
        }
        if emergency {
            let mut stopped = self.dispatcher.emergency_stop()?;
            stopped.sort();
            self.outbox.append(
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
            )?;
            Ok(())
        } else {
            self.append_terminal(
                id,
                &self
                    .dispatcher
                    .cancel(required(command, "action_id", 512)?)?,
                false,
            )
        }
    }
    fn append_terminal(
        &self,
        command: &str,
        outcome: &DispatchOutcome,
        retain: bool,
    ) -> Result<()> {
        self.outbox.append(
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
        )?;
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
