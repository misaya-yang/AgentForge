//! Shipped source integrity is independent of historical generator ancestry.
//! The package lock is reviewed and pinned in this executable, never repaired
//! from database state or regenerated during installation.
use crate::{AuthorityError, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path},
};

pub const LOCK_SHA256: &str = "912572fc1a7abad3587051d7c37ca4d999c79a187b96bd1eba9dac09305b7789";
pub const PREDECESSOR_LOCK_SHA256: &str =
    "13a31fc7ca785e218f5c799f962226fbe4b393df1c0ef1abd079117cd24baf54";
const LOCK: &str = "authority/package-authority-lock.json";
#[derive(Deserialize)]
struct Distribution {
    schema: String,
    baseline_id: String,
    files_sha256: BTreeMap<String, String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMode {
    Development,
    Distribution,
}
impl SourceMode {
    pub fn from_process() -> Result<Self> {
        match std::env::var("AI_GATEWAY_AUTHORITY_SOURCE_MODE").as_deref() {
            Ok("distribution") => Ok(Self::Distribution),
            Ok("development") | Err(std::env::VarError::NotPresent) => Ok(Self::Development),
            _ => Err(refused("unknown authority source mode")),
        }
    }
}
fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn regular(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(refused(
            "authority distribution requires regular package files",
        ));
    }
    Ok(())
}
fn digest(path: &Path) -> Result<String> {
    regular(path)?;
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn inputs(root: &Path, directory: &Path, result: &mut BTreeMap<String, String>) -> Result<()> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(refused(
            "authority distribution requires in-package directories",
        ));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_symlink() {
            return Err(refused("authority distribution does not admit symlinks"));
        }
        if kind.is_dir() {
            inputs(root, &path, result)?;
        } else if matches!(
            path.extension().and_then(|v| v.to_str()),
            Some("sql" | "json" | "yml" | "yaml")
        ) {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| refused("escaped authority input"))?
                .to_str()
                .ok_or_else(|| refused("invalid authority input name"))?
                .replace('\\', "/");
            result.insert(relative, digest(&path)?);
        }
    }
    Ok(())
}
pub fn verify(root: &Path, baseline_id: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(refused("authority distribution root must be a directory"));
    }
    let authority = fs::symlink_metadata(root.join("authority"))?;
    if !authority.is_dir() || authority.file_type().is_symlink() {
        return Err(refused("authority distribution lock directory is invalid"));
    }
    let lock = root.join(LOCK);
    let lock_sha256 = digest(&lock)?;
    if ![LOCK_SHA256, PREDECESSOR_LOCK_SHA256].contains(&lock_sha256.as_str()) {
        return Err(refused(
            "authority distribution lock differs from the release pin",
        ));
    }
    let distribution: Distribution = serde_json::from_slice(&fs::read(lock)?)
        .map_err(|_| refused("invalid authority distribution lock"))?;
    if distribution.schema != "migration-authority/source-distribution/v1"
        || distribution.baseline_id != baseline_id
    {
        return Err(refused("authority distribution baseline does not match"));
    }
    for relative in distribution.files_sha256.keys() {
        let path = Path::new(relative);
        if path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
            || relative.contains('\\')
        {
            return Err(refused("unsafe authority distribution input"));
        }
    }
    let mut actual = BTreeMap::new();
    for directory in ["baselines", "migrations", "bootstrap"] {
        inputs(root, &root.join(directory), &mut actual)?;
    }
    actual.insert(
        "authority/ledger_acl_v1.json".into(),
        digest(&root.join("authority/ledger_acl_v1.json"))?,
    );
    if actual != distribution.files_sha256 {
        return Err(refused(
            "authority distribution input coverage or checksum drift",
        ));
    }
    Ok(())
}
