//! Native Local Node integration; uses the existing exact-intent transport and
//! pinned verifier. Never launches a daemon, exports keys or expands a grant.
use crate::state::DesktopState;
use ai_platform_local_node::{
    config::{OperatorConfig, read_tls_ca},
    credentials::{
        DEVICE_CREDENTIAL_KEYCHAIN_SERVICE, DEVICE_IDENTITY_KEYCHAIN_SERVICE, native_secure_store,
    },
    hardware_key,
    identity::DeviceIdentity,
    signing::PinnedEd25519Verifier,
    transport::{HttpsTransport, PairingRequest, load_credential},
};
use serde_json::{Value, json};
fn config(state: &DesktopState, gateway: &str) -> Result<OperatorConfig, &'static str> {
    let path = state
        .node_config
        .as_ref()
        .ok_or("Local Node operator configuration not supplied")?;
    let config = OperatorConfig::read(path).map_err(|_| "Local Node configuration invalid")?;
    let endpoint = url::Url::parse(&config.endpoint).map_err(|_| "Local Node endpoint invalid")?;
    if endpoint.origin() != crate::policy::gateway(gateway)?.origin() {
        return Err("Local Node and Gateway origins differ");
    }
    Ok(config)
}
pub fn status(state: &DesktopState, gateway: &str) -> Result<Value, &'static str> {
    let config = config(state, gateway)?;
    // A status read must not create a device or hardware key.
    if !config.state_directory.join("device-identity.json").exists() {
        return Ok(json!({"paired":false,"control_connection":"not_observed"}));
    }
    let store = native_secure_store(DEVICE_IDENTITY_KEYCHAIN_SERVICE)
        .map_err(|_| "native vault unavailable")?;
    let identity = DeviceIdentity::load_or_create(&config.state_directory, store.as_ref())
        .map_err(|_| "device identity unavailable")?;
    let outbound = native_secure_store(DEVICE_CREDENTIAL_KEYCHAIN_SERVICE)
        .map_err(|_| "native vault unavailable")?;
    let credential = load_credential(
        outbound.as_ref(),
        &identity.credential_reference(),
        &identity.device_id,
    )
    .map_err(|_| "device credential unavailable")?;
    Ok(
        json!({"paired":credential.is_some(),"device_id":identity.device_id,"control_connection":"not_observed","device_key_policy":config.device_key_policy}),
    )
}
pub async fn pair(
    state: &DesktopState,
    gateway: &str,
    challenge: String,
    code: String,
) -> Result<Value, &'static str> {
    if uuid::Uuid::parse_str(&challenge).is_err()
        || code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err("pairing input invalid");
    }
    let config = config(state, gateway)?;
    if config.capability_claims().is_empty() {
        return Err("pairing requires existing explicit workspace grants");
    }
    // Reuse the same descriptor-pinned grant validation as the headless owner.
    // Grants exist only for this pairing proof; they are not runtime authority.
    #[cfg(unix)]
    let _grants = config
        .workspace_grants
        .iter()
        .map(|entry| {
            let capabilities: Vec<&str> = entry.capabilities.iter().map(String::as_str).collect();
            ai_platform_local_node::workspace::Grant::issue(
                entry.grant_id.clone(),
                &entry.root,
                entry.tenant_id.clone(),
                entry.user_id.clone(),
                &capabilities,
                None,
            )
            .map_err(|_| "explicit workspace grant unavailable")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let digest = config
        .permission_snapshot_digest()
        .map_err(|_| "workspace permission snapshot invalid")?;
    #[cfg(unix)]
    if let Some(helper) = &config.approval_helper {
        ai_platform_local_node::macos::NativeHelper::new(helper.clone())
            .map_err(|_| "trusted approval helper invalid")?;
    }
    let verifier = PinnedEd25519Verifier::new(
        config
            .trusted_platform_keys
            .iter()
            .map(|key| (key.key_id.clone(), key.public_key.clone())),
    )
    .map_err(|_| "pinned platform key required")?;
    let identity_store = native_secure_store(DEVICE_IDENTITY_KEYCHAIN_SERVICE)
        .map_err(|_| "native vault unavailable")?;
    let outbound = native_secure_store(DEVICE_CREDENTIAL_KEYCHAIN_SERVICE)
        .map_err(|_| "native vault unavailable")?;
    let identity = DeviceIdentity::load_or_create(&config.state_directory, identity_store.as_ref())
        .map_err(|_| "device identity unavailable")?;
    let signer = hardware_key::select(config.device_key_policy, &config.state_directory, &identity)
        .map_err(|_| "configured device signing key unavailable")?;
    // Serializes shell processes using this identity reference. The headless
    // Local Node owns its own lifecycle; no daemon is launched by this shell.
    let _lock =
        ai_platform_local_node::credential_lock::try_acquire(&identity.credential_reference())
            .map_err(|_| "device operation lock unavailable")?
            .ok_or("another device operation is active")?;
    let ca = config
        .tls_ca_file
        .as_deref()
        .map(read_tls_ca)
        .transpose()
        .map_err(|_| "Local Node TLS CA invalid")?;
    let transport = HttpsTransport::new_with_ca(
        &config.endpoint,
        std::time::Duration::from_secs(20),
        ca.as_deref(),
    )
    .map_err(|_| "Local Node transport unavailable")?;
    let request = PairingRequest {
        challenge_id: challenge,
        user_code: code,
        device_id: identity.device_id.clone(),
        display_name: config.display_name.clone(),
        platform: config.platform.clone(),
        node_version: config.node_version.clone(),
        capability_claims: config.capability_claims(),
        permission_snapshot_digest: digest,
    };
    let credential = transport
        .redeem_pairing(
            request,
            signer.as_ref(),
            outbound.as_ref(),
            &identity.credential_reference(),
            &verifier,
        )
        .await
        .map_err(
            |_| "pairing outcome not confirmed; inspect device status before reusing a challenge",
        )?;
    Ok(
        json!({"paired":true,"device_id":credential.device_id,"expires_at":credential.expires_at,"key_class":signer.key_class(),"control_connection":"not_observed"}),
    )
}
