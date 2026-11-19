//! A signed policy binds the *whole* Tauri manifest, rollout, target and expiry.
//! Tauri then independently verifies the downloaded artifact with its updater
//! public key. Unsigned endpoint metadata never authorizes installation.
use crate::state::{DesktopState, atomic_json, read_json};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tauri_plugin_updater::UpdaterExt;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleaseConfig {
    enabled: bool,
    channel: String,
    policy_url: String,
    download_origin: String,
    policy_public_key: String,
    updater_public_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    payload: String,
    signature: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    channel: String,
    target: String,
    version: String,
    sequence: u64,
    issued_at: u64,
    expires_at: u64,
    rollout_basis_points: u16,
    manifest_url: String,
    artifact_size: u64,
    manifest: Value,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seen {
    sequence: u64,
    digest: String,
}
fn time() -> Result<u64, &'static str> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs())
        .map_err(|_| "system clock unavailable")
}
fn target() -> String {
    format!(
        "{}-{}",
        if cfg!(target_os = "macos") {
            "darwin"
        } else {
            std::env::consts::OS
        },
        std::env::consts::ARCH
    )
}
fn release() -> Result<ReleaseConfig, &'static str> {
    let config: ReleaseConfig = serde_json::from_str(include_str!(concat!(
        env!("OUT_DIR"),
        "/release-policy.json"
    )))
    .map_err(|_| "release configuration invalid")?;
    if !config.enabled
        || config.updater_public_key.trim().is_empty()
        || STANDARD
            .decode(&config.policy_public_key)
            .map(|v| v.len())
            .ok()
            != Some(32)
    {
        return Err("signed updater is not configured");
    }
    let origin = crate::policy::gateway(&config.download_origin)?;
    approved_url(&origin, &config.policy_url)?;
    Ok(config)
}
fn approved_url(origin: &url::Url, raw: &str) -> Result<url::Url, &'static str> {
    let url = url::Url::parse(raw).map_err(|_| "update URL invalid")?;
    if !crate::policy::console_navigation(origin, &url)
        || url.scheme() != "https"
        || url.fragment().is_some()
        || url.query().is_some()
    {
        return Err("update URL outside pinned origin");
    }
    Ok(url)
}
fn cohort(seed: &str, sequence: u64) -> u16 {
    let hash =
        Sha256::digest(format!("agentforge-desktop-rollout-v1\0{seed}\0{sequence}").as_bytes());
    (u64::from_be_bytes(hash[..8].try_into().expect("SHA256 length")) % 10000) as u16
}
fn verify(
    bytes: &[u8],
    config: &ReleaseConfig,
    now: u64,
    current: &str,
    seed: &str,
    seen: &Seen,
) -> Result<(Policy, String, bool), &'static str> {
    if bytes.len() > 65536 {
        return Err("update policy too large");
    }
    let envelope: Envelope =
        serde_json::from_slice(bytes).map_err(|_| "signed update policy invalid")?;
    let payload = STANDARD
        .decode(envelope.payload)
        .map_err(|_| "update policy encoding invalid")?;
    let signature = STANDARD
        .decode(envelope.signature)
        .map_err(|_| "update policy signature invalid")?;
    let key = STANDARD
        .decode(&config.policy_public_key)
        .map_err(|_| "update policy key invalid")?;
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, key)
        .verify(&payload, &signature)
        .map_err(|_| "update policy signature invalid")?;
    let policy: Policy =
        serde_json::from_slice(&payload).map_err(|_| "update policy shape invalid")?;
    let digest = format!("{:x}", Sha256::digest(&payload));
    if policy.channel != config.channel
        || policy.target != target()
        || policy.sequence == 0
        || policy.sequence < seen.sequence
        || (policy.sequence == seen.sequence && digest != seen.digest)
        || policy.issued_at > now.saturating_add(300)
        || policy.expires_at <= now
        || policy.expires_at <= policy.issued_at
        || policy.expires_at - policy.issued_at > 7 * 86400
        || policy.rollout_basis_points > 10000
        || policy.artifact_size == 0
        || policy.artifact_size > 512 * 1024 * 1024
    {
        return Err("update policy scope, clock or rollback rejected");
    }
    let version = semver::Version::parse(&policy.version).map_err(|_| "update version invalid")?;
    let current = semver::Version::parse(current).map_err(|_| "installed version invalid")?;
    if !version.pre.is_empty() && config.channel == "stable" {
        return Err("prerelease refused by stable channel");
    }
    if version <= current {
        return Err("update is not newer than installed version");
    }
    if policy.manifest.get("version").and_then(Value::as_str) != Some(policy.version.as_str()) {
        return Err("update manifest version mismatch");
    }
    let origin = crate::policy::gateway(&config.download_origin)?;
    approved_url(&origin, &policy.manifest_url)?;
    let platform = policy
        .manifest
        .get("platforms")
        .and_then(|v| v.get(target()))
        .ok_or("update target absent")?;
    approved_url(
        &origin,
        platform
            .get("url")
            .and_then(Value::as_str)
            .ok_or("update artifact URL absent")?,
    )?;
    if platform
        .get("signature")
        .and_then(Value::as_str)
        .is_none_or(|s| s.trim().is_empty())
    {
        return Err("update artifact signature absent");
    }
    let eligible = cohort(seed, policy.sequence) < policy.rollout_basis_points;
    Ok((policy, digest, eligible))
}
async fn fetch(state: &DesktopState, url: &str) -> Result<Vec<u8>, &'static str> {
    let mut response = state
        .http
        .get(url)
        .send()
        .await
        .map_err(|_| "update policy unavailable")?;
    if !response.status().is_success() {
        return Err("update policy unavailable");
    }
    if response.content_length().is_some_and(|size| size > 65536) {
        return Err("update policy too large");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "update policy unavailable")?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err("update policy too large");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
