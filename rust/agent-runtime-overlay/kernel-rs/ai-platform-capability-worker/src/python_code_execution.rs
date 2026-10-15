//! Bounded Python execution owned by the capability-worker container.
//!
//! The Gateway never evaluates submitted source. This module starts one
//! short-lived Python process in a fresh workspace, applies kernel resource
//! limits before `exec`, and returns bytes to the caller. The caller must
//! persist output bytes through the Gateway artifact broker; this module has
//! no storage or provider credentials.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;

#[path = "python_supervisor.rs"]
mod supervisor;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
pub use supervisor::supervisor_main;

use ai_platform_capability_contract::RuntimeCapabilityLeaseV1;
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_CODE_BYTES: usize = 2_000_000;
const MAX_INPUT_BYTES: usize = 24 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 24 * 1024 * 1024;
const MAX_STREAM_BYTES: usize = 2 * 1024 * 1024;
const MAX_OUTPUT_FILES: usize = 64;
const MAX_TIMEOUT_SECONDS: u32 = 300;
const MAX_FILENAME_BYTES: usize = 255;
const MAX_WORKSPACE_ATTEMPTS: usize = 16;
const SCRIPT_NAME: &str = ".ai_platform_entrypoint.py";

const FORBIDDEN_MODULES: &[&str] = &[
    "subprocess",
    "socket",
    "pty",
    "posix",
    "ctypes",
    "_thread",
    "threading",
    "multiprocessing",
    "winreg",
    "signal",
    "importlib",
    "pkgutil",
    "runpy",
    "mmap",
    "resource",
    "fcntl",
    "termios",
    "tty",
    "ssl",
    "syslog",
];
const DANGEROUS_BUILTINS: &[&str] = &["eval", "exec", "compile", "__import__", "breakpoint"];
const DANGEROUS_CALLS: &[&str] = &[
    "system", "popen", "spawn", "fork", "kill", "chmod", "chown", "rmdir", "unlink", "remove",
    "execl", "execle", "execlp", "execv", "execve", "execvp", "execvpe",
];
const DANGEROUS_ATTRIBUTES: &[&str] = &[
    "__subclasses__",
    "__globals__",
    "__code__",
    "__bases__",
    "__mro__",
    "__builtins__",
    "__import__",
    "__class__",
];

#[derive(Debug, Error, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodeExecutionError {
    #[error("code is empty")]
    EmptyCode,
    #[error("code exceeds the {0} byte limit")]
    CodeTooLarge(usize),
    #[error("input attachments exceed the byte limit")]
    InputsTooLarge,
    #[error("static safety check failed: {0}")]
    UnsafeCode(String),
    #[error("arguments hash does not match code execution arguments")]
    ArgumentsHashMismatch,
    #[error("sandbox configuration is invalid")]
    Configuration,
    #[error("sandbox process could not be started")]
    ProcessStart,
    #[error("sandbox process timed out")]
    TimedOut,
    #[error("sandbox process was cancelled")]
    Cancelled,
    #[error("sandbox process exceeded an output limit")]
    OutputLimitExceeded,
    #[error("sandbox process failed")]
    ProcessFailed,
    #[error("sandbox result is malformed")]
    MalformedResult,
    #[error("sandbox execution outcome is unknown")]
    SideEffectUnknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CodeInputAttachment {
    pub filename: String,
    pub mime_type: Option<String>,
    pub content_base64: String,
    pub size_bytes: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PythonSandboxLimits {
    pub timeout_seconds: u32,
    pub memory_bytes: u64,
    pub cpu_millis: u32,
    pub pids: u32,
    pub stdout_bytes: usize,
    pub stderr_bytes: usize,
    pub output_bytes: usize,
    pub output_files: usize,
}

impl Default for PythonSandboxLimits {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            memory_bytes: 512 * 1024 * 1024,
            cpu_millis: 500,
            pids: 32,
            stdout_bytes: MAX_STREAM_BYTES,
            stderr_bytes: MAX_STREAM_BYTES,
            output_bytes: MAX_OUTPUT_BYTES,
            output_files: MAX_OUTPUT_FILES,
        }
    }
}

