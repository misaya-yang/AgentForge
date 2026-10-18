//! Immutable tenant-to-distribution grants. Optional wheels are installed outside
//! the globally readable stdlib tree; only selected dependency closures receive
//! Landlock read access. No package name/path comes from user code or tool args.
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub const CURATED_PACKAGE_ROOT: &str = "/opt/agentforge/python-packages";
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Distribution {
    version: String,
    dependencies: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema_version: String,
    python_minor: String,
    architecture: String,
    packages: BTreeMap<String, Distribution>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyFile {
    schema_version: String,
    catalog_sha256: String,
    tenants: BTreeMap<String, BTreeSet<String>>,
}
#[derive(Clone)]
pub struct TenantPackagePolicy {
    root: PathBuf,
    packages: BTreeMap<String, Distribution>,
    tenants: BTreeMap<String, BTreeSet<String>>,
}
fn canonical_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}
fn read_small(path: &Path, maximum: u64) -> Result<Vec<u8>, &'static str> {
    use std::io::Read;
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "python_package_policy_unavailable")?;
    if !metadata.is_file() || metadata.len() > maximum {
        return Err("python_package_policy_invalid");
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| "python_package_policy_unavailable")?
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "python_package_policy_unavailable")?;
    if bytes.len() as u64 > maximum {
        return Err("python_package_policy_invalid");
    }
    Ok(bytes)
}
impl TenantPackagePolicy {
    pub fn load(path: &Path, python: &Path) -> Result<Self, &'static str> {
        let policy = read_small(path, 1024 * 1024)?;
        let root = Path::new(CURATED_PACKAGE_ROOT);
        let catalog = read_small(&root.join("catalog.json"), 128 * 1024)?;
        let selected = Self::from_bytes(&policy, &catalog, root)?;
        verify_python_runtime(python)?;
        Ok(selected)
    }
    fn from_bytes(policy: &[u8], catalog: &[u8], root: &Path) -> Result<Self, &'static str> {
        let policy: PolicyFile =
            serde_json::from_slice(policy).map_err(|_| "python_package_policy_invalid")?;
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(catalog)));
        if policy.schema_version != "python-tenant-package-policy/v1"
            || policy.catalog_sha256 != digest
            || policy.tenants.len() > 4096
        {
            return Err("python_package_policy_invalid");
        }
        let catalog: Catalog =
            serde_json::from_slice(catalog).map_err(|_| "python_package_catalog_invalid")?;
        let architecture = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => return Err("python_package_architecture_unsupported"),
        };
        if catalog.schema_version != "python-curated-catalog/v1"
            || catalog.python_minor != "3.14"
            || catalog.architecture != architecture
            || catalog.packages.len() > 64
        {
            return Err("python_package_catalog_invalid");
        }
        for (name, package) in &catalog.packages {
            if !canonical_name(name)
                || package.version.is_empty()
                || package.version.len() > 64
                || package
                    .dependencies
                    .iter()
                    .any(|dependency| !catalog.packages.contains_key(dependency))
            {
                return Err("python_package_catalog_invalid");
            }
        }
        for (tenant, selected) in &policy.tenants {
            if tenant.is_empty()
                || tenant.len() > 255
                || tenant.bytes().any(|byte| byte.is_ascii_control())
                || selected
                    .iter()
                    .any(|name| !catalog.packages.contains_key(name))
            {
                return Err("python_package_policy_invalid");
            }
        }
        let policy = Self {
            root: root.into(),
            packages: catalog.packages,
            tenants: policy.tenants,
        };
        for tenant in policy.tenants.keys() {
            policy.paths_for(tenant)?;
        }
        Ok(policy)
    }
    pub fn paths_for(&self, tenant: &str) -> Result<Vec<PathBuf>, &'static str> {
        let mut selected = BTreeSet::new();
        let mut pending: Vec<_> = self
            .tenants
            .get(tenant)
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        while let Some(name) = pending.pop() {
            if !selected.insert(name.clone()) {
                continue;
            }
            let package = self
                .packages
                .get(&name)
                .ok_or("python_package_policy_invalid")?;
            pending.extend(package.dependencies.iter().cloned());
        }
        selected
            .into_iter()
            .map(|name| {
                let path = self.root.join(name);
                validate_package_root_in(&path, &self.root)?;
                Ok(path)
            })
            .collect()
    }
}

