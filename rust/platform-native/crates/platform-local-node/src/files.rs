use crate::{
    Error, Result,
    envelope::{Action, ApprovalVerifier, PlatformVerifier},
    ledger::Ledger,
    sha256,
    workspace::{Grant, open_at},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
pub const MISSING_DIGEST: &str = "missing";
static SERIAL: AtomicU64 = AtomicU64::new(0);
fn unique() -> String {
    format!(
        "{}-{}-{}",
        std::process::id(),
        crate::now().to_bits(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteReceipt {
    pub action_id: String,
    pub grant_id: String,
    pub relative_path: String,
    pub before_sha256: String,
    pub after_sha256: String,
    pub bytes_written: usize,
    pub rollback_ref: String,
    pub restored_from: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Rollback {
    grant_id: String,
    relative_path: String,
    before: Option<String>,
    before_sha256: String,
    #[serde(default)]
    before_mode: Option<u32>,
    #[serde(default)]
    created_at: f64,
}
pub struct FileService {
    pub rollback_dir: PathBuf,
    pub max_bytes: usize,
}
impl FileService {
    pub fn new(rollback_dir: PathBuf) -> Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&rollback_dir)?;
        if std::fs::symlink_metadata(&rollback_dir)?
            .file_type()
            .is_symlink()
        {
            return Err(Error::Denied("rollback directory symlink"));
        }
        std::fs::set_permissions(&rollback_dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            rollback_dir,
            max_bytes: 8 * 1024 * 1024,
        })
    }
    fn optional(&self, g: &Grant, path: &str) -> Result<Option<Vec<u8>>> {
        let (parent, name) = g.parent(path)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(Error::Storage);
        }
        Ok(Some(g.read(path, self.max_bytes)?))
    }
    fn replace(&self, g: &Grant, path: &str, content: &[u8], expected: &str) -> Result<()> {
        let (parent, name) = g.parent(path)?;
        let current = self.optional(g, path)?;
        let hash = current
            .as_ref()
            .map(|v| sha256(v))
            .unwrap_or_else(|| MISSING_DIGEST.into());
        if hash != expected {
            return Err(Error::Stale);
        }
        let temp = format!(".local-node-{}.tmp", unique());
        let temp_name = CString::new(temp.clone()).unwrap();
        let mode: u32 = if current.is_some() {
            open_at(
                &parent,
                name.to_str().map_err(|_| Error::Protocol)?,
                libc::O_RDONLY | libc::O_NONBLOCK,
                0,
            )?
            .metadata()?
            .mode()
                & 0o7777
        } else {
            0o600
        };
        let mut f = open_at(
            &parent,
            &temp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            mode,
        )?;
        let result = (|| {
            f.write_all(content)?;
            f.sync_all()?;
            // Re-open through the pinned parent immediately before publication.
            let latest = self.optional(g, path)?;
            if latest
                .as_ref()
                .map(|v| sha256(v))
                .unwrap_or_else(|| MISSING_DIGEST.into())
                != expected
            {
                return Err(Error::Stale);
            }
            if unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temp_name.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                )
            } < 0
            {
                return Err(Error::Storage);
            }
            parent.sync_all()?;
            Ok(())
        })();
        unsafe { libc::unlinkat(parent.as_raw_fd(), temp_name.as_ptr(), 0) };
        result
    }
    pub fn write(
        &self,
        g: &Grant,
        path: &str,
        content: &[u8],
        expected: Option<&str>,
        a: &Action,
        ledger: &mut Ledger,
        platform: &dyn PlatformVerifier,
        approval: &dyn ApprovalVerifier,
    ) -> Result<WriteReceipt> {
        self.write_with_approval(
            g,
            path,
            content,
            expected,
            a,
            ledger,
            platform,
            approval,
            &crate::runner::ActionSignals::default(),
            |action, _| {
                action
                    .approval
                    .clone()
                    .ok_or(Error::Denied("approval required"))
            },
        )
    }
    /// Obtain device-local approval after the current target hash is known.
    /// The local signature is verified and its nonce consumed before mutation.
    pub fn write_with_approval<F>(
        &self,
        g: &Grant,
        path: &str,
        content: &[u8],
        expected: Option<&str>,
        a: &Action,
        ledger: &mut Ledger,
        platform: &dyn PlatformVerifier,
        approval: &dyn ApprovalVerifier,
        signals: &crate::runner::ActionSignals,
        obtain: F,
    ) -> Result<WriteReceipt>
    where
        F: FnOnce(&Action, &str) -> Result<crate::envelope::Approval>,
    {
        if content.len() > self.max_bytes {
            return Err(Error::Denied("write budget"));
        }
        a.validate(&json!({"grant_id":g.id,"relative_path":path,"content_sha256":sha256(content),"expected_hash":expected}),&g.id,&[g.id.clone(),path.into()],platform)?;
        if a.capability != "file.write" || a.operation != "file.write" {
            return Err(Error::Denied("write operation"));
        }
        g.authorize("write", &a.tenant_id, &a.user_id)?;
        let (created, record) = ledger.begin(a, platform)?;
        if !created {
            return serde_json::from_value(record.result.ok_or(Error::Conflict)?)
                .map_err(|_| Error::Conflict);
        }
        let mut effect_guard = None;
        let result = (|| {
            if signals.is_cancelled(&a.action_id) {
                cancel_if_active(ledger, &a.action_id)?;
                return Err(Error::Denied("action cancelled"));
            }
            let before = self.optional(g, path)?;
            let hash = before
                .as_ref()
                .map(|b| sha256(b))
                .unwrap_or_else(|| MISSING_DIGEST.into());
            if hash != expected.unwrap_or(MISSING_DIGEST) {
                return Err(Error::Stale);
            }
            ledger.transition(&a.action_id, "awaiting_approval", None)?;
            let local_approval = obtain(a, &hash)?;
            a.approve_with(&local_approval, &hash, approval)?;
            effect_guard = Some(signals.lock_effect()?);
            if signals.is_cancelled(&a.action_id) {
                cancel_if_active(ledger, &a.action_id)?;
                return Err(Error::Denied("action cancelled"));
            }
            if ledger
                .get(&a.action_id)?
                .is_some_and(|record| crate::ledger::terminal(&record.status))
            {
                return Err(Error::Denied("action cancelled"));
            }
            ledger.transition(&a.action_id, "dispatched", None)?;
            ledger.transition(&a.action_id, "running", None)?;
            let reference = format!("rollback_{}", unique());
            let mut backup = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(self.rollback_dir.join(format!("{reference}.json")))?;
            let data = serde_json::to_vec(&Rollback {
                grant_id: g.id.clone(),
                relative_path: path.into(),
                before: before
                    .as_ref()
                    .map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
                before_sha256: hash.clone(),
                before_mode: None,
                created_at: crate::now(),
            })
            .map_err(|_| Error::Protocol)?;
            backup.write_all(&data)?;
            backup.sync_all()?;
            File::open(&self.rollback_dir)?.sync_all()?;
            self.replace(g, path, content, &hash)?;
            Ok(WriteReceipt {
                action_id: a.action_id.clone(),
                grant_id: g.id.clone(),
                relative_path: path.into(),
                before_sha256: hash,
                after_sha256: sha256(content),
                bytes_written: content.len(),
                rollback_ref: reference,
                restored_from: None,
            })
        })();
        match result {
            Ok(receipt) => {
                ledger.transition(
                    &a.action_id,
                    "succeeded",
                    Some(serde_json::to_value(&receipt).map_err(|_| Error::Protocol)?),
                )?;
                drop(effect_guard);
                Ok(receipt)
            }
            Err(e) => {
                if ledger
                    .get(&a.action_id)?
                    .is_some_and(|record| crate::ledger::terminal(&record.status))
                {
                    return Err(e);
                }
                ledger.transition(
                    &a.action_id,
                    "failed",
                    Some(json!({"error_code":"file_write_failed"})),
                )?;
                Err(e)
            }
        }
    }
    pub fn rollback(
        &self,
        g: &Grant,
        reference: &str,
        a: &Action,
        ledger: &mut Ledger,
        platform: &dyn PlatformVerifier,
        approval: &dyn ApprovalVerifier,
    ) -> Result<WriteReceipt> {
        use std::io::Read;
        a.verify(platform, crate::now())?;
        if !reference.starts_with("rollback_")
            || reference.len() > 200
            || !reference
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::Denied("rollback reference"));
        }
        let f = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.rollback_dir.join(format!("{reference}.json")))?;
        let m = f.metadata()?;
        if !m.is_file() || m.nlink() != 1 || m.len() > self.max_bytes.saturating_mul(5) as u64 {
            return Err(Error::Denied("rollback storage budget"));
        }
        let mut bytes = Vec::new();
        f.take(self.max_bytes.saturating_mul(5).saturating_add(1) as u64)
            .read_to_end(&mut bytes)?;
        let saved: Rollback = serde_json::from_slice(&bytes).map_err(|_| Error::Protocol)?;
        let before = saved
            .before
            .as_ref()
            .map(|s| base64::engine::general_purpose::STANDARD.decode(s))
            .transpose()
            .map_err(|_| Error::Integrity)?;
        if saved.grant_id != g.id
            || before.as_ref().is_some_and(|b| b.len() > self.max_bytes)
            || before
                .as_ref()
                .map(|b| sha256(b))
                .unwrap_or_else(|| MISSING_DIGEST.into())
                != saved.before_sha256
        {
            return Err(Error::Integrity);
        }
        g.authorize("rollback", &a.tenant_id, &a.user_id)?;
        let current = self.optional(g, &saved.relative_path)?;
        let hash = current
            .as_ref()
            .map(|b| sha256(b))
            .unwrap_or_else(|| MISSING_DIGEST.into());
        a.validate(&json!({"rollback_ref":reference,"grant_id":g.id,"relative_path":saved.relative_path,"expected_current_hash":hash}),&g.id,&[reference.into(),g.id.clone(),saved.relative_path.clone()],platform)?;
        if a.operation != "file.rollback" || a.capability != "file.write" {
            return Err(Error::Denied("rollback operation"));
        }
        let (created, record) = ledger.begin(a, platform)?;
        if !created {
            return serde_json::from_value(record.result.ok_or(Error::Conflict)?)
                .map_err(|_| Error::Conflict);
        }
        let result = (|| {
            ledger.transition(&a.action_id, "awaiting_approval", None)?;
            a.approve(&hash, approval)?;
            ledger.transition(&a.action_id, "dispatched", None)?;
            ledger.transition(&a.action_id, "running", None)?;
            if let Some(content) = &before {
                self.replace(g, &saved.relative_path, content, &hash)?;
            } else {
                let (parent, name) = g.parent(&saved.relative_path)?;
                let latest = self.optional(g, &saved.relative_path)?;
                if latest
                    .as_ref()
                    .map(|b| sha256(b))
                    .unwrap_or_else(|| MISSING_DIGEST.into())
                    != hash
                {
                    return Err(Error::Stale);
                }
                if current.is_some()
                    && unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } < 0
                {
                    return Err(Error::Storage);
                }
                parent.sync_all()?;
            }
            Ok(WriteReceipt {
                action_id: a.action_id.clone(),
                grant_id: g.id.clone(),
                relative_path: saved.relative_path,
                before_sha256: hash,
                after_sha256: saved.before_sha256,
                bytes_written: before.as_ref().map_or(0, Vec::len),
                rollback_ref: reference.into(),
                restored_from: Some(reference.into()),
            })
        })();
        match result {
            Ok(receipt) => {
                ledger.transition(
                    &a.action_id,
                    "succeeded",
                    Some(serde_json::to_value(&receipt).map_err(|_| Error::Protocol)?),
                )?;
                Ok(receipt)
            }
            Err(e) => {
                ledger.transition(
                    &a.action_id,
                    "failed",
                    Some(json!({"error_code":"rollback_failed"})),
                )?;
                Err(e)
            }
        }
    }
    pub fn read(
        &self,
        g: &Grant,
        path: &str,
        a: &Action,
        platform: &dyn PlatformVerifier,
    ) -> Result<serde_json::Value> {
        a.validate(
            &json!({"grant_id":g.id,"path":path}),
            &g.id,
            &[g.id.clone(), path.into()],
            platform,
        )?;
        if a.capability != "file.read"
            || a.operation != "file.read"
            || a.tool_name != "local_file_read"
        {
            return Err(Error::Denied("read operation"));
        }
        g.authorize("read", &a.tenant_id, &a.user_id)?;
        let bytes = g.read(path, self.max_bytes)?;
        let content = String::from_utf8(bytes.clone()).ok();
        let encoding = content.as_ref().map(|_| "utf-8");
        Ok(
            json!({"kind":"file_read","relative_path":path,"content":content,"sha256":sha256(&bytes),"size":bytes.len(),"encoding":encoding}),
        )
    }
}
fn cancel_if_active(ledger: &mut Ledger, action_id: &str) -> Result<()> {
    if let Some(record) = ledger.get(action_id)? {
        if !crate::ledger::terminal(&record.status) {
            let (status, result) =
                if matches!(record.status.as_str(), "policy_check" | "awaiting_approval") {
                    (
                        "cancelled",
                        json!({"cancelled":true,"replay_allowed":false}),
                    )
                } else {
                    (
                        "unknown",
                        json!({"side_effect_unknown":true,"replay_allowed":false}),
                    )
                };
            ledger.transition(action_id, status, Some(result))?;
        }
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
pub struct FileReadResult {
    pub relative_path: String,
    pub content: Vec<u8>,
    pub sha256: String,
    pub size: usize,
    pub modified_ns: i128,
    pub encoding: Option<String>,
}
#[derive(Serialize, Deserialize)]
pub struct SearchMatch {
    pub relative_path: String,
    pub line: usize,
    pub column: usize,
    pub preview: String,
    pub file_sha256: String,
}
#[derive(Serialize, Deserialize)]
pub struct FileAnalysisResult {
    pub read: FileReadResult,
    pub matches: Vec<SearchMatch>,
}
impl FileService {
    pub fn analyze_files(
        &self,
        g: &Grant,
        paths: &[String],
        query: Option<&str>,
        max_files: usize,
        max_matches_per_file: usize,
        tenant: &str,
        user: &str,
    ) -> Result<Vec<FileAnalysisResult>> {
        g.authorize("read", tenant, user)?;
        if query.is_some() {
            g.authorize("search", tenant, user)?;
        }
        if paths.is_empty()
            || paths.len() > max_files
            || max_files == 0
            || max_files > 1000
            || max_matches_per_file == 0
            || max_matches_per_file > 1000
            || paths.iter().collect::<std::collections::HashSet<_>>().len() != paths.len()
            || query.is_some_and(|q| q.is_empty() || q.chars().count() > 10000)
        {
            return Err(Error::Denied("multi-file analysis budget"));
        }
        for path in paths {
            crate::workspace::safe_parts(path)?;
        }
        let mut out = Vec::new();
        for path in paths {
            let (bytes, meta) = g.read_with_metadata(path, self.max_bytes)?;
            let hash = sha256(&bytes);
            let text = std::str::from_utf8(&bytes).ok();
            let mut matches = Vec::new();
            if let (Some(query), Some(text)) = (query, text) {
                for (n, line) in text.lines().enumerate() {
                    if let Some(offset) = line.find(query) {
                        matches.push(SearchMatch {
                            relative_path: path.clone(),
                            line: n + 1,
                            column: line[..offset].chars().count() + 1,
                            preview: line.chars().take(300).collect(),
                            file_sha256: hash.clone(),
                        });
                        if matches.len() >= max_matches_per_file {
                            break;
                        }
                    }
                }
            }
            let encoding = text.map(|_| "utf-8".into());
            out.push(FileAnalysisResult {
                read: FileReadResult {
                    relative_path: path.clone(),
                    size: bytes.len(),
                    content: bytes,
                    sha256: hash,
                    modified_ns: meta.mtime() as i128 * 1_000_000_000 + meta.mtime_nsec() as i128,
                    encoding,
                },
                matches,
            });
        }
        Ok(out)
    }
}
