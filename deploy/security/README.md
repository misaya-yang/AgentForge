# Capability Worker Linux sandbox baseline

The Worker keeps network/mount/user/PID namespaces and strips the child environment. Python additionally requires Landlock ABI >= 3, an installed seccomp filter with an exact native syscall allowlist and default `EPERM`, and a per-execution cgroup v2. A missing boundary returns `IsolationUnavailable`; there is no rlimits-only or namespace-free fallback. Other Worker capabilities remain usable and Python health reports unavailable when the non-mutating isolation preflight fails.

Landlock permits the execution workspace read/write, the canonical interpreter read/execute, system/Python library trees read-only, and narrowly selected loader/timezone/random/null files. The input mount remains read-only. `/proc` and shared scratch mounts remain masked. The Python syscall filter rejects network, process/thread creation, ptrace, mount/namespace changes and all unlisted syscalls; wrong audit architectures kill the process and x32-tagged numbers cannot match. The source lint remains an additional user-facing policy, not an isolation boundary.

`LocalPythonSandboxConfig.cgroup_root` defaults to `/sys/fs/cgroup/agentforge-sandbox`. An operator must delegate an **empty subtree belonging to this Worker** and enable `memory pids cpu` in its `cgroup.subtree_control` before exposing it. The Worker does not enable host controllers or move host services. The root must be a real cgroup2 filesystem with write/search access. Every job must successfully write `memory.max`, `memory.swap.max=0`, `memory.oom.group=1`, `pids.max`, `cpu.max=100000 100000`, and open `cgroup.kill`/`cgroup.procs`. The wrapper joins this group in pre-exec before any unshare/supervisor/user code; descendants inherit it. CPU time also retains the request's RLIMIT_CPU bound. Groups include trusted wrapper/supervisor processes in `pids.max` and memory accounting; very small requested bounds can therefore fail to launch instead of being widened. Cancellation kills the process group and cgroup cleanup kills remaining descendants. OOM is recognized from the real `memory.events` counter.

## Deployment wiring (integration owner)

`capability-worker-seccomp.json` is a container-level default-deny control-plane profile, separate from the tighter Python filter. It permits the existing wrapper's `unshare`, `mount` and `umount2`; it does not grant `setns`, new mount APIs, ptrace, BPF, keyring or module loading. The control plane retains the networking and thread syscalls needed by DB/HTTP/Tokio. No new Rust crates are required.

Replace `seccomp=unconfined` for **agent-capability-worker only**. For root `docker-compose.yml`:

```yaml
security_opt:
  - no-new-privileges:true
  - seccomp=./deploy/security/capability-worker-seccomp.json
volumes:
  # Retain all existing mounts, including the workspace volume.
  - type: bind
    source: ${AGENT_CAPABILITY_WORKER_CGROUP_DELEGATION:?Select this Worker's predelegated cgroup subtree}
    target: /sys/fs/cgroup/agentforge-sandbox
    read_only: false
    bind:
      create_host_path: false
```

For the local stack whose first Compose file is `deploy/local/compose.yml`, the profile entry is `seccomp=../security/capability-worker-seccomp.json`. Bind only the selected delegated subtree, never the entire host cgroup hierarchy. Do not add privileged mode, new capabilities, a new Compose project, or an automatic controller/delegation setup script. If the runtime cannot delegate a usable subtree (including Docker Desktop configurations), leave Python unavailable until the owner provisions a supported Linux environment. Merely binding a directory is not delegation: the per-job join/write checks still fail closed.

The main/startup schema owner can expose a typed path override and pass it as `LocalPythonSandboxConfig { cgroup_root: selected_path, ..Default::default() }`. The default needs no main.rs edit. Never make `require_network_isolation=false` a production escape hatch: broker construction rejects it.

## Verification status and real acceptance

Implemented source is not Linux acceptance. The child allowlist is a reviewed initial list, **not a claim that a syscall trace corpus has been recorded**. Run the actual image's supported Python/library corpus under tracing before release and retain the trace/profile identity; any required syscall must be reviewed rather than widening the default action.

The integration owner serializes Cargo/Docker builds. Commands in the existing authorized Worker build environment:

```sh
cargo test -p ai-platform-capability-worker --lib python_code_execution
# Linux only, after installing the exact supervisor and explicitly delegating its subtree:
cargo test -p ai-platform-capability-worker --lib python_code_execution::escape_tests -- --ignored --nocapture
```

`AGENTFORGE_SANDBOX_TEST_CGROUP_ROOT` may select the explicit test delegation; tests otherwise use the production default. The ignored tests use the real namespace supervisor and native syscalls, not mocks, and fail if required host isolation is absent. They cover outside-file/proc reads, read-only input and writable output, native network/fork/clone/ptrace/mount/unshare/unknown-syscall denial, a memory bomb that must raise MemoryError or increment cgroup OOM-kill, and controller read-back. Do not count ignored tests as passes. Also retain real cancellation/timeout cleanup, stale-fence/unknown-effect, ordinary Python/artifact and mixed-load receipts from the exact integrated image. No host kernel settings, delegated resources or Docker state were changed while implementing this source.
