use super::*;
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

const LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const LANDLOCK_ADD_RULE: libc::c_long = 445;
const LANDLOCK_RESTRICT_SELF: libc::c_long = 446;
const READ_FILE: u64 = 1 << 2;
const READ_DIR: u64 = 1 << 3;
const EXECUTE: u64 = 1;
const WRITE_FILE: u64 = 1 << 1;
const CGROUP2_MAGIC: libc::c_long = 0x63677270;

fn path_string(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

fn landlock_abi() -> io::Result<u32> {
    let abi = unsafe { libc::syscall(LANDLOCK_CREATE_RULESET, std::ptr::null::<u8>(), 0, 1) };
    if abi < 3 {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    Ok(abi as u32)
}

pub(super) fn probe(config: &LocalPythonSandboxConfig) -> io::Result<()> {
    landlock_abi()?;
    if unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return Err(io::Error::from(io::ErrorKind::Unsupported));
    if !config.python_binary.is_file()
        || !Path::new("/usr/bin/unshare").is_file()
        || !Path::new("/usr/local/bin/ai-platform-python-supervisor").is_file()
    {
        return Err(io::Error::from(io::ErrorKind::NotFound));
    }
    let root = fs::canonicalize(&config.cgroup_root)?;
    let path = path_string(&root)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { stat.assume_init() }.f_type as libc::c_long != CGROUP2_MAGIC {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    // Operators must explicitly delegate and enable these controllers. The
    // worker never enables a host controller or moves a host service itself.
    let controllers = fs::read_to_string(root.join("cgroup.subtree_control"))?;
    if ["memory", "pids", "cpu"]
        .iter()
        .any(|name| !controllers.split_whitespace().any(|value| value == *name))
    {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    if unsafe { libc::access(path.as_ptr(), libc::W_OK | libc::X_OK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // This is only a non-mutating preflight. Per-job controller writes below
    // are mandatory and fail closed even if delegation changed after probe.
    Ok(())
}

pub(super) fn verify_supervised(path: &Path, limits: &PythonSandboxLimits) -> io::Result<()> {
    let path_string = path_string(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path_string.as_ptr(), stat.as_mut_ptr()) } != 0
        || unsafe { stat.assume_init() }.f_type as libc::c_long != CGROUP2_MAGIC
    {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    let number = |name: &str| -> io::Result<u64> {
        fs::read_to_string(path.join(name))?
            .trim()
            .parse()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
    };
    let memory = number("memory.max")?;
    let pids = number("pids.max")?;
    if memory == 0
        || memory > limits.memory_bytes
        || pids == 0
        || pids > u64::from(limits.pids)
        || number("memory.swap.max")? != 0
        || number("memory.oom.group")? != 1
        || fs::read_to_string(path.join("cpu.max"))?
            .split_whitespace()
            .collect::<Vec<_>>()
            != ["100000", "100000"]
    {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    let own_pid = std::process::id().to_string();
    if !fs::read_to_string(path.join("cgroup.procs"))?
        .lines()
        .any(|line| line == own_pid)
    {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    Ok(())
}

pub(crate) struct CgroupJob {
    path: PathBuf,
    join: File,
}
impl CgroupJob {
    pub(crate) fn create(
        config: &LocalPythonSandboxConfig,
        limits: &PythonSandboxLimits,
    ) -> io::Result<Self> {
        probe(config)?;
        let root = fs::canonicalize(&config.cgroup_root)?;
        let path = root.join(format!("job-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&path)?;
        let configured = (|| {
            fs::write(path.join("memory.max"), limits.memory_bytes.to_string())?;
            fs::write(path.join("memory.swap.max"), "0")?;
            fs::write(path.join("memory.oom.group"), "1")?;
            fs::write(path.join("pids.max"), limits.pids.to_string())?;
            // At most one CPU per job; RLIMIT_CPU also bounds total CPU time.
            fs::write(path.join("cpu.max"), "100000 100000")?;
            // cgroup.kill is required, so cancellation cannot leave descendants.
            OpenOptions::new()
                .write(true)
                .open(path.join("cgroup.kill"))?;
            OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_CLOEXEC)
                .open(path.join("cgroup.procs"))
        })();
        match configured {
            Ok(join) => Ok(Self { path, join }),
            Err(error) => {
                let _ = fs::remove_dir(&path);
                Err(error)
            }
        }
    }
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn join_fd(&self) -> i32 {
        self.join.as_raw_fd()
    }
    pub(crate) fn oom_killed(&self) -> io::Result<bool> {
        let events = fs::read_to_string(self.path.join("memory.events"))?;
        Ok(events.lines().any(|line| {
            line.strip_prefix("oom_kill ")
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|count| count > 0)
        }))
    }
    #[cfg(test)]
    pub(crate) fn controller(&self, name: &str) -> io::Result<String> {
        fs::read_to_string(self.path.join(name))
    }
}
impl Drop for CgroupJob {
    fn drop(&mut self) {
        let _ = fs::write(self.path.join("cgroup.kill"), "1");
        for _ in 0..50 {
            if fs::remove_dir(&self.path).is_ok() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        tracing::warn!("sandbox cgroup cleanup incomplete; operator reconciliation required");
    }
}

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
}
#[repr(C, packed)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

pub(crate) struct ChildPolicy {
    ruleset: OwnedFd,
    filter: Vec<libc::sock_filter>,
}
impl ChildPolicy {
    pub(crate) fn prepare(python: &Path, workspace: &Path) -> io::Result<Self> {
        let abi = landlock_abi()?;
        let handled = (1u64 << 15) - 1 | if abi >= 5 { 1 << 15 } else { 0 };
        let attr = RulesetAttr {
            handled_access_fs: handled,
        };
        let fd = unsafe {
            libc::syscall(
                LANDLOCK_CREATE_RULESET,
                &attr,
                std::mem::size_of::<RulesetAttr>(),
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let ruleset = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        let mut policy = Self {
            ruleset,
            filter: syscall_filter()?,
        };
        policy.allow(&fs::canonicalize(workspace)?, handled & !EXECUTE)?;
        policy.allow(&fs::canonicalize(python)?, READ_FILE | EXECUTE)?;
        for path in ["/usr/local/lib", "/usr/lib", "/lib", "/lib64"] {
            if let Ok(path) = fs::canonicalize(path) {
                policy.allow(&path, READ_FILE | READ_DIR)?;
            }
        }
        for path in [
            "/lib64/ld-linux-x86-64.so.2",
            "/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2",
            "/lib/ld-linux-aarch64.so.1",
            "/lib/aarch64-linux-gnu/ld-linux-aarch64.so.1",
        ] {
            if let Ok(path) = fs::canonicalize(path) {
                policy.allow(&path, READ_FILE | EXECUTE)?;
            }
        }
        for path in ["/etc/ld.so.cache", "/etc/localtime", "/dev/urandom"] {
            if let Ok(path) = fs::canonicalize(path) {
                policy.allow(&path, READ_FILE)?;
            }
        }
        policy.allow(
            Path::new("/dev/null"),
            READ_FILE | WRITE_FILE | if abi >= 5 { 1 << 15 } else { 0 },
        )?;
        Ok(policy)
    }
    fn allow(&mut self, path: &Path, rights: u64) -> io::Result<()> {
        let path = path_string(path)?;
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let parent = unsafe { OwnedFd::from_raw_fd(fd) };
        let attr = PathBeneathAttr {
            allowed_access: rights,
            parent_fd: parent.as_raw_fd(),
        };
        if unsafe { libc::syscall(LANDLOCK_ADD_RULE, self.ruleset.as_raw_fd(), 1, &attr, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
    /// Called only in the trusted supervisor's child, immediately before exec.
    pub(crate) fn apply(&self) -> io::Result<()> {
        if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0
            || unsafe { libc::syscall(LANDLOCK_RESTRICT_SELF, self.ruleset.as_raw_fd(), 0) } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let program = libc::sock_fprog {
            len: self.filter.len() as u16,
            filter: self.filter.as_ptr() as *mut _,
        };
        if unsafe { libc::prctl(libc::PR_SET_SECCOMP, 2, &program, 0, 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

fn statement(code: u16, k: u32) -> libc::sock_filter {
    libc::sock_filter {
        code,
        jt: 0,
        jf: 0,
        k,
    }
}
fn equal(k: u32, jt: u8, jf: u8) -> libc::sock_filter {
    libc::sock_filter {
        code: 0x15,
        jt,
        jf,
        k,
    }
}
fn syscall_filter() -> io::Result<Vec<libc::sock_filter>> {
    #[cfg(target_arch = "x86_64")]
    let architecture = 0xc000003e;
    #[cfg(target_arch = "aarch64")]
    let architecture = 0xc00000b7;
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return Err(io::Error::from(io::ErrorKind::Unsupported));
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut filter = vec![
            statement(0x20, 4),
            equal(architecture, 1, 0),
            statement(0x06, 0x80000000),
            statement(0x20, 0),
        ];
        // Exact native syscall IDs only: x32-tagged IDs cannot match this list.
        for syscall in allowed_syscalls() {
            filter.push(equal(syscall as u32, 0, 1));
            filter.push(statement(0x06, 0x7fff0000));
        }
        filter.push(statement(0x06, 0x00050000 | libc::EPERM as u32));
        Ok(filter)
    }
}

fn allowed_syscalls() -> Vec<libc::c_long> {
    #[allow(unused_mut)]
    let mut calls = vec![
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_close,
        libc::SYS_openat,
        libc::SYS_newfstatat,
        libc::SYS_fstat,
        libc::SYS_statx,
        libc::SYS_lseek,
        libc::SYS_pread64,
        libc::SYS_pwrite64,
        libc::SYS_readv,
        libc::SYS_writev,
        libc::SYS_getdents64,
        libc::SYS_readlinkat,
        libc::SYS_faccessat,
        libc::SYS_faccessat2,
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_mremap,
        libc::SYS_madvise,
        libc::SYS_brk,
        libc::SYS_futex,
        libc::SYS_rseq,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
        libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack,
        libc::SYS_set_tid_address,
        libc::SYS_set_robust_list,
        libc::SYS_clock_gettime,
        libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep,
        libc::SYS_gettimeofday,
        libc::SYS_getrandom,
        libc::SYS_getpid,
        libc::SYS_getppid,
        libc::SYS_gettid,
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_uname,
        libc::SYS_getcwd,
        libc::SYS_chdir,
        libc::SYS_fchdir,
        libc::SYS_fcntl,
        libc::SYS_ioctl,
        libc::SYS_dup,
        libc::SYS_dup3,
        libc::SYS_prlimit64,
        libc::SYS_getrlimit,
        libc::SYS_getrusage,
        libc::SYS_sched_yield,
        libc::SYS_sched_getaffinity,
        libc::SYS_sysinfo,
        libc::SYS_ftruncate,
        libc::SYS_truncate,
        libc::SYS_fsync,
        libc::SYS_fdatasync,
        libc::SYS_mkdirat,
        libc::SYS_unlinkat,
        libc::SYS_renameat,
        libc::SYS_renameat2,
        libc::SYS_fchmod,
        libc::SYS_fchmodat,
        libc::SYS_umask,
        libc::SYS_utimensat,
        libc::SYS_execve,
        libc::SYS_exit,
        libc::SYS_exit_group,
    ];
    #[cfg(target_arch = "x86_64")]
    calls.extend([
        libc::SYS_open,
        libc::SYS_stat,
        libc::SYS_lstat,
        libc::SYS_access,
        libc::SYS_readlink,
        libc::SYS_arch_prctl,
        libc::SYS_dup2,
        libc::SYS_mkdir,
        libc::SYS_rmdir,
        libc::SYS_unlink,
        libc::SYS_rename,
        libc::SYS_chmod,
        libc::SYS_utime,
    ]);
    calls
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allowlist_omits_escape_syscalls_and_defaults_to_errno() {
        let allowed = allowed_syscalls();
        for denied in [
            libc::SYS_clone,
            libc::SYS_clone3,
            libc::SYS_unshare,
            libc::SYS_setns,
            libc::SYS_mount,
            libc::SYS_ptrace,
            libc::SYS_socket,
            libc::SYS_connect,
            libc::SYS_bpf,
            libc::SYS_perf_event_open,
            libc::SYS_userfaultfd,
            libc::SYS_process_vm_readv,
        ] {
            assert!(!allowed.contains(&denied));
        }
        assert_eq!(
            syscall_filter().unwrap().last().unwrap().k,
            0x00050000 | libc::EPERM as u32
        );
    }
}
