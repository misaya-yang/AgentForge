//! Signed read-only dispatch with owner-bound grants and Python wire result shapes.
use crate::{
    Error, Result,
    envelope::{Action, Approval, ApprovalVerifier, PlatformVerifier},
    files::FileService,
    ledger::Ledger,
    runner::{ActionSignals, ControlledDispatcher, Dispatcher, Outcome, RemoteControl},
    sha256,
    workspace::{Grant, safe_parts},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    ffi::CStr,
    os::fd::IntoRawFd,
    sync::Arc,
    time::Duration,
};
#[derive(Serialize, Deserialize)]
pub struct FileEntry {
    pub relative_path: String,
    pub kind: String,
    pub size: u64,
    pub modified_ns: i128,
}
struct Directory(*mut libc::DIR);
impl Drop for Directory {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.0) };
    }
}
pub fn list(g: &Grant, path: &str, recursive: bool) -> Result<Vec<FileEntry>> {
    let mut queue = VecDeque::from([path.to_owned()]);
    let mut entries = Vec::new();
    while let Some(path) = queue.pop_front() {
        let fd = g.open_dir(&path)?;
        let raw = fd.into_raw_fd();
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() {
            unsafe { libc::close(raw) };
            return Err(Error::Storage);
        }
        let dir = Directory(dir);
        let mut names = Vec::new();
        loop {
            let entry = unsafe { libc::readdir(dir.0) };
            if entry.is_null() {
                break;
            }
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if let Ok(name) = name.to_str() {
                if name != "." && name != ".." {
                    names.push(name.to_owned());
                }
            }
        }
        names.sort();
        for name in names {
            let child = if path == "." || path.is_empty() {
                name.clone()
            } else {
                format!("{path}/{name}")
            };
            if safe_parts(&child).is_err() {
                continue;
            }
            let c = std::ffi::CString::new(name).map_err(|_| Error::Protocol)?;
            let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
            if unsafe {
                libc::fstatat(
                    libc::dirfd(dir.0),
                    c.as_ptr(),
                    st.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            } < 0
            {
                continue;
            }
            let st = unsafe { st.assume_init() };
            let kind = st.st_mode & libc::S_IFMT;
            let modified_ns = st.st_mtime as i128 * 1_000_000_000 + st.st_mtime_nsec as i128;
            if kind == libc::S_IFDIR {
                if recursive {
                    queue.push_back(child.clone())
                }
                entries.push(FileEntry {
                    relative_path: child,
                    kind: "directory".into(),
                    size: 0,
                    modified_ns,
                });
            } else if kind == libc::S_IFREG && st.st_nlink == 1 {
                entries.push(FileEntry {
                    relative_path: child,
                    kind: "file".into(),
                    size: st.st_size as u64,
                    modified_ns,
                });
            }
        }
    }
    Ok(entries)
}
fn bounded(args: &Value, key: &str, default: u64, max: u64) -> Result<u64> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1 && *n <= max)
            .ok_or(Error::Denied("file action budget")),
    }
}
pub struct FileDispatcher<'a> {
    pub grants: &'a HashMap<String, Grant>,
    pub verifier: &'a dyn PlatformVerifier,
    pub signals: Option<&'a ActionSignals>,
}
impl Dispatcher for FileDispatcher<'_> {
    fn dispatch(&mut self, a: &Action, args: &Value) -> Result<Outcome> {
        let operation = a.operation.as_str();
        let (capability, tool, allowed) = match operation {
            "file.list" => ("list", "local_file_list", vec!["grant_id", "path", "limit"]),
            "file.read" => (
                "read",
                "local_file_read",
                vec!["grant_id", "path", "max_bytes"],
            ),
            "file.hash" => (
                "read",
                "local_file_hash",
                vec!["grant_id", "path", "max_bytes"],
            ),
            "file.search" => (
                "search",
                "local_file_search",
                vec!["grant_id", "path", "query", "limit"],
            ),
            "file.watch" => (
                "watch",
                "local_file_watch",
                vec!["grant_id", "path", "after_revision", "timeout_ms"],
            ),
            _ => return Err(Error::Denied("unsupported file operation")),
        };
        let o = args.as_object().ok_or(Error::Protocol)?;
        if o.keys().any(|k| !allowed.contains(&k.as_str()))
            || a.tool_name != tool
            || a.capability != format!("file.{capability}")
        {
            return Err(Error::Denied("file tool binding"));
        }
        let grant = args["grant_id"].as_str().ok_or(Error::Protocol)?;
        let path = args["path"].as_str().ok_or(Error::Protocol)?;
        safe_parts(path)?;
        let g = self
            .grants
            .get(grant)
            .ok_or(Error::Denied("unknown grant"))?;
        a.validate(args, grant, &[grant.into(), path.into()], self.verifier)?;
        g.authorize(capability, &a.tenant_id, &a.user_id)?;
        if self
            .signals
            .is_some_and(|signals| signals.is_cancelled(&a.action_id))
        {
            return Err(Error::Denied("action cancelled"));
        }
        let result = match operation {
            "file.list" => {
                let limit = bounded(args, "limit", 500, 500)? as usize;
                let mut entries = list(g, path, false)?;
                entries.truncate(limit);
                json!({"kind":"file_list","entries":entries})
            }
            "file.read" | "file.hash" => {
                let max = bounded(args, "max_bytes", 8 * 1024 * 1024, 8 * 1024 * 1024)? as usize;
                let bytes = g.read(path, max)?;
                let content = String::from_utf8(bytes.clone()).ok();
                let mut result = json!({"kind":if operation=="file.hash"{"file_hash"}else{"file_read"},"relative_path":path,"encoding":content.as_ref().map(|_|"utf-8"),"size":bytes.len(),"sha256":sha256(&bytes)});
                if operation == "file.read" {
                    result["content"] = json!(content);
                }
                result
            }
            "file.watch" => {
                if args
                    .get("after_revision")
                    .is_some_and(|v| !v.is_null() && v != "0")
                {
                    return Err(Error::Denied("watch cursor unavailable"));
                }
                let timeout = bounded(args, "timeout_ms", 1000, 30000)?;
                let mut watcher = crate::watcher::Watcher::default();
                watcher.scan(g, &a.tenant_id, &a.user_id)?;
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_millis(timeout);
                let mut events = Vec::new();
                while events.is_empty() && std::time::Instant::now() < deadline {
                    if self
                        .signals
                        .is_some_and(|signals| signals.is_cancelled(&a.action_id))
                    {
                        return Err(Error::Denied("action cancelled"));
                    }
                    std::thread::sleep(
                        std::time::Duration::from_millis(50)
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                    events = watcher.scan(g, &a.tenant_id, &a.user_id)?;
                }
                events.retain(|e| {
                    path == "."
                        || e.relative_path == path
                        || e.relative_path
                            .starts_with(&format!("{}/", path.trim_end_matches('/')))
                });
                json!({"kind":"file_watch","events":events})
            }
            "file.search" => {
                g.authorize("read", &a.tenant_id, &a.user_id)?;
                g.authorize("list", &a.tenant_id, &a.user_id)?;
                let query = args["query"]
                    .as_str()
                    .filter(|s| !s.is_empty() && s.chars().count() <= 500 && !s.contains('\0'))
                    .ok_or(Error::Denied("search query"))?;
                let limit = bounded(args, "limit", 200, 200)? as usize;
                let mut matches = Vec::new();
                for entry in list(g, ".", true)? {
                    if entry.kind != "file"
                        || (path != "."
                            && !entry
                                .relative_path
                                .starts_with(&format!("{}/", path.trim_end_matches('/'))))
                    {
                        continue;
                    }
                    let Ok(bytes) = g.read(&entry.relative_path, 8 * 1024 * 1024) else {
                        continue;
                    };
                    let hash = sha256(&bytes);
                    let Ok(text) = String::from_utf8(bytes) else {
                        continue;
                    };
                    for (n, line) in text.lines().enumerate() {
                        if let Some(offset) = line.find(query) {
                            matches.push(json!({"relative_path":entry.relative_path,"line":n+1,"column":line[..offset].chars().count()+1,"preview":line.chars().take(300).collect::<String>(),"file_sha256":hash}));
                            if matches.len() >= limit {
                                break;
                            }
                        }
                    }
                    if matches.len() >= limit {
                        break;
                    }
                }
                json!({"kind":"file_search","matches":matches})
            }
            _ => return Err(Error::Protocol),
        };
        Ok(Outcome {
            status: "succeeded".into(),
            result,
            error_code: None,
        })
    }
}

