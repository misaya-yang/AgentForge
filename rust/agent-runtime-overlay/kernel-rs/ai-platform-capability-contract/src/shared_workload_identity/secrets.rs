//! Bounded private file reads. No plaintext fallback or credential diagnostics.
use super::{Error, Result};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
pub struct SecretBytes(Vec<u8>);
impl SecretBytes {
    /// An explicit configuration owner may already have resolved a private file.
    /// Callers still apply the consuming credential's length/format validation.
    pub fn from_configuration(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
    pub(crate) fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}
impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.fill(0)
    }
}
pub fn read_file(path: &Path, private: bool) -> Result<SecretBytes> {
    if !path.is_absolute() {
        return Err(Error("secret_file_absolute_required"));
    }
    let before = fs::symlink_metadata(path).map_err(|_| Error("secret_file_unavailable"))?;
    if !before.is_file() || before.len() == 0 || before.len() > 256 * 1024 {
        return Err(Error("secret_file_invalid"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o022 != 0
            || (private && before.permissions().mode() & 0o077 != 0)
        {
            return Err(Error("secret_file_permissions"));
        }
    }
    #[cfg(not(unix))]
    if private {
        return Err(Error("secret_file_private_acl_adapter_required"));
    }
    let file = fs::File::open(path).map_err(|_| Error("secret_file_unavailable"))?;
    let opened = file
        .metadata()
        .map_err(|_| Error("secret_file_unavailable"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != opened.dev() || before.ino() != opened.ino() {
            return Err(Error("secret_file_changed"));
        }
    }
    if !opened.is_file() {
        return Err(Error("secret_file_invalid"));
    }
    let mut bytes = SecretBytes::new(Vec::new());
    file.take(256 * 1024 + 1)
        .read_to_end(&mut bytes.0)
        .map_err(|_| Error("secret_file_unavailable"))?;
    if bytes.0.is_empty() || bytes.0.len() > 256 * 1024 {
        return Err(Error("secret_file_invalid"));
    }
    Ok(bytes)
}
/// `_FILE` values replace inline secrets at the configuration owner. A conflicting
/// inline value is an error, never a precedence rule. Returned bytes are not logged.
pub fn environment_secret(
    env: &BTreeMap<String, String>,
    name: &str,
) -> Result<Option<SecretBytes>> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(Error("secret_setting_invalid"));
    }
    if env.get(name).is_some_and(|v| !v.is_empty()) {
        return Err(Error("inline_secret_forbidden"));
    }
    let Some(path) = env.get(&format!("{name}_FILE")) else {
        return Ok(None);
    };
    let mut bytes = read_file(Path::new(path), true)?;
    // Secret mounts commonly have one terminal newline, never trim significant spaces.
    if bytes.0.ends_with(b"\n") {
        bytes.0.pop();
        if bytes.0.ends_with(b"\r") {
            bytes.0.pop();
        }
    }
    if bytes.0.is_empty() || bytes.0.len() > 64 * 1024 || bytes.0.contains(&0) {
        return Err(Error("secret_value_invalid"));
    }
    Ok(Some(bytes))
}
#[derive(Clone)]
pub struct CredentialFiles {
    root: PathBuf,
}
impl CredentialFiles {
    pub fn new(root: PathBuf) -> Result<Self> {
        if !root.is_absolute() {
            return Err(Error("workload_credentials_absolute_required"));
        }
        Ok(Self { root })
    }
    /// Resolve one generation once, so an atomic `current` switch cannot mix keys,
    /// certificates and roots from two generations. Kube projected files must be
    /// copied by the rotator into an owned private generation, never relaxed here.
    pub(crate) fn generation(&self) -> Result<PathBuf> {
        let root =
            fs::canonicalize(&self.root).map_err(|_| Error("workload_generation_unavailable"))?;
        let generation = fs::canonicalize(root.join("current"))
            .map_err(|_| Error("workload_generation_unavailable"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for directory in [&root, &generation] {
                let metadata = fs::metadata(directory)
                    .map_err(|_| Error("workload_generation_unavailable"))?;
                if metadata.permissions().mode() & 0o022 != 0 {
                    return Err(Error("workload_generation_writable"));
                }
            }
        }
        if generation == root || !generation.starts_with(&root) || !generation.is_dir() {
            return Err(Error("workload_generation_escape"));
        }
        Ok(generation)
    }
}
