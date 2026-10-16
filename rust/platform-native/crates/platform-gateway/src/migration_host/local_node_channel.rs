//! Native Local Node companion channel. This feature is opt-in and fails closed
//! unless the operator supplies its signing/TLS keys and the credential schema.
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    future::Future,
    net::SocketAddr,
    pin::Pin,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const DEVICE_CONTROL_PATH: &str = "/api/v1/local-nodes/control";
pub const WORKER_BASE_PATH: &str = "/internal/v2/agent-capabilities/local-node";
pub const PROTOCOL_V2: &str = "ai-platform.local-node.v2";
pub const WORKER_SCHEMA_V1: &str = "ai-platform.local-node.worker.v1";
const MAX_COMPANION_BODY: usize = 4 * 1024 * 1024;
const MAX_COMPANION_RESPONSE: usize = 900 * 1024;
const MAX_RECEIPTS: usize = 200;
const MAX_COMMANDS: usize = 100;
const PAIRING_TTL_SECONDS: u64 = 24 * 60 * 60;

pub type ChannelFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ChannelStoreError>> + Send + 'a>>;

#[derive(Clone)]
pub struct LocalNodeChannelConfig {
    pub platform_key_id: String,
    pub platform_signing_seed_b64: String,
    pub tls_certificate_path: std::path::PathBuf,
    pub tls_private_key_path: std::path::PathBuf,
    pub tls_listen_address: SocketAddr,
}

impl LocalNodeChannelConfig {
    /// Missing values leave the feature disabled. Partial explicit configuration
    /// fails closed instead of starting a plaintext or unsigned endpoint.
    pub fn from_environment(
        values: &std::collections::BTreeMap<String, String>,
    ) -> Result<Option<Self>, &'static str> {
        let enabled = values
            .get("AI_PLATFORM_LOCAL_NODE_CHANNEL_ENABLED")
            .map(String::as_str)
            .unwrap_or("false");
        match enabled {
            "false" | "0" | "disabled" => return Ok(None),
            "true" | "1" | "enabled" => {}
            _ => return Err("local_node_channel_enabled"),
        }
        let required = |name: &'static str| {
            values
                .get(name)
                .map(String::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or(name)
        };
        let platform_key_id = required("AI_PLATFORM_LOCAL_NODE_PLATFORM_KEY_ID")?.to_owned();
        let platform_signing_seed_b64 =
            required("AI_PLATFORM_LOCAL_NODE_PLATFORM_SIGNING_SEED_B64")?.to_owned();
        let tls_certificate_path =
            std::path::PathBuf::from(required("AI_PLATFORM_LOCAL_NODE_TLS_CERT_PATH")?);
        let tls_private_key_path =
            std::path::PathBuf::from(required("AI_PLATFORM_LOCAL_NODE_TLS_KEY_PATH")?);
        if !tls_certificate_path.is_absolute() || !tls_private_key_path.is_absolute() {
            return Err("local_node_channel_tls_path");
        }
        let tls_listen_address = values
            .get("AI_PLATFORM_LOCAL_NODE_TLS_LISTEN")
            .map(String::as_str)
            .unwrap_or("0.0.0.0:8443")
            .parse()
            .map_err(|_| "local_node_channel_tls_listen")?;
        if platform_key_id.len() > 255
            || platform_key_id
                .bytes()
                .any(|byte| !(byte.is_ascii_alphanumeric() || b"._:-".contains(&byte)))
        {
            return Err("local_node_channel_platform_key_id");
        }
        Ok(Some(Self {
            platform_key_id,
            platform_signing_seed_b64,
            tls_certificate_path,
            tls_private_key_path,
            tls_listen_address,
        }))
    }
}

#[derive(Clone)]
pub struct PlatformSigner {
    key_id: Arc<str>,
    seed: Arc<[u8; 32]>,
    public_key_b64: Arc<str>,
}

impl PlatformSigner {
    pub fn from_config(config: &LocalNodeChannelConfig) -> Result<Self, &'static str> {
        let seed = URL_SAFE_NO_PAD
            .decode(&config.platform_signing_seed_b64)
            .map_err(|_| "local_node_channel_signing_seed")?;
        let seed: [u8; 32] = seed
            .try_into()
            .map_err(|_| "local_node_channel_signing_seed")?;
        let key = Ed25519KeyPair::from_seed_unchecked(&seed)
            .map_err(|_| "local_node_channel_signing_seed")?;
        Ok(Self {
            key_id: Arc::from(config.platform_key_id.as_str()),
            seed: Arc::new(seed),
            public_key_b64: Arc::from(URL_SAFE_NO_PAD.encode(key.public_key().as_ref())),
        })
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn public_key_b64(&self) -> &str {
        &self.public_key_b64
    }

