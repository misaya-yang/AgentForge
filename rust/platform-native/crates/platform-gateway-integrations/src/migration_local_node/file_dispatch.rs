use super::{
    LocalNodeError as Error, Result, files::LocalFileService, models::*,
    transport::DispatchOutcome, watcher::DirectoryWatcher, workspace::SecureWorkspace,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
pub struct ReadOnlyFileActionHandlers {
    pub files: Arc<LocalFileService>,
}
fn integer(value: Option<&Value>, min: u64, max: u64, default: u64) -> Result<u64> {
    match value {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .filter(|v| *v >= min && *v <= max)
            .ok_or_else(|| Error::denied("file action budget invalid")),
    }
}
impl ReadOnlyFileActionHandlers {
    pub fn dispatch(&self, action: &ActionContext, args: &Value) -> Result<DispatchOutcome> {
        let operation = action.operation.as_str();
        let (tool, allowed, cap) = match operation {
            "file.list" => ("local_file_list", vec!["grant_id", "path", "limit"], "list"),
            "file.read" => (
                "local_file_read",
                vec!["grant_id", "path", "max_bytes"],
                "read",
            ),
            "file.hash" => (
                "local_file_hash",
                vec!["grant_id", "path", "max_bytes"],
                "read",
            ),
            "file.search" => (
                "local_file_search",
                vec!["grant_id", "path", "query", "limit"],
                "search",
            ),
            "file.watch" => (
                "local_file_watch",
                vec!["grant_id", "path", "after_revision", "timeout_ms"],
                "watch",
            ),
            _ => return Err(Error::denied("unsupported file action")),
        };
        let obj = args
            .as_object()
            .ok_or_else(|| Error::denied("file action arguments invalid"))?;
        if action.tool_name != tool || obj.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::denied(
                "signed file action tool or arguments changed",
            ));
        }
        let grant = args["grant_id"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| Error::denied("grant missing"))?;
        let relative = args["path"]
            .as_str()
            .ok_or_else(|| Error::denied("path missing"))?;
        if action.capability
            != (if operation == "file.hash" {
                "file.read"
            } else {
                operation
            })
        {
            return Err(Error::denied("signed file capability changed"));
        }
        action.validate_payload(
            args,
            self.files.ledger.platform_signature_verifier.as_deref(),
            Some(grant),
            Some(&[grant.into(), relative.into()]),
        )?;
        self.files
            .grants
            .get(grant, cap, Some((&action.tenant_id, &action.user_id)))?;
        SecureWorkspace::parts(relative)?;
        let result = match operation {
            "file.list" => {
                let limit = integer(args.get("limit"), 1, 500, 500)? as usize;
                json!({"kind":"file_list","entries":self.files.list_files(grant,relative,false)?.into_iter().take(limit).collect::<Vec<_>>()})
            }
            "file.read" | "file.hash" => {
                let max = integer(args.get("max_bytes"), 1, 8 * 1024 * 1024, 8 * 1024 * 1024)?;
                let read = self.files.read_file(grant, relative)?;
                if read.size as u64 > max {
                    return Err(Error::denied("file exceeds signed read budget"));
                }
                let mut result = json!({"kind":if operation=="file.hash"{"file_hash"}else{"file_read"},"relative_path":read.relative_path,"encoding":read.encoding,"size":read.size,"sha256":read.sha256});
                if operation == "file.read" {
                    result["content"] = json!(std::str::from_utf8(&read.content).ok());
                }
                result
            }
            "file.search" => {
                let query = args["query"]
                    .as_str()
                    .filter(|q| !q.is_empty() && q.chars().count() <= 500 && !q.contains('\0'))
                    .ok_or_else(|| Error::denied("file search query invalid"))?;
                let limit = integer(args.get("limit"), 1, 200, 200)?;
                let glob = if relative == "." {
                    "*".into()
                } else {
                    format!("{}/*", relative.trim_end_matches('/'))
                };
                json!({"kind":"file_search","matches":self.files.search(grant,query,&glob,limit as usize)?})
            }
            _ => {
                if !args["after_revision"].is_null() && args["after_revision"].as_str() != Some("0")
                {
                    return Err(Error::denied(
                        "file watch cursor unavailable for this session",
                    ));
                }
                let timeout = integer(args.get("timeout_ms"), 1, 30000, 1000)?;
                let mut watcher = DirectoryWatcher::new(self.files.clone(), grant.into())?;
                watcher.scan_once()?;
                let deadline = Instant::now() + Duration::from_millis(timeout);
                let mut events = Vec::new();
                while events.is_empty() && Instant::now() < deadline {
                    std::thread::sleep(
                        Duration::from_millis(50)
                            .min(deadline.saturating_duration_since(Instant::now())),
                    );
                    events = watcher.scan_once()?;
                }
                events.retain(|e| {
                    relative == "."
                        || e.relative_path == relative
                        || e.relative_path
                            .starts_with(&format!("{}/", relative.trim_end_matches('/')))
                });
                json!({"kind":"file_watch","events":events})
            }
        };
        Ok(DispatchOutcome {
            action_id: action.action_id.clone(),
            status: ActionStatus::Succeeded,
            result,
            error_code: None,
        })
    }
}
