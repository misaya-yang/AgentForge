use super::{
    LocalNodeError as Error, Result, database,
    grants::DirectoryGrantStore,
    ledger::ActionLedger,
    models::*,
    workspace::{self, SecureWorkspace},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
pub const MISSING_DIGEST: &str = "missing";
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEntry {
    pub relative_path: String,
    pub kind: String,
    pub size: u64,
    pub modified_ns: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileReadResult {
    pub relative_path: String,
    pub content: Vec<u8>,
    pub sha256: String,
    pub size: usize,
    pub modified_ns: i64,
    pub encoding: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchMatch {
    pub relative_path: String,
    pub line: usize,
    pub column: usize,
    pub preview: String,
    pub file_sha256: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileAnalysisResult {
    pub read: FileReadResult,
    pub matches: Vec<SearchMatch>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
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
pub struct LocalFileService {
    pub grants: Arc<DirectoryGrantStore>,
    pub rollback_dir: PathBuf,
    pub ledger: Arc<ActionLedger>,
    pub max_read_bytes: usize,
}
pub fn modified_ns(info: &fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        info.mtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(info.mtime_nsec())
    }
    #[cfg(not(unix))]
    {
        info.modified()
            .ok()
            .and_then(|v| v.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |v| v.as_nanos().min(i64::MAX as u128) as i64)
    }
}
fn bounded_read(mut file: File, max: usize) -> Result<(Vec<u8>, fs::Metadata)> {
    let info = file.metadata()?;
    if info.len() > max as u64 {
        return Err(Error::denied("file exceeds local read budget"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::denied("file changed beyond local read budget"));
    }
    Ok((bytes, info))
}
fn mode(info: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        info.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        0o600
    }
}
fn set_mode(file: &File, value: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(value))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (file, value);
    }
    Ok(())
}
/// Python fnmatchcase semantics for the common wildcard forms used by the file API.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            Some('?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some('[') => {
                if let Some(end) = p.iter().position(|c| *c == ']') {
                    if s.is_empty() {
                        return false;
                    }
                    let mut chars = &p[1..end];
                    let neg = chars.first() == Some(&'!');
                    if neg {
                        chars = &chars[1..];
                    }
                    let mut matched = false;
                    let mut i = 0;
                    while i < chars.len() {
                        if i + 2 < chars.len() && chars[i + 1] == '-' {
                            matched |= chars[i] <= s[0] && s[0] <= chars[i + 2];
                            i += 3;
                        } else {
                            matched |= chars[i] == s[0];
                            i += 1;
                        }
                    }
                    (matched != neg) && go(&p[end + 1..], &s[1..])
                } else {
                    !s.is_empty() && s[0] == '[' && go(&p[1..], &s[1..])
                }
            }
            Some(c) => !s.is_empty() && *c == s[0] && go(&p[1..], &s[1..]),
        }
    }
    go(
        &pattern.chars().collect::<Vec<_>>(),
        &text.chars().collect::<Vec<_>>(),
    )
}
impl LocalFileService {
    pub fn new(
        grants: Arc<DirectoryGrantStore>,
        rollback_dir: &Path,
        ledger: Arc<ActionLedger>,
    ) -> Result<Self> {
        database::private_directory(rollback_dir)?;
        Ok(Self {
            grants,
            rollback_dir: rollback_dir.into(),
            ledger,
            max_read_bytes: 8 * 1024 * 1024,
        })
    }
    pub fn workspace(
        &self,
        grant: &str,
        cap: &str,
        action: Option<&ActionContext>,
    ) -> Result<SecureWorkspace> {
        Ok(SecureWorkspace::new(self.grants.get(
            grant,
            cap,
            action.map(|a| (a.tenant_id.as_str(), a.user_id.as_str())),
        )?))
    }
    pub fn list_files(
        &self,
        grant: &str,
        relative: &str,
        recursive: bool,
    ) -> Result<Vec<FileEntry>> {
        let workspace = self.workspace(grant, "list", None)?;
        SecureWorkspace::parts(relative)?;
        let mut queue = VecDeque::from([relative.to_string()]);
        let mut entries = Vec::new();
        while let Some(directory) = queue.pop_front() {
            let fd = workspace.open_dir(&directory)?;
            for name in workspace::names(&fd)? {
                let child = if directory == "." || directory.is_empty() {
                    name.clone()
                } else {
                    format!("{directory}/{name}")
                };
                if SecureWorkspace::parts(&child).is_err() {
                    continue;
                }
                if let Ok(opened) = workspace::open_at(&fd, &name, true, false) {
                    let info = opened.metadata()?;
                    entries.push(FileEntry {
                        relative_path: child.clone(),
                        kind: "directory".into(),
                        size: 0,
                        modified_ns: modified_ns(&info),
                    });
                    if recursive {
                        queue.push_back(child);
                    }
                } else if let Ok(opened) = workspace::open_at(&fd, &name, false, false) {
                    if workspace::check_regular(&opened).is_err() {
                        continue;
                    }
                    let info = opened.metadata()?;
                    entries.push(FileEntry {
                        relative_path: child,
                        kind: "file".into(),
                        size: info.len(),
                        modified_ns: modified_ns(&info),
                    });
                }
            }
        }
        Ok(entries)
    }
    pub fn read_file(&self, grant: &str, relative: &str) -> Result<FileReadResult> {
        let workspace = self.workspace(grant, "read", None)?;
        let (bytes, info) = bounded_read(workspace.open_read(relative)?, self.max_read_bytes)?;
        Ok(FileReadResult {
            relative_path: relative.into(),
            sha256: sha256(&bytes),
            size: bytes.len(),
            modified_ns: modified_ns(&info),
            encoding: std::str::from_utf8(&bytes).ok().map(|_| "utf-8".into()),
            content: bytes,
        })
    }
    fn matches(read: &FileReadResult, query: &str, limit: usize) -> Vec<SearchMatch> {
        let Ok(text) = std::str::from_utf8(&read.content) else {
            return Vec::new();
        };
        text.lines()
            .enumerate()
            .filter_map(|(line, text)| {
                text.find(query).map(|offset| SearchMatch {
                    relative_path: read.relative_path.clone(),
                    line: line + 1,
                    column: text[..offset].chars().count() + 1,
                    preview: text.chars().take(300).collect(),
                    file_sha256: read.sha256.clone(),
                })
            })
            .take(limit)
            .collect()
    }
    pub fn search(
        &self,
        grant: &str,
        query: &str,
        glob: &str,
        max_matches: usize,
    ) -> Result<Vec<SearchMatch>> {
        self.grants.get(grant, "search", None)?;
        if query.is_empty() || max_matches == 0 {
            return Ok(Vec::new());
        }
        let mut matches = Vec::new();
        for entry in self.list_files(grant, ".", true)? {
            if entry.kind != "file" || !glob_matches(glob, &entry.relative_path) {
                continue;
            }
            match self.read_file(grant, &entry.relative_path) {
                Ok(read) => {
                    matches.extend(Self::matches(&read, query, max_matches - matches.len()))
                }
                Err(e) if ["capability_denied", "path_escape"].contains(&e.code) => continue,
                Err(e) => return Err(e),
            }
            if matches.len() >= max_matches {
                break;
            }
        }
        Ok(matches)
    }
    pub fn analyze_files(
        &self,
        grant: &str,
        paths: &[String],
        query: Option<&str>,
        max_files: usize,
        max_matches: usize,
    ) -> Result<Vec<FileAnalysisResult>> {
        self.grants.get(grant, "read", None)?;
        if query.is_some() {
            self.grants.get(grant, "search", None)?;
        }
        if paths.is_empty()
            || paths.len() > max_files
            || !(1..=1000).contains(&max_files)
            || !(1..=1000).contains(&max_matches)
            || paths.iter().collect::<HashSet<_>>().len() != paths.len()
            || query.is_some_and(|q| q.is_empty() || q.chars().count() > 10000)
        {
            return Err(Error::denied("multi-file analysis budget or query invalid"));
        }
        for path in paths {
            SecureWorkspace::parts(path)?;
        }
        paths
            .iter()
            .map(|path| {
                let read = self.read_file(grant, path)?;
                let matches = query.map_or_else(Vec::new, |q| Self::matches(&read, q, max_matches));
                Ok(FileAnalysisResult { read, matches })
            })
            .collect()
    }
    fn read_optional(
        &self,
        workspace: &SecureWorkspace,
        path: &str,
    ) -> Result<(Option<Vec<u8>>, Option<fs::Metadata>)> {
        match workspace.open_read(path) {
            Ok(file) => {
                let (b, m) = bounded_read(file, self.max_read_bytes)?;
                Ok((Some(b), Some(m)))
            }
            Err(e) if e.code == "file_not_found" => Ok((None, None)),
            Err(e) => Err(e),
        }
    }
    fn save_rollback(
        &self,
        grant: &str,
        path: &str,
        before: Option<&[u8]>,
        before_mode: Option<u32>,
    ) -> Result<String> {
        let reference = format!("rollback_{}", uuid::Uuid::new_v4().simple());
        let payload = json!({"grant_id":grant,"relative_path":path,"before":before.map(|b|STANDARD.encode(b)),"before_sha256":before.map(sha256).unwrap_or_else(||MISSING_DIGEST.into()),"before_mode":before_mode,"created_at":now()});
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(self.rollback_dir.join(format!("{reference}.json")))?;
        file.write_all(canonical_json(&payload).as_bytes())?;
        file.sync_all()?;
        File::open(&self.rollback_dir)?.sync_all()?;
        Ok(reference)
    }
    fn atomic_replace(
        &self,
        workspace: &SecureWorkspace,
        path: &str,
        content: &[u8],
        expected: &str,
    ) -> Result<String> {
        let (parent, name) = workspace.open_parent(path)?;
        let temporary = format!(".local-node-{}.tmp", uuid::Uuid::new_v4().simple());
        let result = (|| {
            let existing = workspace::open_at(&parent, &name, false, false);
            let before_mode = match existing {
                Ok(file) => {
                    workspace::check_regular(&file)?;
                    let (bytes, opened) = bounded_read(file, self.max_read_bytes)?;
                    if sha256(&bytes) != expected {
                        return Err(Error::stale("target changed before write"));
                    }
                    let current = workspace::open_at(&parent, &name, false, false)?.metadata()?;
                    if super::grants::identity(&opened)? != super::grants::identity(&current)?
                        || opened.len() != current.len()
                        || modified_ns(&opened) != modified_ns(&current)
                    {
                        return Err(Error::stale("target identity changed before write"));
                    }
                    mode(&opened)
                }
                Err(e) if e.code == "file_not_found" && expected == MISSING_DIGEST => 0o600,
                Err(e) if e.code == "file_not_found" => {
                    return Err(Error::stale("target removed before write"));
                }
                Err(e) => return Err(e),
            };
            let mut temp = workspace::open_at(&parent, &temporary, false, true)?;
            set_mode(&temp, before_mode)?;
            temp.write_all(content)?;
            temp.sync_all()?;
            workspace::rename_at(&parent, &temporary, &name)?;
            parent.sync_all()?;
            Ok(sha256(content))
        })();
        let _ = workspace::remove_at(&parent, &temporary);
        result
    }
    pub fn write_atomic(
        &self,
        grant: &str,
        path: &str,
        content: &[u8],
        expected: Option<&str>,
        action: &ActionContext,
    ) -> Result<WriteReceipt> {
        let args = json!({"grant_id":grant,"relative_path":path,"content_sha256":sha256(content),"expected_hash":expected});
        action.validate_payload(
            &args,
            self.ledger.platform_signature_verifier.as_deref(),
            Some(grant),
            Some(&[grant.into(), path.into()]),
        )?;
        if action.capability != "file.write" || action.operation != "file.write" {
            return Err(Error::denied("signed file.write authority required"));
        }
        let begin = self.ledger.begin(action)?;
        if !begin.created {
            return serde_json::from_value(
                begin
                    .record
                    .result
                    .ok_or_else(|| Error::denied("matching action already in progress"))?,
            )
            .map_err(Into::into);
        }
        let result: Result<WriteReceipt> = (|| {
            let workspace = self.workspace(grant, "write", Some(action))?;
            let (before, info) = self.read_optional(&workspace, path)?;
            let before_hash = before
                .as_ref()
                .map(|b| sha256(b))
                .unwrap_or_else(|| MISSING_DIGEST.into());
            if expected.unwrap_or(MISSING_DIGEST) != before_hash {
                return Err(Error::stale("expected hash does not match current target"));
            }
            self.ledger.mark_awaiting_approval(&action.action_id)?;
            action.require_approval(
                &before_hash,
                self.ledger.trusted_local_approval_verifier.as_deref(),
            )?;
            self.ledger.mark_dispatched(&action.action_id)?;
            self.ledger.mark_running(&action.action_id)?;
            let reference =
                self.save_rollback(grant, path, before.as_deref(), info.as_ref().map(mode))?;
            let after = self.atomic_replace(&workspace, path, content, &before_hash)?;
            let receipt = WriteReceipt {
                action_id: action.action_id.clone(),
                grant_id: grant.into(),
                relative_path: path.into(),
                before_sha256: before_hash,
                after_sha256: after,
                bytes_written: content.len(),
                rollback_ref: reference,
                restored_from: None,
            };
            self.ledger.finish(
                &action.action_id,
                ActionStatus::Succeeded,
                serde_json::to_value(&receipt)?,
            )?;
            Ok(receipt)
        })();
        if let Err(e) = &result {
            self.ledger.finish(
                &action.action_id,
                ActionStatus::Failed,
                json!({"error_code":e.code}),
            )?;
        }
        result
    }
    pub fn rollback(&self, reference: &str, action: &ActionContext) -> Result<WriteReceipt> {
        action.verify_platform_signature(self.ledger.platform_signature_verifier.as_deref())?;
        if !reference.starts_with("rollback_")
            || !reference
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::denied("rollback reference invalid"));
        }
        let path = self.rollback_dir.join(format!("{reference}.json"));
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_symlink() {
            return Err(Error::denied("rollback reference invalid"));
        }
        let payload: Value = serde_json::from_reader(File::open(path)?)?;
        let grant = payload["grant_id"]
            .as_str()
            .ok_or_else(|| Error::denied("rollback grant invalid"))?;
        let relative = payload["relative_path"]
            .as_str()
            .ok_or_else(|| Error::denied("rollback path invalid"))?;
        let workspace = self.workspace(grant, "rollback", Some(action))?;
        let (current, _) = self.read_optional(&workspace, relative)?;
        let current_hash = current
            .as_deref()
            .map(sha256)
            .unwrap_or_else(|| MISSING_DIGEST.into());
        action.validate_payload(&json!({"rollback_ref":reference,"grant_id":grant,"relative_path":relative,"expected_current_hash":current_hash}),self.ledger.platform_signature_verifier.as_deref(),Some(grant),Some(&[reference.into(),grant.into(),relative.into()]))?;
        if action.capability != "file.write" || action.operation != "file.rollback" {
            return Err(Error::denied(
                "rollback requires signed file.write authority",
            ));
        }
        let begin = self.ledger.begin(action)?;
        if !begin.created {
            return serde_json::from_value(
                begin
                    .record
                    .result
                    .ok_or_else(|| Error::denied("matching rollback in progress"))?,
            )
            .map_err(Into::into);
        }
        let result: Result<WriteReceipt> = (|| {
            self.ledger.mark_awaiting_approval(&action.action_id)?;
            action.require_approval(
                &current_hash,
                self.ledger.trusted_local_approval_verifier.as_deref(),
            )?;
            self.ledger.mark_dispatched(&action.action_id)?;
            self.ledger.mark_running(&action.action_id)?;
            let (restored, size) = if let Some(encoded) = payload["before"].as_str() {
                let bytes = STANDARD
                    .decode(encoded)
                    .map_err(|_| Error::denied("rollback data invalid"))?;
                (
                    self.atomic_replace(&workspace, relative, &bytes, &current_hash)?,
                    bytes.len(),
                )
            } else {
                let (parent, name) = workspace.open_parent(relative)?;
                workspace::remove_at(&parent, &name)?;
                parent.sync_all()?;
                (MISSING_DIGEST.into(), 0)
            };
            let receipt = WriteReceipt {
                action_id: action.action_id.clone(),
                grant_id: grant.into(),
                relative_path: relative.into(),
                before_sha256: current_hash,
                after_sha256: restored,
                bytes_written: size,
                rollback_ref: reference.into(),
                restored_from: Some(reference.into()),
            };
            self.ledger.finish(
                &action.action_id,
                ActionStatus::Succeeded,
                serde_json::to_value(&receipt)?,
            )?;
            Ok(receipt)
        })();
        if let Err(e) = &result {
            self.ledger.finish(
                &action.action_id,
                ActionStatus::Failed,
                json!({"error_code":e.code}),
            )?;
        }
        result
    }
}