    pub fn sign(&self, message: &[u8]) -> Result<String, ChannelStoreError> {
        let key = Ed25519KeyPair::from_seed_unchecked(self.seed.as_ref())
            .map_err(|_| ChannelStoreError::Unavailable)?;
        Ok(URL_SAFE_NO_PAD.encode(key.sign(message).as_ref()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChannelStoreError {
    Rejected,
    Unavailable,
    Conflict,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PairingRedeemRequest {
    pub protocol_version: String,
    pub kind: String,
    pub challenge_id: String,
    pub user_code: String,
    pub device_id: String,
    pub display_name: String,
    pub platform: String,
    pub node_version: String,
    pub capability_claims: Vec<String>,
    pub permission_snapshot_digest: String,
    pub proof_algorithm: String,
    pub proof_public_key: String,
    pub device_proof: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CompanionReceipt {
    pub sequence: u64,
    pub event_id: String,
    pub action_id: Option<String>,
    pub event_type: String,
    pub status: Option<String>,
    pub occurred_at: f64,
    pub result_digest: Option<String>,
    pub error_code: Option<String>,
    pub summary: Option<String>,
    pub result: Option<Value>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct HeartbeatRequest {
    pub protocol_version: String,
    pub kind: String,
    pub device_id: String,
    pub receipts: Vec<CompanionReceipt>,
    pub doctor: Value,
    pub sent_at: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerContext {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub lease_id: String,
    pub attempt_id: String,
    pub capability_id: String,
    pub capability_revision: u64,
    pub effect: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDescribeRequest {
    pub schema_version: String,
    pub context: WorkerContext,
    pub device_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDispatchRequest {
    pub schema_version: String,
    pub context: WorkerContext,
    pub device_id: String,
    pub operation: String,
    pub arguments: Value,
    pub arguments_hash: String,
    pub idempotency_key: String,
    pub approval_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerStatusRequest {
    pub schema_version: String,
    pub context: WorkerContext,
    pub device_id: String,
    pub after_sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerCancelRequest {
    pub schema_version: String,
    pub context: WorkerContext,
    pub device_id: String,
    pub action_id: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerDescriptor {
    pub device_id: String,
    pub capability_revision: u64,
    pub capabilities: std::collections::BTreeSet<String>,
    pub online: bool,
    pub expires_at_epoch_ms: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerReceiptEvent {
    pub execution_id: String,
    pub device_id: String,
    pub sequence: u64,
    pub status: String,
    pub event: String,
    pub payload: std::collections::BTreeMap<String, Value>,
}

#[derive(Clone)]
pub struct PendingClaim {
    pub command_id: String,
    pub action: Value,
    pub normalized_arguments: Value,
}

#[derive(Clone, Debug)]
pub struct PendingControl {
    pub kind: String,
    pub command_id: String,
    pub action_id: Option<String>,
    pub device_id: String,
    pub issued_at: f64,
    pub expires_at: f64,
    pub nonce: String,
}

#[derive(Clone)]
pub struct HeartbeatResult {
    pub accepted_through_sequence: u64,
    pub claims: Vec<PendingClaim>,
    pub controls: Vec<PendingControl>,
}

/// SQL-backed owner of pairing, credentials, device receipt cursors and the
/// Local Node execution ledger. Implementations must never return plaintext
/// credentials or accept model-supplied channel/grant selectors.
pub trait LocalNodeChannelRepository: Send + Sync {
    fn schema_ready(&self) -> ChannelFuture<'_, bool>;
    fn pair<'a>(
        &'a self,
        request: &'a PairingRedeemRequest,
        channel_fingerprint: &'a str,
        credential_sha256: &'a str,
        channel_id: &'a str,
        expires_at: f64,
    ) -> ChannelFuture<'a, ()>;
    fn heartbeat<'a>(
        &'a self,
        request: &'a HeartbeatRequest,
        credential_sha256: &'a str,
    ) -> ChannelFuture<'a, HeartbeatResult>;
    fn describe<'a>(
        &'a self,
        context: &'a WorkerContext,
        device_id: &'a str,
    ) -> ChannelFuture<'a, WorkerDescriptor>;
    fn dispatch<'a>(
        &'a self,
        context: &'a WorkerContext,
        request: &'a WorkerDispatchRequest,
    ) -> ChannelFuture<'a, Value>;
    fn status<'a>(
        &'a self,
        context: &'a WorkerContext,
        device_id: &'a str,
        after_sequence: u64,
    ) -> ChannelFuture<'a, Vec<WorkerReceiptEvent>>;
    fn cancel<'a>(
        &'a self,
        context: &'a WorkerContext,
        request: &'a WorkerCancelRequest,
    ) -> ChannelFuture<'a, Value>;
    fn verify_device_credential<'a>(
        &'a self,
        credential_sha256: &'a str,
        expected_device_id: &'a str,
    ) -> ChannelFuture<'a, crate::migration_local_node::protocol::LocalNodeDeviceScope>;
    fn dispatch_adapter_accepted<'a>(
        &'a self,
        action: &'a crate::migration_local_node::protocol::LocalNodeAction,
    ) -> ChannelFuture<'a, ()>;
}

#[derive(Clone)]
pub struct LocalNodeChannelState {
    pub repository: Arc<dyn LocalNodeChannelRepository>,
    pub signer: PlatformSigner,
    pub worker_authorization: Arc<dyn crate::migration_local_node::broker::BrokerAuthorization>,
    replay: Arc<ProofReplayCache>,
    rate_limit: Arc<ChannelRateLimit>,
}

impl LocalNodeChannelState {
    pub async fn new(
        repository: Arc<dyn LocalNodeChannelRepository>,
        signer: PlatformSigner,
        worker_authorization: Arc<dyn crate::migration_local_node::broker::BrokerAuthorization>,
    ) -> Result<Option<Self>, &'static str> {
        let schema_ready = repository
            .schema_ready()
            .await
            .map_err(|_| "local_node_channel_schema")?;
        if !schema_ready {
            return Ok(None);
        }
        Ok(Some(Self {
            repository,
            signer,
            worker_authorization,
            replay: Arc::new(ProofReplayCache::default()),
            rate_limit: Arc::new(ChannelRateLimit::default()),
        }))
    }
}

#[derive(Default)]
struct ProofReplayCache {
    used: Mutex<HashMap<String, i64>>,
}

impl ProofReplayCache {
    fn reserve(&self, key: String, expires_at: i64, now: i64) -> bool {
        let Ok(mut used) = self.used.lock() else {
            return false;
        };
        used.retain(|_, expiry| *expiry > now);
        if used.contains_key(&key) || used.len() >= 20_000 {
            return false;
        }
        used.insert(key, expires_at);
        true
    }
}

#[derive(Default)]
struct ChannelRateLimit {
    buckets: Mutex<HashMap<String, RateBucket>>,
}
struct RateBucket {
    window_started: Instant,
    count: u16,
}
impl ChannelRateLimit {
    fn allow(&self, key: String, limit: u16, window: Duration) -> bool {
        let Ok(mut buckets) = self.buckets.lock() else {
            return false;
        };
        let now = Instant::now();
        buckets.retain(|_, bucket| now.duration_since(bucket.window_started) < window);
        if !buckets.contains_key(&key) && buckets.len() >= 4096 {
            return false;
        }
        let bucket = buckets.entry(key).or_insert(RateBucket {
            window_started: now,
            count: 0,
        });
        if now.duration_since(bucket.window_started) >= window {
            bucket.window_started = now;
            bucket.count = 0;
        }
        if bucket.count >= limit {
            return false;
        }
        bucket.count += 1;
        true
    }
}

pub fn local_node_companion_router(state: Option<Arc<LocalNodeChannelState>>) -> Router {
    Router::new()
        .route(DEVICE_CONTROL_PATH, post(companion_post))
        .with_state(state)
}

/// Worker dispatch stays on the existing HTTP listener behind its independent
/// capability proof. Companion credentials and payloads only exist on TLS.
pub fn local_node_worker_router(state: Option<Arc<LocalNodeChannelState>>) -> Router {
    Router::new()
        .route(
            &format!("{WORKER_BASE_PATH}/describe"),
            post(worker_describe),
        )
        .route(
            &format!("{WORKER_BASE_PATH}/dispatch"),
            post(worker_dispatch),
        )
        .route(&format!("{WORKER_BASE_PATH}/status"), post(worker_status))
        .route(&format!("{WORKER_BASE_PATH}/cancel"), post(worker_cancel))
        .with_state(state)
}

async fn companion_post(
    State(state): State<Option<Arc<LocalNodeChannelState>>>,
    request: Request,
) -> Response {
    if request
        .extensions()
        .get::<super::local_node_tls::HttpsRequest>()
        .is_none()
    {
        return error_response(StatusCode::UPGRADE_REQUIRED, "HTTPS_REQUIRED");
    }
    let Some(state) = state else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "LOCAL_NODE_CHANNEL_UNAVAILABLE",
        );
    };
    let peer = request
        .extensions()
        .get::<ConnectInfo<super::local_node_tls::TlsPeerAddress>>()
        .map(|ConnectInfo(super::local_node_tls::TlsPeerAddress(address))| address.ip().to_string())
        .unwrap_or_else(|| "unknown-peer".into());
    let headers = request.headers().clone();
    if headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        != Some("application/json")
    {
        return error_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, "JSON_REQUIRED");
    }
    let bytes = match to_bytes(request.into_body(), MAX_COMPANION_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => return error_response(StatusCode::PAYLOAD_TOO_LARGE, "BODY_TOO_LARGE"),
    };
    let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
        return error_response(StatusCode::BAD_REQUEST, "INVALID_JSON");
    };
    match value.get("kind").and_then(Value::as_str) {
        Some("pairing_redeem") => {
            if !state
                .rate_limit
                .allow(format!("pair:{peer}"), 5, Duration::from_secs(60))
            {
                return error_response(StatusCode::TOO_MANY_REQUESTS, "PAIRING_RATE_LIMITED");
            }
            let Ok(pairing) = serde_json::from_value::<PairingRedeemRequest>(value.clone()) else {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "PAIRING_BODY_INVALID");
            };
            match redeem_pairing(&state, pairing).await {
                Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                Err(status) => error_response(status, "PAIRING_REDEMPTION_REJECTED"),
            }
        }
        Some("heartbeat") => {
            if !state
                .rate_limit
                .allow(format!("heartbeat:{peer}"), 60, Duration::from_secs(60))
            {
                return error_response(StatusCode::TOO_MANY_REQUESTS, "HEARTBEAT_RATE_LIMITED");
            }
            let Ok(heartbeat) = serde_json::from_value::<HeartbeatRequest>(value) else {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "HEARTBEAT_BODY_INVALID");
            };
            match exchange_heartbeat(&state, &headers, heartbeat).await {
                Ok(response) => (StatusCode::OK, Json(response)).into_response(),
                Err((status, code)) => error_response(status, code),
            }
        }
        _ => error_response(StatusCode::UNPROCESSABLE_ENTITY, "LOCAL_NODE_KIND_INVALID"),
    }
}

