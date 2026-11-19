//! Deployment-owned signed plugin packs v2 over compiled plugin executors.
//! No package code, network, shell, credentials, or implicit host plugin loading.
//! Install is inert. An immutable activation generation is loaded at startup;
//! the companion Gateway catalog must be selected for the same generation.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use ai_platform_capability_contract::{CapabilityDescriptorV2, canonical_json_hash};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::platform_catalog::{load_platform_catalog, worker_capability_catalog_with_writes};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_FILES: usize = 128;
const MAX_PACKS: usize = 32;
const MANIFEST_SCHEMA: &str = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json";
const TRUST_SCHEMA: &str = "ai-platform-plugin-trust/v2";
#[path = "publisher_signatures.rs"]
mod publisher_signatures;
const ACTIVATION_SCHEMA: &str = "ai-platform-plugin-activation/v1";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PackError {
    #[error("plugin_pack_invalid")]
    Invalid,
    #[error("plugin_pack_untrusted")]
    Untrusted,
    #[error("plugin_pack_incompatible")]
    Incompatible,
    #[error("plugin_pack_storage_unavailable")]
    Storage,
    #[error("plugin_activation_conflict")]
    Conflict,
    #[error("plugin_pack_platform_unsupported")]
    Unsupported,
}
type Result<T> = std::result::Result<T, PackError>;
fn require(value: bool) -> Result<()> {
    if value {
        Ok(())
    } else {
        Err(PackError::Invalid)
    }
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}
fn plugin_name(value: &str) -> bool {
    name(value)
        && !value.contains("--")
        && !value.contains("..")
        && value
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || b".-".contains(&c))
}
fn hash(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|v| {
        v.len() == 64
            && v.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
fn json_hash<T: Serialize>(value: &T) -> Result<String> {
    canonical_json_hash(&serde_json::to_value(value).map_err(|_| PackError::Invalid)?)
        .map_err(|_| PackError::Invalid)
}
// Duplicate keys must not acquire different meanings in the operator review,
// standard manifest reader and activation authority.
fn strict_json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    struct Strict(Value);
    impl<'de> Deserialize<'de> for Strict {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Strict;
                fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                    f.write_str("JSON without duplicate keys")
                }
                fn visit_bool<E: serde::de::Error>(
                    self,
                    v: bool,
                ) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Strict, E> {
                    serde_json::Number::from_f64(v)
                        .map(|v| Strict(Value::Number(v)))
                        .ok_or_else(|| E::custom("invalid number"))
                }
                fn visit_str<E: serde::de::Error>(self, v: &str) -> std::result::Result<Strict, E> {
                    Ok(Strict(json!(v)))
                }
                fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Strict, E> {
                    Ok(Strict(Value::Null))
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut values = Vec::new();
                    while let Some(v) = a.next_element::<Strict>()? {
                        values.push(v.0);
                    }
                    Ok(Strict(Value::Array(values)))
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    mut a: A,
                ) -> std::result::Result<Strict, A::Error> {
                    let mut values = serde_json::Map::new();
                    while let Some((key, value)) = a.next_entry::<String, Strict>()? {
                        if values.insert(key, value.0).is_some() {
                            return Err(serde::de::Error::custom("duplicate key"));
                        }
                    }
                    Ok(Strict(Value::Object(values)))
                }
            }
            d.deserialize_any(Visitor)
        }
    }
    let value: Strict = serde_json::from_slice(bytes).map_err(|_| PackError::Invalid)?;
    serde_json::from_value(value.0).map_err(|_| PackError::Invalid)
}
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PackError::Storage)?;
    require(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= limit as u64,
    )?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|_| PackError::Storage)?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PackError::Storage)?;
    require(bytes.len() <= limit)?;
    Ok(bytes)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPack {
    pub publisher_id: String,
    /// Detached minisign raw-Ed signature over the exact digest-pinned USTAR.
    pub publisher_signature: String,
    pub id: String,
    pub version: String,
    pub artifact_sha256: String,
    /// Exact fingerprints of the compiled descriptors before activation tags.
    pub capabilities: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedPublisher {
    pub minisign_public_key: String,
    pub allowed_packages: BTreeSet<String>,
    #[serde(default)]
    pub revoked: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TrustPolicy {
    pub publishers: BTreeMap<String, TrustedPublisher>,
    /// Exact tenant -> admitted publisher IDs. No wildcard/deployment fallback.
    pub tenant_publishers: BTreeMap<String, BTreeSet<String>>,
    pub schema_version: String,
    /// Exact installed Worker executable, distributed by the pinned OCI release.
    pub worker_binary_sha256: String,
    /// Retain names here when revoking/removing a package. They stay denied until
    /// an explicitly trusted package activates them; package omission is no grant.
    pub managed_capabilities: BTreeSet<String>,
    pub packages: Vec<TrustedPack>,
}
impl TrustPolicy {
    pub fn load(path: &Path) -> Result<Self> {
        let policy: Self = strict_json(&read_bounded(path, MAX_FILE_BYTES)?)?;
        policy.validate()?;
        Ok(policy)
    }
    fn validate(&self) -> Result<()> {
        require(
            self.schema_version == TRUST_SCHEMA
                && hash(&self.worker_binary_sha256)
                && self.packages.len() <= MAX_PACKS
                && !self.managed_capabilities.is_empty()
                && self.managed_capabilities.len() <= 256,
        )?;
        require(
            !self.publishers.is_empty()
                && self.publishers.len() <= 64
                && !self.tenant_publishers.is_empty()
                && self.tenant_publishers.len() <= 256,
        )?;
        let mut publisher_keys = BTreeSet::new();
        let mut publisher_key_ids = BTreeSet::new();
        for (id, publisher) in &self.publishers {
            require(
                name(id)
                    && !publisher.allowed_packages.is_empty()
                    && publisher.allowed_packages.len() <= 64
                    && publisher.allowed_packages.iter().all(|p| plugin_name(p)),
            )?;
            let (key, key_id) = publisher_signatures::public_key(&publisher.minisign_public_key)?;
            require(publisher_keys.insert(key.to_bytes()) && publisher_key_ids.insert(key_id))?;
        }
        for (tenant, publishers) in &self.tenant_publishers {
            require(
                !tenant.is_empty()
                    && tenant.len() <= 128
                    && tenant != "*"
                    && !tenant.chars().any(char::is_control)
                    && publishers.len() <= 64
                    && publishers
                        .iter()
                        .all(|id| self.publishers.get(id).is_some_and(|p| !p.revoked)),
            )?;
        }
        // A removed quiz pack cannot regain a legacy built-in default.
        require(
            self.managed_capabilities
                .contains(ai_platform_plugin_quiz::CAPABILITY_ID),
        )?;
        let compiled =
            worker_capability_catalog_with_writes(true).map_err(|_| PackError::Incompatible)?;
        let source = load_platform_catalog().map_err(|_| PackError::Incompatible)?;
        let names: BTreeSet<_> = source.capabilities.iter().map(|r| r.id.as_str()).collect();
        require(
            self.managed_capabilities
                .iter()
                .all(|id| compiled.contains_key(id) && names.contains(id.as_str())),
        )?;
        let mut identities = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        for pack in &self.packages {
            if pack
                .capabilities
                .contains_key(ai_platform_plugin_quiz::CAPABILITY_ID)
                && pack.id != ai_platform_plugin_quiz::PLUGIN_ID
            {
                return Err(PackError::Untrusted);
            }
            let publisher = self.publisher(pack)?;
            require(
                !pack.publisher_signature.is_empty()
                    && pack.publisher_signature.len() <= 8192
                    && publisher.allowed_packages.contains(&pack.id),
            )?;
            require(
                plugin_name(&pack.id)
                    && name(&pack.version)
                    && hash(&pack.artifact_sha256)
                    && identities.insert((&pack.id, &pack.version))
                    && hashes.insert(&pack.artifact_sha256)
                    && !pack.capabilities.is_empty()
                    && pack.capabilities.len() <= 64,
            )?;
            for (id, expected) in &pack.capabilities {
                require(self.managed_capabilities.contains(id) && hash(expected))?;
                let descriptor = compiled.get(id).ok_or(PackError::Incompatible)?;
                if json_hash(descriptor)? != *expected {
                    return Err(PackError::Incompatible);
                }
            }
        }
        Ok(())
    }
    fn publisher(&self, pack: &TrustedPack) -> Result<&TrustedPublisher> {
        let publisher = self
            .publishers
            .get(&pack.publisher_id)
            .ok_or(PackError::Untrusted)?;
        if publisher.revoked
            || !publisher.allowed_packages.contains(&pack.id)
            || !self
                .tenant_publishers
                .values()
                .any(|ids| ids.contains(&pack.publisher_id))
        {
            return Err(PackError::Untrusted);
        }
        Ok(publisher)
    }
    fn verify_publisher(&self, bytes: &[u8], pack: &TrustedPack) -> Result<()> {
        publisher_signatures::verify(
            &self.publisher(pack)?.minisign_public_key,
            &pack.publisher_signature,
            bytes,
        )
    }
    fn get(&self, id: &str, version: &str) -> Result<&TrustedPack> {
        self.packages
            .iter()
            .find(|p| p.id == id && p.version == version)
            .ok_or(PackError::Untrusted)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackReference {
    pub id: String,
    pub version: String,
    pub artifact_sha256: String,
}
impl From<&TrustedPack> for PackReference {
    fn from(p: &TrustedPack) -> Self {
        Self {
            id: p.id.clone(),
            version: p.version.clone(),
            artifact_sha256: p.artifact_sha256.clone(),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationManifest {
    pub schema_version: String,
    pub activation_id: String,
    pub generation: u64,
    pub previous_activation_id: Option<String>,
    pub trust_sha256: String,
    pub catalog_sha256: String,
    pub managed_capabilities: BTreeSet<String>,
    pub packages: Vec<PackReference>,
}
#[derive(Clone, Debug, Serialize)]
pub struct ActivationReceipt {
    pub worker_binary_sha256: String,
    pub activation_id: String,
    pub generation: u64,
    pub catalog_sha256: String,
    pub packages: Vec<PackReference>,
}
pub struct ActivePacks {
    tenant_publishers: BTreeMap<String, BTreeSet<String>>,
    capability_publishers: BTreeMap<String, String>,
    capability_packages: BTreeMap<String, PackReference>,
    pub receipt: ActivationReceipt,
    pub managed: BTreeSet<String>,
    pub registry: BTreeMap<String, CapabilityDescriptorV2>,
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PackExecutionBinding {
    pub activation_id: String,
    pub package_id: String,
    pub artifact_sha256: String,
    pub publisher_id: String,
}
impl ActivePacks {
    /// Construct actual invocation provenance only after per-tenant publisher
    /// admission. Hosts use this at dispatch, including durable recovery.
    pub fn execution_binding(
        &self,
        tenant: &str,
        capability: &str,
    ) -> Result<PackExecutionBinding> {
        if !self.managed.contains(capability)
            || !self.admits_tenant(tenant, capability)
            || !self.registry.contains_key(capability)
        {
            return Err(PackError::Untrusted);
        }
        let package = self
            .capability_packages
            .get(capability)
            .ok_or(PackError::Untrusted)?;
        let publisher = self
            .capability_publishers
            .get(capability)
            .ok_or(PackError::Untrusted)?;
        Ok(PackExecutionBinding {
            activation_id: self.receipt.activation_id.clone(),
            package_id: package.id.clone(),
            artifact_sha256: package.artifact_sha256.clone(),
            publisher_id: publisher.clone(),
        })
    }

    pub fn admits_tenant(&self, tenant: &str, capability: &str) -> bool {
        !self.managed.contains(capability)
            || self
                .capability_publishers
                .get(capability)
                .is_some_and(|publisher| {
                    self.tenant_publishers
                        .get(tenant)
                        .is_some_and(|allowed| allowed.contains(publisher))
                })
    }
    pub fn publisher_for(&self, capability: &str) -> Option<&str> {
        self.capability_publishers
            .get(capability)
            .map(String::as_str)
    }
}
#[derive(Debug, Serialize)]
pub struct StagedActivation {
    pub status: &'static str,
    pub activation_path: PathBuf,
    pub gateway_catalog_path: PathBuf,
    pub manifest: ActivationManifest,
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Desired {
    activation_id: String,
    generation: u64,
}

/// A deliberately small, strict USTAR reader. Archive libraries commonly hide
/// PAX/GNU indirection before returning entries; this format rejects it instead.
/// Uncompressed tar bounds both stored and expanded size. No extraction API is
/// called, no mode bits are honored, and no link or executable file is accepted.
fn archive_files(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    require(bytes.len() >= 1024 && bytes.len() <= MAX_BYTES && bytes.len() % 512 == 0)?;
    let mut offset = 0usize;
    let mut count = 0usize;
    let mut total = 0usize;
    let mut files = BTreeMap::new();
    let mut seen = BTreeSet::new();
    loop {
        let header = bytes.get(offset..offset + 512).ok_or(PackError::Invalid)?;
        if header.iter().all(|b| *b == 0) {
            require(bytes.len() - offset >= 1024 && bytes[offset..].iter().all(|b| *b == 0))?;
            break;
        }
        count += 1;
        require(
            count <= MAX_FILES && &header[257..263] == b"ustar\0" && &header[263..265] == b"00",
        )?;
        let expected = octal(&header[148..156])?;
        let actual: u64 = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    u64::from(*b)
                }
            })
            .sum();
        require(
            actual == expected
                && header[157..257].iter().all(|b| *b == 0)
                && header[345..500].iter().all(|b| *b == 0),
        )?;
        let raw_name = tar_text(&header[..100])?;
        let kind = header[156];
        require(matches!(kind, 0 | b'0' | b'5'))?;
        let path = if kind == b'5' {
            raw_name.strip_suffix('/').unwrap_or(raw_name)
        } else {
            raw_name
        };
        require(
            !path.is_empty()
                && !path.contains('\\')
                && path.len() <= 99
                && path.split('/').all(|p| {
                    !p.is_empty()
                        && p != "."
                        && p != ".."
                        && p.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
                })
                && seen.insert(path.to_owned()),
        )?;
        let size = usize::try_from(octal(&header[124..136])?).map_err(|_| PackError::Invalid)?;
        require(size <= MAX_FILE_BYTES && (kind != b'5' || size == 0))?;
        offset = offset.checked_add(512).ok_or(PackError::Invalid)?;
        let content = bytes
            .get(offset..offset.checked_add(size).ok_or(PackError::Invalid)?)
            .ok_or(PackError::Invalid)?;
        if kind != b'5' {
            require(path == "plugin.json" || path == "LICENSE" || path.ends_with(".md"))?;
            require(std::str::from_utf8(content).is_ok())?;
            total = total.checked_add(size).ok_or(PackError::Invalid)?;
            require(total <= MAX_BYTES)?;
            files.insert(path.to_owned(), content.to_vec());
        }
        let rounded = size.checked_add(511).ok_or(PackError::Invalid)? / 512 * 512;
        let end = offset.checked_add(rounded).ok_or(PackError::Invalid)?;
        require(
            bytes
                .get(offset + size..end)
                .is_some_and(|p| p.iter().all(|b| *b == 0)),
        )?;
        offset = end;
    }
    require(files.contains_key("plugin.json"))?;
    // Reject file/parent collisions, even if no explicit directory entry exists.
    for path in files.keys() {
        let parts: Vec<_> = path.split('/').collect();
        for n in 1..parts.len() {
            require(!files.contains_key(&parts[..n].join("/")))?;
        }
    }
    Ok(files)
}
fn tar_text(bytes: &[u8]) -> Result<&str> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    require(bytes[end..].iter().all(|b| *b == 0))?;
    std::str::from_utf8(&bytes[..end]).map_err(|_| PackError::Invalid)
}
fn octal(bytes: &[u8]) -> Result<u64> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| PackError::Invalid)?
        .trim_matches(['\0', ' ']);
    require(!text.is_empty() && text.bytes().all(|c| (b'0'..=b'7').contains(&c)))?;
    u64::from_str_radix(text, 8).map_err(|_| PackError::Invalid)
}
fn validate_artifact(
    bytes: &[u8],
    trusted: &TrustedPack,
    policy: &TrustPolicy,
) -> Result<BTreeMap<String, Vec<u8>>> {
    if digest(bytes) != trusted.artifact_sha256 {
        return Err(PackError::Untrusted);
    }
    let files = archive_files(bytes)?;
    policy.verify_publisher(bytes, trusted)?;
    let manifest: Value = strict_json(&files["plugin.json"])?;
    require(
        manifest["$schema"] == MANIFEST_SCHEMA
            && manifest["name"] == trusted.id
            && manifest["version"] == trusted.version
            && !files.contains_key("mcp.json"),
    )?;
    require(files["plugin.json"].len() <= 64 * 1024 && manifest.is_object())?;
    // Same portable manifest field types as agent_plugins.py; unknown fields
    // are inert metadata. Skills/agent Markdown is stored, never made trusted
    // instructions or interpreted by this executable-capability activation.
    for key in [
        "version",
        "description",
        "homepage",
        "repository",
        "license",
    ] {
        require(manifest.get(key).is_none_or(Value::is_string))?;
    }
    if let Some(author) = manifest.get("author") {
        let author = author.as_object().ok_or(PackError::Invalid)?;
        require(author.iter().all(|(key, value)| {
            ["name", "email", "url"].contains(&key.as_str()) && value.is_string()
        }))?;
    }
    if let Some(keywords) = manifest.get("keywords") {
        require(
            keywords
                .as_array()
                .is_some_and(|values| values.iter().all(Value::is_string)),
        )?;
    }
    require(
        manifest["extensions"]
            .as_object()
            .is_some_and(|values| values.values().all(Value::is_object)),
    )?;
    if manifest["extensions"]["com.misaya.ai-gateway"]["publisher_id"].as_str()
        != Some(trusted.publisher_id.as_str())
    {
        return Err(PackError::Untrusted);
    }
    let ids = manifest["extensions"]["com.misaya.ai-gateway"]["worker_capabilities"]
        .as_array()
        .ok_or(PackError::Invalid)?;
    let declared: BTreeSet<_> = ids
        .iter()
        .map(|id| id.as_str().ok_or(PackError::Invalid))
        .collect::<Result<_>>()?;
    require(
        declared.len() == ids.len()
            && declared == trusted.capabilities.keys().map(String::as_str).collect(),
    )?;
    Ok(files)
}

fn safe_directory(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.is_dir()
            && !metadata.file_type().is_symlink()
            && metadata.mode() & 0o022 == 0
            && (metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}
fn canonical_root(root: &Path, create: bool) -> Result<PathBuf> {
    require(root.is_absolute())?;
    if create && !root.exists() {
        private_dir(root)?;
    }
    let canonical = root.canonicalize().map_err(|_| PackError::Storage)?;
    require(
        canonical == root
            && safe_directory(&fs::symlink_metadata(root).map_err(|_| PackError::Storage)?),
    )?;
    Ok(canonical)
}
fn private_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder
            .mode(0o700)
            .create(path)
            .map_err(|_| PackError::Storage)?;
        sync_dir(path.parent().ok_or(PackError::Invalid)?)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(PackError::Unsupported)
    }
}
fn ensure_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => require(safe_directory(&meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => private_dir(path),
        Err(_) => Err(PackError::Storage),
    }
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|_| PackError::Storage)?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| PackError::Storage)?;
    sync_dir(path.parent().ok_or(PackError::Invalid)?)
}
fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| PackError::Storage)
}
fn json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    serde_json::to_vec_pretty(value).map_err(|_| PackError::Invalid)
}
struct StoreLock(File);
impl StoreLock {
    fn acquire(root: &Path) -> Result<Self> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::fs::OpenOptionsExt;
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(root.join("operation.lock"))
                .map_err(|_| PackError::Storage)?;
            // Nonblocking OS lock is released even on process death. No stale
            // lock-file removal or PID reuse policy is needed.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(PackError::Conflict);
            }
            Ok(Self(file))
        }
        #[cfg(not(unix))]
        {
            let _ = root;
            Err(PackError::Unsupported)
        }
    }
}
impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = self.0.sync_all();
    }
}
fn object_path(root: &Path, sha: &str) -> Result<PathBuf> {
    require(hash(sha))?;
    Ok(root.join("objects").join(&sha[7..]))
}
fn snapshot_dir(root: &Path, id: &str) -> Result<PathBuf> {
    require(Uuid::parse_str(id).is_ok_and(|v| v.to_string() == id))?;
    Ok(root.join("generations").join(id))
}
fn stored_artifact(root: &Path, trusted: &TrustedPack) -> Result<Vec<u8>> {
    let object = object_path(root, &trusted.artifact_sha256)?;
    require(object.canonicalize().map_err(|_| PackError::Storage)? == object)?;
    read_bounded(&object.join("artifact.tar"), MAX_BYTES)
}

