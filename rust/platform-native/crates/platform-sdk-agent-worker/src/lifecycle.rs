//! Crash-safe session ownership. Unknown execution is fenced, never replayed.
use crate::contract::{Launch, Result};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
pub struct Session {
    pub path: PathBuf,
    pub _lock: File,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRun {
    pub request_id: String,
    pub fingerprint: String,
}
pub fn sync(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(|_| "sdk_state_unavailable")
}
pub fn safe_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => Ok(()),
        Ok(_) => Err("sdk_state_unavailable"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| "sdk_state_unavailable")?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| "sdk_state_unavailable")
        }
        Err(_) => Err("sdk_state_unavailable"),
    }
}
fn file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "sdk_state_unavailable")
}
fn lock(path: &Path) -> Result<File> {
    let f = file(path)?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("sdk_session_busy");
    }
    Ok(f)
}
pub fn atomic(path: &Path, value: &Value) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&tmp)
        .map_err(|_| "sdk_state_unavailable")?;
    f.write_all(
        serde_json::to_string(value)
            .map_err(|_| "sdk_state_unavailable")?
            .as_bytes(),
    )
    .and_then(|_| f.sync_all())
    .map_err(|_| "sdk_state_unavailable")?;
    fs::rename(tmp, path).map_err(|_| "sdk_state_unavailable")?;
    sync(path.parent().ok_or("sdk_state_unavailable")?)
}
pub fn read(path: &Path) -> Result<Value> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|_| "sdk_state_unavailable")?;
    use std::io::Read;
    let mut bytes = Vec::new();
    f.take(8 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "sdk_state_unavailable")?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err("sdk_state_unavailable");
    }
    serde_json::from_slice(&bytes).map_err(|_| "sdk_state_unavailable")
}
pub fn touch(path: &Path) -> Result<()> {
    let f = file(&path.join(".activity"))?;
    f.set_modified(SystemTime::now())
        .map_err(|_| "sdk_state_unavailable")
}
pub fn valid_scope(scope: &str) -> bool {
    scope.len() == 64
        && scope
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn marker(root: &Path, dir: &str, scope: &str) -> PathBuf {
    root.join(dir).join(format!("{scope}.json"))
}
pub fn acquire(root: &Path, scope: &str) -> Result<Session> {
    if !valid_scope(scope) {
        return Err("sdk_request_invalid");
    }
    let _gate = lock(&root.join(".admission.lock"))?;
    if marker(root, ".tombstones", scope).exists() {
        return Err("sdk_session_deleted");
    }
    let path = root.join(scope);
    let created = !path.exists();
    safe_dir(&path)?;
    let held = lock(&path.join(".lock"))?;
    if marker(root, ".cleanup", scope).exists() {
        drop(held);
        drop(_gate);
        return Err("sdk_session_reconciliation_required");
    }
    if created {
        touch(&path)?;
        sync(root)?;
    }
    Ok(Session { path, _lock: held })
}
pub fn begin_cleanup(session: &Session) -> Result<()> {
    let root = session.path.parent().ok_or("sdk_state_unavailable")?;
    let scope = session
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("sdk_state_unavailable")?;
    safe_dir(&root.join(".cleanup"))?;
    let proof = session.path.join(".cleanup-complete");
    if proof.exists() {
        fs::remove_file(proof).map_err(|_| "sdk_state_unavailable")?;
    }
    sync(&session.path)?;
    atomic(
        &marker(root, ".cleanup", scope),
        &json!({"schema":"sdk-cleanup/v1"}),
    )?;
    sync(root)
}
pub fn complete_cleanup(session: &Session, request_id: &str, fingerprint: &str) -> Result<()> {
    let root = session.path.parent().ok_or("sdk_state_unavailable")?;
    let scope = session
        .path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("sdk_state_unavailable")?;
    if read_pending(session)?.is_some_and(|pending|pending.request_id!=request_id || pending.fingerprint!=fingerprint) {
        return Err("sdk_state_unavailable");
    }
    atomic(
        &session.path.join(".cleanup-complete"),
        &json!({"schema":"sdk-cleanup-complete/v2","request_id":request_id,"fingerprint":fingerprint}),
    )?;
    let m = marker(root, ".cleanup", scope);
    if m.exists() {
        fs::remove_file(m).map_err(|_| "sdk_state_unavailable")?;
        sync(&root.join(".cleanup"))?;
    }
    touch(&session.path)
}
pub fn read_pending(session: &Session) -> Result<Option<PendingRun>> {
    let path = session.path.join("pending.json");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("sdk_session_reconciliation_required"),
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            return Err("sdk_session_reconciliation_required");
        }
        Ok(_) => {}
    }
    let value = read(&path).map_err(|_| "sdk_session_reconciliation_required")?;
    let request_id = value["request_id"]
        .as_str()
        .filter(|value| !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control))
        .ok_or("sdk_session_reconciliation_required")?;
    let fingerprint = value["fingerprint"]
        .as_str()
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        .ok_or("sdk_session_reconciliation_required")?;
    Ok(Some(PendingRun {
        request_id: request_id.into(),
        fingerprint: fingerprint.into(),
    }))
}
fn cleanup_proof_matches(session: &Session, pending: &PendingRun) -> bool {
    let Some(root) = session.path.parent() else { return false; };
    let Some(scope) = session.path.file_name().and_then(|name| name.to_str()) else { return false; };
    if marker(root, ".cleanup", scope).exists() { return false; }
    let proof = session.path.join(".cleanup-complete");
    if fs::symlink_metadata(&proof).is_err_and(|error| error.kind() != std::io::ErrorKind::NotFound)
        || fs::symlink_metadata(&proof).is_ok_and(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
    { return false; }
    if let Ok(value) = read(&proof) {
        if value["schema"] == "sdk-cleanup-complete/v2"
            && value["request_id"] == pending.request_id
            && value["fingerprint"] == pending.fingerprint
        { return true; }
    }
    // Older Worker versions wrote a generic text marker. It is usable only
    // when its filesystem timestamp proves cleanup followed this reservation.
    if !matches!(fs::read_to_string(&proof).as_deref(), Ok("sdk-cleanup/v1\n")) { return false; }
    let pending_time = fs::metadata(session.path.join("pending.json")).and_then(|metadata| metadata.modified()).ok();
    let cleanup_time = fs::metadata(&proof).and_then(|metadata| metadata.modified()).ok();
    matches!((pending_time, cleanup_time), (Some(pending), Some(cleanup)) if cleanup > pending)
}
pub fn effect_free_profile(session: &Session) -> bool {
    let Ok(profile) = read(&session.path.join("sdk-profile.json")) else { return false; };
    profile["schema"] == "sdk-session-tools/v1"
        && profile["dataset_ids"].as_array().is_some_and(Vec::is_empty)
        && profile.get("capability_tools").is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty))
}
pub fn locally_reconcilable(session: &Session, pending: &PendingRun) -> bool {
    cleanup_proof_matches(session, pending) && effect_free_profile(session)
        && read_pending(session).ok().flatten().as_ref() == Some(pending)
}
/// Drop an old reservation only after the Gateway has verified its durable
/// cancelled run and no-tool audit fence. The caller holds this session lock.
pub fn reconcile_cancelled(session: &Session, receipt: &Path, pending: &PendingRun) -> Result<()> {
    if !locally_reconcilable(session, pending) {
        return Err("sdk_session_reconciliation_required");
    }
    if read_pending(session)?.as_ref() != Some(pending) {
        return Err("sdk_session_reconciliation_required");
    }
    let cancellation = json!({
        "fingerprint": pending.fingerprint,
        "result": {"event":"error","code":"sdk_execution_cancelled"},
        "reconciliation": {
            "schema":"sdk-cancelled-run-reconciliation/v1",
            "request_id":pending.request_id,
            "provider_outcome":"unknown",
            "usage_state":"unavailable",
            "cost_state":"unavailable",
            "tool_effects":"none"
        }
    });
    if receipt.exists() {
        let existing = read(receipt).map_err(|_| "sdk_session_reconciliation_required")?;
        if existing["fingerprint"] != pending.fingerprint
            || existing["result"]["event"] != "error"
            || existing["result"]["code"] != "sdk_execution_cancelled"
            || existing["reconciliation"] != cancellation["reconciliation"]
        { return Err("sdk_session_reconciliation_required"); }
    } else {
        atomic(receipt, &cancellation).map_err(|_| "sdk_session_reconciliation_required")?;
    }
    let path = session.path.join("pending.json");
    if read_pending(session)?.as_ref() != Some(pending) {
        return Err("sdk_session_reconciliation_required");
    }
    fs::remove_file(path).map_err(|_| "sdk_session_reconciliation_required")?;
    sync(&session.path).map_err(|_| "sdk_session_reconciliation_required")
}
pub fn profile(session: &Session, launch: &Launch) -> Result<()> {
    let mut profile = json!({"schema":"sdk-session-tools/v1","dataset_ids":launch.bridge("read_bridge").map(|v|v["dataset_ids"].clone()).unwrap_or(json!([]))});
    let tools = launch.body["capability_bridge"]["tools"].as_array();
    if tools.is_some_and(|t| !t.is_empty()) {
        profile["capability_tools"] = json!(tools.unwrap());
    }
    let path = session.path.join("sdk-profile.json");
    if path.exists() {
        if read(&path)? != profile {
            return Err("sdk_session_profile_changed");
        }
        return Ok(());
    }
    if launch.bridge("read_bridge").is_some() || tools.is_some_and(|t| !t.is_empty()) {
        let history = session.path.join("openai.sqlite").exists()
            || session.path.join("claude-session.json").exists()
            || fs::read_dir(&session.path)
                .map_err(|_| "sdk_state_unavailable")?
                .filter_map(|e| e.ok())
                .any(|e| {
                    e.file_name()
                        .to_str()
                        .is_some_and(|s| s.starts_with("run-") && s.ends_with(".json"))
                });
        if history {
            return Err("sdk_session_profile_changed");
        }
    }
    atomic(&path, &profile)
}
fn cleanup_unknown(root: &Path, scope: &str, state: &Path) -> bool {
    if marker(root, ".cleanup", scope).exists() {
        return true;
    }
    if state.join("pending.json").exists() {
        let p = state.join(".cleanup-complete");
        if fs::symlink_metadata(&p).map(|m| m.file_type().is_symlink()).unwrap_or(true) {
            return true;
        }
        let Ok(pending) = read(&state.join("pending.json")) else { return true; };
        if let Ok(proof) = read(&p) {
            return proof["schema"] != "sdk-cleanup-complete/v2"
                || proof["request_id"] != pending["request_id"]
                || proof["fingerprint"] != pending["fingerprint"];
        }
        return !matches!(fs::read_to_string(p).as_deref(), Ok("sdk-cleanup/v1\n"));
    }
    false
}
pub fn remove(root: &Path, scope: &str, reason: &str, cutoff: Option<SystemTime>) -> Result<bool> {
    if !valid_scope(scope) {
        return Err("sdk_request_invalid");
    }
    let _gate = lock(&root.join(".admission.lock"))?;
    let state = root.join(scope);
    let tomb = marker(root, ".tombstones", scope);
    if fs::symlink_metadata(&state).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("sdk_state_unavailable");
    }
    if !state.exists() && marker(root, ".cleanup", scope).exists() {
        return Err("sdk_session_reconciliation_required");
    }
    let held = if state.exists() {
        Some(lock(&state.join(".lock"))?)
    } else {
        None
    };
    if held.is_some() && cleanup_unknown(root, scope, &state) {
        drop(held);
        drop(_gate);
        return Err("sdk_session_reconciliation_required");
    }
    if let Some(cutoff) = cutoff {
        if !state.exists() {
            return Ok(false);
        }
        let changed = fs::metadata(state.join(".activity"))
            .or_else(|_| fs::metadata(&state))
            .and_then(|m| m.modified())
            .map_err(|_| "sdk_state_unavailable")?;
        if !tomb.exists() && changed > cutoff {
            return Ok(false);
        }
    }
    if !tomb.exists() {
        safe_dir(&root.join(".tombstones"))?;
        atomic(
            &tomb,
            &json!({"reason":format!("{reason}{}",if state.join("pending.json").exists(){"_unknown"}else{""}),"deleted_at":SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()}),
        )?;
        sync(root)?;
    }
    if held.is_some() {
        fs::remove_dir_all(&state).map_err(|_| "sdk_deletion_incomplete")?;
        sync(root)?;
        return Ok(true);
    }
    Ok(false)
}
pub struct Sweeper {
    entries: Option<fs::ReadDir>,
}
impl Sweeper {
    pub fn new() -> Self {
        Self { entries: None }
    }
    pub fn tick(&mut self, root: &Path, ttl: u64) -> Result<()> {
        if self.entries.is_none() {
            self.entries = Some(fs::read_dir(root).map_err(|_| "sdk_retention_unavailable")?);
        }
        for _ in 0..100 {
            let Some(entry) = self.entries.as_mut().unwrap().next() else {
                self.entries = None;
                break;
            };
            let entry = entry.map_err(|_| "sdk_retention_unavailable")?;
            let name = entry.file_name();
            let Some(scope) = name.to_str() else {
                continue;
            };
            if !valid_scope(scope)
                || !entry
                    .file_type()
                    .map_err(|_| "sdk_retention_unavailable")?
                    .is_dir()
            {
                continue;
            }
            match remove(
                root,
                scope,
                "expired",
                Some(SystemTime::now() - Duration::from_secs(ttl)),
            ) {
                Ok(_) | Err("sdk_session_busy" | "sdk_session_reconciliation_required") => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}
pub fn validate_root(root: &Path) -> Result<()> {
    let m = fs::symlink_metadata(root).map_err(|_| "sdk_worker_state_invalid")?;
    if !root.is_absolute()
        || !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != unsafe { libc::getuid() }
        || m.permissions().mode() & 0o777 != 0o700
    {
        return Err("sdk_worker_state_permissions_invalid");
    }
    for name in [".tombstones", ".cleanup"] {
        if fs::symlink_metadata(root.join(name))
            .is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink())
        {
            return Err("sdk_worker_state_invalid");
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sdk-native-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        p
    }
    #[test]
    fn ownership_and_tombstone() {
        let root = root();
        let id = "a".repeat(64);
        let s = acquire(&root, &id).unwrap();
        assert!(matches!(acquire(&root, &id), Err("sdk_session_busy")));
        drop(s);
        assert!(remove(&root, &id, "deleted", None).unwrap());
        assert!(matches!(acquire(&root, &id), Err("sdk_session_deleted")));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn crash_fence_survives_lock_release() {
        let root = root();
        let id = "b".repeat(64);
        let s = acquire(&root, &id).unwrap();
        begin_cleanup(&s).unwrap();
        drop(s);
        assert!(matches!(
            acquire(&root, &id),
            Err("sdk_session_reconciliation_required")
        ));
        assert_eq!(
            remove(&root, &id, "deleted", None),
            Err("sdk_session_reconciliation_required")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn clean_unknown_run_can_be_deleted_but_not_replayed() {
        let root = root();
        let id = "c".repeat(64);
        let s = acquire(&root, &id).unwrap();
        let fingerprint = "c".repeat(64);
        atomic(&s.path.join("pending.json"), &json!({"request_id":"r","fingerprint":fingerprint})).unwrap();
        begin_cleanup(&s).unwrap();
        complete_cleanup(&s,"r",&fingerprint).unwrap();
        drop(s);
        assert!(remove(&root, &id, "deleted", None).unwrap());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn fenced_pure_text_cancellation_reconciles_without_touching_prior_history() {
        let root=root();let id="f".repeat(64);let session=acquire(&root,&id).unwrap();
        atomic(&session.path.join("sdk-profile.json"),&json!({"schema":"sdk-session-tools/v1","dataset_ids":[]})).unwrap();
        let prior=json!({"schema":"sdk-native-history/v1","framework":"openai_agents","messages":[
            {"role":"user","content":"prior turn"},{"type":"message","role":"assistant","content":[]}
        ]});
        atomic(&session.path.join("native-history.json"),&prior).unwrap();
        let history_before=fs::read(session.path.join("native-history.json")).unwrap();
        let pending=PendingRun{request_id:"cancelled-request".into(),fingerprint:"a".repeat(64)};
        atomic(&session.path.join("pending.json"),&json!({"request_id":pending.request_id,"fingerprint":pending.fingerprint})).unwrap();
        begin_cleanup(&session).unwrap();
        complete_cleanup(&session,&pending.request_id,&pending.fingerprint).unwrap();
        let receipt=session.path.join("run-cancelled.json");
        assert!(locally_reconcilable(&session,&pending));
        reconcile_cancelled(&session,&receipt,&pending).unwrap();
        let saved=read(&receipt).unwrap();
        assert_eq!(saved["result"]["code"],"sdk_execution_cancelled");
        assert_eq!(saved["reconciliation"]["provider_outcome"],"unknown");
        assert_eq!(saved["reconciliation"]["usage_state"],"unavailable");
        assert_eq!(saved["reconciliation"]["cost_state"],"unavailable");
        assert_eq!(saved["reconciliation"]["tool_effects"],"none");
        assert!(read_pending(&session).unwrap().is_none());
        assert_eq!(fs::read(session.path.join("native-history.json")).unwrap(),history_before);
        drop(session);
        let resumed=acquire(&root,&id).unwrap();
        assert!(read_pending(&resumed).unwrap().is_none());
        drop(resumed);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn effectful_or_stale_cleanup_pending_remains_fenced() {
        let root=root();let id="e".repeat(64);let session=acquire(&root,&id).unwrap();
        atomic(&session.path.join("sdk-profile.json"),&json!({"schema":"sdk-session-tools/v1","dataset_ids":[],"capability_tools":[{"effect":"write"}]})).unwrap();
        let pending=PendingRun{request_id:"effectful-request".into(),fingerprint:"b".repeat(64)};
        atomic(&session.path.join("pending.json"),&json!({"request_id":pending.request_id,"fingerprint":pending.fingerprint})).unwrap();
        begin_cleanup(&session).unwrap();
        complete_cleanup(&session,&pending.request_id,&pending.fingerprint).unwrap();
        let receipt=session.path.join("run-effectful.json");
        assert!(!locally_reconcilable(&session,&pending));
        assert_eq!(reconcile_cancelled(&session,&receipt,&pending),Err("sdk_session_reconciliation_required"));
        assert!(session.path.join("pending.json").exists());
        assert!(!receipt.exists());
        fs::remove_dir_all(&root).unwrap();

        let root=self::root();let id="d".repeat(64);let session=acquire(&root,&id).unwrap();
        atomic(&session.path.join("sdk-profile.json"),&json!({"schema":"sdk-session-tools/v1","dataset_ids":[]})).unwrap();
        let proof=session.path.join(".cleanup-complete");
        fs::write(&proof,"sdk-cleanup/v1\n").unwrap();
        File::open(&proof).unwrap().set_modified(UNIX_EPOCH).unwrap();
        let stale=PendingRun{request_id:"unknown-request".into(),fingerprint:"c".repeat(64)};
        atomic(&session.path.join("pending.json"),&json!({"request_id":stale.request_id,"fingerprint":stale.fingerprint})).unwrap();
        assert!(!locally_reconcilable(&session,&stale));
        assert!(session.path.join("pending.json").exists());
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn atomic_unicode() {
        let root = root();
        atomic(&root.join("v.json"), &json!({"a":"中"})).unwrap();
        assert_eq!(
            crate::contract::canonical(&read(&root.join("v.json")).unwrap(), true),
            "{\"a\":\"\\u4e2d\"}"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
#[cfg(test)]
#[path = "sdk_migration_parity_lifecycle.rs"]
mod sdk_migration_parity_lifecycle;