async fn redeem_pairing(
    state: &LocalNodeChannelState,
    request: PairingRedeemRequest,
) -> Result<Value, StatusCode> {
    validate_pairing(&request)?;
    let raw = serde_json::to_value(&request).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut preproof = raw.clone();
    let object = preproof.as_object_mut().ok_or(StatusCode::BAD_REQUEST)?;
    object.remove("device_proof");
    object.remove("user_code");
    object.insert(
        "user_code_digest".into(),
        json!(sha256(request.user_code.as_bytes())),
    );
    let proof_message = json!({"redemption_digest":format!("sha256:{}", digest_value(&preproof))});
    let public_key = URL_SAFE_NO_PAD
        .decode(&request.proof_public_key)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let device_signature = URL_SAFE_NO_PAD
        .decode(&request.device_proof)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    UnparsedPublicKey::new(&ED25519, &public_key)
        .verify(canonical_json(&proof_message).as_bytes(), &device_signature)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;

    let now = unix_now();
    let expires_at = now + PAIRING_TTL_SECONDS as f64;
    let mut token = [0u8; 32];
    SystemRandom::new()
        .fill(&mut token)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let credential = format!("lnv2.{}", URL_SAFE_NO_PAD.encode(token));
    let credential_sha256 = sha256(credential.as_bytes());
    let channel_id = uuid::Uuid::new_v4().to_string();
    let fingerprint = format!("sha256:{}", sha256(&public_key));
    let mut response = json!({
        "protocol_version": PROTOCOL_V2,
        "challenge_id": &request.challenge_id,
        "device_id": &request.device_id,
        "credential": &credential,
        "expires_at": expires_at,
        "platform_key_id": state.signer.key_id(),
    });
    let signature = state
        .signer
        .sign(canonical_json(&response).as_bytes())
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    response["platform_signature"] = json!(signature);
    state
        .repository
        .pair(
            &request,
            &fingerprint,
            &credential_sha256,
            &channel_id,
            expires_at,
        )
        .await
        .map_err(|error| match error {
            ChannelStoreError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ChannelStoreError::Rejected | ChannelStoreError::Conflict => StatusCode::CONFLICT,
        })?;
    Ok(response)
}

