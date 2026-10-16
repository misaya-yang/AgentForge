//! Headless native companion. Credentials never appear in arguments or output.
use ai_platform_local_node::{
    config::{OperatorConfig, read_tls_ca},
    credentials::{
        DEVICE_CREDENTIAL_KEYCHAIN_SERVICE, DEVICE_IDENTITY_KEYCHAIN_SERVICE,
        SecureCredentialStore, UnavailableStore,
    },
    identity::DeviceIdentity,
    signing::PinnedEd25519Verifier,
    transport::{HttpsTransport, PROTOCOL_VERSION, PairingRequest, load_credential},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const HELP: &str = "local-node doctor [--endpoint URL | --config FILE]
local-node config validate --file FILE
local-node pair --config FILE --challenge-id ID --user-code-stdin
local-node serve --config FILE

The explicit JSON config contains an HTTPS endpoint, a private state directory,
operator-pinned platform Ed25519 public keys, and explicit workspace grants.
Private keys and device credentials are held in the native secure credential
store. Pairing codes are read only from piped stdin. The companion opens no
listener, discovers no endpoint from environment, and never expands grants.";

#[derive(Debug)]
enum PairingError {
    Message(&'static str),
    HttpStatus(u16),
}
impl From<&'static str> for PairingError {
    fn from(message: &'static str) -> Self {
        Self::Message(message)
    }
}
impl fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(message) => formatter.write_str(message),
            Self::HttpStatus(status) => write!(
                formatter,
                "pairing returned HTTP status {status}; inspect owner device status before retrying this one-use challenge"
            ),
        }
    }
}

#[cfg(target_os = "macos")]
fn secure_store(service: &str) -> Result<Box<dyn SecureCredentialStore>, &'static str> {
    ai_platform_local_node::credentials::MacKeychainStore::new(service.into())
        .map(|store| Box::new(store) as Box<dyn SecureCredentialStore>)
        .map_err(|_| "native Keychain storage unavailable")
}

#[cfg(not(target_os = "macos"))]
fn secure_store(_: &str) -> Result<Box<dyn SecureCredentialStore>, &'static str> {
    Ok(Box::new(UnavailableStore))
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "Darwin"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else if cfg!(target_os = "linux") {
        "Linux"
    } else {
        std::env::consts::OS
    }
}

fn endpoint_status(raw: &str) -> (&'static str, &'static str, &'static str) {
    match url::Url::parse(raw) {
        Ok(url)
            if ["https", "wss"].contains(&url.scheme())
                && url.path() == ai_platform_local_node::transport::CONTROL_PATH
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none() =>
        {
            if url.scheme() == "https" {
                ("ready", "available_not_started", "not_started")
            } else {
                ("ready", "needs_adapter", "needs_action")
            }
        }
        _ => ("denied", "unavailable", "needs_action"),
    }
}

fn doctor_report(endpoint: Option<&str>, config: Option<&OperatorConfig>) -> Value {
    let (endpoint_status, transport, reason) = match (endpoint, config) {
        (Some(endpoint), _) => endpoint_status(endpoint),
        (_, Some(config)) => endpoint_status(&config.endpoint),
        _ => ("needs_action", "unavailable", "needs_action"),
    };
    let mac = cfg!(target_os = "macos");
    let configured_keys = config.map_or(0, |value| value.trusted_platform_keys.len());
    let configured_grants = config.map_or(0, |value| value.workspace_grants.len());
    json!({
        "status":"unavailable",
        "operating_system":platform_name(),
        "network_started":false,
        "listener":"disabled",
        "endpoint":endpoint_status,
        "transport":transport,
        "secure_credential_storage":if mac {"available_adapter_not_probed"} else {"unavailable"},
        "device_pairing_signer":if mac {"available_after_secure_identity_injection"} else {"unavailable"},
        "platform_signature_verifier":if configured_keys>0 {"pinned_keys_configured_not_probed"} else {"unavailable"},
        "trusted_local_approval":if config.is_some_and(|value|value.approval_helper.is_some()) {"helper_configured_not_probed"} else {"unavailable"},
        "file_dispatcher":if cfg!(unix)&&configured_grants>0 {"explicit_grants_configured_not_opened"} else {"unavailable"},
        "configured_workspace_grant_count":configured_grants,
        "computer":{"status":"disabled","driver":"unavailable"},
        "reason":if endpoint==Some("") {"outbound endpoint is invalid"} else {reason}
    })
}

