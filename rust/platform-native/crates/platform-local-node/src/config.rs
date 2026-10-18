//! Explicit, secret-free operator configuration for the headless companion.
//! Credentials and private keys are never accepted from this document.
use crate::{Error, Result, digest};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_CONFIG_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    /// Exact HTTPS endpoint used for both pairing redemption and heartbeat.
    pub endpoint: String,
    #[serde(default)]
    pub device_key_policy: crate::hardware_key::DeviceKeyPolicy,
    /// Explicit state location. Databases and non-secret identity metadata live here.
    pub state_directory: PathBuf,
    #[serde(default)]
    pub tls_ca_file: Option<PathBuf>,
    #[serde(default = "default_display_name")]
    pub display_name: String,
    #[serde(default = "default_platform")]
    pub platform: String,
    #[serde(default = "default_node_version")]
    pub node_version: String,
    #[serde(default = "default_heartbeat_seconds")]
    pub heartbeat_interval_seconds: u64,
    #[serde(default)]
    pub trusted_platform_keys: Vec<TrustedPlatformKey>,
    #[serde(default)]
    pub workspace_grants: Vec<WorkspaceGrantConfig>,
    /// Explicit packaged helper path. It is only used for local approval signature
    /// verification; the helper is never launched by doctor or config validation.
    #[serde(default)]
    pub approval_helper: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPlatformKey {
    pub key_id: String,
    /// URL-safe, unpadded Base64 of a 32-byte Ed25519 public key.
    pub public_key: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceGrantConfig {
    pub grant_id: String,
    /// This root is an explicit local user grant. No parent, sibling, or ambient path is added.
    pub root: PathBuf,
    pub tenant_id: String,
    pub user_id: String,
    pub capabilities: Vec<String>,
}

fn default_display_name() -> String {
    "AI-Platform Local Node".into()
}
fn default_platform() -> String {
    if cfg!(target_os = "macos") {
        "macos".into()
    } else if cfg!(target_os = "windows") {
        "windows".into()
    } else {
        "linux".into()
    }
}
fn default_node_version() -> String {
    env!("CARGO_PKG_VERSION").into()
}
fn default_heartbeat_seconds() -> u64 {
    5
}
fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(&byte))
}