async fn exchange_heartbeat(
    state: &LocalNodeChannelState,
    headers: &HeaderMap,
    request: HeartbeatRequest,
) -> Result<Value, (StatusCode, &'static str)> {
    validate_heartbeat(&request)
        .map_err(|_| (StatusCode::UNPROCESSABLE_ENTITY, "HEARTBEAT_BODY_INVALID"))?;
    let token = device_credential(headers)
        .ok_or((StatusCode::UNAUTHORIZED, "DEVICE_CREDENTIAL_REQUIRED"))?;
    let credential_sha256 = sha256(token.as_bytes());
    let result = state
        .repository
        .heartbeat(&request, &credential_sha256)
        .await
        .map_err(|error| match error {
            ChannelStoreError::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "HEARTBEAT_STORE_UNAVAILABLE",
            ),
            ChannelStoreError::Conflict => (StatusCode::CONFLICT, "HEARTBEAT_SEQUENCE_CONFLICT"),
            ChannelStoreError::Rejected => (StatusCode::UNAUTHORIZED, "DEVICE_CREDENTIAL_REJECTED"),
        })?;
    if result.claims.len() + result.controls.len() > MAX_COMMANDS {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "COMMAND_BATCH_UNAVAILABLE"));
    }
    let accepted_through_sequence = result.accepted_through_sequence;
    let mut commands = Vec::with_capacity(result.claims.len() + result.controls.len());
    for claim in result.claims {
        let command = sign_claim_command(&state.signer, claim).map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "COMMAND_SIGNING_UNAVAILABLE",
            )
        })?;
        commands.push(command);
        if canonical_json(&json!({
            "protocol_version":PROTOCOL_V2,
            "accepted_through_sequence":accepted_through_sequence,
            "commands":commands,
        }))
        .len()
            > MAX_COMPANION_RESPONSE
        {
            commands.pop();
            break;
        }
    }
    for control in result.controls {
        let command = sign_control_command(&state.signer, control).map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "COMMAND_SIGNING_UNAVAILABLE",
            )
        })?;
        commands.push(command);
        if canonical_json(&json!({
            "protocol_version":PROTOCOL_V2,
            "accepted_through_sequence":accepted_through_sequence,
            "commands":commands,
        }))
        .len()
            > MAX_COMPANION_RESPONSE
        {
            commands.pop();
            break;
        }
    }
    let response = json!({
        "protocol_version": PROTOCOL_V2,
        "accepted_through_sequence": accepted_through_sequence,
        "commands": commands,
    });
    Ok(response)
}

async fn worker_describe(
    State(state): State<Option<Arc<LocalNodeChannelState>>>,
    headers: HeaderMap,
    Json(request): Json<WorkerDescribeRequest>,
) -> Response {
    worker_request(
        state,
        headers,
        WORKER_BASE_PATH.to_owned() + "/describe",
        request,
    )
    .await
}

async fn worker_dispatch(
    State(state): State<Option<Arc<LocalNodeChannelState>>>,
    headers: HeaderMap,
    Json(request): Json<WorkerDispatchRequest>,
) -> Response {
    worker_request(
        state,
        headers,
        WORKER_BASE_PATH.to_owned() + "/dispatch",
        request,
    )
    .await
}

async fn worker_status(
    State(state): State<Option<Arc<LocalNodeChannelState>>>,
    headers: HeaderMap,
    Json(request): Json<WorkerStatusRequest>,
) -> Response {
    worker_request(
        state,
        headers,
        WORKER_BASE_PATH.to_owned() + "/status",
        request,
    )
    .await
}

async fn worker_cancel(
    State(state): State<Option<Arc<LocalNodeChannelState>>>,
    headers: HeaderMap,
    Json(request): Json<WorkerCancelRequest>,
) -> Response {
    worker_request(
        state,
        headers,
        WORKER_BASE_PATH.to_owned() + "/cancel",
        request,
    )
    .await
}

