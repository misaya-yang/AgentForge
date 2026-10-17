//! Trusted namespace supervisor. User code never holds mount capabilities.
use super::*;
const MAX_MESSAGE: usize = 40 * 1024 * 1024;

pub fn supervisor_main() -> Result<(), CodeExecutionError> {
    let (bytes, exceeded) = read_capped(io::stdin(), MAX_MESSAGE);
    if exceeded {
        return Err(CodeExecutionError::InputsTooLarge);
    }
    let request: PythonCodeExecutionRequest =
        serde_json::from_slice(&bytes).map_err(|_| CodeExecutionError::MalformedResult)?;
    validate_request(&request)?;
    let result = (|| {
        let cgroup_path = std::env::args_os()
            .nth(2)
            .map(PathBuf::from)
            .ok_or(CodeExecutionError::IsolationUnavailable)?;
        isolation::verify_supervised(&cgroup_path, &request.limits)?;
        mount_workspace(&request)?;
        let package_roots: Vec<PathBuf> = std::env::args_os()
            .nth(3)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| CodeExecutionError::Configuration)
            })
            .transpose()?
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| CodeExecutionError::Configuration)
            })
            .transpose()?
            .unwrap_or_default();
        if package_roots.len() > 64
            || package_roots
                .iter()
                .any(|path| crate::python_package_policy::validate_package_root(path).is_err())
        {
            return Err(CodeExecutionError::Configuration);
        }
        let config = LocalPythonSandboxConfig {
            package_roots,
            python_binary: std::env::args_os()
                .nth(1)
                .map(PathBuf::from)
                .ok_or(CodeExecutionError::Configuration)?,
            ..LocalPythonSandboxConfig::default()
        };
        run_python_process(&config, &request, &AtomicBool::new(false))
    })();
    // stdout/stderr of Python were captured by the supervisor. Only this
    // trusted serializer can emit the parent's control message.
    serde_json::to_writer(io::stdout(), &result).map_err(|_| CodeExecutionError::MalformedResult)
}

pub(super) fn run_isolated(
    config: &LocalPythonSandboxConfig,
    request: &PythonCodeExecutionRequest,
    cancelled: &AtomicBool,
) -> Result<PythonCodeExecutionResult, CodeExecutionError> {
    isolation::available(config)?;
    #[cfg(target_os = "linux")]
    let cgroup = isolation::CgroupJob::create(config, &request.limits)
        .map_err(|_| CodeExecutionError::IsolationUnavailable)?;
    #[cfg(target_os = "linux")]
    let cgroup_fd = cgroup.join_fd();
    let bytes = serde_json::to_vec(request).map_err(|_| CodeExecutionError::MalformedResult)?;
    if bytes.len() > MAX_MESSAGE {
        return Err(CodeExecutionError::InputsTooLarge);
    }
    let mut command = Command::new("/usr/bin/unshare");
    command
        .args([
            "--user",
            "--map-root-user",
            "--net",
            "--mount",
            "--pid",
            "--fork",
            "--kill-child=SIGKILL",
            "--",
        ])
        .arg("/usr/local/bin/ai-platform-python-supervisor")
        .arg(&config.python_binary)
        .arg({
            #[cfg(target_os = "linux")]
            {
                cgroup.path()
            }
            #[cfg(not(target_os = "linux"))]
            {
                Path::new("/unavailable")
            }
        })
        .arg(
            serde_json::to_string(&config.package_roots)
                .map_err(|_| CodeExecutionError::Configuration)?,
        )
        .current_dir(&config.workspace_root)
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0
                        || libc::write(cgroup_fd, b"0\n".as_ptr().cast(), 2) != 2
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
    let mut child = command
        .spawn()
        .map_err(|_| CodeExecutionError::ProcessStart)?;
    let stdout = child
        .stdout
        .take()
        .ok_or(CodeExecutionError::ProcessStart)?;
    let reader = thread::spawn(move || read_capped(stdout, MAX_MESSAGE));
    let written = child
        .stdin
        .take()
        .ok_or(CodeExecutionError::ProcessStart)?
        .write_all(&bytes);
    if written.is_err() {
        kill_process_group(&mut child);
    }
    let started = Instant::now();
    let timeout = Duration::from_secs(u64::from(request.limits.timeout_seconds) + 3);
    let mut failure = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if cancelled.load(Ordering::Acquire) || started.elapsed() >= timeout {
                    failure = Some(if cancelled.load(Ordering::Acquire) {
                        CodeExecutionError::Cancelled
                    } else {
                        CodeExecutionError::TimedOut
                    });
                    kill_process_group(&mut child);
                    break child
                        .wait()
                        .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                kill_process_group(&mut child);
                let _ = child.wait();
                failure = Some(CodeExecutionError::SideEffectUnknown);
                break child
                    .wait()
                    .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
            }
        }
    };
    let (result, exceeded) = reader
        .join()
        .map_err(|_| CodeExecutionError::SideEffectUnknown)?;
    if let Some(error) = failure {
        return Err(error);
    }
    if exceeded {
        return Err(CodeExecutionError::OutputLimitExceeded);
    }
    #[cfg(target_os = "linux")]
    if cgroup
        .oom_killed()
        .map_err(|_| CodeExecutionError::SideEffectUnknown)?
    {
        return Err(CodeExecutionError::ResourceLimitExceeded);
    }
    if !status.success() || written.is_err() {
        return Err(CodeExecutionError::ProcessFailed);
    }
    serde_json::from_slice(&result).map_err(|_| CodeExecutionError::MalformedResult)?
}