impl OperatorConfig {
    pub fn read(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|_| Error::Denied("operator config unavailable"))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_CONFIG_BYTES
        {
            return Err(Error::Denied(
                "operator config must be a bounded regular file",
            ));
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(path)
            .map_err(|_| Error::Denied("operator config unavailable"))?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Denied("operator config unreadable"))?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(Error::Denied("operator config exceeds size limit"));
        }
        let config: Self =
            serde_json::from_slice(&bytes).map_err(|_| Error::Denied("operator config invalid"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        let endpoint = url::Url::parse(&self.endpoint)
            .map_err(|_| Error::Denied("HTTPS endpoint required"))?;
        if endpoint.scheme() != "https"
            || endpoint.path() != crate::transport::CONTROL_PATH
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(Error::Denied("HTTPS endpoint required"));
        }
        if !self.state_directory.is_absolute()
            || self.state_directory == Path::new("/")
            || self.display_name.trim().is_empty()
            || self.display_name.chars().count() > 80
            || self.display_name.chars().any(char::is_control)
            || !["macos", "windows", "linux"].contains(&self.platform.as_str())
            || self.node_version.is_empty()
            || self.node_version.chars().count() > 64
            || self.node_version.chars().any(char::is_control)
            || !(1..=300).contains(&self.heartbeat_interval_seconds)
        {
            return Err(Error::Denied("operator config values invalid"));
        }
        if self
            .tls_ca_file
            .as_ref()
            .is_some_and(|path| !path.is_absolute())
            || self
                .approval_helper
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(Error::Denied("operator paths must be absolute"));
        }

        let mut key_ids = HashSet::new();
        for key in &self.trusted_platform_keys {
            let raw = URL_SAFE_NO_PAD
                .decode(&key.public_key)
                .map_err(|_| Error::Denied("trusted platform key invalid"))?;
            if !valid_identifier(&key.key_id, 255)
                || raw.len() != 32
                || !key_ids.insert(key.key_id.as_str())
            {
                return Err(Error::Denied("trusted platform key invalid"));
            }
        }

        let mut grant_ids = HashSet::new();
        for grant in &self.workspace_grants {
            if !valid_identifier(&grant.grant_id, 200)
                || !grant.root.is_absolute()
                || grant.tenant_id.is_empty()
                || grant.tenant_id.chars().count() > 128
                || grant.user_id.is_empty()
                || grant.user_id.chars().count() > 128
                || grant.capabilities.is_empty()
                || grant.capabilities.iter().any(|capability| {
                    !["list", "read", "search", "watch", "write", "rollback"]
                        .contains(&capability.as_str())
                })
                || grant.capabilities.iter().collect::<HashSet<_>>().len()
                    != grant.capabilities.len()
                || (grant.capabilities.contains(&"rollback".into())
                    && !grant.capabilities.contains(&"write".into()))
                || !grant_ids.insert(grant.grant_id.as_str())
            {
                return Err(Error::Denied("workspace grant config invalid"));
            }
        }
        if self.workspace_grants.iter().any(|grant| {
            grant
                .capabilities
                .iter()
                .any(|capability| matches!(capability.as_str(), "write" | "rollback"))
        }) && self.approval_helper.is_none()
        {
            return Err(Error::Denied(
                "write grants require an explicit local approval helper",
            ));
        }
        Ok(())
    }

    pub fn permission_snapshot_digest(&self) -> Result<String> {
        let mut grants: Vec<_> = self
            .workspace_grants
            .iter()
            .map(|grant| {
                let metadata = std::fs::symlink_metadata(&grant.root)
                    .map_err(|_| Error::Denied("workspace grant root unavailable"))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(Error::Denied("workspace grant root invalid"));
                }
                #[cfg(unix)]
                let (root_device, root_inode) = {
                    use std::os::unix::fs::MetadataExt;
                    (metadata.dev(), metadata.ino())
                };
                #[cfg(not(unix))]
                let (root_device, root_inode) = {
                    let _ = metadata;
                    return Err(Error::Denied("workspace grants unsupported on this platform"));
                };
                let mut capabilities = grant.capabilities.clone();
                if capabilities.contains(&"write".into())
                    && !capabilities.contains(&"rollback".into())
                {
                    capabilities.push("rollback".into());
                }
                capabilities.sort();
                Ok(json!({"grant_id":grant.grant_id,"tenant_id":grant.tenant_id,"user_id":grant.user_id,"root_device":root_device,"root_inode":root_inode,"capabilities":capabilities}))
            })
            .collect::<Result<Vec<_>>>()?;
        grants.sort_by(|left, right| left["grant_id"].as_str().cmp(&right["grant_id"].as_str()));
        let mut snapshot = json!({"workspace_grants":grants});
        if self.device_key_policy == crate::hardware_key::DeviceKeyPolicy::HardwareRequired {
            snapshot["device_key_policy"] = json!("hardware_required");
        }
        Ok(digest(&snapshot))
    }

    pub fn capability_claims(&self) -> Vec<String> {
        let mut claims = BTreeMap::new();
        for grant in &self.workspace_grants {
            for capability in &grant.capabilities {
                let claim = match capability.as_str() {
                    "list" | "read" => "file.read",
                    "search" => "file.search",
                    "watch" => "file.watch",
                    "write" | "rollback" => "file.write",
                    _ => continue,
                };
                claims.insert(claim, ());
            }
        }
        claims.into_keys().map(str::to_owned).collect()
    }
}