async fn worker_request<T: Serialize + for<'de> Deserialize<'de>>(
    state: Option<Arc<LocalNodeChannelState>>,
    headers: HeaderMap,
    path: String,
    request: T,
) -> Response {
    let Some(state) = state else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "LOCAL_NODE_CHANNEL_UNAVAILABLE",
        );
    };
    let Ok(body) = serde_json::to_value(&request) else {
        return error_response(StatusCode::BAD_REQUEST, "WORKER_BODY_INVALID");
    };
    let scope = match state.worker_authorization.authorize(&headers, &path, &body) {
        Ok(scope) => scope,
        Err(_) => return error_response(StatusCode::UNAUTHORIZED, "CAPABILITY_PROOF_REJECTED"),
    };
    let Some(context) = body
        .get("context")
        .cloned()
        .and_then(|v| serde_json::from_value::<WorkerContext>(v).ok())
    else {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_CONTEXT_INVALID");
    };
    if !context_matches_headers(&context, &scope, &headers)
        || !validate_worker_context(&context)
        || body["schema_version"] != WORKER_SCHEMA_V1
    {
        return error_response(StatusCode::FORBIDDEN, "WORKER_SCOPE_MISMATCH");
    }
    let Some(device_id) = body["device_id"]
        .as_str()
        .filter(|value| valid_text(value, 255))
    else {
        return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_DEVICE_INVALID");
    };
    let proof = match capability_proof(&headers) {
        Some(proof) => proof,
        None => return error_response(StatusCode::UNAUTHORIZED, "CAPABILITY_PROOF_REJECTED"),
    };
    let now = unix_now() as i64;
    if proof.expires_at <= now || proof.expires_at > now + 30 {
        return error_response(StatusCode::UNAUTHORIZED, "CAPABILITY_PROOF_EXPIRED");
    }
    let nonce_key = format!(
        "{}:{}:{}:{}",
        context.tenant_id, context.execution_id, path, proof.nonce
    );
    if !state.replay.reserve(nonce_key, proof.expires_at, now) {
        return error_response(StatusCode::CONFLICT, "CAPABILITY_PROOF_REPLAYED");
    }
    match path.as_str() {
        p if p.ends_with("/describe") => {
            let Ok(req) = serde_json::from_value::<WorkerDescribeRequest>(body) else {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_BODY_INVALID");
            };
            match state.repository.describe(&context, &req.device_id).await {
                Ok(result) => Json(result).into_response(),
                Err(ChannelStoreError::Rejected | ChannelStoreError::Conflict) => {
                    error_response(StatusCode::CONFLICT, "LOCAL_NODE_SCOPE_REJECTED")
                }
                Err(ChannelStoreError::Unavailable) => error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "LOCAL_NODE_STORE_UNAVAILABLE",
                ),
            }
        }
        p if p.ends_with("/dispatch") => {
            let Ok(req) = serde_json::from_value::<WorkerDispatchRequest>(body) else {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_BODY_INVALID");
            };
            if !valid_worker_dispatch(&req) {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_DISPATCH_INVALID");
            }
            match state.repository.dispatch(&context, &req).await {
                Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
                Err(ChannelStoreError::Conflict) => {
                    error_response(StatusCode::CONFLICT, "LOCAL_NODE_DISPATCH_REJECTED")
                }
                Err(_) => error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "LOCAL_NODE_DISPATCH_UNKNOWN",
                ),
            }
        }
        p if p.ends_with("/status") => {
            let Ok(req) = serde_json::from_value::<WorkerStatusRequest>(body) else {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_BODY_INVALID");
            };
            match state
                .repository
                .status(&context, &req.device_id, req.after_sequence)
                .await
            {
                Ok(events) => Json(json!({"events":events})).into_response(),
                Err(ChannelStoreError::Rejected | ChannelStoreError::Conflict) => {
                    error_response(StatusCode::CONFLICT, "LOCAL_NODE_STATUS_REJECTED")
                }
                Err(ChannelStoreError::Unavailable) => error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "LOCAL_NODE_STORE_UNAVAILABLE",
                ),
            }
        }
        p if p.ends_with("/cancel") => {
            let Ok(req) = serde_json::from_value::<WorkerCancelRequest>(body) else {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_BODY_INVALID");
            };
            if req.action_id != context.execution_id
                || req.reason.as_ref().is_some_and(|v| !valid_text(v, 128))
            {
                return error_response(StatusCode::UNPROCESSABLE_ENTITY, "WORKER_CANCEL_INVALID");
            }
            match state.repository.cancel(&context, &req).await {
                Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
                Err(ChannelStoreError::Rejected | ChannelStoreError::Conflict) => {
                    error_response(StatusCode::CONFLICT, "LOCAL_NODE_CANCEL_REJECTED")
                }
                Err(ChannelStoreError::Unavailable) => error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "LOCAL_NODE_STORE_UNAVAILABLE",
                ),
            }
        }
        _ => error_response(StatusCode::NOT_FOUND, "LOCAL_NODE_ROUTE_NOT_FOUND"),
    }
}

fn sign_claim_command(
    signer: &PlatformSigner,
    command: PendingClaim,
) -> Result<Value, ChannelStoreError> {
    if !command.action.is_object() || !command.normalized_arguments.is_object() {
        return Err(ChannelStoreError::Rejected);
    }
    let mut action = command.action;
    let object = action.as_object_mut().ok_or(ChannelStoreError::Rejected)?;
    let operation = object
        .get("operation")
        .and_then(Value::as_str)
        .ok_or(ChannelStoreError::Rejected)?;
    let mut intent = command.normalized_arguments.clone();
    if operation == "file.write" {
        let args = intent.as_object_mut().ok_or(ChannelStoreError::Rejected)?;
        args.remove("content_base64");
        let expected_target = command.normalized_arguments["expected_hash"]
            .as_str()
            .unwrap_or("missing");
        if object.get("target_snapshot_digest").and_then(Value::as_str) != Some(expected_target) {
            return Err(ChannelStoreError::Rejected);
        }
    }
    let expected_digest = digest_value(&intent);
    if object.get("arguments_digest").and_then(Value::as_str) != Some(expected_digest.as_str()) {
        return Err(ChannelStoreError::Rejected);
    }
    object.insert("platform_key_id".into(), json!(signer.key_id()));
    object.insert("platform_signature".into(), json!("pending"));
    object.entry("approval").or_insert(Value::Null);
    object.entry("trace_context").or_insert(Value::Null);
    let signature = signer.sign(canonical_action_payload(&action)?.as_bytes())?;
    action["platform_signature"] = json!(signature);
    Ok(json!({
        "kind":"claim",
        "command_id":command.command_id,
        "action":action,
        "normalized_arguments":command.normalized_arguments,
    }))
}

fn sign_control_command(
    signer: &PlatformSigner,
    command: PendingControl,
) -> Result<Value, ChannelStoreError> {
    if !["cancel", "emergency_stop"].contains(&command.kind.as_str())
        || !valid_text(&command.command_id, 512)
        || !valid_text(&command.device_id, 255)
        || !command.issued_at.is_finite()
        || !command.expires_at.is_finite()
        || command.expires_at <= command.issued_at
        || command.expires_at - command.issued_at > 600.0
    {
        return Err(ChannelStoreError::Rejected);
    }
    let mut value = json!({
        "kind":command.kind,
        "command_id":command.command_id,
        "device_id":command.device_id,
        "nonce":command.nonce,
        "issued_at":command.issued_at,
        "expires_at":command.expires_at,
        "platform_key_id":signer.key_id(),
    });
    if let Some(action_id) = command.action_id {
        value["action_id"] = json!(action_id);
    }
    let signature = signer.sign(canonical_json(&value).as_bytes())?;
    value["platform_signature"] = json!(signature);
    Ok(value)
}