pub async fn check(
    app: &tauri::AppHandle,
    state: &DesktopState,
    install: bool,
) -> Result<Value, &'static str> {
    let config = release()?;
    let lock_reference = format!(
        "desktop_update_{:x}",
        Sha256::digest(state.directory.to_string_lossy().as_bytes())
    );
    let _process_lock = ai_platform_local_node::credential_lock::try_acquire(&lock_reference)
        .map_err(|_| "update state lock unavailable")?
        .ok_or("another update operation is active")?;
    let bytes = fetch(state, &config.policy_url).await?;
    let seed = state.preferences.lock().await.cohort.clone();
    let path = state.directory.join("update-sequence.json");
    let seen = if path.exists() {
        read_json(&path)?
    } else {
        Seen::default()
    };
    let (policy, digest, eligible) = verify(
        &bytes,
        &config,
        time()?,
        &app.package_info().version.to_string(),
        &seed,
        &seen,
    )?;
    // Bind the sequence even for withheld cohorts, so replaying an old wider
    // rollout cannot roll a device backwards. Failure to persist closes updates.
    atomic_json(
        &path,
        &Seen {
            sequence: policy.sequence,
            digest,
        },
    )?;
    if !eligible {
        return Ok(json!({"available":false,"reason":"staged_rollout","version":policy.version}));
    }
    let endpoint = approved_url(
        &crate::policy::gateway(&config.download_origin)?,
        &policy.manifest_url,
    )?;
    let updater = app
        .updater_builder()
        .pubkey(config.updater_public_key)
        .endpoints(vec![endpoint])
        .map_err(|_| "updater endpoint invalid")?
        .build()
        .map_err(|_| "updater unavailable")?;
    let Some(update) = updater
        .check()
        .await
        .map_err(|_| "update manifest unavailable")?
    else {
        return Err("signed update missing from provider");
    };
    // Prevent manifest TOCTOU: plugin's independently fetched JSON must equal
    // the exact manifest inside the verified signed envelope.
    if update.raw_json != policy.manifest || update.version != policy.version {
        return Err("provider update differs from signed policy");
    }
    if !install {
        return Ok(json!({"available":true,"version":policy.version,"sequence":policy.sequence}));
    }
    if !crate::confirm(
        app,
        &format!(
            "Install signed AgentForge version {}? The application may restart.",
            policy.version
        ),
    ) {
        return Err("update installation cancelled");
    }
    // Expiry is checked again after the human confirmation; no stale policy can
    // cross the install boundary while a modal dialog remained open.
    if time()? >= policy.expires_at {
        return Err("update policy expired before installation");
    }
    let expected_size = policy.artifact_size;
    let (overflow, mut overflow_receiver) = tokio::sync::watch::channel(false);
    let mut received = 0u64;
    let downloading = update.download(
        move |chunk, _| {
            received = received.saturating_add(chunk as u64);
            if received > expected_size {
                overflow.send_replace(true);
            }
        },
        || {},
    );
    tokio::pin!(downloading);
    let over_budget = async {
        if overflow_receiver.wait_for(|over| *over).await.is_ok() {
            return;
        }
        // The progress closure can be dropped when a successful download ends.
        // Channel closure is not evidence of overflow; await the download result.
        std::future::pending::<()>().await;
    };
    tokio::pin!(over_budget);
    let bytes = tokio::select! {
        result = &mut downloading => result.map_err(|_| "signed update download failed")?,
        _ = &mut over_budget => return Err("update artifact exceeded signed size budget"),
    };
    if bytes.len() as u64 != policy.artifact_size {
        return Err("update artifact size mismatch");
    }
    if time()? >= policy.expires_at {
        return Err("update policy expired during download");
    }
    update
        .install(bytes)
        .map_err(|_| "update install outcome not confirmed")?;
    Ok(json!({"installed":true,"version":policy.version,"restart_required":true}))
}
#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::KeyPair;
    fn fixture() -> (ReleaseConfig, Value, ring::signature::Ed25519KeyPair) {
        let key = ring::signature::Ed25519KeyPair::from_seed_unchecked(&[7; 32]).unwrap();
        let config = ReleaseConfig {
            enabled: true,
            channel: "stable".into(),
            policy_url: "https://updates.test/policy".into(),
            download_origin: "https://updates.test".into(),
            policy_public_key: STANDARD.encode(key.public_key().as_ref()),
            updater_public_key: "fixture".into(),
        };
        let policy = json!({"channel":"stable","target":target(),"version":"0.2.0","sequence":2,"issued_at":1000,"expires_at":2000,"rollout_basis_points":10000,"artifact_size":10,"manifest_url":"https://updates.test/manifest.json","manifest":{"version":"0.2.0","platforms":{target():{"url":"https://updates.test/app.tar.gz","signature":"artifact-signature"}}}});
        (config, policy, key)
    }
    fn signed(policy: &Value, key: &ring::signature::Ed25519KeyPair) -> Vec<u8> {
        let payload = serde_json::to_vec(policy).unwrap();
        serde_json::to_vec(&json!({"payload":STANDARD.encode(&payload),"signature":STANDARD.encode(key.sign(&payload).as_ref())})).unwrap()
    }
    #[test]
    fn signatures_expiry_scope_and_rollback_fail_closed() {
        let (config, mut policy, key) = fixture();
        let valid = signed(&policy, &key);
        assert!(
            verify(&valid, &config, 1100, "0.1.0", "device", &Seen::default())
                .unwrap()
                .2
        );
        assert!(verify(&valid, &config, 2100, "0.1.0", "device", &Seen::default()).is_err());
        assert!(verify(&valid, &config, 1100, "0.3.0", "device", &Seen::default()).is_err());
        assert!(
            verify(
                &valid,
                &config,
                1100,
                "0.1.0",
                "device",
                &Seen {
                    sequence: 3,
                    digest: "later".into()
                }
            )
            .is_err()
        );
        policy["channel"] = json!("beta");
        assert!(
            verify(
                &signed(&policy, &key),
                &config,
                1100,
                "0.1.0",
                "device",
                &Seen::default()
            )
            .is_err()
        );
        let (config, mut policy, key) = fixture();
        policy["manifest"]["platforms"][target()]["url"] = json!("https://evil.test/app");
        assert!(
            verify(
                &signed(&policy, &key),
                &config,
                1100,
                "0.1.0",
                "device",
                &Seen::default()
            )
            .is_err()
        );
        let mut broken: Value = serde_json::from_slice(&valid).unwrap();
        broken["signature"] = json!(STANDARD.encode([0; 64]));
        assert!(
            verify(
                &serde_json::to_vec(&broken).unwrap(),
                &config,
                1100,
                "0.1.0",
                "device",
                &Seen::default()
            )
            .is_err()
        );
    }
    #[test]
    fn rollout_is_stable_and_zero_never_enrolls() {
        let (config, mut policy, key) = fixture();
        assert_eq!(cohort("device", 2), cohort("device", 2));
        policy["rollout_basis_points"] = json!(0);
        assert!(
            !verify(
                &signed(&policy, &key),
                &config,
                1100,
                "0.1.0",
                "device",
                &Seen::default()
            )
            .unwrap()
            .2
        );
    }
}
