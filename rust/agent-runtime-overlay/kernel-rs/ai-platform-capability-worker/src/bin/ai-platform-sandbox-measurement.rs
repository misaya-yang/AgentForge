//! Trusted-image init probe. Carries no key and makes no attestation claim.
fn main()->std::process::ExitCode {
    let result=(||->Result<(),&'static str>{
        use std::io::Write;use std::os::unix::fs::OpenOptionsExt;
        ai_platform_capability_worker::sandbox_service::credential_free_preflight()?;
        let path=std::env::args_os().nth(1).map(std::path::PathBuf::from).ok_or("probe_path_required")?;
        if !path.is_absolute(){return Err("probe_path_invalid")}
        let boot=std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|_|"probe_boot_unavailable")?;
        let hostname=std::fs::read_to_string("/proc/sys/kernel/hostname").map_err(|_|"probe_hostname_unavailable")?;
        let value=serde_json::json!({"schema":"sandbox-runtime-probe/v1","boot_id":boot.trim(),"workload_id":hostname.trim()});
        let mut file=std::fs::OpenOptions::new().create_new(true).write(true).mode(0o644).custom_flags(libc::O_NOFOLLOW).open(path).map_err(|_|"probe_write_failed")?;
        file.write_all(&serde_json::to_vec(&value).map_err(|_|"probe_invalid")?).and_then(|_|file.sync_all()).map_err(|_|"probe_write_failed")
    })();
    if let Err(error)=result{eprintln!("{error}");std::process::ExitCode::FAILURE}else{std::process::ExitCode::SUCCESS}
}
