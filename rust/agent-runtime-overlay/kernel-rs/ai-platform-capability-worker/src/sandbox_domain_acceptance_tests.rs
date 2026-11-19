//! Real-process checks for the dedicated, credential-free sandbox image. These
//! are ignored until the integration owner runs inside that exact Linux image.
use super::*;
use crate::python_code_execution::PythonSandboxLimits;
use crate::sandbox_transport::RemoteSandboxBroker;
use ai_platform_capability_contract::sandbox_job::{SANDBOX_JOB_SCHEMA, SandboxJobSigner};
use std::process::{Child, Command, Stdio};

struct ProcessGuard(Child);
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Root(PathBuf);
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
#[ignore = "run inside the exact credential-free Linux sandbox image with explicit cgroup delegation"]
async fn dedicated_process_has_no_platform_authority_and_executes_only_sealed_jobs() {
    let root =
        Root(std::env::temp_dir().join(format!("agentforge-domain-test-{}", uuid::Uuid::now_v7())));
    private_directory(&root.0).unwrap();
    let socket = root.0.join("control.sock");
    let signer = SandboxJobSigner::new("test-control".into(), [17; 32]).unwrap();
    let mut command = Command::new("/usr/local/bin/ai-platform-python-sandbox");
    command
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .args([
            "--public-key-id",
            "test-control",
            "--public-key-hex",
            &hex::encode(signer.public_key_bytes()),
        ])
        .arg("--socket")
        .arg(&socket)
        .arg("--journal-root")
        .arg(root.0.join("journal"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(path) = std::env::var_os("AGENTFORGE_SANDBOX_TEST_CGROUP_ROOT") {
        command.arg("--cgroup-root").arg(path);
    }
    let mut child = ProcessGuard(
        command
            .spawn()
            .expect("exact sandbox binary must be installed"),
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "sandbox startup failed closed; environment is not accepted"
            );
            if socket.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let process_root = PathBuf::from(format!("/proc/{}", child.0.id()));
    let environment = fs::read(process_root.join("environ")).unwrap();
    for item in environment
        .split(|byte| *byte == 0)
        .filter(|item| !item.is_empty())
    {
        let key = item.split(|byte| *byte == b'=').next().unwrap();
        assert!(allowed_environment_key(std::str::from_utf8(key).unwrap()));
    }
    for path in [
        "run/secrets",
        "var/run/secrets",
        "run/credentials",
        "root/.aws/credentials",
        "root/.config/gcloud/application_default_credentials.json",
        "app/.env",
        "workspace/.env",
    ] {
        assert!(
            credential_paths_clean([process_root.join("root").join(path).as_path()]).is_ok(),
            "credential mount detected"
        );
    }
    for entry in fs::read_dir(process_root.join("fd")).unwrap() {
        let target = fs::read_link(entry.unwrap().path()).unwrap();
        let path = target.to_string_lossy();
        assert!(
            !path.contains("/secrets/")
                && !path.contains("/credentials/")
                && !path.ends_with(".env")
        );
    }
    let now = crate::now_epoch_ms();
    let job = SandboxJob {
        schema_version: SANDBOX_JOB_SCHEMA.into(),
        job_id: uuid::Uuid::now_v7().to_string(),
        scope: CapabilityScopeV2 {
            tenant_id: "domain-test".into(),
            user_id: "test".into(),
            session_id: "test".into(),
        },
        run_id: uuid::Uuid::now_v7().to_string(),
        tool_call_id: "test".into(),
        arguments_hash: format!("sha256:{}", "a".repeat(64)),
        issued_at_epoch_ms: now,
        expires_at_epoch_ms: now + 60_000,
        code: "import os\nprint(','.join(sorted(os.environ)))".into(),
        inputs: serde_json::json!([]),
        limits: serde_json::to_value(PythonSandboxLimits::default()).unwrap(),
    };
    let mut request = local_request(&job).unwrap();
    request.lease.nonce = job.job_id.clone();
    request.lease.signature = "platform-proof-sentinel-must-not-cross".into();
    request.lease.approval_id = Some("platform-approval-sentinel-must-not-cross".into());
    let broker = RemoteSandboxBroker::new(socket, Arc::new(signer)).unwrap();
    let result = broker
        .execute_cancellable(request, Arc::new(AtomicBool::new(false)))
        .await
        .unwrap();
    assert_eq!(result.status, "succeeded");
    for key in result.stdout.trim().split(',') {
        assert!(matches!(
            key,
            "PATH"
                | "PYTHONNOUSERSITE"
                | "PYTHONUNBUFFERED"
                | "PYTHONDONTWRITEBYTECODE"
                | "LC_CTYPE"
                | "OPENBLAS_NUM_THREADS"
                | "OMP_NUM_THREADS"
                | "MKL_NUM_THREADS"
                | "NUMEXPR_NUM_THREADS"
                | "HOME"
                | "XDG_CACHE_HOME"
                | "MPLCONFIGDIR"
                | "TMPDIR"
        ));
    }
}

#[test]
#[ignore = "requires the exact standalone sandbox binary inside the Linux image"]
fn standalone_process_rejects_an_inherited_control_plane_token() {
    let status = Command::new("/usr/local/bin/ai-platform-python-sandbox")
        .env_clear()
        .env("AI_PLATFORM_INTERNAL_TOKEN", "test-sentinel-not-real")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
}
