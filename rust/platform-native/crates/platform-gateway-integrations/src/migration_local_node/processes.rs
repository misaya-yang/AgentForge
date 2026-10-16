//! Structured argv execution with no shell or inherited host environment.
use super::{
    LocalNodeError as Error, Result,
    grants::{DirectoryGrantStore, identity},
    ledger::ActionLedger,
    models::*,
    workspace::SecureWorkspace,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
#[derive(Clone, Debug)]
pub struct ProcessPolicy {
    pub allowed_executables: BTreeSet<PathBuf>,
    pub allowed_env_names: BTreeSet<String>,
    pub path: String,
    pub max_timeout_seconds: f64,
    pub max_output_bytes: usize,
    pub allow_inherited_network: bool,
}
impl Default for ProcessPolicy {
    fn default() -> Self {
        Self {
            allowed_executables: BTreeSet::new(),
            allowed_env_names: BTreeSet::new(),
            path: "/usr/bin:/bin".into(),
            max_timeout_seconds: 300.0,
            max_output_bytes: 2 * 1024 * 1024,
            allow_inherited_network: false,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessRequest {
    pub argv: Vec<String>,
    pub grant_id: String,
    pub cwd: String,
    pub timeout_seconds: f64,
    pub env: BTreeMap<String, String>,
    pub network_policy: String,
}
impl ProcessRequest {
    pub fn normalized_arguments(&self) -> Value {
        json!({"argv":self.argv,"grant_id":self.grant_id,"cwd":self.cwd,"timeout_seconds":self.timeout_seconds,"env_names":self.env.keys().collect::<Vec<_>>(),"network_policy":self.network_policy})
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProcessResult {
    pub action_id: String,
    pub status: ActionStatus,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub started_at: f64,
    pub ended_at: f64,
    pub output_truncated: bool,
    pub error_code: Option<String>,
}
pub type OutputCallback<'a> = dyn FnMut(&str, &[u8]) + Send + 'a;
pub struct ProcessRunner {
    pub grants: Arc<DirectoryGrantStore>,
    pub policy: ProcessPolicy,
    pub ledger: Arc<ActionLedger>,
    processes: Mutex<HashMap<String, u32>>,
    cancelled: Mutex<HashSet<String>>,
}
fn denied(message: &str) -> Error {
    Error::new("process_policy_denied", message)
}
impl ProcessRunner {
    pub fn new(
        grants: Arc<DirectoryGrantStore>,
        policy: ProcessPolicy,
        ledger: Arc<ActionLedger>,
    ) -> Self {
        Self {
            grants,
            policy,
            ledger,
            processes: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashSet::new()),
        }
    }
    pub fn environment(
        &self,
        requested: &BTreeMap<String, String>,
    ) -> Result<BTreeMap<String, String>> {
        let mut env = BTreeMap::from([
            ("PATH".into(), self.policy.path.clone()),
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
        ]);
        for (name, value) in requested {
            if !self.policy.allowed_env_names.contains(name) {
                return Err(denied("environment variable is not allowlisted"));
            }
            if [
                "KEY",
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "PASSWD",
                "CREDENTIAL",
                "AUTH",
                "COOKIE",
                "SSH_",
                "AWS_",
                "AZURE_",
                "GOOGLE_",
                "OPENAI_",
                "DASHSCOPE_",
            ]
            .iter()
            .any(|p| name.to_uppercase().contains(p))
            {
                return Err(denied(
                    "secret-like environment variables cannot be injected",
                ));
            }
            if name.contains(['\0', '=']) || value.contains('\0') {
                return Err(denied("invalid environment entry"));
            }
            env.insert(name.clone(), value.clone());
        }
        Ok(env)
    }
    fn executable(&self, name: &str) -> Result<PathBuf> {
        let candidate = if name.contains(std::path::MAIN_SEPARATOR) {
            Path::new(name).canonicalize()?
        } else {
            std::env::split_paths(&self.policy.path)
                .map(|p| p.join(name))
                .find(|p| p.is_file())
                .ok_or_else(|| denied("executable not found in sanitized PATH"))?
                .canonicalize()?
        };
        let allowed = self
            .policy
            .allowed_executables
            .iter()
            .map(|p| p.canonicalize())
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        if !allowed.contains(&candidate) {
            return Err(denied("executable is not in the local allowlist"));
        }
        Ok(candidate)
    }
    pub fn cwd_snapshot_digest(
        &self,
        request: &ProcessRequest,
        tenant: &str,
        user: &str,
    ) -> Result<(PathBuf, String)> {
        let workspace = SecureWorkspace::new(self.grants.get(
            &request.grant_id,
            "read",
            Some((tenant, user)),
        )?);
        let directory = workspace.open_dir(&request.cwd)?;
        let (device, inode) = identity(&directory.metadata()?)?;
        Ok((
            workspace.display_path(&request.cwd)?,
            digest_payload(
                &json!({"grant_id":request.grant_id,"cwd":request.cwd,"device":device,"inode":inode}),
            ),
        ))
    }
    pub fn run(
        &self,
        request: &ProcessRequest,
        action: &ActionContext,
        mut on_output: Option<&mut OutputCallback<'_>>,
    ) -> Result<ProcessResult> {
        if action.capability != "process.run" || action.operation != "process.run" {
            return Err(Error::denied("action envelope lacks process.run"));
        }
        if request.argv.is_empty() || request.argv.iter().any(|s| s.contains('\0')) {
            return Err(denied("argv must be a non-empty NUL-free vector"));
        }
        action.validate_payload(
            &request.normalized_arguments(),
            self.ledger.platform_signature_verifier.as_deref(),
            Some(&request.grant_id),
            Some(&[request.grant_id.clone(), request.cwd.clone()]),
        )?;
        if !request.timeout_seconds.is_finite()
            || request.timeout_seconds <= 0.0
            || request.timeout_seconds > self.policy.max_timeout_seconds
        {
            return Err(denied("process timeout exceeds local policy"));
        }
        match request.network_policy.as_str() {
            "inherit" if self.policy.allow_inherited_network => (),
            "inherit" => return Err(denied("host network inheritance is not allowed")),
            "deny" => {
                return Err(denied(
                    "network-denied execution requires a configured sandbox backend",
                ));
            }
            _ => return Err(denied("unsupported network policy")),
        }
        #[cfg(not(unix))]
        {
            return Err(denied("process tree cancellation requires an OS adapter"));
        }
        let executable = self.executable(&request.argv[0])?;
        let (cwd, digest) =
            self.cwd_snapshot_digest(request, &action.tenant_id, &action.user_id)?;
        let environment = self.environment(&request.env)?;
        let begin = self.ledger.begin(action)?;
        if !begin.created {
            return serde_json::from_value(
                begin
                    .record
                    .result
                    .ok_or_else(|| Error::denied("matching process action already running"))?,
            )
            .map_err(Into::into);
        }
        let started = now();
        let mut process: Option<Child> = None;
        let result: Result<ProcessResult> = (|| {
            self.ledger.mark_awaiting_approval(&action.action_id)?;
            action.require_approval(
                &digest,
                self.ledger.trusted_local_approval_verifier.as_deref(),
            )?;
            self.ledger.mark_dispatched(&action.action_id)?;
            let mut command = Command::new(executable);
            command
                .args(&request.argv[1..])
                .current_dir(cwd)
                .env_clear()
                .envs(environment)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                unsafe {
                    command.pre_exec(|| {
                        if libc::setsid() < 0 {
                            Err(std::io::Error::last_os_error())
                        } else {
                            Ok(())
                        }
                    });
                }
            }
            process = Some(command.spawn()?);
            let child = process.as_mut().unwrap();
            self.processes
                .lock()
                .map_err(|_| denied("process state unavailable"))?
                .insert(action.action_id.clone(), child.id());
            self.ledger.mark_running(&action.action_id)?;
            let (stdout, stderr, truncated, timed_out) = self.collect(
                child,
                &action.action_id,
                request.timeout_seconds,
                &mut on_output,
            )?;
            let cancelled = self
                .cancelled
                .lock()
                .map_err(|_| denied("process state unavailable"))?
                .contains(&action.action_id);
            let status = if timed_out {
                ActionStatus::Failed
            } else if cancelled {
                ActionStatus::Cancelled
            } else {
                ActionStatus::Succeeded
            };
            let result = ProcessResult {
                action_id: action.action_id.clone(),
                status,
                exit_code: child.try_wait()?.and_then(|s| s.code()),
                stdout: if timed_out {
                    String::new()
                } else {
                    String::from_utf8_lossy(&stdout).into_owned()
                },
                stderr: if timed_out {
                    String::new()
                } else {
                    String::from_utf8_lossy(&stderr).into_owned()
                },
                started_at: started,
                ended_at: now(),
                output_truncated: !timed_out && truncated,
                error_code: if timed_out {
                    Some("timeout".into())
                } else if cancelled {
                    Some("cancelled".into())
                } else {
                    None
                },
            };
            self.ledger
                .finish(&action.action_id, status, serde_json::to_value(&result)?)?;
            Ok(result)
        })();
        if let Err(error) = &result {
            if let Some(child) = process.as_mut() {
                let _ = terminate(child);
            }
            self.ledger.finish(
                &action.action_id,
                ActionStatus::Failed,
                json!({"error_code":error.code}),
            )?;
        }
        self.processes
            .lock()
            .map_err(|_| denied("process state unavailable"))?
            .remove(&action.action_id);
        result
    }
    fn collect(
        &self,
        child: &mut Child,
        id: &str,
        timeout: f64,
        callback: &mut Option<&mut OutputCallback<'_>>,
    ) -> Result<(Vec<u8>, Vec<u8>, bool, bool)> {
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| denied("stdout unavailable"))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| denied("stderr unavailable"))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0
                    || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
            }
        }
        let deadline = Instant::now() + Duration::from_secs_f64(timeout);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_open, mut err_open, mut truncated) = (true, true, false);
        let mut used = 0;
        while out_open || err_open || child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                terminate(child)?;
                return Ok((out, err, truncated, true));
            }
            if self
                .cancelled
                .lock()
                .map_err(|_| denied("process state unavailable"))?
                .contains(id)
            {
                terminate(child)?;
            }
            for (channel, reader, bytes, open) in [
                (
                    "stdout",
                    &mut stdout as &mut dyn Read,
                    &mut out,
                    &mut out_open,
                ),
                (
                    "stderr",
                    &mut stderr as &mut dyn Read,
                    &mut err,
                    &mut err_open,
                ),
            ] {
                if !*open {
                    continue;
                }
                let mut buffer = [0u8; 65536];
                match reader.read(&mut buffer) {
                    Ok(0) => *open = false,
                    Ok(length) => {
                        let keep = length.min(self.policy.max_output_bytes.saturating_sub(used));
                        bytes.extend_from_slice(&buffer[..keep]);
                        used += keep;
                        truncated |= keep < length;
                        if keep > 0 {
                            if let Some(cb) = callback.as_deref_mut() {
                                cb(channel, &buffer[..keep]);
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (),
                    Err(e) => return Err(e.into()),
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait()?;
        Ok((out, err, truncated, false))
    }
    pub fn cancel(&self, id: &str) -> Result<bool> {
        self.cancelled
            .lock()
            .map_err(|_| denied("process state unavailable"))?
            .insert(id.into());
        let pid = self
            .processes
            .lock()
            .map_err(|_| denied("process state unavailable"))?
            .get(id)
            .copied();
        if let Some(pid) = pid {
            signal_group(pid, false)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
    pub fn cancel_all(&self) -> Result<Vec<String>> {
        let ids: Vec<_> = self
            .processes
            .lock()
            .map_err(|_| denied("process state unavailable"))?
            .keys()
            .cloned()
            .collect();
        let mut first_error = None;
        for id in &ids {
            if let Err(error) = self.cancel(id) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(ids)
    }
}
pub(crate) fn signal_group(pid: u32, force: bool) -> Result<()> {
    #[cfg(unix)]
    {
        let result = unsafe {
            libc::kill(
                -(pid as i32),
                if force { libc::SIGKILL } else { libc::SIGTERM },
            )
        };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, force);
        Err(denied("process group signalling unavailable"))
    }
}
pub(crate) fn terminate(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    signal_group(child.id(), false)?;
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    signal_group(child.id(), true)?;
    child.wait()?;
    Ok(())
}