fn validate_config(path: &Path) -> Result<Value, &'static str> {
    let config = OperatorConfig::read(path).map_err(|_| "operator config invalid")?;
    if let Some(ca_path) = &config.tls_ca_file {
        let pem = read_tls_ca(ca_path).map_err(|_| "TLS CA config invalid")?;
        reqwest::Certificate::from_pem(&pem).map_err(|_| "TLS CA config invalid")?;
    }
    #[cfg(unix)]
    if let Some(helper_path) = &config.approval_helper {
        ai_platform_local_node::macos::NativeHelper::new(helper_path.clone())
            .map_err(|_| "local approval helper invalid")?;
    }
    Ok(json!({
        "status":"config_valid",
        "network_started":false,
        "listener":"disabled",
        "endpoint":"https_configured",
        "tls_ca":"operator_selected_only",
        "private_key_in_config":false,
        "credential_in_config":false,
        "trusted_platform_key_count":config.trusted_platform_keys.len(),
        "workspace_grant_count":config.workspace_grants.len(),
        "approval_helper_configured":config.approval_helper.is_some()
    }))
}

fn prepare_private_directory(path: &Path) -> Result<(), &'static str> {
    if !path.is_absolute() || path == Path::new("/") {
        return Err("state directory must be an explicit absolute path");
    }
    std::fs::create_dir_all(path).map_err(|_| "state directory unavailable")?;
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "state directory unavailable")?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("state directory must be a real directory");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "state directory permissions unavailable")?;
    }
    Ok(())
}

fn database_path(state: &Path, name: &str) -> Result<PathBuf, &'static str> {
    let path = state.join(name);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err("state database must be a regular file")
        }
        Ok(_) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(_) => Err("state database unavailable"),
    }
}

#[cfg(unix)]
fn configured_grants(
    config: &OperatorConfig,
) -> Result<HashMap<String, ai_platform_local_node::workspace::Grant>, &'static str> {
    use ai_platform_local_node::workspace::Grant;
    let mut grants = HashMap::new();
    for entry in &config.workspace_grants {
        let capabilities: Vec<&str> = entry.capabilities.iter().map(String::as_str).collect();
        let grant = Grant::issue(
            entry.grant_id.clone(),
            &entry.root,
            entry.tenant_id.clone(),
            entry.user_id.clone(),
            &capabilities,
            None,
        )
        .map_err(|_| "explicit workspace grant unavailable")?;
        if grants.insert(entry.grant_id.clone(), grant).is_some() {
            return Err("duplicate workspace grant identity");
        }
    }
    Ok(grants)
}

#[cfg(not(unix))]
fn configured_grants(_: &OperatorConfig) -> Result<HashMap<String, ()>, &'static str> {
    Err("native filesystem dispatcher unavailable on this platform")
}

fn pinned_platform_verifier(
    config: &OperatorConfig,
) -> Result<PinnedEd25519Verifier, &'static str> {
    PinnedEd25519Verifier::new(
        config
            .trusted_platform_keys
            .iter()
            .map(|key| (key.key_id.clone(), key.public_key.clone())),
    )
    .map_err(|_| "explicit pinned platform key required")
}

fn transport(config: &OperatorConfig) -> Result<HttpsTransport, &'static str> {
    let ca = config
        .tls_ca_file
        .as_ref()
        .map(|path| read_tls_ca(path).map_err(|_| "TLS CA file invalid"))
        .transpose()?;
    HttpsTransport::new_with_ca(&config.endpoint, Duration::from_secs(30), ca.as_deref())
        .map_err(|_| "secure HTTPS transport unavailable")
}

fn read_pairing_code_from_pipe() -> Result<String, &'static str> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Err("pairing code must be piped through stdin");
    }
    let mut bytes = Vec::new();
    stdin
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| "pairing code input unavailable")?;
    if bytes.len() > 4096 {
        return Err("pairing code input exceeds limit");
    }
    let code = String::from_utf8(bytes).map_err(|_| "pairing code input invalid")?;
    let code = code.trim_end_matches(['\r', '\n']);
    if code.len() != 12
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err("pairing code input invalid");
    }
    Ok(code.to_owned())
}