impl PythonSandboxLimits {
    pub fn validate(&self) -> Result<(), CodeExecutionError> {
        if self.timeout_seconds == 0
            || self.timeout_seconds > MAX_TIMEOUT_SECONDS
            || self.memory_bytes == 0
            || self.memory_bytes > 512 * 1024 * 1024
            || self.cpu_millis == 0
            || self.cpu_millis > 300_000
            || self.pids == 0
            || self.pids > 32
            || self.stdout_bytes == 0
            || self.stdout_bytes > MAX_STREAM_BYTES
            || self.stderr_bytes == 0
            || self.stderr_bytes > MAX_STREAM_BYTES
            || self.output_bytes == 0
            || self.output_bytes > MAX_OUTPUT_BYTES
            || self.output_files == 0
            || self.output_files > MAX_OUTPUT_FILES
        {
            return Err(CodeExecutionError::Configuration);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonCodeExecutionRequest {
    pub lease: RuntimeCapabilityLeaseV1,
    pub arguments_hash: String,
    pub code: String,
    #[serde(default)]
    pub inputs: Vec<CodeInputAttachment>,
    #[serde(default)]
    pub limits: PythonSandboxLimits,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CodeOutputArtifact {
    pub filename: String,
    pub mime_type: Option<String>,
    pub content_base64: String,
    pub size_bytes: usize,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PythonCodeExecutionResult {
    pub execution_id: String,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    #[serde(default)]
    pub output_files: Vec<CodeOutputArtifact>,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    pub error_message: Option<String>,
    /// `known` means the ephemeral process reached a terminal state. A
    /// transport/store timeout must be promoted by durable execution to
    /// `side_effect_unknown`, never blindly retried.
    pub side_effect_state: String,
}

#[derive(Clone, Debug)]
pub struct LocalPythonSandboxConfig {
    pub python_binary: PathBuf,
    pub workspace_root: PathBuf,
    /// The image is expected to provide a network-isolated child namespace.
    /// If the kernel cannot create it, execution fails closed.
    pub require_network_isolation: bool,
}

impl Default for LocalPythonSandboxConfig {
    fn default() -> Self {
        Self {
            python_binary: PathBuf::from("/usr/local/bin/python3"),
            workspace_root: PathBuf::from("/workspace"),
            require_network_isolation: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct LocalPythonSandboxBroker {
    config: LocalPythonSandboxConfig,
}

impl LocalPythonSandboxBroker {
    pub fn new(config: LocalPythonSandboxConfig) -> Result<Self, CodeExecutionError> {
        if !config.python_binary.is_absolute() || !config.workspace_root.is_absolute() {
            return Err(CodeExecutionError::Configuration);
        }
        fs::create_dir_all(&config.workspace_root)
            .map_err(|_| CodeExecutionError::Configuration)?;
        Ok(Self { config })
    }
}

#[async_trait]
pub trait PythonSandboxBroker: Send + Sync {
    async fn execute(
        &self,
        request: PythonCodeExecutionRequest,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError>;
}

#[async_trait]
impl PythonSandboxBroker for LocalPythonSandboxBroker {
    async fn execute(
        &self,
        request: PythonCodeExecutionRequest,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
        self.execute_cancellable(request, Arc::new(AtomicBool::new(false)))
            .await
    }
}

impl LocalPythonSandboxBroker {
    pub async fn execute_cancellable(
        &self,
        request: PythonCodeExecutionRequest,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
        validate_request(&request)?;
        if cancelled.load(Ordering::Acquire) {
            return Err(CodeExecutionError::Cancelled);
        }
        let config = self.config.clone();
        tokio::task::spawn_blocking(move || {
            if config.require_network_isolation {
                supervisor::run_isolated(&config, &request, &cancelled)
            } else {
                run_python_process(&config, &request, &cancelled)
            }
        })
        .await
        .map_err(|_| CodeExecutionError::SideEffectUnknown)?
    }
}

/// Conservative lexical guard. It is only an additional policy layer:
/// kernel limits, process isolation and the container boundary are the
/// security boundary.
pub fn audit_python_source(code: &str) -> Result<(), CodeExecutionError> {
    if code.trim().is_empty() {
        return Err(CodeExecutionError::EmptyCode);
    }
    if code.len() > MAX_CODE_BYTES {
        return Err(CodeExecutionError::CodeTooLarge(MAX_CODE_BYTES));
    }
    let tokens = tokenize_python(code)?;
    let forbidden: BTreeSet<&str> = FORBIDDEN_MODULES.iter().copied().collect();
    let dangerous_builtins: BTreeSet<&str> = DANGEROUS_BUILTINS.iter().copied().collect();
    let dangerous_calls: BTreeSet<&str> = DANGEROUS_CALLS.iter().copied().collect();
    let dangerous_attrs: BTreeSet<&str> = DANGEROUS_ATTRIBUTES.iter().copied().collect();
    for (index, token) in tokens.iter().enumerate() {
        if (token == "import" || token == "from")
            && tokens.get(index + 1).is_some_and(|module| {
                forbidden.contains(module.split('.').next().unwrap_or(module))
            })
        {
            return Err(CodeExecutionError::UnsafeCode(format!(
                "forbidden module '{}'",
                tokens[index + 1]
            )));
        }
        if dangerous_builtins.contains(token.as_str())
            && tokens.get(index + 1).is_some_and(|v| v == "(")
        {
            return Err(CodeExecutionError::UnsafeCode(format!(
                "dangerous builtin '{token}'"
            )));
        }
        let final_name = token.rsplit('.').next().unwrap_or(token);
        if dangerous_calls.contains(final_name) && tokens.get(index + 1).is_some_and(|v| v == "(") {
            return Err(CodeExecutionError::UnsafeCode(format!(
                "dangerous call '{token}()'"
            )));
        }
        if dangerous_attrs.contains(final_name) {
            return Err(CodeExecutionError::UnsafeCode(format!(
                "reflection attribute '{token}'"
            )));
        }
    }
    Ok(())
}

fn tokenize_python(source: &str) -> Result<Vec<String>, CodeExecutionError> {
    let mut tokens = Vec::new();
    let mut chars = source.chars().peekable();
    let mut delimiters = Vec::new();
    while let Some(character) = chars.next() {
        if character == '#' {
            for next in chars.by_ref() {
                if next == '\n' {
                    break;
                }
            }
            continue;
        }
        if character == '\'' || character == '"' {
            let quote = character;
            let triple = chars.peek() == Some(&quote);
            if triple {
                chars.next();
                chars.next();
            }
            let mut escaped = false;
            let mut closed = false;
            while let Some(next) = chars.next() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if next == '\\' {
                    escaped = true;
                    continue;
                }
                if next == quote {
                    if triple && chars.next() == Some(quote) && chars.next() == Some(quote) {
                        closed = true;
                        break;
                    }
                    if !triple {
                        closed = true;
                        break;
                    }
                }
            }
            if !closed {
                return Err(CodeExecutionError::UnsafeCode("unterminated string".into()));
            }
            continue;
        }
        if character.is_ascii_alphanumeric() || character == '_' {
            let mut word = character.to_string();
            while let Some(next) = chars.peek().copied() {
                if next.is_ascii_alphanumeric() || next == '_' || next == '.' {
                    word.push(next);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(word);
            continue;
        }
        if "()[]{}.;".contains(character) {
            tokens.push(character.to_string());
        }
        match character {
            '(' | '[' | '{' => delimiters.push(character),
            ')' => {
                if delimiters.pop() != Some('(') {
                    return Err(CodeExecutionError::UnsafeCode(
                        "unbalanced delimiters".into(),
                    ));
                }
            }
            ']' => {
                if delimiters.pop() != Some('[') {
                    return Err(CodeExecutionError::UnsafeCode(
                        "unbalanced delimiters".into(),
                    ));
                }
            }
            '}' => {
                if delimiters.pop() != Some('{') {
                    return Err(CodeExecutionError::UnsafeCode(
                        "unbalanced delimiters".into(),
                    ));
                }
            }
            _ => {}
        }
    }
    if delimiters.is_empty() {
        Ok(tokens)
    } else {
        Err(CodeExecutionError::UnsafeCode(
            "unbalanced delimiters".into(),
        ))
    }
}

pub fn validate_request(request: &PythonCodeExecutionRequest) -> Result<(), CodeExecutionError> {
    if request.lease.capability_id != "execute_python_code"
        || request.arguments_hash != request.lease.arguments_hash
    {
        return Err(CodeExecutionError::ArgumentsHashMismatch);
    }
    audit_python_source(&request.code)?;
    request.limits.validate()?;
    let input_bytes = request.inputs.iter().try_fold(0usize, |total, input| {
        total
            .checked_add(input.size_bytes)
            .ok_or(CodeExecutionError::InputsTooLarge)
    })?;
    if input_bytes > MAX_INPUT_BYTES || request.inputs.len() > MAX_OUTPUT_FILES {
        return Err(CodeExecutionError::InputsTooLarge);
    }
    for input in &request.inputs {
        validate_filename(&input.filename)?;
        let bytes = STANDARD
            .decode(&input.content_base64)
            .map_err(|_| CodeExecutionError::InputsTooLarge)?;
        if bytes.len() != input.size_bytes {
            return Err(CodeExecutionError::InputsTooLarge);
        }
        if input
            .mime_type
            .as_ref()
            .is_some_and(|mime| mime.is_empty() || mime.bytes().any(|byte| byte.is_ascii_control()))
        {
            return Err(CodeExecutionError::InputsTooLarge);
        }
    }
    Ok(())
}

fn validate_filename(filename: &str) -> Result<(), CodeExecutionError> {
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.len() > MAX_FILENAME_BYTES
        || filename.contains('/')
        || filename.contains('\\')
        || filename
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(CodeExecutionError::InputsTooLarge);
    }
    Ok(())
}

fn run_python_process(
    config: &LocalPythonSandboxConfig,
    request: &PythonCodeExecutionRequest,
    cancelled: &AtomicBool,
) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
    let workspace = create_workspace(&config.workspace_root)?;
    let started = Instant::now();
    let result = (|| {
        fs::create_dir(workspace.join(".proc")).map_err(|_| CodeExecutionError::ProcessStart)?;
        fs::create_dir(workspace.join(".mask")).map_err(|_| CodeExecutionError::ProcessStart)?;
        fs::create_dir(workspace.join("output")).map_err(|_| CodeExecutionError::ProcessStart)?;
        fs::create_dir(workspace.join("input")).map_err(|_| CodeExecutionError::ProcessStart)?;
        for input in &request.inputs {
            let path = workspace.join("input").join(&input.filename);
            let bytes = STANDARD
                .decode(&input.content_base64)
                .map_err(|_| CodeExecutionError::InputsTooLarge)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|_| CodeExecutionError::ProcessStart)?;
            file.write_all(&bytes)
                .map_err(|_| CodeExecutionError::ProcessStart)?;
        }
        supervisor::read_only_input(&workspace.join("input"))?;
        let script = workspace.join(SCRIPT_NAME);
        let mut script_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&script)
            .map_err(|_| CodeExecutionError::ProcessStart)?;
        script_file
            .write_all(request.code.as_bytes())
            .map_err(|_| CodeExecutionError::ProcessStart)?;
        drop(script_file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o500))
                .map_err(|_| CodeExecutionError::ProcessStart)?;
        }

        let mut command = python_command(config, request.limits.pids);
        command
            .arg("-I")
            .arg("-S")
            .arg("-B")
            .arg(SCRIPT_NAME)
            .current_dir(&workspace)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONDONTWRITEBYTECODE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_child(
            &mut command,
            &request.limits,
            config.require_network_isolation,
        )?;
        let mut child = command
            .spawn()
            .map_err(|_| CodeExecutionError::ProcessStart)?;
        let stdout = child
            .stdout
            .take()
            .ok_or(CodeExecutionError::ProcessStart)?;
        let stderr = child
            .stderr
            .take()
            .ok_or(CodeExecutionError::ProcessStart)?;
        let stdout_limit = request.limits.stdout_bytes;
        let stderr_limit = request.limits.stderr_bytes;
        let out_thread = thread::spawn(move || read_capped(stdout, stdout_limit));
        let err_thread = thread::spawn(move || read_capped(stderr, stderr_limit));
        let timeout = Duration::from_secs(u64::from(request.limits.timeout_seconds));
        let mut timed_out = false;
        let mut was_cancelled = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if cancelled.load(Ordering::Acquire) || started.elapsed() >= timeout => {
                    was_cancelled = cancelled.load(Ordering::Acquire);
                    timed_out = !was_cancelled;
                    kill_process_group(&mut child);
                    break child
                        .wait()
                        .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
                }
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    kill_process_group(&mut child);
                    return Err(CodeExecutionError::SideEffectUnknown);
                }
            }
        };
        let (stdout, stdout_limited) = out_thread
            .join()
            .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
        let (stderr, stderr_limited) = err_thread
            .join()
            .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
        if was_cancelled {
            return Err(CodeExecutionError::Cancelled);
        }
        if timed_out {
            return Err(CodeExecutionError::TimedOut);
        }
        if stdout_limited || stderr_limited {
            return Err(CodeExecutionError::OutputLimitExceeded);
        }
        let output_files = collect_output_files(&workspace, &request.limits)?;
        let success = status.success();
        Ok(PythonCodeExecutionResult {
            execution_id: request.lease.lease_id.clone(),
            status: if success {
                "succeeded".into()
            } else {
                "failed".into()
            },
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            output_files,
            duration_ms: started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            exit_code: status.code(),
            error_message: (!success).then(|| "python process exited unsuccessfully".into()),
            side_effect_state: "known".into(),
        })
    })();
    let _ = fs::remove_dir_all(&workspace);
    result
}

fn create_workspace(root: &Path) -> Result<PathBuf, CodeExecutionError> {
    for attempt in 0..MAX_WORKSPACE_ATTEMPTS {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = root.join(format!(
            ".python-{}-{}",
            std::process::id(),
            nonce + attempt as u128
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(CodeExecutionError::ProcessStart),
        }
    }
    Err(CodeExecutionError::ProcessStart)
}

fn read_capped<R: Read>(mut reader: R, limit: usize) -> (Vec<u8>, bool) {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut exceeded = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => {
                let remaining = limit.saturating_sub(bytes.len());
                bytes.extend_from_slice(&buffer[..size.min(remaining)]);
                if size > remaining {
                    exceeded = true;
                    break;
                }
            }
            Err(_) => break,
        }
    }
    (bytes, exceeded)
}