/// Reads a private CA bundle explicitly selected by the operator. Reqwest uses
/// only this certificate for TLS roots; it never disables certificate checks.
pub fn read_tls_ca(path: &Path) -> Result<Vec<u8>> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| Error::Denied("TLS CA file unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(Error::Denied("TLS CA file must be a bounded regular file"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|_| Error::Denied("TLS CA file unavailable"))?;
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Denied("TLS CA file unreadable"))?;
    if bytes.is_empty() || bytes.len() > 1024 * 1024 {
        return Err(Error::Denied("TLS CA file invalid"));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn valid_config() -> OperatorConfig {
        OperatorConfig {
            endpoint: "https://gateway.example.test/api/v1/local-nodes/control".into(),
            device_key_policy: Default::default(),
            state_directory: PathBuf::from("/tmp/native-local-node-state"),
            tls_ca_file: None,
            display_name: "fixture node".into(),
            platform: "macos".into(),
            node_version: "test".into(),
            heartbeat_interval_seconds: 5,
            trusted_platform_keys: vec![],
            workspace_grants: vec![WorkspaceGrantConfig {
                grant_id: "grant-fixture".into(),
                root: PathBuf::from("/tmp/local-node-fixture"),
                tenant_id: "tenant-fixture".into(),
                user_id: "user-fixture".into(),
                capabilities: vec!["write".into(), "read".into()],
            }],
            approval_helper: Some(PathBuf::from("/tmp/native-local-approval-helper")),
        }
    }

    #[test]
    fn rejects_http_query_userinfo_and_unbounded_heartbeat() {
        let mut config = valid_config();
        config.endpoint = "http://127.0.0.1:8080/api/v1/local-nodes/control".into();
        assert!(config.validate().is_err());
        config.endpoint =
            "https://user:secret@gateway.example.test/api/v1/local-nodes/control".into();
        assert!(config.validate().is_err());
        config.endpoint =
            "https://gateway.example.test/api/v1/local-nodes/control?token=synthetic".into();
        assert!(config.validate().is_err());
        config.endpoint = "https://gateway.example.test/wrong-control-path".into();
        assert!(config.validate().is_err());
        config.endpoint = "https://gateway.example.test/api/v1/local-nodes/control".into();
        config.heartbeat_interval_seconds = 301;
        assert!(config.validate().is_err());
    }

    #[test]
    fn pairing_claims_are_unique_and_config_digest_omits_local_paths() {
        let mut config = valid_config();
        assert_eq!(config.capability_claims(), vec!["file.read", "file.write"]);
        let claims = config.capability_claims();
        assert_eq!(claims, vec!["file.read", "file.write"]);
    }

    #[test]
    fn secret_fields_are_rejected_by_strict_operator_config() {
        let value = json!({
            "endpoint":"https://gateway.example.test/local-node",
            "state_directory":"/tmp/native-local-node-state",
            "token":"synthetic-do-not-accept",
            "trusted_platform_keys":[],
            "workspace_grants":[]
        });
        assert!(serde_json::from_value::<OperatorConfig>(value).is_err());
    }

    #[test]
    fn write_grants_require_explicit_device_local_approval_helper() {
        let mut config = valid_config();
        config.endpoint = "https://gateway.example.test/api/v1/local-nodes/control".into();
        assert!(config.validate().is_ok());
        config.approval_helper = None;
        assert!(matches!(config.validate(), Err(Error::Denied(_))));
    }

    #[cfg(unix)]
    #[test]
    fn config_and_ca_readers_reject_symlinks() {
        use std::os::unix::fs::symlink;
        let directory =
            std::env::temp_dir().join(format!("local-node-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let real = directory.join("real.json");
        let linked = directory.join("linked.json");
        std::fs::write(&real, b"{}").unwrap();
        symlink(&real, &linked).unwrap();
        assert!(OperatorConfig::read(&linked).is_err());
        assert!(read_tls_ca(&linked).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