/// Offline import only. It cannot activate a package or launch any service.
pub fn install(
    root: &Path,
    policy: &TrustPolicy,
    id: &str,
    version: &str,
    archive: &Path,
) -> Result<PackReference> {
    policy.validate()?;
    let trusted = policy.get(id, version)?;
    let bytes = read_bounded(archive, MAX_BYTES)?;
    let files = validate_artifact(&bytes, trusted, policy)?;
    let root = canonical_root(root, true)?;
    let _lock = StoreLock::acquire(&root)?;
    ensure_dir(&root.join("objects"))?;
    ensure_dir(&root.join("identities"))?;
    let identity = root
        .join("identities")
        .join(format!("{}@{}.json", trusted.id, trusted.version));
    match fs::symlink_metadata(&identity) {
        Ok(_) => {
            let existing: PackReference = strict_json(&read_bounded(&identity, MAX_FILE_BYTES)?)?;
            if existing != PackReference::from(trusted) {
                return Err(PackError::Conflict);
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(PackError::Storage),
    }
    let destination = object_path(&root, &trusted.artifact_sha256)?;
    if destination.exists() {
        validate_artifact(&stored_artifact(&root, trusted)?, trusted, policy)?;
        save_identity(&root, &identity, trusted)?;
        return Ok(PackReference::from(trusted));
    }
    let staging = root
        .join("objects")
        .join(format!(".staging-{}", Uuid::now_v7()));
    private_dir(&staging)?;
    let result = (|| {
        write_new(&staging.join("artifact.tar"), &bytes)?;
        let contents = staging.join("contents");
        private_dir(&contents)?;
        for (relative, payload) in files {
            let path = contents.join(&relative);
            let mut cursor = contents.clone();
            if let Some(parent) = Path::new(&relative).parent() {
                for component in parent.components() {
                    cursor.push(component);
                    ensure_dir(&cursor)?;
                }
            }
            write_new(&path, &payload)?;
        }
        write_new(
            &staging.join("installed.json"),
            &json_bytes(&PackReference::from(trusted))?,
        )?;
        sync_dir(&staging)?;
        fs::rename(&staging, &destination).map_err(|_| PackError::Storage)?;
        sync_dir(&root.join("objects"))?;
        save_identity(&root, &identity, trusted)?;
        Ok(PackReference::from(trusted))
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

fn save_identity(root: &Path, identity: &Path, trusted: &TrustedPack) -> Result<()> {
    if identity.exists() {
        return Ok(());
    }
    let temp = root
        .join("identities")
        .join(format!(".staging-{}", Uuid::now_v7()));
    write_new(&temp, &json_bytes(&PackReference::from(trusted))?)?;
    fs::rename(&temp, identity).map_err(|_| PackError::Storage)?;
    sync_dir(&root.join("identities"))
}

pub fn desired_activation(
    root: &Path,
    policy: &TrustPolicy,
    writes_enabled: bool,
) -> Result<StagedActivation> {
    let root = canonical_root(root, false)?;
    let desired: Desired = strict_json(&read_bounded(&root.join("desired.json"), MAX_FILE_BYTES)?)?;
    let activation_path = snapshot_dir(&root, &desired.activation_id)?.join("activation.json");
    let active = load_activation(&root, policy, &activation_path, writes_enabled)?;
    require(active.receipt.generation == desired.generation)?;
    let manifest = strict_json(&read_bounded(&activation_path, MAX_FILE_BYTES)?)?;
    Ok(StagedActivation {
        status: "desired_restart_not_observed",
        gateway_catalog_path: activation_path.with_file_name("catalog.json"),
        activation_path,
        manifest,
    })
}

fn projection(
    root: &Path,
    policy: &TrustPolicy,
    manifest: &ActivationManifest,
    writes_enabled: bool,
) -> Result<(BTreeMap<String, CapabilityDescriptorV2>, Value)> {
    policy.validate()?;
    require(
        manifest.schema_version == ACTIVATION_SCHEMA
            && manifest.generation > 0
            && Uuid::parse_str(&manifest.activation_id)
                .is_ok_and(|v| v.to_string() == manifest.activation_id)
            && manifest.packages.len() <= MAX_PACKS,
    )?;
    if manifest.trust_sha256 != json_hash(policy)?
        || manifest.managed_capabilities != policy.managed_capabilities
    {
        return Err(PackError::Untrusted);
    }
    let mut registry = worker_capability_catalog_with_writes(writes_enabled)
        .map_err(|_| PackError::Incompatible)?;
    let compiled = registry.clone();
    registry.retain(|id, _| !policy.managed_capabilities.contains(id));
    let mut attributed = BTreeMap::<String, &PackReference>::new();
    let mut seen = BTreeSet::new();
    for reference in &manifest.packages {
        require(seen.insert(&reference.id))?;
        let trusted = policy.get(&reference.id, &reference.version)?;
        if reference != &PackReference::from(trusted) {
            return Err(PackError::Untrusted);
        }
        let identity = root
            .join("identities")
            .join(format!("{}@{}.json", trusted.id, trusted.version));
        require(identity.canonicalize().map_err(|_| PackError::Storage)? == identity)?;
        let installed: PackReference = strict_json(&read_bounded(&identity, MAX_FILE_BYTES)?)?;
        if installed != *reference {
            return Err(PackError::Conflict);
        }
        validate_artifact(&stored_artifact(root, trusted)?, trusted, policy)?;
        for id in trusted.capabilities.keys() {
            require(attributed.insert(id.clone(), reference).is_none())?;
            let mut descriptor = compiled.get(id).ok_or(PackError::Incompatible)?.clone();
            if json_hash(&descriptor)? != trusted.capabilities[id] {
                return Err(PackError::Incompatible);
            }
            // Runtime snapshot/PG authorization already binds version. Binding
            // the activation and archive here prevents old leases from being
            // relabelled with a newer package or a rollback generation.
            descriptor.version =
                json_hash(&json!({"package":reference,"activation_id":manifest.activation_id}))?
                    [7..]
                    .to_owned();
            descriptor.tags.extend([
                format!("plugin-id:{}", reference.id),
                format!("plugin-version:{}", reference.version),
                format!("plugin-publisher:{}", trusted.publisher_id),
                format!("plugin-pack:{}", &reference.artifact_sha256[7..]),
                format!("plugin-activation:{}", manifest.activation_id),
            ]);
            descriptor.tags.sort();
            descriptor.tags.dedup();
            descriptor.validate().map_err(|_| PackError::Incompatible)?;
            registry.insert(id.clone(), descriptor);
        }
    }
    let mut catalog =
        serde_json::to_value(load_platform_catalog().map_err(|_| PackError::Incompatible)?)
            .map_err(|_| PackError::Invalid)?;
    catalog["capability_revision"] = json!(manifest.generation);
    let records = catalog["capabilities"]
        .as_array_mut()
        .ok_or(PackError::Invalid)?;
    for record in records {
        let id = record["id"].as_str().ok_or(PackError::Invalid)?.to_owned();
        if let Some(pack) = attributed.get(&id) {
            record["version"] = json!(registry[&id].version);
            record["plugin_id"] = json!(pack.id);
            record["plugin_version"] = json!(pack.version);
            record["content_hash"] = json!(pack.artifact_sha256);
            let publisher = &policy.get(&pack.id, &pack.version)?.publisher_id;
            let tenants: BTreeSet<&str> = policy
                .tenant_publishers
                .iter()
                .filter(|(_, allowed)| allowed.contains(publisher))
                .map(|(tenant, _)| tenant.as_str())
                .collect();
            record["plugin_metadata"] = json!({"activation_id":manifest.activation_id,
                "artifact_sha256":pack.artifact_sha256,"publisher_id":publisher,
                "publisher_tenants":serde_json::to_string(&tenants).map_err(|_| PackError::Invalid)?,
                "worker_active":true});
        } else if policy.managed_capabilities.contains(&id) {
            // Tombstones retain the deployment authority even for deactivate-all
            // and revoked packages, without inventing a plugin identity.
            record["plugin_metadata"] =
                json!({"activation_id":manifest.activation_id,"worker_active":false});
        }
    }
    Ok((registry, catalog))
}

/// Stage an immutable generation. Caller must restart both services using the
/// returned paths and compare actual health receipts; this is not live success.
pub fn activate(
    root: &Path,
    policy: &TrustPolicy,
    selected: &[(String, String)],
    expected_generation: u64,
    writes_enabled: bool,
) -> Result<StagedActivation> {
    require(selected.len() <= MAX_PACKS)?;
    let root = canonical_root(root, true)?;
    let _lock = StoreLock::acquire(&root)?;
    ensure_dir(&root.join("generations"))?;
    let desired = match fs::symlink_metadata(root.join("desired.json")) {
        Ok(_) => Some(strict_json::<Desired>(&read_bounded(
            &root.join("desired.json"),
            MAX_FILE_BYTES,
        )?)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(PackError::Storage),
    };
    if desired.as_ref().map_or(0, |d| d.generation) != expected_generation {
        return Err(PackError::Conflict);
    }
    if let Some(previous) = &desired {
        let path = snapshot_dir(&root, &previous.activation_id)?.join("activation.json");
        let prior: ActivationManifest = strict_json(&read_bounded(&path, MAX_FILE_BYTES)?)?;
        require(
            prior.activation_id == previous.activation_id
                && prior.generation == previous.generation,
        )?;
        // Revocation cannot restore deployment defaults by dropping a name from
        // the trust file. Management is monotonic for this installation store.
        if !prior
            .managed_capabilities
            .is_subset(&policy.managed_capabilities)
        {
            return Err(PackError::Untrusted);
        }
    }
    let generation = expected_generation
        .checked_add(1)
        .ok_or(PackError::Conflict)?;
    let mut packages = selected
        .iter()
        .map(|(id, version)| policy.get(id, version).map(PackReference::from))
        .collect::<Result<Vec<_>>>()?;
    packages.sort_by(|a, b| a.id.cmp(&b.id));
    let mut manifest = ActivationManifest {
        schema_version: ACTIVATION_SCHEMA.into(),
        activation_id: Uuid::now_v7().to_string(),
        generation,
        previous_activation_id: desired.map(|d| d.activation_id),
        trust_sha256: json_hash(policy)?,
        catalog_sha256: String::new(),
        managed_capabilities: policy.managed_capabilities.clone(),
        packages,
    };
    let (_, catalog) = projection(&root, policy, &manifest, writes_enabled)?;
    let catalog_bytes = json_bytes(&catalog)?;
    manifest.catalog_sha256 = digest(&catalog_bytes);
    let destination = snapshot_dir(&root, &manifest.activation_id)?;
    let staging = root
        .join("generations")
        .join(format!(".staging-{}", manifest.activation_id));
    private_dir(&staging)?;
    let result = (|| {
        write_new(&staging.join("catalog.json"), &catalog_bytes)?;
        write_new(&staging.join("activation.json"), &json_bytes(&manifest)?)?;
        sync_dir(&staging)?;
        fs::rename(&staging, &destination).map_err(|_| PackError::Storage)?;
        sync_dir(&root.join("generations"))?;
        let temp = root.join(format!(".desired-{}", manifest.activation_id));
        write_new(
            &temp,
            &json_bytes(&Desired {
                activation_id: manifest.activation_id.clone(),
                generation,
            })?,
        )?;
        fs::rename(&temp, root.join("desired.json")).map_err(|_| PackError::Storage)?;
        sync_dir(&root)?;
        Ok(StagedActivation {
            status: "staged_restart_required",
            activation_path: destination.join("activation.json"),
            gateway_catalog_path: destination.join("catalog.json"),
            manifest,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub fn rollback(
    root: &Path,
    policy: &TrustPolicy,
    prior_id: &str,
    expected_generation: u64,
    writes_enabled: bool,
) -> Result<StagedActivation> {
    let root = canonical_root(root, false)?;
    let bytes = read_bounded(
        &snapshot_dir(&root, prior_id)?.join("activation.json"),
        MAX_FILE_BYTES,
    )?;
    let prior: ActivationManifest = strict_json(&bytes)?;
    require(prior.activation_id == prior_id && prior.schema_version == ACTIVATION_SCHEMA)?;
    // Old approval is never grandfathered. Current allowlist and compiled
    // descriptor fingerprints are checked again by activate/projection.
    let mut selected = Vec::new();
    for pack in &prior.packages {
        let current = policy.get(&pack.id, &pack.version)?;
        if pack != &PackReference::from(current) {
            return Err(PackError::Untrusted);
        }
        selected.push((pack.id.clone(), pack.version.clone()));
    }
    activate(
        &root,
        policy,
        &selected,
        expected_generation,
        writes_enabled,
    )
}

/// Startup admission is distinct from inspection: only a fully published desired
/// generation may become a new running snapshot. Re-read after validation so a
/// pointer change during artifact/catalog loading cannot admit stale startup.
/// A later activation does not mutate a snapshot already admitted here.
pub fn load_current_activation(
    root: &Path,
    policy: &TrustPolicy,
    activation_path: &Path,
    writes_enabled: bool,
) -> Result<ActivePacks> {
    let active = load_activation(root, policy, activation_path, writes_enabled)?;
    require_current_activation(root, &active.receipt)?;
    Ok(active)
}

fn require_current_activation(root: &Path, receipt: &ActivationReceipt) -> Result<()> {
    let root = canonical_root(root, false)?;
    let desired: Desired = strict_json(&read_bounded(&root.join("desired.json"), MAX_FILE_BYTES)?)?;
    if desired.activation_id != receipt.activation_id || desired.generation != receipt.generation {
        return Err(PackError::Conflict);
    }
    Ok(())
}

/// Inspect an immutable generation under current trust without admitting startup.
pub fn load_activation(
    root: &Path,
    policy: &TrustPolicy,
    activation_path: &Path,
    writes_enabled: bool,
) -> Result<ActivePacks> {
    let root = canonical_root(root, false)?;
    let manifest: ActivationManifest =
        strict_json(&read_bounded(activation_path, MAX_FILE_BYTES)?)?;
    let expected = snapshot_dir(&root, &manifest.activation_id)?.join("activation.json");
    require(
        activation_path == expected
            && expected.canonicalize().map_err(|_| PackError::Storage)? == expected,
    )?;
    // Selecting an old generation path directly is not a supported way to
    // undo newer managed-ID restrictions. Use rollback to create a new receipt.
    let desired: Desired = strict_json(&read_bounded(&root.join("desired.json"), MAX_FILE_BYTES)?)?;
    let current_path = snapshot_dir(&root, &desired.activation_id)?.join("activation.json");
    let current: ActivationManifest = strict_json(&read_bounded(&current_path, MAX_FILE_BYTES)?)?;
    require(
        current.activation_id == desired.activation_id && current.generation == desired.generation,
    )?;
    if !current
        .managed_capabilities
        .is_subset(&manifest.managed_capabilities)
    {
        return Err(PackError::Untrusted);
    }
    let (registry, catalog) = projection(&root, policy, &manifest, writes_enabled)?;
    let catalog_bytes = read_bounded(&expected.with_file_name("catalog.json"), MAX_BYTES)?;
    require(
        digest(&catalog_bytes) == manifest.catalog_sha256 && catalog_bytes == json_bytes(&catalog)?,
    )?;
    let capability_publishers = manifest
        .packages
        .iter()
        .map(|p| policy.get(&p.id, &p.version).map(|trusted| (p, trusted)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flat_map(|(_, t)| {
            t.capabilities
                .keys()
                .map(move |id| (id.clone(), t.publisher_id.clone()))
        })
        .collect();
    let capability_packages = manifest
        .packages
        .iter()
        .map(|p| policy.get(&p.id, &p.version).map(|t| (p, t)))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .flat_map(|(p, t)| t.capabilities.keys().map(move |id| (id.clone(), p.clone())))
        .collect();
    Ok(ActivePacks {
        capability_packages,
        receipt: ActivationReceipt {
            worker_binary_sha256: policy.worker_binary_sha256.clone(),
            activation_id: manifest.activation_id,
            generation: manifest.generation,
            catalog_sha256: manifest.catalog_sha256,
            packages: manifest.packages,
        },
        capability_publishers,
        tenant_publishers: policy.tenant_publishers.clone(),
        managed: policy.managed_capabilities.clone(),
        registry,
    })
}

/// Fingerprints are provenance input, not permission to execute an artifact.
pub fn compiled_fingerprints() -> Result<BTreeMap<String, String>> {
    worker_capability_catalog_with_writes(true)
        .map_err(|_| PackError::Incompatible)?
        .iter()
        .map(|(id, value)| Ok((id.clone(), json_hash(value)?)))
        .collect()
}

/// Hashes an explicit executable file without running it or reading credentials.
/// Startup uses current_exe, never a request-controlled executable or environment
/// version label, to bind activation to actual executable bytes.
pub fn executable_hash(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PackError::Storage)?;
    require(
        metadata.is_file()
            && !metadata.file_type().is_symlink()
            && metadata.len() > 0
            && metadata.len() <= 256 * 1024 * 1024,
    )?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(path).map_err(|_| PackError::Storage)?;
    let mut digest = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut chunk).map_err(|_| PackError::Storage)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        require(total <= 256 * 1024 * 1024)?;
        digest.update(&chunk[..count]);
    }
    require(total == metadata.len())?;
    Ok(format!("sha256:{:x}", digest.finalize()))
}
pub fn verify_running_worker(policy: &TrustPolicy) -> Result<()> {
    let executable = std::env::current_exe().map_err(|_| PackError::Storage)?;
    if executable_hash(&executable)? != policy.worker_binary_sha256 {
        return Err(PackError::Incompatible);
    }
    Ok(())
}

#[cfg(test)]
mod publisher_v2_tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use ed25519_dalek::{Signer, SigningKey};
    fn signing(bytes: &[u8], seed: u8) -> (String, String) {
        let key = SigningKey::from_bytes(&[seed; 32]);
        let id = [seed; 8];
        let raw = key.sign(bytes);
        let mut p = b"Ed".to_vec();
        p.extend(id);
        p.extend(key.verifying_key().as_bytes());
        let mut s = b"Ed".to_vec();
        s.extend(id);
        s.extend(raw.to_bytes());
        let comment = "timestamp:1700000000 file:ai-quiz.tar";
        let mut message = raw.to_bytes().to_vec();
        message.extend(comment.as_bytes());
        (
            format!(
                "untrusted comment: fixture publisher\n{}\n",
                STANDARD.encode(p)
            ),
            format!(
                "untrusted comment: fixture signature\n{}\ntrusted comment: {comment}\n{}\n",
                STANDARD.encode(s),
                STANDARD.encode(key.sign(&message).to_bytes())
            ),
        )
    }
    fn archive() -> Vec<u8> {
        let manifest=serde_json::to_vec(&json!({"$schema":MANIFEST_SCHEMA,"name":"ai-quiz","version":"1.1.0","description":"fixture","extensions":{"com.misaya.ai-gateway":{"worker_capabilities":["generate_quiz"],"publisher_id":"reviewed"}}})).unwrap();
        let mut header = [0u8; 512];
        header[..11].copy_from_slice(b"plugin.json");
        header[124..136].copy_from_slice(format!("{:011o}\0", manifest.len()).as_bytes());
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum: u32 = header.iter().map(|v| u32::from(*v)).sum();
        header[148..156].copy_from_slice(format!("{checksum:06o}\0 ").as_bytes());
        let mut bytes = header.to_vec();
        bytes.extend(&manifest);
        bytes.resize(512 + manifest.len().div_ceil(512) * 512 + 1024, 0);
        bytes
    }
    fn policy(bytes: &[u8]) -> TrustPolicy {
        let (key, signature) = signing(bytes, 12);
        TrustPolicy {
            schema_version: TRUST_SCHEMA.into(),
            worker_binary_sha256: format!("sha256:{}", "a".repeat(64)),
            managed_capabilities: BTreeSet::from(["generate_quiz".into()]),
            publishers: BTreeMap::from([(
                "reviewed".into(),
                TrustedPublisher {
                    minisign_public_key: key,
                    allowed_packages: BTreeSet::from(["ai-quiz".into()]),
                    revoked: false,
                },
            )]),
            tenant_publishers: BTreeMap::from([(
                "tenant-one".into(),
                BTreeSet::from(["reviewed".into()]),
            )]),
            packages: vec![TrustedPack {
                id: "ai-quiz".into(),
                version: "1.1.0".into(),
                artifact_sha256: digest(bytes),
                capabilities: BTreeMap::from([(
                    "generate_quiz".into(),
                    compiled_fingerprints().unwrap()["generate_quiz"].clone(),
                )]),
                publisher_id: "reviewed".into(),
                publisher_signature: signature,
            }],
        }
    }
    #[test]
    fn trusted_publisher_signature_is_required_in_addition_to_every_digest_pin() {
        let bytes = archive();
        let p = policy(&bytes);
        p.validate().unwrap();
        validate_artifact(&bytes, &p.packages[0], &p).unwrap();
        let mut unsigned = p.clone();
        unsigned.packages[0].publisher_signature.clear();
        assert!(unsigned.validate().is_err());
        assert!(validate_artifact(&bytes, &unsigned.packages[0], &unsigned).is_err());
        let mut changed = bytes.clone();
        changed[512] ^= 1;
        assert!(validate_artifact(&changed, &p.packages[0], &p).is_err());
        let mut repinned = p.clone();
        repinned.packages[0].artifact_sha256 = digest(&changed);
        assert!(validate_artifact(&changed, &repinned.packages[0], &repinned).is_err());
        let mut wrong = p.clone();
        wrong.packages[0].publisher_id = "foreign".into();
        wrong.publishers.insert(
            "foreign".into(),
            TrustedPublisher {
                minisign_public_key: signing(b"", 13).0,
                allowed_packages: BTreeSet::from(["ai-quiz".into()]),
                revoked: false,
            },
        );
        wrong
            .tenant_publishers
            .get_mut("tenant-one")
            .unwrap()
            .insert("foreign".into());
        assert!(validate_artifact(&bytes, &wrong.packages[0], &wrong).is_err());
    }
    #[test]
    fn revoked_publishers_unapproved_names_and_v1_cannot_restore_unsigned_install() {
        let bytes = archive();
        let p = policy(&bytes);
        let mut revoked = p.clone();
        revoked.publishers.get_mut("reviewed").unwrap().revoked = true;
        assert!(revoked.validate().is_err());
        assert!(validate_artifact(&bytes, &revoked.packages[0], &revoked).is_err());
        let mut denied = p.clone();
        denied
            .publishers
            .get_mut("reviewed")
            .unwrap()
            .allowed_packages = BTreeSet::from(["other-plugin".into()]);
        assert!(validate_artifact(&bytes, &denied.packages[0], &denied).is_err());
        let mut alias = p.clone();
        alias.publishers.insert(
            "same-key-other-name".into(),
            alias.publishers["reviewed"].clone(),
        );
        assert!(alias.validate().is_err());
        let mut old = p.clone();
        old.schema_version = "ai-platform-plugin-trust/v1".into();
        assert!(old.validate().is_err());
        let mut wildcard = p;
        wildcard.tenant_publishers =
            BTreeMap::from([("*".into(), BTreeSet::from(["reviewed".into()]))]);
        assert!(wildcard.validate().is_err());
    }
    #[test]
    fn per_tenant_publisher_selection_is_closed_for_unknown_tenants_and_disabled_packs() {
        let mut active = ActivePacks {
            capability_packages: BTreeMap::new(),
            tenant_publishers: BTreeMap::from([(
                "tenant-one".into(),
                BTreeSet::from(["reviewed".into()]),
            )]),
            capability_publishers: BTreeMap::from([("generate_quiz".into(), "reviewed".into())]),
            receipt: ActivationReceipt {
                worker_binary_sha256: format!("sha256:{}", "a".repeat(64)),
                activation_id: Uuid::now_v7().to_string(),
                generation: 1,
                catalog_sha256: format!("sha256:{}", "b".repeat(64)),
                packages: vec![],
            },
            managed: BTreeSet::from(["generate_quiz".into()]),
            registry: BTreeMap::new(),
        };
        assert!(active.admits_tenant("tenant-one", "generate_quiz"));
        assert!(!active.admits_tenant("tenant-two", "generate_quiz"));
        assert!(!active.admits_tenant("", "generate_quiz"));
        active.capability_publishers.clear();
        assert!(!active.admits_tenant("tenant-one", "generate_quiz"));
    }
    #[tokio::test]
    async fn signed_quiz_activation_controls_worker_admission_and_persistence_context() {
        let bytes = archive();
        let policy = policy(&bytes);
        let base = std::env::temp_dir().canonicalize().unwrap();
        let root = base.join(format!("ai-quiz-wiring-{}", Uuid::now_v7()));
        private_dir(&root).unwrap();
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _scratch = Scratch(root.clone());
        let archive_path = root.join("fixture.tar");
        write_new(&archive_path, &bytes).unwrap();
        install(&root, &policy, "ai-quiz", "1.1.0", &archive_path).unwrap();
        let staged = activate(
            &root,
            &policy,
            &[("ai-quiz".into(), "1.1.0".into())],
            0,
            true,
        )
        .unwrap();
        let active =
            load_current_activation(&root, &policy, &staged.activation_path, true).unwrap();
        let descriptor = active.registry["generate_quiz"].clone();
        let catalog: Value =
            serde_json::from_slice(&fs::read(&staged.gateway_catalog_path).unwrap()).unwrap();
        let record = catalog["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "generate_quiz")
            .unwrap();
        assert_eq!(record["plugin_metadata"]["publisher_id"], "reviewed");
        let tenants: Vec<String> = serde_json::from_str(
            record["plugin_metadata"]["publisher_tenants"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(tenants, vec!["tenant-one"]);
        let binding = active
            .execution_binding("tenant-one", "generate_quiz")
            .unwrap();
        assert!(
            active
                .execution_binding("tenant-two", "generate_quiz")
                .is_err()
        );
        let quiz_binding = crate::quiz_capabilities::QuizPluginBinding {
            activation_id: binding.activation_id,
            artifact_sha256: binding.artifact_sha256,
            publisher_id: binding.publisher_id,
        };
        assert!(quiz_binding.matches(&json!({"descriptor":descriptor,
            "plugin_activation":{"activation_id":active.receipt.activation_id,"capability_version":descriptor.version}})));
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://fixture:fixture@127.0.0.1:1/fixture")
            .unwrap();
        let state = crate::http_service::WorkerState::try_new_with_writes(
            std::sync::Arc::new(crate::MemoryStore::default()),
            "token".into(),
            vec![7; 32],
            false,
            true,
        )
        .unwrap()
        .with_quiz_executor(std::sync::Arc::new(
            crate::quiz_capabilities::QuizPersistenceAdapter::new(pool),
        ))
        .with_plugin_activation(active)
        .unwrap();
        assert!(state.admits_plugin_descriptor("tenant-one", &descriptor));
        assert!(!state.admits_plugin_descriptor("tenant-two", &descriptor));
        let mut stale = descriptor.clone();
        stale.version = "old-generation".into();
        assert!(!state.admits_plugin_descriptor("tenant-one", &stale));
        let disabled = activate(&root, &policy, &[], 1, true).unwrap();
        let disabled =
            load_current_activation(&root, &policy, &disabled.activation_path, true).unwrap();
        let state = state.with_plugin_activation(disabled).unwrap();
        assert!(!state.capability_registry.contains_key("generate_quiz"));
        assert!(!state.admits_plugin_descriptor("tenant-one", &descriptor));
        assert!(load_current_activation(&root, &policy, &staged.activation_path, true).is_err());
    }
}