fn collect_output_files(
    workspace: &Path,
    limits: &PythonSandboxLimits,
) -> Result<Vec<CodeOutputArtifact>, CodeExecutionError> {
    let mut files = Vec::new();
    let mut total = 0usize;
    let mut names = BTreeSet::new();
    for entry in
        fs::read_dir(workspace.join("output")).map_err(|_| CodeExecutionError::MalformedResult)?
    {
        let entry = entry.map_err(|_| CodeExecutionError::MalformedResult)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        collect_output_file(
            entry.path(),
            name,
            limits,
            &mut files,
            &mut total,
            &mut names,
        )?;
    }

    Ok(files)
}

fn collect_output_file(
    path: PathBuf,
    name: String,
    limits: &PythonSandboxLimits,
    files: &mut Vec<CodeOutputArtifact>,
    total: &mut usize,
    names: &mut BTreeSet<String>,
) -> Result<(), CodeExecutionError> {
    validate_filename(&name)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| CodeExecutionError::MalformedResult)?;
    if !metadata.is_file() || !names.insert(name.clone()) {
        return Err(CodeExecutionError::MalformedResult);
    }
    let size =
        usize::try_from(metadata.len()).map_err(|_| CodeExecutionError::OutputLimitExceeded)?;
    *total = (*total)
        .checked_add(size)
        .ok_or(CodeExecutionError::OutputLimitExceeded)?;
    if files.len() >= limits.output_files || *total > limits.output_bytes {
        return Err(CodeExecutionError::OutputLimitExceeded);
    }
    let mut content = Vec::with_capacity(size);
    File::open(path)
        .map_err(|_| CodeExecutionError::MalformedResult)?
        .read_to_end(&mut content)
        .map_err(|_| CodeExecutionError::MalformedResult)?;
    files.push(CodeOutputArtifact {
        filename: name,
        mime_type: None,
        content_base64: STANDARD.encode(&content),
        size_bytes: content.len(),
        sha256: hex::encode(Sha256::digest(&content)),
    });
    Ok(())
}

