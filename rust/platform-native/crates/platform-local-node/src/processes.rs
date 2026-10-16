//! Allowlisted argv execution. Network denial fails closed without a sandbox backend.
use crate::{
    Error, Result, digest,
    envelope::{Action, ApprovalVerifier, PlatformVerifier},
    ledger::Ledger,
    workspace::Grant,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    os::{
        fd::AsRawFd,
        unix::{fs::MetadataExt, process::CommandExt},
    },
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt};
pub struct ProcessPolicy {
    pub allowed_executables: HashSet<PathBuf>,
    pub allowed_env_names: HashSet<String>,
    pub path: String,
    pub max_timeout_seconds: f64,
    pub max_output_bytes: usize,
    pub allow_inherited_network: bool,
}
impl Default for ProcessPolicy {
    fn default() -> Self {
        Self {
            allowed_executables: HashSet::new(),
            allowed_env_names: HashSet::new(),
            path: "/usr/bin:/bin".into(),
            max_timeout_seconds: 300.,
            max_output_bytes: 2 * 1024 * 1024,
            allow_inherited_network: false,
        }
    }
}
#[derive(Serialize, Deserialize)]
pub struct ProcessRequest {
    pub argv: Vec<String>,
    pub grant_id: String,
    pub cwd: String,
    pub timeout_seconds: f64,
    pub env: BTreeMap<String, String>,
    pub network_policy: String,
}
impl ProcessRequest {
    pub fn arguments(&self) -> serde_json::Value {
        json!({"argv":self.argv,"grant_id":self.grant_id,"cwd":self.cwd,"timeout_seconds":self.timeout_seconds,"env_names":self.env.keys().collect::<Vec<_>>(),"network_policy":self.network_policy})
    }
}
#[derive(Serialize, Deserialize)]
pub struct ProcessResult {
    pub action_id: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub started_at: f64,
    pub ended_at: f64,
    pub output_truncated: bool,
    pub error_code: Option<String>,
}
#[derive(Clone, Default)]
pub struct ProcessControl {
    running: Arc<Mutex<HashMap<String, i32>>>,
    cancelled: Arc<Mutex<HashSet<String>>>,
}
impl ProcessControl {
    pub fn cancel(&self, id: &str) -> bool {
        self.cancelled.lock().unwrap().insert(id.into());
        if let Some(pid) = self.running.lock().unwrap().get(id) {
            unsafe { libc::kill(-*pid, libc::SIGTERM) };
            true
        } else {
            false
        }
    }
    pub fn cancel_all(&self) -> Vec<String> {
        let ids: Vec<_> = self.running.lock().unwrap().keys().cloned().collect();
        for id in &ids {
            self.cancel(id);
        }
        ids
    }
}
pub struct ProcessRunner {
    pub policy: ProcessPolicy,
    pub control: ProcessControl,
}
pub type OutputCallback = Arc<dyn Fn(&str, &[u8]) + Send + Sync>;
async fn collect<R: AsyncRead + Unpin>(
    mut stream: R,
    used: Arc<AtomicUsize>,
    max: usize,
    channel: &str,
    callback: Option<OutputCallback>,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 65536];
    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let old = used.fetch_add(n, Ordering::Relaxed);
        let keep = n.min(max.saturating_sub(old));
        out.extend_from_slice(&buf[..keep]);
        if keep > 0 {
            if let Some(callback) = &callback {
                callback(channel, &buf[..keep]);
            }
        }
        truncated |= keep < n;
    }
    Ok((out, truncated))
}
impl ProcessRunner {
    pub async fn run(
        &self,
        request: &ProcessRequest,
        g: &Grant,
        a: &Action,
        ledger: &mut Ledger,
        platform: &dyn PlatformVerifier,
        approval: &dyn ApprovalVerifier,
    ) -> Result<ProcessResult> {
        self.run_with_output(request, g, a, ledger, platform, approval, None)
            .await
    }
    pub async fn run_with_output(
        &self,
        request: &ProcessRequest,
        g: &Grant,
        a: &Action,
        ledger: &mut Ledger,
        platform: &dyn PlatformVerifier,
        approval: &dyn ApprovalVerifier,
        callback: Option<OutputCallback>,
    ) -> Result<ProcessResult> {
        if a.capability != "process.run"
            || a.operation != "process.run"
            || request.argv.is_empty()
            || request.argv.iter().any(|s| s.contains('\0'))
            || !request.timeout_seconds.is_finite()
            || request.timeout_seconds <= 0.
            || request.timeout_seconds > self.policy.max_timeout_seconds
        {
            return Err(Error::Denied("process request policy"));
        }
        if request.network_policy != "inherit" || !self.policy.allow_inherited_network {
            return Err(Error::Denied(
                "network-denied execution requires sandbox backend",
            ));
        }
        a.validate(
            &request.arguments(),
            &request.grant_id,
            &[request.grant_id.clone(), request.cwd.clone()],
            platform,
        )?;
        if request.grant_id != g.id {
            return Err(Error::Denied("process grant"));
        }
        g.authorize("read", &a.tenant_id, &a.user_id)?;
        let supplied = &request.argv[0];
        let candidate = if supplied.contains('/') {
            PathBuf::from(supplied)
        } else {
            self.policy
                .path
                .split(':')
                .map(|p| PathBuf::from(p).join(supplied))
                .find(|p| p.is_file())
                .ok_or(Error::Denied("executable not found"))?
        }
        .canonicalize()?;
        let allowed = self
            .policy
            .allowed_executables
            .iter()
            .filter_map(|p| p.canonicalize().ok())
            .any(|p| p == candidate);
        if !allowed {
            return Err(Error::Denied("executable allowlist"));
        }
        let mut env = BTreeMap::from([
            ("PATH".into(), self.policy.path.clone()),
            ("LANG".into(), "C.UTF-8".into()),
            ("LC_ALL".into(), "C.UTF-8".into()),
        ]);
        for (k, v) in &request.env {
            if !self.policy.allowed_env_names.contains(k)
                || k.contains(['\0', '='])
                || v.contains('\0')
                || [
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
                .any(|part| k.to_uppercase().contains(part))
            {
                return Err(Error::Denied("process environment"));
            }
            env.insert(k.clone(), v.clone());
        }
        let cwd = g.open_dir(&request.cwd)?;
        let info = cwd.metadata()?;
        let target = digest(
            &json!({"grant_id":g.id,"cwd":request.cwd,"device":info.dev(),"inode":info.ino()}),
        );
        let (created, record) = ledger.begin(a, platform)?;
        if !created {
            return serde_json::from_value(record.result.ok_or(Error::Conflict)?)
                .map_err(|_| Error::Conflict);
        }
        let started = crate::now();
        let result = async {
            ledger.transition(&a.action_id, "awaiting_approval", None)?;
            a.approve(&target, approval)?;
            ledger.transition(&a.action_id, "dispatched", None)?;
            let mut cmd = std::process::Command::new(candidate);
            cmd.args(&request.argv[1..])
                .env_clear()
                .envs(env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let fd = cwd.as_raw_fd();
            unsafe {
                cmd.pre_exec(move || {
                    if libc::fchdir(fd) < 0 || libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            let mut command = tokio::process::Command::from(cmd);
            command.kill_on_drop(true);
            let mut child = command.spawn()?;
            let pid = child.id().ok_or(Error::Storage)? as i32;
            self.control
                .running
                .lock()
                .unwrap()
                .insert(a.action_id.clone(), pid);
            ledger.transition(&a.action_id, "running", None)?;
            let stdout = child.stdout.take().ok_or(Error::Storage)?;
            let stderr = child.stderr.take().ok_or(Error::Storage)?;
            let used = Arc::new(AtomicUsize::new(0));
            let gathered =
                tokio::time::timeout(Duration::from_secs_f64(request.timeout_seconds), async {
                    tokio::try_join!(
                        child.wait(),
                        collect(
                            stdout,
                            used.clone(),
                            self.policy.max_output_bytes,
                            "stdout",
                            callback.clone()
                        ),
                        collect(
                            stderr,
                            used,
                            self.policy.max_output_bytes,
                            "stderr",
                            callback
                        )
                    )
                })
                .await;
            let cancelled = self
                .control
                .cancelled
                .lock()
                .unwrap()
                .contains(&a.action_id);
            let (status, exit, out, err, truncated, error) = match gathered {
                Ok(Ok((exit, (out, t1), (err, t2)))) => (
                    if cancelled { "cancelled" } else { "succeeded" },
                    exit.code(),
                    out,
                    err,
                    t1 || t2,
                    if cancelled { Some("cancelled") } else { None },
                ),
                other => {
                    unsafe { libc::kill(-pid, libc::SIGTERM) };
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    unsafe { libc::kill(-pid, libc::SIGKILL) };
                    let exit = child.wait().await.ok().and_then(|s| s.code());
                    (
                        "failed",
                        exit,
                        Vec::new(),
                        Vec::new(),
                        false,
                        Some(if other.is_err() {
                            "timeout"
                        } else {
                            "process_failed"
                        }),
                    )
                }
            };
            self.control.running.lock().unwrap().remove(&a.action_id);
            Ok(ProcessResult {
                action_id: a.action_id.clone(),
                status: status.into(),
                exit_code: exit,
                stdout: String::from_utf8_lossy(&out).into_owned(),
                stderr: String::from_utf8_lossy(&err).into_owned(),
                started_at: started,
                ended_at: crate::now(),
                output_truncated: truncated,
                error_code: error.map(str::to_owned),
            })
        }
        .await;
        match result {
            Ok(r) => {
                ledger.transition(
                    &a.action_id,
                    &r.status,
                    Some(serde_json::to_value(&r).map_err(|_| Error::Protocol)?),
                )?;
                Ok(r)
            }
            Err(e) => {
                ledger.transition(
                    &a.action_id,
                    "failed",
                    Some(json!({"error_code":"process_failed"})),
                )?;
                Err(e)
            }
        }
    }
}