#[cfg(target_os = "linux")]
fn mount(
    source: &str,
    target: &str,
    kind: &str,
    flags: libc::c_ulong,
    options: &str,
) -> Result<(), CodeExecutionError> {
    use std::ffi::CString;
    let values: Vec<CString> = [source, target, kind, options]
        .into_iter()
        .map(|v| CString::new(v).map_err(|_| CodeExecutionError::Configuration))
        .collect::<Result<_, _>>()?;
    if unsafe {
        libc::mount(
            values[0].as_ptr(),
            values[1].as_ptr(),
            if kind.is_empty() {
                std::ptr::null()
            } else {
                values[2].as_ptr()
            },
            flags,
            values[3].as_ptr().cast(),
        )
    } != 0
    {
        return Err(CodeExecutionError::ProcessStart);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn mount_workspace(request: &PythonCodeExecutionRequest) -> Result<(), CodeExecutionError> {
    mount("", "/", "", libc::MS_REC | libc::MS_PRIVATE, "")?;
    let input_bytes: usize = request.inputs.iter().map(|input| input.size_bytes).sum();
    let size = input_bytes + request.code.len() + request.limits.output_bytes + 1024 * 1024;
    let inodes = request.inputs.len() + request.limits.output_files + 32;
    mount(
        "tmpfs",
        "/workspace",
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        &format!("size={size},nr_inodes={inodes},mode=0700"),
    )?;
    fs::create_dir("/workspace/.mask").map_err(|_| CodeExecutionError::ProcessStart)?;
    mount(
        "tmpfs",
        "/workspace/.mask",
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
        "size=4096,nr_inodes=2,mode=0555",
    )?;
    for target in [
        "/proc",
        "/tmp",
        "/var/tmp",
        "/run/lock",
        "/dev/shm",
        "/dev/mqueue",
    ] {
        mount("/workspace/.mask", target, "", libc::MS_BIND, "")?;
        mount(
            "",
            target,
            "",
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
            "",
        )?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn mount_workspace(_request: &PythonCodeExecutionRequest) -> Result<(), CodeExecutionError> {
    Err(CodeExecutionError::Configuration)
}

pub(super) fn read_only_input(path: &Path) -> Result<(), CodeExecutionError> {
    #[cfg(target_os = "linux")]
    {
        let path = path.to_str().ok_or(CodeExecutionError::Configuration)?;
        mount(path, path, "", libc::MS_BIND, "")?;
        mount(
            "",
            path,
            "",
            libc::MS_REMOUNT | libc::MS_BIND | libc::MS_RDONLY,
            "",
        )?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn restrict_python_process(policy: &isolation::ChildPolicy) -> io::Result<()> {
    // Namespace root must not retain the ability to enlarge its tmpfs quota.
    #[repr(C)]
    struct CapHeader {
        version: u32,
        pid: i32,
    }
    #[repr(C)]
    struct CapData {
        effective: u32,
        permitted: u32,
        inheritable: u32,
    }
    unsafe {
        if libc::prctl(libc::PR_SET_SECUREBITS, 15, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        for capability in 0..64 {
            if libc::prctl(libc::PR_CAPBSET_READ, capability, 0, 0, 0) == 1
                && libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) != 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        let header = CapHeader {
            version: 0x20080522,
            pid: 0,
        };
        let data = [
            CapData {
                effective: 0,
                permitted: 0,
                inheritable: 0,
            },
            CapData {
                effective: 0,
                permitted: 0,
                inheritable: 0,
            },
        ];
        if libc::syscall(libc::SYS_capset, &header, data.as_ptr()) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    policy.apply()
}