#[cfg(unix)]
fn configure_child(
    command: &mut Command,
    limits: &PythonSandboxLimits,
    require_network_isolation: bool,
) -> Result<(), CodeExecutionError> {
    use std::os::unix::process::CommandExt;
    let cpu_seconds = u64::from(limits.cpu_millis).div_ceil(1000).max(1);
    let memory = limits.memory_bytes;
    let pids = u64::from(limits.pids);
    let file_bytes = limits.output_bytes as u64;
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if !require_network_isolation
                && libc::setrlimit(
                    libc::RLIMIT_NPROC,
                    &libc::rlimit {
                        rlim_cur: pids,
                        rlim_max: pids,
                    },
                ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            if libc::setrlimit(
                libc::RLIMIT_CPU,
                &libc::rlimit {
                    rlim_cur: cpu_seconds,
                    rlim_max: cpu_seconds + 1,
                },
            ) != 0
                || libc::setrlimit(
                    libc::RLIMIT_AS,
                    &libc::rlimit {
                        rlim_cur: memory,
                        rlim_max: memory,
                    },
                ) != 0
                || libc::setrlimit(
                    libc::RLIMIT_RSS,
                    &libc::rlimit {
                        rlim_cur: memory,
                        rlim_max: memory,
                    },
                ) != 0
                || libc::setrlimit(
                    libc::RLIMIT_FSIZE,
                    &libc::rlimit {
                        rlim_cur: file_bytes,
                        rlim_max: file_bytes,
                    },
                ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            #[cfg(target_os = "linux")]
            {
                supervisor::restrict_python_process()?;
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    Ok(())
}

fn python_command(config: &LocalPythonSandboxConfig, pids: u32) -> Command {
    #[cfg(target_os = "linux")]
    if config.require_network_isolation {
        let mut command = Command::new("/usr/bin/unshare");
        command
            .arg("--user")
            .arg("--map-root-user")
            .arg("--net")
            .arg("--mount")
            .arg("--pid")
            .arg("--fork")
            .arg("--")
            .arg("/bin/sh")
            .arg("-c")
            .arg(
                "mount --make-rprivate / && mount -t tmpfs -o size=4k,mode=0555,nosuid,nodev,noexec tmpfs \"$PWD/.proc\" && mount --bind \"$PWD/.proc\" /proc && mount -o remount,bind,ro /proc && mount -t tmpfs -o size=4k,mode=0555,nosuid,nodev,noexec tmpfs \"$PWD/.mask\" && for target in /tmp /var/tmp /run/lock /dev/shm /dev/mqueue; do mount --bind \"$PWD/.mask\" \"$target\" && mount -o remount,bind,ro \"$target\" || exit 1; done && mount --bind \"$PWD\" /workspace && mount -o remount,rw,bind /workspace && cd /workspace && pids=\"$1\" && shift && exec /usr/bin/prlimit --nproc=\"$pids\" -- \"$@\"",
            )
            .arg("python-sandbox")
            .arg(pids.to_string())
            .arg(&config.python_binary);
        return command;
    }
    Command::new(&config.python_binary)
}

#[cfg(not(unix))]
fn configure_child(
    _command: &mut Command,
    _limits: &PythonSandboxLimits,
    require_network_isolation: bool,
) -> Result<(), CodeExecutionError> {
    if require_network_isolation {
        Err(CodeExecutionError::Configuration)
    } else {
        Ok(())
    }
}

fn kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        let pid = child.id() as libc::pid_t;
        let _ = libc::kill(-pid, libc::SIGKILL);
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lexical_guard_is_not_fooled_by_literals() {
        assert!(audit_python_source("print('subprocess __globals__')").is_ok());
        assert!(audit_python_source("import subprocess").is_err());
        assert!(audit_python_source("x.__subclasses__()").is_err());
    }
    #[test]
    fn request_rejects_paths_and_unbounded_limits() {
        let mut limits = PythonSandboxLimits::default();
        limits.output_files = MAX_OUTPUT_FILES + 1;
        assert!(limits.validate().is_err());
        assert!(validate_filename("../escape").is_err());
    }

    #[test]
    fn empty_limits_object_uses_bounded_defaults() {
        let limits: PythonSandboxLimits = serde_json::from_value(json!({})).unwrap();
        assert_eq!(limits, PythonSandboxLimits::default());
        assert!(limits.validate().is_ok());
    }

    #[test]
    fn linux_network_isolation_uses_an_unprivileged_namespace_wrapper() {
        let command = python_command(&LocalPythonSandboxConfig::default(), 32);
        #[cfg(target_os = "linux")]
        {
            assert_eq!(command.get_program(), "/usr/bin/unshare");
            let arguments = command
                .get_args()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                arguments,
                [
                    "--user",
                    "--map-root-user",
                    "--net",
                    "--mount",
                    "--pid",
                    "--fork",
                    "--",
                    "/bin/sh",
                    "-c",
                    "mount --make-rprivate / && mount -t tmpfs -o size=4k,mode=0555,nosuid,nodev,noexec tmpfs \"$PWD/.proc\" && mount --bind \"$PWD/.proc\" /proc && mount -o remount,bind,ro /proc && mount -t tmpfs -o size=4k,mode=0555,nosuid,nodev,noexec tmpfs \"$PWD/.mask\" && for target in /tmp /var/tmp /run/lock /dev/shm /dev/mqueue; do mount --bind \"$PWD/.mask\" \"$target\" && mount -o remount,bind,ro \"$target\" || exit 1; done && mount --bind \"$PWD\" /workspace && mount -o remount,rw,bind /workspace && cd /workspace && pids=\"$1\" && shift && exec /usr/bin/prlimit --nproc=\"$pids\" -- \"$@\"",
                    "python-sandbox",
                    "32",
                    "/usr/local/bin/python3",
                ]
            );
        }
    }

    #[test]
    fn collects_files_from_the_compatibility_output_directory() {
        let workspace = create_workspace(&std::env::temp_dir()).unwrap();
        let output = workspace.join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("result.txt"), b"result").unwrap();
        let files = collect_output_files(&workspace, &PythonSandboxLimits::default()).unwrap();
        fs::remove_dir_all(&workspace).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].filename, "result.txt");
        assert_eq!(
            STANDARD.decode(&files[0].content_base64).unwrap(),
            b"result"
        );
    }
}