async fn run_pair(config_path: &Path, challenge_id: &str) -> Result<Value, PairingError> {
    let config = OperatorConfig::read(config_path).map_err(|_| "operator config invalid")?;
    if uuid::Uuid::parse_str(challenge_id).is_err() {
        return Err(PairingError::Message("challenge ID invalid"));
    }
    if config.capability_claims().is_empty() {
        return Err(PairingError::Message("pairing requires an explicit workspace grant"));
    }
    #[cfg(unix)]
    if let Some(helper_path) = &config.approval_helper {
        ai_platform_local_node::macos::NativeHelper::new(helper_path.clone())
            .map_err(|_| "trusted local approval helper invalid")?;
    }
    let verifier = pinned_platform_verifier(&config)?;
    let grants = configured_grants(&config)?;
    if grants.is_empty() {
        return Err(PairingError::Message("pairing requires an explicit workspace grant"));
    }
    let permission_snapshot_digest = config
        .permission_snapshot_digest()
        .map_err(|_| "workspace permission snapshot invalid")?;
    prepare_private_directory(&config.state_directory)?;
    let identity_store = secure_store(DEVICE_IDENTITY_KEYCHAIN_SERVICE)?;
    let outbound_store = secure_store(DEVICE_CREDENTIAL_KEYCHAIN_SERVICE)?;
    if !identity_store.ready() || !outbound_store.ready() {
        return Err(PairingError::Message("native secure credential storage unavailable"));
    }
    let identity = DeviceIdentity::load_or_create(&config.state_directory, identity_store.as_ref())
        .map_err(|_| "native secure device identity unavailable")?;
    let signer = identity
        .pairing_signer()
        .map_err(|_| "native pairing signer unavailable")?;
    let user_code = read_pairing_code_from_pipe()?;
    // Pin/check configured roots before sending the device proof; this call touches
    // only roots explicitly supplied in the operator config.
    drop(grants);
    let request = PairingRequest {
        challenge_id: challenge_id.into(),
        user_code,
        device_id: identity.device_id.clone(),
        display_name: config.display_name.clone(),
        platform: config.platform.clone(),
        node_version: config.node_version.clone(),
        capability_claims: config.capability_claims(),
        permission_snapshot_digest,
    };
    let transport = transport(&config)?;
    let credential_reference = identity.credential_reference();
    let credential = transport
        .redeem_pairing(
            request,
            &signer,
            outbound_store.as_ref(),
            &credential_reference,
            &verifier,
        )
        .await;
    match credential {
        Ok(credential) => Ok(json!({
            "status":"paired",
            "protocol_version":PROTOCOL_VERSION,
            "device_id":credential.device_id,
            "expires_at":credential.expires_at,
            "credential":"[REDACTED]"
        })),
        Err(ai_platform_local_node::Error::HttpStatus(status)) => {
            Err(PairingError::HttpStatus(status))
        }
        Err(_) => Err(PairingError::Message(
            "pairing outcome not confirmed; inspect owner device status before retrying this one-use challenge",
        )),
    }
}

