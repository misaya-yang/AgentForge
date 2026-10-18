//! Fail-closed admission for an orchestrator-attested strong sandbox. The trusted
//! attestor must measure the actual RuntimeClass/microVM and network policy; an
//! environment variable, pod label or unsigned marker is never evidence.
use ai_platform_capability_contract::lease_v3::{PublicKeyring, SignedEnvelope};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StrongRuntime {
    Gvisor,
    Kata,
    Firecracker,
}
#[derive(Clone, Debug)]
pub struct StrongTierConfig {
    pub runtime: StrongRuntime,
    pub evidence_file: PathBuf,
    pub public_keyring_file: PathBuf,
}
pub struct StrongTierAdmission {
    envelope: SignedEnvelope,
    keys: PublicKeyring,
    runtime: StrongRuntime,
    boot_id: String,
    workload_id: String,
}
impl StrongTierAdmission {
    pub fn load(config: &StrongTierConfig) -> Result<Self, &'static str> {
        #[cfg(not(target_os = "linux"))]
        {
            let _ = config;
            return Err("strong_sandbox_requires_linux");
        }
        #[cfg(target_os = "linux")]
        {
            let envelope: SignedEnvelope = serde_json::from_slice(&read(&config.evidence_file)?)
                .map_err(|_| "strong_sandbox_evidence_invalid")?;
            let keys: PublicKeyring = serde_json::from_slice(&read(&config.public_keyring_file)?)
                .map_err(|_| "strong_sandbox_keyring_invalid")?;
            let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .map_err(|_| "strong_sandbox_boot_unavailable")?
                .trim()
                .to_owned();
            let workload_id = std::fs::read_to_string("/proc/sys/kernel/hostname")
                .map_err(|_| "strong_sandbox_workload_unavailable")?
                .trim()
                .to_owned();
            let admission = Self {
                envelope,
                keys,
                runtime: config.runtime.clone(),
                boot_id,
                workload_id,
            };
            admission.verify(crate::now_epoch_ms())?;
            Ok(admission)
        }
    }
    pub fn available(&self) -> bool {
        self.verify(crate::now_epoch_ms()).is_ok()
    }
    fn verify(&self, now: u64) -> Result<(), &'static str> {
        self.keys.validate()?;
        self.keys
            .verify(&self.envelope, "sandbox-runtime-attestation", now)?;
        let c = &self.envelope.claims;
        if c["schema"] != "sandbox-runtime-attestation/v2"
            || c["boot_id"].as_str() != Some(self.boot_id.as_str())
            || c["workload_id"].as_str() != Some(self.workload_id.as_str())
            || c["runtime"]
                != serde_json::to_value(&self.runtime)
                    .map_err(|_| "strong_sandbox_runtime_invalid")?
            || c["measurement"] != "cri+proc"
            || c["pod_uid"]
                .as_str()
                .is_none_or(|v| uuid::Uuid::parse_str(v).is_err())
            || c["image_digest"]
                .as_str()
                .is_none_or(|v| !v.contains("@sha256:"))
            || c["network"] != "none"
            || c["credentials"] != "none"
            || c["one_execution"] != true
            || self
                .envelope
                .expires_at_ms
                .saturating_sub(self.envelope.issued_at_ms)
                > 900_000
        {
            return Err("strong_sandbox_evidence_binding_invalid");
        }
        Ok(())
    }
}
fn read(path: &Path) -> Result<Vec<u8>, &'static str> {
    use std::io::Read;
    if !path.is_absolute() {
        return Err("strong_sandbox_evidence_invalid");
    }
    // Kubernetes projects the public ConfigMap through a generation symlink.
    // Resolve only within this explicitly mounted directory; do not reject the
    // normal read-only projection or follow a sandbox path outside its mount.
    let parent = std::fs::canonicalize(path.parent().ok_or("strong_sandbox_evidence_invalid")?)
        .map_err(|_| "strong_sandbox_evidence_unavailable")?;
    let resolved =
        std::fs::canonicalize(path).map_err(|_| "strong_sandbox_evidence_unavailable")?;
    if !resolved.starts_with(parent) {
        return Err("strong_sandbox_evidence_path_escape");
    }
    let metadata =
        std::fs::symlink_metadata(&resolved).map_err(|_| "strong_sandbox_evidence_unavailable")?;
    if !metadata.is_file() || metadata.len() > 16_384 {
        return Err("strong_sandbox_evidence_invalid");
    }
    let mut bytes = Vec::new();
    std::fs::File::open(resolved)
        .map_err(|_| "strong_sandbox_evidence_unavailable")?
        .take(16_385)
        .read_to_end(&mut bytes)
        .map_err(|_| "strong_sandbox_evidence_unavailable")?;
    if bytes.len() > 16_384 {
        return Err("strong_sandbox_evidence_invalid");
    };
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn public_key_projection_stays_inside_its_mounted_directory() {
        use std::os::unix::fs::symlink;
        let root =
            std::env::temp_dir().join(format!("sandbox-key-projection-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(root.join("mount/generation")).unwrap();
        std::fs::write(root.join("mount/generation/keys.json"), b"public-fixture").unwrap();
        symlink("generation/keys.json", root.join("mount/keys.json")).unwrap();
        assert_eq!(
            read(&root.join("mount/keys.json")).unwrap(),
            b"public-fixture"
        );
        std::fs::write(root.join("outside"), b"outside-fixture").unwrap();
        symlink("../outside", root.join("mount/escape")).unwrap();
        assert_eq!(
            read(&root.join("mount/escape")),
            Err("strong_sandbox_evidence_path_escape")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn signed_measurement_is_bound_to_boot_runtime_workload_and_deadline() {
        use ai_platform_capability_contract::lease_v3::{PublicKey, unhex};
        let seed = unhex::<32>("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
            .unwrap();
        let keys = PublicKeyring {
            schema_version: "gateway-verification-keys/v2".into(),
            keys: vec![PublicKey {
                key_id: "test-only".into(),
                public_key_hex: "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
                    .into(),
                not_before_ms: 1,
                not_after_ms: 2000,
                revoked: false,
            }],
        };
        let claims = serde_json::json!({"schema":"sandbox-runtime-attestation/v2","runtime":"gvisor","boot_id":"boot","workload_id":"pod","measurement":"cri+proc","pod_uid":"00000000-0000-0000-0000-000000000001","image_digest":"sandbox@sha256:fixture","network":"none","credentials":"none","one_execution":true});
        let sign = |claims| {
            SignedEnvelope::issue(
                &seed,
                "test-only",
                "sandbox-runtime-attestation",
                claims,
                10,
                1000,
                &keys,
            )
            .unwrap()
        };
        let admission = StrongTierAdmission {
            envelope: sign(claims.clone()),
            keys: keys.clone(),
            runtime: StrongRuntime::Gvisor,
            boot_id: "boot".into(),
            workload_id: "pod".into(),
        };
        assert!(admission.verify(20).is_ok());
        assert!(admission.verify(1001).is_err());
        for field in [
            "runtime",
            "boot_id",
            "workload_id",
            "network",
            "credentials",
        ] {
            let mut altered = claims.clone();
            altered[field] = "other".into();
            let bad = StrongTierAdmission {
                envelope: sign(altered),
                keys: keys.clone(),
                runtime: StrongRuntime::Gvisor,
                boot_id: "boot".into(),
                workload_id: "pod".into(),
            };
            assert!(bad.verify(20).is_err());
        }
    }
    #[test]
    fn unsigned_expired_wrong_boot_and_weaker_tier_are_never_admitted() {
        // The constructor's production path requires a signed orchestration
        // measurement. No filesystem/RuntimeClass assertion can substitute it.
        let root = std::env::temp_dir().join(format!("sandbox-tier-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir(&root).unwrap();
        let evidence = root.join("evidence.json");
        let keys = root.join("keys.json");
        std::fs::write(&evidence, b"{\"runtime\":\"gvisor\"}").unwrap();
        std::fs::write(&keys, b"{}").unwrap();
        for runtime in [
            StrongRuntime::Gvisor,
            StrongRuntime::Kata,
            StrongRuntime::Firecracker,
        ] {
            assert!(
                StrongTierAdmission::load(&StrongTierConfig {
                    runtime,
                    evidence_file: evidence.clone(),
                    public_keyring_file: keys.clone()
                })
                .is_err()
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
