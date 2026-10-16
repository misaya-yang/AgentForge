//! Original local_os_acceptance pairing and unavailable-computer contracts.
use ai_platform_gateway_integrations::migration_local_node::{
    Result,
    computer::{
        ComputerController, ComputerDriver, ComputerScope, HealthStatus, MacOSComputerDriver,
    },
    credentials::{CredentialStorageStatus, SecureCredentialStore},
    identity::{DeviceIdentity, PairingManager},
    ledger::ActionLedger,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{Arc, Mutex},
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pairing-parity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[derive(Default)]
struct Store(Mutex<BTreeMap<String, Vec<u8>>>);
impl SecureCredentialStore for Store {
    fn status(&self) -> CredentialStorageStatus {
        CredentialStorageStatus::Ready
    }
    fn load(&self, r: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(r).cloned())
    }
    fn store(&self, r: &str, v: &[u8]) -> Result<()> {
        self.0.lock().unwrap().insert(r.into(), v.into());
        Ok(())
    }
}
fn manager(t: &Temp, name: &str) -> PairingManager {
    PairingManager::new(Arc::new(
        DeviceIdentity::load_or_create(&t.0.join(name), &Store::default()).unwrap(),
    ))
}
#[test]
fn pairing_challenge_is_single_use() {
    let t = Temp::new();
    let m = manager(&t, "identity");
    let c = m.issue(60).unwrap();
    let credential = m.redeem(&c.challenge_id, &c.secret, 3600).unwrap();
    assert!(m.validate_credential(&credential));
    let e = m.redeem(&c.challenge_id, &c.secret, 3600).err().unwrap();
    assert_eq!(e.code, "pairing_error");
    assert!(e.message.contains("consumed"));
}
#[test]
fn wrong_secret_consumes_challenge() {
    let t = Temp::new();
    let m = manager(&t, "identity");
    let c = m.issue(60).unwrap();
    let e = m
        .redeem(&c.challenge_id, "attacker-secret", 3600)
        .err()
        .unwrap();
    assert_eq!(e.code, "pairing_error");
    assert!(e.message.contains("invalid"));
    assert!(
        m.redeem(&c.challenge_id, &c.secret, 3600)
            .err()
            .unwrap()
            .message
            .contains("consumed")
    );
}
#[test]
fn credential_bound_to_issuing_device() {
    let t = Temp::new();
    let a = manager(&t, "first");
    let b = manager(&t, "second");
    let c = a.issue(60).unwrap();
    let credential = a.redeem(&c.challenge_id, &c.secret, 3600).unwrap();
    assert!(a.validate_credential(&credential));
    assert!(!b.validate_credential(&credential));
}
fn driver(platform: &str) -> MacOSComputerDriver {
    MacOSComputerDriver {
        backend: None,
        accessibility_probe: None,
        screen_recording_probe: None,
        platform_name: platform.into(),
    }
}
#[test]
fn missing_mac_backend_and_permissions_never_fake_success() {
    let d = driver("Darwin");
    assert_eq!(d.doctor().status, HealthStatus::NeedsAction);
    assert_eq!(
        d.observe("com.example.TextEditor", None).unwrap_err().code,
        "driver_unavailable"
    );
}
#[test]
fn non_macos_health_is_explicitly_unsupported() {
    let h = driver("Linux").doctor();
    assert_eq!(h.status, HealthStatus::Unsupported);
    assert_eq!(h.accessibility, HealthStatus::Unsupported);
    assert_eq!(h.screen_recording, HealthStatus::Unsupported);
}
#[test]
fn unavailable_driver_cannot_acquire_computer_lease() {
    let t = Temp::new();
    let ledger = Arc::new(ActionLedger::open(&t.0.join("ledger"), None, None).unwrap());
    let c = ComputerController::new(
        Arc::new(driver("Darwin")),
        ComputerScope {
            allowed_apps: BTreeSet::from(["com.example.TextEditor".into()]),
            allowed_origins: BTreeSet::new(),
        },
        ledger,
    );
    assert_eq!(
        c.acquire("com.example.TextEditor", "session-a", None, 60)
            .err()
            .unwrap()
            .code,
        "driver_unavailable"
    );
}