fn canonical_action_payload(action: &Value) -> Result<String, ChannelStoreError> {
    let mut payload = action.clone();
    let object = payload.as_object_mut().ok_or(ChannelStoreError::Rejected)?;
    object.remove("platform_signature");
    object.remove("trace_context");
    let resources = object
        .get("resource_refs")
        .cloned()
        .ok_or(ChannelStoreError::Rejected)?;
    object.insert(
        "resource_refs_digest".into(),
        json!(digest_value(&resources)),
    );
    Ok(canonical_json(&payload))
}

#[derive(Clone, Debug)]
struct CapabilityProof {
    expires_at: i64,
    nonce: String,
}

fn capability_proof(headers: &HeaderMap) -> Option<CapabilityProof> {
    let encoded = headers
        .get("x-ai-capability-proof")?
        .to_str()
        .ok()?
        .strip_prefix("v1.")?;
    if encoded.len() > 4096 {
        return None;
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let mut envelope = serde_json::from_slice::<Value>(&bytes).ok()?;
    let object = envelope.as_object_mut()?;
    let signature = object
        .remove("signature")
        .and_then(|v| v.as_str().map(str::to_owned))?;
    if signature.len() != 64 {
        return None;
    }
    let expires_at = envelope.get("expires_at")?.as_i64()?;
    let nonce = envelope.get("nonce")?.as_str()?.to_owned();
    if nonce.is_empty()
        || nonce.len() > 128
        || envelope.get("schema_version")? != "ai-platform-capability-proof/v1"
    {
        return None;
    }
    Some(CapabilityProof { expires_at, nonce })
}

fn validate_pairing(request: &PairingRedeemRequest) -> Result<(), StatusCode> {
    let bounded = |value: &str, max: usize| {
        !value.is_empty() && value.chars().count() <= max && !value.chars().any(|c| c < ' ')
    };
    if request.protocol_version != PROTOCOL_V2
        || request.kind != "pairing_redeem"
        || uuid::Uuid::parse_str(&request.challenge_id).is_err()
        || !bounded(&request.user_code, 64)
        || !bounded(&request.device_id, 255)
        || !request
            .device_id
            .strip_prefix("dev_")
            .is_some_and(|suffix| lower_hex(suffix, 24))
        || !bounded(&request.display_name, 80)
        || !bounded(&request.node_version, 64)
        || !["macos", "windows", "linux"].contains(&request.platform.as_str())
        || request.capability_claims.is_empty()
        || request.capability_claims.len() > 32
        || request.capability_claims.iter().any(|v| !bounded(v, 128))
        || request.capability_claims.windows(2).any(|w| w[0] >= w[1])
        || !lower_hex(&request.permission_snapshot_digest, 64)
        || request.proof_algorithm != "ed25519"
        || !bounded(&request.proof_public_key, 128)
        || !bounded(&request.device_proof, 256)
    {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let public_key = URL_SAFE_NO_PAD
        .decode(&request.proof_public_key)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    let signature = URL_SAFE_NO_PAD
        .decode(&request.device_proof)
        .map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    if public_key.len() != 32 || signature.len() != 64 {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    Ok(())
}

fn validate_heartbeat(request: &HeartbeatRequest) -> Result<(), ()> {
    if request.protocol_version != PROTOCOL_V2
        || request.kind != "heartbeat"
        || !valid_text(&request.device_id, 255)
        || !request.device_id.starts_with("dev_")
        || request.receipts.len() > MAX_RECEIPTS
        || !request.sent_at.is_finite()
        || (unix_now() - request.sent_at).abs() > 300.0
        || !request.doctor.is_object()
        || canonical_json(&request.doctor).len() > 64 * 1024
    {
        return Err(());
    }
    if !request.doctor["permission_snapshot_digest"]
        .as_str()
        .is_some_and(|digest| lower_hex(digest, 64))
    {
        return Err(());
    }
    let claims = request.doctor["capability_claims"].as_array().ok_or(())?;
    if claims.len() > 32
        || claims.iter().any(|claim| {
            claim.as_str().is_none_or(|claim| {
                !matches!(
                    claim,
                    "file.read" | "file.search" | "file.watch" | "file.write"
                )
            })
        })
        || claims.windows(2).any(|pair| {
            pair[0]
                .as_str()
                .zip(pair[1].as_str())
                .is_none_or(|(left, right)| left >= right)
        })
    {
        return Err(());
    }
    let mut previous = None;
    for receipt in &request.receipts {
        if receipt.sequence == 0
            || previous.is_some_and(|last| receipt.sequence != last + 1)
            || !valid_text(&receipt.event_id, 512)
            || !valid_text(&receipt.event_type, 80)
            || receipt
                .action_id
                .as_ref()
                .is_some_and(|v| !valid_text(v, 512))
            || receipt
                .error_code
                .as_ref()
                .is_some_and(|v| !valid_text(v, 80))
            || receipt
                .summary
                .as_ref()
                .is_some_and(|v| !valid_text(v, 500))
            || !receipt.occurred_at.is_finite()
            || receipt
                .result
                .as_ref()
                .is_some_and(|v| canonical_json(v).len() > 2 * 1024 * 1024)
            || receipt
                .result_digest
                .as_ref()
                .is_some_and(|v| !lower_hex(v, 64))
            || receipt
                .result
                .as_ref()
                .zip(receipt.result_digest.as_ref())
                .is_some_and(|(result, digest)| digest_value(result) != *digest)
        {
            return Err(());
        }
        if let Some(status) = receipt.status.as_deref()
            && ![
                "proposed",
                "policy_check",
                "awaiting_approval",
                "dispatched",
                "running",
                "observed",
                "succeeded",
                "failed",
                "cancelled",
                "timeout",
                "unknown",
                "side_effect_unknown",
            ]
            .contains(&status)
        {
            return Err(());
        }
        previous = Some(receipt.sequence);
    }
    Ok(())
}

fn valid_worker_dispatch(request: &WorkerDispatchRequest) -> bool {
    let full_arguments = json!({
        "operation": request.operation,
        "arguments": request.arguments,
    });
    request.schema_version == WORKER_SCHEMA_V1
        && request.context.capability_id == "local_node_action"
        && request.context.effect == "write"
        && valid_text(&request.operation, 160)
        && request.arguments.is_object()
        && request.arguments_hash == format!("sha256:{}", digest_value(&full_arguments))
        && valid_text(&request.idempotency_key, 255)
        && request
            .approval_id
            .as_ref()
            .is_some_and(|v| valid_text(v, 255))
        && canonical_json(&request.arguments).len() <= 4 * 1024 * 1024
}

fn validate_worker_context(context: &WorkerContext) -> bool {
    [
        &context.tenant_id,
        &context.user_id,
        &context.session_id,
        &context.execution_id,
        &context.run_id,
        &context.tool_call_id,
        &context.lease_id,
        &context.attempt_id,
        &context.capability_id,
    ]
    .into_iter()
    .all(|value| valid_text(value, 255))
        && context.capability_revision > 0
        && ["read", "write", "unknown"].contains(&context.effect.as_str())
}

fn context_matches_headers(
    context: &WorkerContext,
    scope: &crate::migration_local_node::broker::BrokerScope,
    headers: &HeaderMap,
) -> bool {
    context.tenant_id == scope.tenant_id
        && context.user_id == scope.user_id
        && context.session_id == scope.session_id
        && context.execution_id == scope.execution_id
        && context.run_id == scope.run_id
        && context.tool_call_id
            == crate::migration_local_node::broker::header(headers, "x-ai-tool-call-id")
        && context.lease_id == crate::migration_local_node::broker::header(headers, "x-ai-lease-id")
}

fn device_credential(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    let credential = value
        .strip_prefix("Device ")
        .filter(|credential| credential.len() <= 128)?;
    let encoded = credential.strip_prefix("lnv2.")?;
    let token = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    (token.len() == 32 && URL_SAFE_NO_PAD.encode(token) == encoded).then_some(credential)
}

fn error_response(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"code":code,"detail":code}))).into_response()
}