pub(crate) fn validate_package_root(path: &Path) -> Result<(), &'static str> {
    validate_package_root_in(path, Path::new(CURATED_PACKAGE_ROOT))
}
fn validate_package_root_in(path: &Path, root: &Path) -> Result<(), &'static str> {
    if !path.is_absolute()
        || path.parent() != Some(root)
        || !canonical_name(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(""),
        )
    {
        return Err("python_package_root_invalid");
    }
    let metadata =
        std::fs::symlink_metadata(path).map_err(|_| "python_package_root_unavailable")?;
    if !metadata.is_dir() {
        return Err("python_package_root_invalid");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o222 != 0 {
            return Err("python_package_root_writable");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tenant_closures_are_exact_and_unknown_tenant_is_stdlib_only() {
        let root =
            std::env::temp_dir().join(format!("agentforge-package-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        for name in ["numpy", "pandas", "pillow"] {
            let path = root.join(name);
            std::fs::create_dir(&path).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555)).unwrap();
            }
        }
        let architecture = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "amd64"
        };
        let catalog = serde_json::to_vec(&serde_json::json!({"schema_version":"python-curated-catalog/v1","python_minor":"3.14","architecture":architecture,
            "packages":{"numpy":{"version":"pinned","dependencies":[]},"pandas":{"version":"pinned","dependencies":["numpy"]},"pillow":{"version":"pinned","dependencies":[]}}})).unwrap();
        let policy = serde_json::to_vec(&serde_json::json!({"schema_version":"python-tenant-package-policy/v1",
            "catalog_sha256":format!("sha256:{}",hex::encode(Sha256::digest(&catalog))),"tenants":{"tenant-a":["pandas"],"tenant-b":["pillow"]}})).unwrap();
        let selected = TenantPackagePolicy::from_bytes(&policy, &catalog, &root).unwrap();
        assert_eq!(
            selected.paths_for("tenant-a").unwrap(),
            vec![root.join("numpy"), root.join("pandas")]
        );
        assert_eq!(
            selected.paths_for("tenant-b").unwrap(),
            vec![root.join("pillow")]
        );
        assert!(selected.paths_for("unknown").unwrap().is_empty());
        let mut corrupted = catalog.clone();
        corrupted.push(b' ');
        assert!(TenantPackagePolicy::from_bytes(&policy, &corrupted, &root).is_err());
        for name in ["numpy", "pandas", "pillow"] {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o700))
                    .unwrap();
            }
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn package_names_cannot_select_paths_or_wheels_at_runtime() {
        for name in [
            "../secret",
            "/etc",
            "numpy==latest",
            "NumPy",
            "",
            "wheel.whl",
        ] {
            assert!(!canonical_name(name));
        }
        assert!(canonical_name("python-dateutil"));
        assert!(validate_package_root(Path::new("/etc")).is_err());
    }
}

/// The stdlib-only sandbox has the same pinned interpreter contract as tenants
/// that select curated wheels. A missing policy must not admit Python 3.11.
pub fn verify_python_runtime(python: &Path) -> Result<(), &'static str> {
    let mut child = std::process::Command::new(python)
        .args([
            "-I",
            "-S",
            "-c",
            "import sys; raise SystemExit(sys.version_info[:2] != (3, 14))",
        ])
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "python_314_unavailable")?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err("python_314_unavailable")
                };
            }
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("python_314_unavailable");
            }
        }
    }
}