#[cfg(unix)]
async fn run_serve(config_path: &Path) -> Result<Value, &'static str> {
    use ai_platform_local_node::{
        approvals::OneUseApproval,
        file_dispatch::FileControlledDispatcher,
        files::FileService,
        ledger::Ledger,
        outbox::Outbox,
        runner::{ReplayGuard, run_forever},
    };

    let config = OperatorConfig::read(config_path).map_err(|_| "operator config invalid")?;
    if config.workspace_grants.is_empty() {
        return Err("serve requires an explicit workspace grant");
    }
    #[cfg(unix)]
    if let Some(helper_path) = &config.approval_helper {
        ai_platform_local_node::macos::NativeHelper::new(helper_path.clone())
            .map_err(|_| "trusted local approval helper invalid")?;
    }
    let verifier = pinned_platform_verifier(&config)?;
    let grants = configured_grants(&config)?;
    prepare_private_directory(&config.state_directory)?;
    let identity_store = secure_store(DEVICE_IDENTITY_KEYCHAIN_SERVICE)?;
    let outbound_store = secure_store(DEVICE_CREDENTIAL_KEYCHAIN_SERVICE)?;
    if !identity_store.ready() || !outbound_store.ready() {
        return Err("native secure credential storage unavailable");
    }
    let identity = DeviceIdentity::load_or_create(&config.state_directory, identity_store.as_ref())
        .map_err(|_| "native secure device identity unavailable")?;
    let credential_reference = identity.credential_reference();
    let credential = load_credential(
        outbound_store.as_ref(),
        &credential_reference,
        &identity.device_id,
    )
    .map_err(|_| "stored device credential invalid")?
    .ok_or("paired device credential required")?;

    let ledger_path = database_path(&config.state_directory, "ledger.sqlite")?;
    let outbox_path = database_path(&config.state_directory, "outbox.sqlite")?;
    let replay_path = database_path(&config.state_directory, "replay.sqlite")?;
    let approval_path = database_path(&config.state_directory, "approval-nonces.sqlite")?;
    let rollback_dir = config.state_directory.join("rollback");
    let files = FileService::new(rollback_dir).map_err(|_| "private rollback store unavailable")?;
    let ledger = Ledger::open(&ledger_path).map_err(|_| "local action ledger unavailable")?;
    let outbox = Outbox::open(&outbox_path).map_err(|_| "local receipt outbox unavailable")?;
    let replay = ReplayGuard::open(&replay_path).map_err(|_| "local replay guard unavailable")?;

    let helper = config
        .approval_helper
        .as_ref()
        .map(|path| ai_platform_local_node::macos::NativeHelper::new(path.clone()))
        .transpose()
        .map_err(|_| "trusted local approval helper invalid")?
        .map(std::sync::Arc::new);
    let approval_account = identity.device_id.clone();
    let verification_helper = helper.clone();
    let approvals = OneUseApproval::open(
        identity.device_id.clone(),
        &approval_path,
        move |payload, signature| {
            verification_helper.as_ref().is_some_and(|native| {
                native.verify_approval(
                    payload,
                    signature,
                    "ai-platform.local-node.trusted-approval.v1",
                    &approval_account,
                )
            })
        },
    )
    .map_err(|_| "durable approval nonce store unavailable")?;
    let runner_verifier = pinned_platform_verifier(&config)?;

    let transport = transport(&config)?;
    let doctor = json!({
        "status":"ready",
        "operating_system":platform_name(),
        "listener":"disabled",
        "computer":"disabled",
        "capability_claims":config.capability_claims(),
        "permission_snapshot_digest":config.permission_snapshot_digest().map_err(|_|"workspace permission snapshot invalid")?,
        "os_permission_evidence":"not_provided_by_device_signature"
    });
    let mut dispatcher = FileControlledDispatcher::new(grants, verifier, approvals, files, ledger)
        .map_err(|_| "local control store unavailable")?;
    if let Some(prompt) = helper {
        dispatcher = dispatcher.with_approval_prompt(prompt);
    }
    let (stop_sender, stop_receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop_sender.send(true);
    });
    run_forever(
        identity.device_id.clone(),
        doctor,
        transport,
        credential,
        std::sync::Arc::new(runner_verifier),
        replay,
        outbox,
        dispatcher,
        Duration::from_secs(config.heartbeat_interval_seconds),
        stop_receiver,
    )
    .await
    .map_err(|_| "local node stopped after an unconfirmed transport or dispatch result")?;
    Ok(json!({"status":"stopped","device_id":identity.device_id}))
}

#[cfg(not(unix))]
async fn run_serve(_: &Path) -> Result<Value, &'static str> {
    Err("native filesystem dispatcher unavailable on this platform")
}

fn output(value: &Value) {
    println!(
        "{}",
        serde_json::to_string(value).unwrap_or_else(|_| "{}".into())
    );
}

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    match args.as_slice() {
        [command] if command == "doctor" => {
            output(&doctor_report(None, None));
            ExitCode::from(2)
        }
        [command, flag, endpoint] if command == "doctor" && flag == "--endpoint" => {
            output(&doctor_report(Some(endpoint), None));
            ExitCode::from(2)
        }
        [command, flag, path] if command == "doctor" && flag == "--config" => {
            match OperatorConfig::read(Path::new(path)) {
                Ok(config) => {
                    output(&doctor_report(None, Some(&config)));
                    ExitCode::from(2)
                }
                Err(_) => {
                    eprintln!("local-node: error: operator config invalid");
                    ExitCode::from(2)
                }
            }
        }
        [command, operation, flag, path]
            if command == "config" && operation == "validate" && flag == "--file" =>
        {
            match validate_config(Path::new(path)) {
                Ok(report) => {
                    output(&report);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("local-node: error: {error}");
                    ExitCode::from(2)
                }
            }
        }
        [
            command,
            config_flag,
            path,
            challenge_flag,
            challenge,
            code_flag,
        ] if command == "pair"
            && config_flag == "--config"
            && challenge_flag == "--challenge-id"
            && code_flag == "--user-code-stdin" =>
        {
            match run_pair(Path::new(path), challenge).await {
                Ok(report) => {
                    output(&report);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("local-node: error: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        [command, flag, path] if command == "serve" && flag == "--config" => {
            match run_serve(Path::new(path)).await {
                Ok(report) => {
                    output(&report);
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("local-node: error: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!("local-node: error: invalid arguments; use --help");
            ExitCode::from(2)
        }
    }
}
