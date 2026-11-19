//! Real Linux boundary tests. Explicitly ignored until the integration owner
//! provisions the supervisor binary and a delegated cgroup subtree. No mocks.
use super::*;

fn config() -> LocalPythonSandboxConfig {
    let mut config = LocalPythonSandboxConfig::default();
    if let Some(root) = std::env::var_os("AGENTFORGE_SANDBOX_TEST_CGROUP_ROOT") {
        config.cgroup_root = PathBuf::from(root);
    }
    isolation::available(&config).expect("required Linux isolation and explicit delegation");
    config
}
fn request(code: &str) -> PythonCodeExecutionRequest {
    let arguments_hash =
        ai_platform_capability_contract::canonical_json_hash(&serde_json::json!({"code": code}))
            .unwrap();
    PythonCodeExecutionRequest {
        lease: RuntimeCapabilityLeaseV1 {
            schema_version:
                ai_platform_capability_contract::RUNTIME_CAPABILITY_LEASE_SCHEMA_VERSION.into(),
            lease_id: "00000000-0000-0000-0000-000000000001".into(),
            tenant_id: "sandbox-test".into(),
            user_id: "test".into(),
            session_id: "test".into(),
            run_id: "00000000-0000-0000-0000-000000000002".into(),
            tool_call_id: "test-call".into(),
            attempt_id: "attempt-1".into(),
            capability_id: "execute_python_code".into(),
            capability_revision: 1,
            arguments_hash: arguments_hash.clone(),
            effect: ai_platform_capability_contract::CapabilityEffect::Write,
            approval_id: Some("test-approval".into()),
            issued_at_epoch_ms: 1,
            expires_at_epoch_ms: 120001,
            nonce: "sandbox-test-nonce".into(),
            signature: format!("sha256:{}", "0".repeat(64)),
        },
        arguments_hash,
        code: code.into(),
        inputs: vec![],
        limits: PythonSandboxLimits {
            timeout_seconds: 10,
            cpu_millis: 10000,
            ..Default::default()
        },
    }
}
fn run(code: &str) -> PythonCodeExecutionResult {
    let result = supervisor::run_isolated(&config(), &request(code), &AtomicBool::new(false))
        .expect("must execute through the real supervisor, never skip unsupported isolation");
    assert_eq!(result.status, "succeeded", "{}", result.stderr);
    result
}

#[test]
#[ignore = "requires Linux supervisor binary, namespaces, Landlock ABI>=3 and delegated cgroup v2"]
fn linux_escape_files_proc_and_input_write_are_denied() {
    let mut request = request(
        r#"
for path in ['/etc/passwd', '/etc/hostname', '/proc/1/environ', '/proc/self/root/etc/passwd', '/sys/fs/cgroup/cgroup.procs']:
    try:
        open(path).read()
    except OSError:
        continue
    raise AssertionError('outside read allowed: ' + path)
assert open('input/protected.txt').read() == 'input'
try:
    open('input/protected.txt', 'w').write('changed')
except OSError:
    pass
else:
    raise AssertionError('input write allowed')
open('output/ok.txt', 'w').write('workspace read/write works')
print('files-denied')
"#,
    );
    request.inputs.push(CodeInputAttachment {
        filename: "protected.txt".into(),
        mime_type: None,
        content_base64: STANDARD.encode(b"input"),
        size_bytes: 5,
    });
    let result = supervisor::run_isolated(&config(), &request, &AtomicBool::new(false)).unwrap();
    assert_eq!(result.status, "succeeded", "{}", result.stderr);
    assert!(result.stdout.contains("files-denied"));
    assert_eq!(result.output_files.len(), 1);
}

#[test]
#[ignore = "requires Linux supervisor binary, namespaces, Landlock ABI>=3 and delegated cgroup v2"]
fn linux_escape_network_fork_ptrace_mount_and_unknown_syscall_are_denied() {
    // Intentionally bypass lexical lint using constructed names: the kernel,
    // not the static source guard, must reject actual native syscalls.
    let result = run(r#"
import builtins as b
imp = getattr(b, '__im' + 'port__')
c = imp('ct' + 'ypes')
p = imp('plat' + 'form')
lib = getattr(c, 'CD' + 'LL')(None, use_errno=True)
call = getattr(lib, 'sys' + 'call')
if p.machine() == 'x86_64':
    forbidden = [41, 42, 56, 57, 58, 101, 165, 272, 435, 999999]
elif p.machine() in ['aarch64', 'arm64']:
    forbidden = [198, 203, 220, 117, 40, 97, 435, 999999]
else:
    raise AssertionError('unsupported test architecture')
for number in forbidden:
    for attempt in range(16):
        getattr(c, 'set_errno')(0)
        result = call(number, 0, 0, 0, 0, 0, 0)
        assert result == -1 and getattr(c, 'get_errno')() == 1, str(number)
print('native-syscalls-denied')
"#);
    assert!(result.stdout.contains("native-syscalls-denied"));
}

#[test]
#[ignore = "requires Linux supervisor binary, namespaces, Landlock ABI>=3 and delegated cgroup v2"]
fn linux_escape_memory_bomb_is_bounded() {
    let mut request = request(
        r#"
chunks = []
try:
    for count in range(256):
        chunks.append(bytearray(4 * 1024 * 1024))
except MemoryError:
    print('memory-bounded')
else:
    raise AssertionError('memory limit bypassed')
"#,
    );
    request.limits.memory_bytes = 96 * 1024 * 1024;
    match supervisor::run_isolated(&config(), &request, &AtomicBool::new(false)) {
        Ok(result) => assert!(
            result.stdout.contains("memory-bounded"),
            "{}",
            result.stderr
        ),
        Err(CodeExecutionError::ResourceLimitExceeded) => {}
        Err(error) => panic!("memory test did not establish a memory bound: {error}"),
    }
}

#[test]
#[ignore = "requires Linux delegated cgroup v2; creates only one job within the selected subtree"]
fn linux_cgroup_applies_memory_pid_cpu_and_swap_limits() {
    let config = config();
    let limits = PythonSandboxLimits {
        memory_bytes: 96 * 1024 * 1024,
        pids: 8,
        ..Default::default()
    };
    // Job controller values are read back below, not inferred from config.
    let job = isolation::CgroupJob::create(&config, &limits).unwrap();
    assert!(job.join_fd() >= 0);
    assert_eq!(
        job.controller("memory.max").unwrap().trim(),
        limits.memory_bytes.to_string()
    );
    assert_eq!(job.controller("pids.max").unwrap().trim(), "8");
    assert_eq!(job.controller("cpu.max").unwrap().trim(), "100000 100000");
    assert_eq!(job.controller("memory.swap.max").unwrap().trim(), "0");
    assert_eq!(job.controller("memory.oom.group").unwrap().trim(), "1");
}

#[test]
fn missing_delegation_fails_closed_without_running_python() {
    let config = LocalPythonSandboxConfig {
        cgroup_root: PathBuf::from("/nonexistent/agentforge-delegation"),
        ..Default::default()
    };
    let result = supervisor::run_isolated(
        &config,
        &request("print('must not run')"),
        &AtomicBool::new(false),
    );
    assert_eq!(
        result.unwrap_err(),
        CodeExecutionError::IsolationUnavailable
    );
}