/// Production file-only dispatcher used by the headless companion. Roots are
/// constructed only from explicit operator grants; writes pass through the
/// durable ledger and one-use trusted local approval verifier.
pub struct FileControlledDispatcher<V: PlatformVerifier, A: ApprovalVerifier> {
    pub grants: HashMap<String, Grant>,
    pub verifier: V,
    pub approvals: A,
    pub files: FileService,
    pub ledger: Ledger,
    pub approval_prompt: Option<Arc<dyn LocalApprovalPrompt>>,
    signals: ActionSignals,
    control: Arc<FileRemoteControl>,
}
/// A platform-local prompt. Implementations must obtain a direct owner decision
/// and sign only the supplied canonical approval payload.
pub trait LocalApprovalPrompt: Send + Sync {
    fn sign(
        &self,
        payload: &[u8],
        summary: &str,
        device_id: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<String>>;
}
impl<V: PlatformVerifier, A: ApprovalVerifier> FileControlledDispatcher<V, A> {
    pub fn new(
        grants: HashMap<String, Grant>,
        verifier: V,
        approvals: A,
        files: FileService,
        ledger: Ledger,
    ) -> Result<Self> {
        let signals = ActionSignals::default();
        let control = Arc::new(FileRemoteControl {
            ledger: std::sync::Mutex::new(ledger.fork()?),
            signals: signals.clone(),
        });
        Ok(Self {
            grants,
            verifier,
            approvals,
            files,
            ledger,
            approval_prompt: None,
            signals,
            control,
        })
    }
    pub fn with_approval_prompt(mut self, prompt: Arc<dyn LocalApprovalPrompt>) -> Self {
        self.approval_prompt = Some(prompt);
        self
    }
}
struct FileRemoteControl {
    ledger: std::sync::Mutex<Ledger>,
    signals: ActionSignals,
}
impl RemoteControl for FileRemoteControl {
    fn cancel(&self, action_id: &str) -> Result<Outcome> {
        self.signals.cancel(action_id);
        let _effect = self.signals.lock_effect()?;
        let mut ledger = self.ledger.lock().map_err(|_| Error::Storage)?;
        let Some(record) = ledger.get(action_id)? else {
            return Ok(Outcome {
                status: "cancelled".into(),
                result: json!({"cancelled":true,"action_known":false,"replay_allowed":false}),
                error_code: None,
            });
        };
        if crate::ledger::terminal(&record.status) {
            return Ok(Outcome {
                status: "failed".into(),
                result: json!({"cancelled":false,"prior_status":record.status}),
                error_code: Some("action_not_active".into()),
            });
        }
        let (status, result, error_code) =
            if matches!(record.status.as_str(), "policy_check" | "awaiting_approval") {
                (
                    "cancelled",
                    json!({"cancelled":true,"replay_allowed":false}),
                    None,
                )
            } else {
                (
                    "unknown",
                    json!({"cancelled":false,"side_effect_unknown":true,"replay_allowed":false}),
                    Some("side_effect_unknown".into()),
                )
            };
        ledger.transition(action_id, status, Some(result.clone()))?;
        Ok(Outcome {
            status: status.into(),
            result,
            error_code,
        })
    }
    fn emergency_stop(&self) -> Result<Vec<(String, Outcome)>> {
        self.signals.emergency();
        let _effect = self.signals.lock_effect()?;
        let mut ledger = self.ledger.lock().map_err(|_| Error::Storage)?;
        let ids = ledger.disconnect()?;
        ids.into_iter().map(|id|{
            self.signals.cancel(&id);
            let record=ledger.get(&id)?.ok_or(Error::Integrity)?;
            let (status,result,error_code)=if record.status=="interrupted"{
                ("cancelled",json!({"cancelled":true,"replay_allowed":false}),None)
            }else{
                ("unknown",json!({"cancelled":false,"side_effect_unknown":true,"replay_allowed":false}),Some("side_effect_unknown".into()))
            };
            Ok((id,Outcome{status:status.into(),result,error_code}))
        }).collect()
    }
    fn is_cancelled(&self, action_id: &str) -> bool {
        self.signals.is_cancelled(action_id)
    }
    fn completion_override(&self, action_id: &str) -> Result<Option<Outcome>> {
        let ledger = self.ledger.lock().map_err(|_| Error::Storage)?;
        let Some(record) = ledger.get(action_id)? else {
            return Ok(self.signals.is_cancelled(action_id).then(|| Outcome {
                status: "cancelled".into(),
                result: json!({"cancelled":true,"replay_allowed":false}),
                error_code: None,
            }));
        };
        let outcome = match record.status.as_str() {
            "cancelled" | "interrupted" => Some(Outcome {
                status: "cancelled".into(),
                result: record
                    .result
                    .unwrap_or(json!({"cancelled":true,"replay_allowed":false})),
                error_code: None,
            }),
            "unknown" => Some(Outcome {
                status: "unknown".into(),
                result: record
                    .result
                    .unwrap_or(json!({"side_effect_unknown":true,"replay_allowed":false})),
                error_code: Some("side_effect_unknown".into()),
            }),
            _ => None,
        };
        Ok(outcome)
    }
    fn finish_action(&self, action_id: &str) {
        self.signals.finish_action(action_id);
    }
}
impl<V: PlatformVerifier, A: ApprovalVerifier> Dispatcher for FileControlledDispatcher<V, A> {
    fn dispatch(&mut self, a: &Action, args: &Value) -> Result<Outcome> {
        if a.operation == "file.write" {
            let (intent, content) = crate::runner::file_write_intent(args)?;
            let grant_id = intent["grant_id"].as_str().ok_or(Error::Protocol)?;
            let path = intent["relative_path"].as_str().ok_or(Error::Protocol)?;
            let expected = intent["expected_hash"].as_str();
            let content_hash = intent["content_sha256"]
                .as_str()
                .ok_or(Error::Protocol)?
                .to_owned();
            let content_len = content.len();
            let path_summary = path.to_owned();
            let grant_summary = grant_id.to_owned();
            let grant = self
                .grants
                .get(grant_id)
                .ok_or(Error::Denied("unknown grant"))?;
            let prompt = self.approval_prompt.clone();
            let signals = self.signals.clone();
            match self.files.write_with_approval(
                grant,
                path,
                &content,
                expected,
                a,
                &mut self.ledger,
                &self.verifier,
                &self.approvals,
                &self.signals,
                move |action, target| {
                    if let Some(approval) = &action.approval {
                        return Ok(approval.clone());
                    }
                    let prompt = prompt
                        .as_ref()
                        .ok_or(Error::Denied("approval prompt unavailable"))?;
                    let now = crate::now();
                    let expires_at = action.expires_at.min(now + 120.0);
                    if !expires_at.is_finite() || expires_at <= now {
                        return Err(Error::Denied("approval expired"));
                    }
                    let mut nonce = [0u8; 32];
                    getrandom::getrandom(&mut nonce)
                        .map_err(|_| Error::Denied("approval entropy unavailable"))?;
                    let mut approval = Approval {
                        approval_id: format!("approval_{}", uuid::Uuid::new_v4().simple()),
                        action_id: action.action_id.clone(),
                        device_id: action.device_id.clone(),
                        arguments_digest: action.arguments_digest.clone(),
                        target_snapshot_digest: target.into(),
                        policy_snapshot_digest: action.policy_snapshot_digest.clone(),
                        nonce: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce),
                        expires_at,
                        local_signature: String::new(),
                    };
                    let mut payload =
                        serde_json::to_value(&approval).map_err(|_| Error::Protocol)?;
                    payload
                        .as_object_mut()
                        .ok_or(Error::Protocol)?
                        .remove("local_signature");
                    let payload = crate::canonical(&payload);
                    let summary = format!(
                        "Write {} byte(s) to {} in grant {} (SHA-256 {})",
                        content_len, path_summary, grant_summary, content_hash
                    );
                    let signals = signals.clone();
                    let cancelled = || signals.is_cancelled(&action.action_id);
                    approval.local_signature = prompt
                        .sign(payload.as_bytes(), &summary, &action.device_id, &cancelled)
                        .map_err(|_| {
                            if cancelled() {
                                Error::Denied("action cancelled")
                            } else {
                                Error::Denied("approval denied")
                            }
                        })?
                        .ok_or(Error::Denied("approval denied"))?;
                    Ok(approval)
                },
            ) {
                Ok(receipt) => Ok(Outcome {
                    status: "succeeded".into(),
                    result: serde_json::to_value(receipt).map_err(|_| Error::Protocol)?,
                    error_code: None,
                }),
                Err(Error::Denied("action cancelled")) => Ok(Outcome {
                    status: "cancelled".into(),
                    result: json!({"cancelled":true,"replay_allowed":false}),
                    error_code: None,
                }),
                Err(Error::Denied(
                    "approval required"
                    | "approval binding"
                    | "approval signature or replay"
                    | "approval denied"
                    | "approval prompt unavailable"
                    | "approval entropy unavailable"
                    | "approval expired",
                )) => Ok(Outcome {
                    status: "failed".into(),
                    result: json!({"error_code":"approval_denied","replay_allowed":false,"side_effect_unknown":false}),
                    error_code: Some("approval_denied".into()),
                }),
                Err(Error::Denied(_) | Error::Stale) => Ok(Outcome {
                    status: "failed".into(),
                    result: json!({"error_code":"file_write_rejected","replay_allowed":false,"side_effect_unknown":false}),
                    error_code: Some("file_write_rejected".into()),
                }),
                Err(error) => Err(error),
            }
        } else {
            FileDispatcher {
                grants: &self.grants,
                verifier: &self.verifier,
                signals: Some(&self.signals),
            }
            .dispatch(a, args)
        }
    }
}
impl<V: PlatformVerifier, A: ApprovalVerifier> ControlledDispatcher
    for FileControlledDispatcher<V, A>
{
    fn connect(&mut self) -> Result<()> {
        Ok(())
    }
    fn cancel(&mut self, action_id: &str) -> Result<Outcome> {
        self.control.cancel(action_id)
    }
    fn emergency_stop(&mut self) -> Result<Vec<String>> {
        Ok(self
            .control
            .emergency_stop()?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }
    fn disconnect(&mut self) -> Result<Vec<String>> {
        self.ledger.disconnect()
    }
    fn remote_control(&self) -> Arc<dyn RemoteControl> {
        self.control.clone()
    }
}
