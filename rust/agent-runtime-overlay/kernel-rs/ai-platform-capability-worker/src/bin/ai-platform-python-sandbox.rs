//! Standalone sandbox process. No database/client/provider configuration is read.
use ai_platform_capability_contract::sandbox_job::SandboxJobVerifier;
use ai_platform_capability_worker::sandbox_service::{
    SandboxService, SandboxServiceConfig, credential_free_preflight,
};
use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

fn start() -> Result<(), &'static str> {
    // Discard inherited authority-bearing descriptors before initializing any
    // service state. Linux >= 6.2 is already required by the Landlock baseline.
    #[cfg(target_os = "linux")]
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } != 0 {
        return Err("sandbox_inherited_descriptor_cleanup_failed");
    }
    credential_free_preflight()?;
    let mut config = SandboxServiceConfig::default();
    let mut socket = PathBuf::from("/run/agentforge-sandbox/control.sock");
    let mut key_id = None;
    let mut public_key = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(name) = args.next() {
        let value = args.next().ok_or("sandbox_argument_missing")?;
        match name.to_str().ok_or("sandbox_argument_invalid")? {
            "--socket" => socket = value.into(),
            "--journal-root" => config.journal_root = value.into(),
            "--package-policy" => config.package_policy = Some(value.into()),
            "--cgroup-root" => config.local.cgroup_root = value.into(),
            "--workspace-root" => config.local.workspace_root = value.into(),
            "--python" => config.local.python_binary = value.into(),
            "--public-key-id" => {
                key_id = Some(value.into_string().map_err(|_| "sandbox_key_id_invalid")?)
            }
            "--public-key-hex" => {
                let bytes = hex::decode(value.to_str().ok_or("sandbox_public_key_invalid")?)
                    .map_err(|_| "sandbox_public_key_invalid")?;
                public_key =
                    Some(<[u8; 32]>::try_from(bytes).map_err(|_| "sandbox_public_key_invalid")?);
            }
            _ => return Err("sandbox_argument_unknown"),
        }
    }
    let verifier = SandboxJobVerifier::new(BTreeMap::from([(
        key_id.ok_or("sandbox_public_key_required")?,
        public_key.ok_or("sandbox_public_key_required")?,
    )]))?;
    let service = SandboxService::new(config, verifier)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|_| "sandbox_runtime_unavailable")?;
    #[cfg(unix)]
    {
        runtime.block_on(ai_platform_capability_worker::sandbox_service::serve(
            socket, service,
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = (runtime, socket, service);
        Err("sandbox_requires_unix")
    }
}
fn main() -> ExitCode {
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("sandbox startup failed: {error}");
            ExitCode::FAILURE
        }
    }
}
