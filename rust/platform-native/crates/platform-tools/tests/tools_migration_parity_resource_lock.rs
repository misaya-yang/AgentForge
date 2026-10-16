//! Real native lock process/receipt tests using isolated Git repositories.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("lock-parity-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        let f = Self(p);
        f.git(&["init", "-q"]);
        f.git(&["config", "user.email", "fixture@example.invalid"]);
        f.git(&["config", "user.name", "Fixture"]);
        fs::write(f.0.join("README"), "fixture\n").unwrap();
        f.git(&["add", "."]);
        f.git(&["commit", "-qm", "fixture"]);
        f
    }
    fn git(&self, args: &[&str]) -> String {
        let o = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap().trim().into()
    }
    fn root(&self) -> PathBuf {
        self.0.join(".git/ai-gateway-locks")
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_platform-tools"));
        c.args(["resource-lock", "--repo-root"]).arg(&self.0);
        c
    }
    fn start(
        &self,
        resource: &str,
        identity: Option<&str>,
        script: &str,
        args: &[&str],
    ) -> Running {
        let mut c = self.command();
        c.args([
            "run",
            "--resource",
            resource,
            "--timeout-seconds",
            "5",
            "--heartbeat-seconds",
            "0.05",
            "--expected-end-condition",
            "fixture child completes",
        ]);
        if let Some(i) = identity {
            c.args(["--runtime-identity-json", i]);
        }
        c.args(["--", "sh", "-c", script, "fixture"])
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        Running(c.spawn().unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Running(Child);
impl Running {
    fn finish(&mut self) {
        let status = self.0.wait().unwrap();
        assert!(status.success(), "native lock child failed: {status}");
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            unsafe {
                libc::kill(self.0.id() as i32, libc::SIGTERM);
            };
            let end = Instant::now() + Duration::from_secs(3);
            while Instant::now() < end {
                if !matches!(self.0.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}
fn receipt(path: &Path) -> Value {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        if let Ok(b) = fs::read(path) {
            if let Ok(v) = serde_json::from_slice(&b) {
                return v;
            }
        }
        assert!(
            Instant::now() < end,
            "receipt not published: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn lock_receipt_heartbeat_and_release() {
    let f = Fixture::new();
    let mut child = f.start("rust-build", None, "sleep 1", &[]);
    let p = f.root().join("rust-build/owner.json");
    let first = receipt(&p);
    let end = Instant::now() + Duration::from_secs(2);
    let next = loop {
        let v = receipt(&p);
        if v["heartbeat_time"] != first["heartbeat_time"] {
            break v;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(receipt(&f.root().join(".low-memory/owner.json")), next);
    assert_eq!(next["resource"], "rust-build");
    assert_eq!(next["pid"], child.0.id());
    assert_eq!(next["worktree_path"], f.0.to_string_lossy().as_ref());
    assert_eq!(next["git_head"], f.git(&["rev-parse", "HEAD"]));
    assert_eq!(next["timeout_seconds"], 5);
    assert_eq!(next["expected_end_condition"], "fixture child completes");
    child.finish();
    assert!(!p.parent().unwrap().exists());
    assert!(!f.root().join(".low-memory").exists());
}
#[test]
fn receipt_redacts_secrets_while_child_receives_original_command() {
    let f = Fixture::new();
    let out = f.0.join("observed.txt");
    let identity=json!({"runtime":"fixture","nested":{"apiKey":"identity-secret"},"endpoint":"https://user:pass@runtime.invalid/v1"}).to_string();
    let mut child = f.start(
        "rust-build",
        Some(&identity),
        "printf '%s\\n' \"$@\" > observed.txt; sleep 1",
        &[
            "--api-key",
            "sk-live-secret",
            "TOKEN=token-secret",
            "https://user:pass@runtime.invalid/v1",
        ],
    );
    let owner = receipt(&f.root().join("rust-build/owner.json"));
    let encoded = owner.to_string();
    for secret in [
        "sk-live-secret",
        "token-secret",
        "identity-secret",
        "user:pass",
    ] {
        assert!(!encoded.contains(secret), "receipt leaked synthetic secret");
    }
    assert!(encoded.contains("<redacted>"));
    child.finish();
    let observed = fs::read_to_string(out).unwrap();
    for value in [
        "--api-key",
        "sk-live-secret",
        "TOKEN=token-secret",
        "https://user:pass@runtime.invalid/v1",
    ] {
        assert!(observed.lines().any(|l| l == value));
    }
}
#[test]
fn runtime_identity_is_strict_and_recursively_redacted() {
    for invalid in ["[]", "{\"duplicate\":1,\"duplicate\":2}", "{\"nan\":NaN}"] {
        let f = Fixture::new();
        let mut c = f.start("rust-build", Some(invalid), "touch should-not-run", &[]);
        assert!(!c.0.wait().unwrap().success());
        assert!(!f.0.join("should-not-run").exists());
        assert!(!f.root().join(".low-memory").exists());
    }
}
#[test]
fn low_memory_resources_are_mutually_exclusive_across_git_worktrees() {
    let f = Fixture::new();
    let sibling = f.0.join("sibling");
    f.git(&[
        "worktree",
        "add",
        "--detach",
        sibling.to_str().unwrap(),
        "HEAD",
    ]);
    let mut first = f.start("rust-build", None, "sleep 1", &[]);
    receipt(&f.root().join("rust-build/owner.json"));
    for root in [&f.0, &sibling] {
        let o = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(["resource-lock", "run", "--repo-root"])
            .arg(root)
            .args([
                "--resource",
                "integration-runtime",
                "--timeout-seconds",
                "2",
                "--heartbeat-seconds",
                "0.05",
                "--expected-end-condition",
                "must not run",
                "--",
                "sh",
                "-c",
                "touch forbidden",
            ])
            .output()
            .unwrap();
        assert!(!o.status.success());
        assert!(!root.join("forbidden").exists());
    }
    first.finish();
}
#[test]
fn force_release_requires_exact_token_reason_and_live_owner_override() {
    let f = Fixture::new();
    let mut child = f.start("rust-build", None, "sleep 1", &[]);
    let owner = receipt(&f.root().join("rust-build/owner.json"));
    for (token, reason) in [
        ("wrong-token", "fixture"),
        (owner["owner_token"].as_str().unwrap(), ""),
        (owner["owner_token"].as_str().unwrap(), "fixture"),
    ] {
        let o = f
            .command()
            .args([
                "force-release",
                "--resource",
                "rust-build",
                "--owner-token",
                token,
                "--reason",
                reason,
            ])
            .output()
            .unwrap();
        assert!(!o.status.success());
        assert!(f.root().join("rust-build/owner.json").exists());
    }
    child.finish();
}
fn restored_receipts(f: &Fixture, resource: &str) -> Value {
    let mut child = f.start(resource, None, "sleep 1", &[]);
    let mut owner = receipt(&f.root().join(resource).join("owner.json"));
    child.finish();
    owner["pid"] = json!(std::process::id());
    for name in [".low-memory", resource] {
        fs::create_dir_all(f.root().join(name)).unwrap();
        fs::write(
            f.root().join(name).join("owner.json"),
            serde_json::to_vec(&owner).unwrap(),
        )
        .unwrap();
    }
    owner
}
#[test]
fn force_release_live_override_removes_both_receipts_and_redacts_reason() {
    let f = Fixture::new();
    let owner = restored_receipts(&f, "rust-build");
    let output = f
        .command()
        .args([
            "force-release",
            "--resource",
            "rust-build",
            "--owner-token",
            owner["owner_token"].as_str().unwrap(),
            "--reason",
            "operator recovery API_TOKEN=reason-secret",
            "--allow-live-owner",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["live_owner_override"], true);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("reason-secret"));
    assert!(!f.root().join(".low-memory").exists());
    assert!(!f.root().join("rust-build").exists());
}
#[test]
fn force_release_cross_host_and_known_ambiguous_receipt_require_explicit_override() {
    let f = Fixture::new();
    let mut owner = restored_receipts(&f, "integration-runtime");
    owner["hostname"] = json!("other-host.invalid");
    fs::write(
        f.root().join(".low-memory/owner.json"),
        serde_json::to_vec(&owner).unwrap(),
    )
    .unwrap();
    fs::write(
        f.root().join("integration-runtime/owner.json"),
        "{not-json\n",
    )
    .unwrap();
    let args = [
        "force-release",
        "--resource",
        "integration-runtime",
        "--owner-token",
        owner["owner_token"].as_str().unwrap(),
        "--reason",
        "confirmed remote owner is gone",
    ];
    assert!(!f.command().args(args).output().unwrap().status.success());
    let output = f
        .command()
        .args(args)
        .arg("--allow-cross-host")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let summary: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(summary["cross_host_override"], true);
    assert_eq!(summary["ambiguous_receipts_removed"], 1);
    assert!(!f.root().join(".low-memory").exists());
    assert!(!f.root().join("integration-runtime").exists());
}
#[test]
fn force_release_refuses_unknown_files_even_with_all_overrides() {
    let f = Fixture::new();
    let owner = restored_receipts(&f, "rust-build");
    let unknown = f.root().join("rust-build/unexpected.data");
    fs::write(&unknown, "do not delete\n").unwrap();
    let output = f
        .command()
        .args([
            "force-release",
            "--resource",
            "rust-build",
            "--owner-token",
            owner["owner_token"].as_str().unwrap(),
            "--reason",
            "operator recovery",
            "--allow-cross-host",
            "--allow-live-owner",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(unknown).unwrap(), "do not delete\n");
    assert!(f.root().join(".low-memory/owner.json").exists());
    assert!(f.root().join("rust-build/owner.json").exists());
}
fn rewrite_receipts(f: &Fixture, resource: &str, owner: &Value) {
    for name in [".low-memory", resource] {
        fs::write(
            f.root().join(name).join("owner.json"),
            serde_json::to_vec(owner).unwrap(),
        )
        .unwrap();
    }
}
fn contender(f: &Fixture) -> std::process::Output {
    f.command()
        .args([
            "run",
            "--resource",
            "integration-runtime",
            "--timeout-seconds",
            "2",
            "--heartbeat-seconds",
            "0.05",
            "--expected-end-condition",
            "fixture completes",
            "--",
            "sh",
            "-c",
            "exit 0",
        ])
        .output()
        .unwrap()
}
#[test]
fn only_same_host_dead_and_timed_out_owner_is_reaped() {
    assert_eq!(unsafe { libc::kill(i32::MAX, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let f = Fixture::new();
    let mut owner = restored_receipts(&f, "rust-build");
    owner["pid"] = json!(i32::MAX);
    owner["heartbeat_time"] = json!("2000-01-01T00:00:00Z");
    rewrite_receipts(&f, "rust-build", &owner);
    let out = contender(&f);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for name in [".low-memory", "rust-build", "integration-runtime"] {
        assert!(!f.root().join(name).exists());
    }
}
#[test]
fn owner_is_not_reaped_without_all_dead_timeout_and_same_host_conditions() {
    for updates in [
        json!({"heartbeat_time":"2000-01-01T00:00:00Z"}),
        json!({"pid":i32::MAX,"heartbeat_time":"2999-01-01T00:00:00Z"}),
        json!({"pid":i32::MAX,"hostname":"different-host.invalid","heartbeat_time":"2000-01-01T00:00:00Z"}),
    ] {
        let f = Fixture::new();
        let mut owner = restored_receipts(&f, "rust-build");
        for (k, v) in updates.as_object().unwrap() {
            owner[k] = v.clone();
        }
        rewrite_receipts(&f, "rust-build", &owner);
        assert!(!contender(&f).status.success());
        assert!(f.root().join(".low-memory").exists());
        assert!(f.root().join("rust-build").exists());
    }
}
#[test]
fn ambiguous_orphan_resource_is_not_deleted() {
    let f = Fixture::new();
    let orphan = f.root().join("rust-build");
    fs::create_dir_all(&orphan).unwrap();
    assert!(!contender(&f).status.success());
    assert!(orphan.exists());
    assert!(!f.root().join(".low-memory").exists());
}
#[test]
fn signal_cleanup_removes_both_receipts() {
    let f = Fixture::new();
    let mut process = f.start("rust-build", None, "sleep 30", &[]);
    receipt(&f.root().join("rust-build/owner.json"));
    assert_eq!(
        unsafe { libc::kill(process.0.id() as i32, libc::SIGTERM) },
        0
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(s) = process.0.try_wait().unwrap() {
            break s;
        }
        assert!(Instant::now() < deadline, "signal cleanup exceeded budget");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(status.code(), Some(128 + libc::SIGTERM));
    assert!(!f.root().join(".low-memory").exists());
    assert!(!f.root().join("rust-build").exists());
}
#[test]
fn heartbeat_failure_kills_sigterm_resistant_child_without_timeout_leak() {
    let f = Fixture::new();
    let ready = f.0.join("child-ready");
    let mut process = f.start(
        "rust-build",
        None,
        "trap '' TERM; printf ready > \"$1\"; exec sleep 30",
        &[ready.to_str().unwrap()],
    );
    let owner = receipt(&f.root().join("rust-build/owner.json"));
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready.exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Existing owned atomic-write temporary file causes a real create_new failure.
    // It remains a recognized owned file, so confirmed process cleanup can release it.
    let temp = f.root().join("rust-build").join(format!(
        ".owner-{}-{}.tmp",
        owner["owner_token"].as_str().unwrap(),
        owner["pid"].as_u64().unwrap()
    ));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(s) = process.0.try_wait().unwrap() {
            break s;
        }
        assert!(
            started.elapsed() < Duration::from_secs(6),
            "heartbeat cleanup exceeded budget"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    assert!(!f.root().join(".low-memory").exists());
    assert!(!f.root().join("rust-build").exists());
}
#[test]
fn heartbeat_failure_kills_descendant_process_group() {
    let f = Fixture::new();
    let pid_file = f.0.join("descendant-pid");
    let mut process = f.start(
        "rust-build",
        None,
        "trap '' TERM; sleep 30 & child=$!; printf '%s' \"$child\" > \"$1\"; wait \"$child\"",
        &[pid_file.to_str().unwrap()],
    );
    let owner = receipt(&f.root().join("rust-build/owner.json"));
    let deadline = Instant::now() + Duration::from_secs(3);
    let pid = loop {
        if let Ok(text) = fs::read_to_string(&pid_file) {
            if let Ok(pid) = text.parse::<i32>() {
                break pid;
            }
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    };
    let temp = f.root().join("rust-build").join(format!(
        ".owner-{}-{}.tmp",
        owner["owner_token"].as_str().unwrap(),
        owner["pid"].as_u64().unwrap()
    ));
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    let status = loop {
        if let Some(s) = process.0.try_wait().unwrap() {
            break s;
        }
        assert!(
            Instant::now() < deadline,
            "descendant cleanup exceeded budget"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(!status.success());
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "owned descendant survived cleanup"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    assert!(!f.root().join(".low-memory").exists());
    assert!(!f.root().join("rust-build").exists());
}
