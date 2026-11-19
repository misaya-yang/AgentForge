use super::{
    Result, computer::DriverHealth, credentials::CredentialStorageStatus,
    grants::DirectoryGrantStore, identity::DeviceIdentity, service::LocalNodeRuntime,
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DoctorReport {
    pub device_id: Option<String>,
    pub secure_credential_storage: CredentialStorageStatus,
    pub trusted_local_approval: String,
    pub operating_system: String,
    pub ledger: String,
    pub service_boundary: String,
    pub control_plane: String,
    pub directory_grants: usize,
    pub process_runner: String,
    pub computer: DriverHealth,
}
pub fn capability_doctor(
    identity: Option<&DeviceIdentity>,
    grants: &DirectoryGrantStore,
    runtime: &LocalNodeRuntime,
) -> Result<DoctorReport> {
    Ok(DoctorReport {
        device_id: identity.map(|v| v.device_id.clone()),
        secure_credential_storage: identity.map_or(CredentialStorageStatus::Unavailable, |v| {
            v.credential_storage
        }),
        trusted_local_approval: if runtime.ledger.trusted_local_approval_verifier.is_some() {
            "ready"
        } else {
            "unavailable"
        }
        .into(),
        operating_system: if cfg!(target_os = "macos") {
            "Darwin"
        } else if cfg!(target_os = "windows") {
            "Windows"
        } else if cfg!(target_os = "linux") {
            "Linux"
        } else {
            std::env::consts::OS
        }
        .into(),
        ledger: if runtime.ledger.verify_integrity().is_ok() {
            "ready"
        } else {
            "denied"
        }
        .into(),
        service_boundary: if runtime.binding.validate().is_ok() {
            "ready"
        } else {
            "denied"
        }
        .into(),
        control_plane: match &runtime.control_plane {
            None => "needs_action",
            Some(cp) if cp.validate().is_ok() => "ready",
            _ => "denied",
        }
        .into(),
        directory_grants: grants.active()?.len(),
        process_runner: if runtime.process_runner.policy.allowed_executables.is_empty() {
            "needs_action"
        } else {
            "unavailable"
        }
        .into(),
        computer: runtime.computer.driver.doctor(),
    })
}
