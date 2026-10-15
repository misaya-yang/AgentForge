use super::{LocalNodeError as Error, Result, models::now};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::RwLock,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectoryGrant {
    pub grant_id: String,
    pub root: PathBuf,
    pub capabilities: BTreeSet<String>,
    pub tenant_id: String,
    pub user_id: String,
    pub issued_at: f64,
    pub expires_at: Option<f64>,
    pub root_device: u64,
    pub root_inode: u64,
}
pub fn identity(metadata: &fs::Metadata) -> Result<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(Error::new(
            "driver_unavailable",
            "descriptor-contained filesystem requires an OS adapter",
        ))
    }
}
#[derive(Default)]
pub struct DirectoryGrantStore {
    grants: RwLock<HashMap<String, DirectoryGrant>>,
    revoked: RwLock<HashSet<String>>,
}
impl DirectoryGrantStore {
    pub fn issue(
        &self,
        root: &Path,
        mut capabilities: BTreeSet<String>,
        ttl: Option<f64>,
        tenant_id: &str,
        user_id: &str,
        grant_id: Option<&str>,
    ) -> Result<DirectoryGrant> {
        if !root.is_absolute() {
            return Err(Error::path("grant root must be absolute"));
        }
        let info = fs::symlink_metadata(root)?;
        if info.file_type().is_symlink() || !info.is_dir() {
            return Err(Error::path("grant root must be a non-symlink directory"));
        }
        if capabilities.is_empty()
            || capabilities.iter().any(|c| {
                !["list", "read", "search", "watch", "write", "rollback"].contains(&c.as_str())
            })
            || (capabilities.contains("rollback") && !capabilities.contains("write"))
        {
            return Err(Error::denied("unsupported directory grant capabilities"));
        }
        if capabilities.contains("write") {
            capabilities.insert("rollback".into());
        }
        if ttl.is_some_and(|v| !v.is_finite() || v <= 0.0) {
            return Err(Error::denied("grant lifetime must be positive"));
        }
        let id = grant_id
            .map(str::to_owned)
            .unwrap_or_else(|| format!("grant_{}", uuid::Uuid::new_v4().simple()));
        if id.len() > 200
            || id.is_empty()
            || !id.as_bytes()[0].is_ascii_alphanumeric()
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        {
            return Err(Error::denied("directory grant id invalid"));
        }
        let mut grants = self
            .grants
            .write()
            .map_err(|_| Error::denied("directory grant store unavailable"))?;
        if grants.contains_key(&id) {
            return Err(Error::denied("directory grant id already in use"));
        }
        let (device, inode) = identity(&info)?;
        let timestamp = now();
        let grant = DirectoryGrant {
            grant_id: id.clone(),
            root: root.canonicalize()?,
            capabilities,
            tenant_id: tenant_id.into(),
            user_id: user_id.into(),
            issued_at: timestamp,
            expires_at: ttl.map(|v| timestamp + v),
            root_device: device,
            root_inode: inode,
        };
        grants.insert(id, grant.clone());
        Ok(grant)
    }
    pub fn get(
        &self,
        id: &str,
        capability: &str,
        owner: Option<(&str, &str)>,
    ) -> Result<DirectoryGrant> {
        let revoked = self
            .revoked
            .read()
            .map_err(|_| Error::denied("grant store unavailable"))?;
        let grants = self
            .grants
            .read()
            .map_err(|_| Error::denied("grant store unavailable"))?;
        let grant = grants
            .get(id)
            .filter(|_| !revoked.contains(id))
            .ok_or_else(|| Error::denied("directory grant unavailable"))?;
        if grant.expires_at.is_some_and(|v| now() >= v) {
            return Err(Error::denied("directory grant expired"));
        }
        if !grant.capabilities.contains(capability) {
            return Err(Error::denied("directory grant lacks capability"));
        }
        if owner.is_some_and(|(tenant, user)| tenant != grant.tenant_id || user != grant.user_id) {
            return Err(Error::denied("directory grant belongs to another owner"));
        }
        let info = fs::symlink_metadata(&grant.root)?;
        if info.file_type().is_symlink()
            || identity(&info)? != (grant.root_device, grant.root_inode)
        {
            return Err(Error::denied("directory grant root identity changed"));
        }
        Ok(grant.clone())
    }
    pub fn revoke(&self, id: &str) -> Result<()> {
        self.revoked
            .write()
            .map_err(|_| Error::denied("grant store unavailable"))?
            .insert(id.into());
        Ok(())
    }
    pub fn active(&self) -> Result<Vec<DirectoryGrant>> {
        let grants: Vec<_> = self
            .grants
            .read()
            .map_err(|_| Error::denied("grant store unavailable"))?
            .values()
            .cloned()
            .collect();
        Ok(grants
            .into_iter()
            .filter(|g| {
                g.capabilities
                    .iter()
                    .next()
                    .is_some_and(|c| self.get(&g.grant_id, c, None).is_ok())
            })
            .collect())
    }
}