fn valid_text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(|c| c < ' ')
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn digest_value(value: &Value) -> String {
    sha256(canonical_json(value).as_bytes())
}

fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |value| value.as_secs_f64())
}

/// Sorted-key, compact JSON bytes shared with the companion protocol.
pub(super) fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                out.push('{');
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).expect("JSON string key"));
                    out.push(':');
                    write(&map[key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            _ => out.push_str(&serde_json::to_string(value).expect("JSON value")),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heartbeat_request() -> HeartbeatRequest {
        HeartbeatRequest {
            protocol_version: PROTOCOL_V2.into(),
            kind: "heartbeat".into(),
            device_id: "dev_aaaaaaaaaaaaaaaaaaaaaaaa".into(),
            receipts: vec![],
            doctor: json!({
                "status":"ready",
                "capability_claims":["file.read","file.search","file.watch"],
                "permission_snapshot_digest":"a".repeat(64),
                "os_permission_evidence":"not_provided_by_device_signature"
            }),
            sent_at: unix_now(),
        }
    }

    #[test]
    fn heartbeat_requires_a_canonical_bounded_permission_snapshot() {
        let mut request = heartbeat_request();
        assert!(validate_heartbeat(&request).is_ok());

        request.doctor["permission_snapshot_digest"] = json!("A".repeat(64));
        assert!(validate_heartbeat(&request).is_err());

        request = heartbeat_request();
        request.doctor["capability_claims"] = json!(["file.read", "file.read"]);
        assert!(validate_heartbeat(&request).is_err());

        request = heartbeat_request();
        request.doctor["capability_claims"] = json!(["file.read", "filesystem.root"]);
        assert!(validate_heartbeat(&request).is_err());
    }

    fn context() -> WorkerContext {
        WorkerContext {
            tenant_id: "tenant-a".into(),
            user_id: "user-a".into(),
            session_id: "session-a".into(),
            execution_id: uuid::Uuid::new_v4().to_string(),
            run_id: uuid::Uuid::new_v4().to_string(),
            tool_call_id: "call-a".into(),
            lease_id: uuid::Uuid::new_v4().to_string(),
            attempt_id: "attempt-a".into(),
            capability_id: "local_node_action".into(),
            capability_revision: 1,
            effect: "write".into(),
        }
    }

    #[test]
    fn worker_dispatch_requires_hash_of_full_operation_and_arguments() {
        let arguments = json!({
            "grant_id":"grant-a",
            "relative_path":"notes.txt",
            "content_sha256":"a".repeat(64),
            "expected_hash":null,
            "content_base64":"aGVsbG8",
        });
        let full = json!({"operation":"file.write","arguments":arguments});
        let mut request = WorkerDispatchRequest {
            schema_version: WORKER_SCHEMA_V1.into(),
            context: context(),
            device_id: "dev_aaaaaaaaaaaaaaaaaaaaaaaa".into(),
            operation: "file.write".into(),
            arguments,
            arguments_hash: format!("sha256:{}", digest_value(&full)),
            idempotency_key: "idempotency-a".into(),
            approval_id: Some("approval-a".into()),
        };

        assert!(valid_worker_dispatch(&request));
        request.arguments["content_base64"] = json!("dGFtcGVyZWQ");
        assert!(!valid_worker_dispatch(&request));
    }

    #[test]
    fn file_write_claim_binds_intent_and_expected_target_before_signing() {
        let config = LocalNodeChannelConfig {
            platform_key_id: "test-platform-key".into(),
            platform_signing_seed_b64: URL_SAFE_NO_PAD.encode([7u8; 32]),
            tls_certificate_path: "unused-cert.pem".into(),
            tls_private_key_path: "unused-key.pem".into(),
            tls_listen_address: "127.0.0.1:8443".parse().unwrap(),
        };
        let signer = PlatformSigner::from_config(&config).unwrap();
        let arguments = json!({
            "grant_id":"grant-a",
            "relative_path":"notes.txt",
            "content_sha256":"a".repeat(64),
            "expected_hash":null,
            "content_base64":"aGVsbG8",
        });
        let mut intent = arguments.clone();
        intent.as_object_mut().unwrap().remove("content_base64");
        let action = json!({
            "action_id":uuid::Uuid::new_v4().to_string(),
            "idempotency_key":"idempotency-a",
            "operation":"file.write",
            "arguments_digest":digest_value(&intent),
            "target_snapshot_digest":"missing",
            "resource_refs":["grant-a","notes.txt"],
        });
        let command = PendingClaim {
            command_id: "command-a".into(),
            action,
            normalized_arguments: arguments.clone(),
        };
        let result = sign_claim_command(&signer, command).unwrap();
        assert_eq!(result["normalized_arguments"], arguments);
        let action = &result["action"];
        let payload = canonical_action_payload(action).unwrap();
        let signature = URL_SAFE_NO_PAD
            .decode(action["platform_signature"].as_str().unwrap())
            .unwrap();
        let public_key = URL_SAFE_NO_PAD.decode(signer.public_key_b64()).unwrap();
        UnparsedPublicKey::new(&ED25519, &public_key)
            .verify(payload.as_bytes(), &signature)
            .unwrap();

        let mut mismatched = result["action"].clone();
        mismatched["target_snapshot_digest"] = json!("a".repeat(64));
        assert!(
            sign_claim_command(
                &signer,
                PendingClaim {
                    command_id: "command-b".into(),
                    action: mismatched,
                    normalized_arguments: arguments,
                },
            )
            .is_err()
        );
    }

    #[test]
    fn pairing_rejects_device_ids_unbound_to_expected_shape() {
        let mut request = PairingRedeemRequest {
            protocol_version: PROTOCOL_V2.into(),
            kind: "pairing_redeem".into(),
            challenge_id: uuid::Uuid::new_v4().to_string(),
            user_code: "ABCD-EFGH".into(),
            device_id: "dev_aaaaaaaaaaaaaaaaaaaaaaaa".into(),
            display_name: "fixture".into(),
            platform: "macos".into(),
            node_version: "test".into(),
            capability_claims: vec!["file.read".into()],
            permission_snapshot_digest: "a".repeat(64),
            proof_algorithm: "ed25519".into(),
            proof_public_key: URL_SAFE_NO_PAD.encode([1u8; 32]),
            device_proof: URL_SAFE_NO_PAD.encode([2u8; 64]),
        };
        assert!(validate_pairing(&request).is_ok());
        request.device_id = "dev_short".into();
        assert!(validate_pairing(&request).is_err());
    }

    #[test]
    fn channel_configuration_is_disabled_by_default_and_partial_enablement_fails_closed() {
        let empty = std::collections::BTreeMap::new();
        assert!(
            LocalNodeChannelConfig::from_environment(&empty)
                .unwrap()
                .is_none()
        );

        let partial = std::collections::BTreeMap::from([(
            "AI_PLATFORM_LOCAL_NODE_CHANNEL_ENABLED".to_owned(),
            "true".to_owned(),
        )]);
        assert!(matches!(
            LocalNodeChannelConfig::from_environment(&partial),
            Err("AI_PLATFORM_LOCAL_NODE_PLATFORM_KEY_ID")
        ));
    }

    #[test]
    fn channel_configuration_uses_explicit_ring_key_and_listener_settings() {
        let environment = std::collections::BTreeMap::from([
            (
                "AI_PLATFORM_LOCAL_NODE_CHANNEL_ENABLED".to_owned(),
                "true".to_owned(),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_PLATFORM_KEY_ID".to_owned(),
                "test-platform-key".to_owned(),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_PLATFORM_SIGNING_SEED_B64".to_owned(),
                URL_SAFE_NO_PAD.encode([7u8; 32]),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_TLS_CERT_PATH".to_owned(),
                "/tmp/local-node-test-chain.pem".to_owned(),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_TLS_KEY_PATH".to_owned(),
                "/tmp/local-node-test-key.pem".to_owned(),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_TLS_LISTEN".to_owned(),
                "127.0.0.1:18443".to_owned(),
            ),
        ]);
        let config = LocalNodeChannelConfig::from_environment(&environment)
            .unwrap()
            .unwrap();
        let signer = PlatformSigner::from_config(&config).unwrap();
        assert_eq!(signer.key_id(), "test-platform-key");
        assert_eq!(
            config.tls_listen_address,
            "127.0.0.1:18443".parse().unwrap()
        );
        assert_eq!(
            config.tls_certificate_path,
            std::path::PathBuf::from("/tmp/local-node-test-chain.pem")
        );
        assert_eq!(
            config.tls_private_key_path,
            std::path::PathBuf::from("/tmp/local-node-test-key.pem")
        );
    }

    #[tokio::test]
    async fn companion_control_is_tls_only_and_worker_routes_are_separate() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;

        let companion = local_node_companion_router(None);
        let worker = local_node_worker_router(None);
        let post = |path: &str| {
            Request::builder()
                .method("POST")
                .uri(path)
                .body(Body::empty())
                .unwrap()
        };

        assert_eq!(
            companion
                .clone()
                .oneshot(post(DEVICE_CONTROL_PATH))
                .await
                .unwrap()
                .status(),
            StatusCode::UPGRADE_REQUIRED
        );
        assert_eq!(
            companion
                .oneshot(post(&format!("{WORKER_BASE_PATH}/describe")))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            worker
                .oneshot(post(DEVICE_CONTROL_PATH))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
}
