//! Behavioral translation of tests/services/assistant/local_os_acceptance.
#![cfg(unix)]
use ai_platform_local_node::{
    Error,
    approvals::OneUseApproval,
    canonical,
    credentials::{SecureCredentialStore, UnavailableStore},
    digest,
    envelope::{Action, Approval, PlatformVerifier},
    identity::DeviceIdentity,
    ledger::Ledger,
    now,
    service::{LocalServiceBinding, OutboundControlPlane},
    workspace::{DirectoryGrantStore, Grant, safe_parts},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::Mutex,
};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("local-parity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
const PLATFORM: &[u8] = b"local-os-acceptance-platform-signature-key";
const LOCAL: &[u8] = b"local-os-acceptance-trusted-local-approval-key";
fn sign(key: &[u8], bytes: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    let mut h = Hmac::<sha2::Sha256>::new_from_slice(key).unwrap();
    h.update(bytes);
    format!("{:x}", h.finalize().into_bytes())
}
struct Trusted;
impl PlatformVerifier for Trusted {
    fn verify(&self, id: &str, payload: &[u8], signature: &str) -> bool {
        id == "test-platform-key" && sign(PLATFORM, payload) == signature
    }
}
struct Missing;
impl PlatformVerifier for Missing {
    fn verify(&self, _: &str, _: &[u8], _: &str) -> bool {
        false
    }
}
fn resign(a: &mut Action) {
    a.platform_signature = sign(PLATFORM, canonical(&a.signed_payload()).as_bytes())
}
fn action() -> Action {
    let mut a:Action=serde_json::from_value(json!({"action_id":"action-001","idempotency_key":"idem-001","tenant_id":"tenant-a","user_id":"user-a","device_id":"device-a","session_id":"session-a","run_id":"run-a","agent_id":"agent-a","agent_version":"agent-version-a","call_id":"call-a","envelope_version":1,"capability":"file.write","tool_name":"local_file_write","operation":"file.write","capability_lease_id":"lease-a","resource_refs":["lease-a","notes/result.txt"],"arguments_digest":digest(&json!({"path":"notes/result.txt","content_sha256":"content-v1"})),"target_snapshot_digest":"target-v1","policy_snapshot_digest":"policy-v1","nonce":"nonce-action-001","issued_at":now()-1.,"expires_at":now()+60.,"platform_key_id":"test-platform-key","platform_signature":"","approval":null,"trace_context":null})).unwrap();
    resign(&mut a);
    a
}
fn approved(mut a: Action) -> Action {
    let mut p = Approval {
        approval_id: "approval-local-001".into(),
        action_id: a.action_id.clone(),
        device_id: a.device_id.clone(),
        arguments_digest: a.arguments_digest.clone(),
        target_snapshot_digest: a.target_snapshot_digest.clone(),
        policy_snapshot_digest: a.policy_snapshot_digest.clone(),
        nonce: format!("local-approval-{}", a.action_id),
        expires_at: a.expires_at,
        local_signature: String::new(),
    };
    let mut v = serde_json::to_value(&p).unwrap();
    v.as_object_mut().unwrap().remove("local_signature");
    p.local_signature = sign(LOCAL, canonical(&v).as_bytes());
    a.approval = Some(p);
    resign(&mut a);
    a
}
fn approval(t: &Temp) -> OneUseApproval<impl Fn(&[u8], &str) -> bool + Send + Sync> {
    OneUseApproval::open("device-a".into(), &t.0.join("approvals.sqlite"), |p, s| {
        sign(LOCAL, p) == s
    })
    .unwrap()
}
fn mutate(a: &Action, field: &str, value: Value) -> Action {
    let mut v = serde_json::to_value(a).unwrap();
    v[field] = value;
    serde_json::from_value(v).unwrap()
}
#[test]
fn normalized_arguments_and_secret_digest() {
    assert_eq!(
        digest(&json!({"path":"notes/result.txt","patch":{"b":2,"a":1}})),
        digest(&json!({"patch":{"a":1,"b":2},"path":"notes/result.txt"}))
    );
    let canary = "sk-local-os-canary-never-persist";
    let hash = digest(&json!({"token":canary,"path":"notes.txt"}));
    assert_eq!(hash.len(), 64);
    assert!(!hash.contains(canary));
}
#[test]
fn payload_tamper_and_absent_authority_fail_before_dispatch() {
    let a = action();
    assert!(matches!(
        a.validate(
            &json!({"path":"../outside.txt","content_sha256":"attacker-content"}),
            &a.capability_lease_id,
            &a.resource_refs,
            &Trusted
        ),
        Err(Error::Denied(_))
    ));
    let t = Temp::new();
    let mut l = Ledger::open(&t.0.join("ledger")).unwrap();
    assert!(matches!(l.begin(&a, &Missing), Err(Error::Denied(_))));
    assert!(l.get(&a.action_id).unwrap().is_none());
}
#[test]
fn platform_signature_binds_all_23_authority_fields() {
    let a = action();
    let values = [
        ("action_id", json!("action-b")),
        ("idempotency_key", json!("idem-b")),
        ("tenant_id", json!("tenant-b")),
        ("user_id", json!("user-b")),
        ("device_id", json!("device-b")),
        ("session_id", json!("session-b")),
        ("run_id", json!("run-b")),
        ("agent_id", json!("agent-b")),
        ("agent_version", json!("agent-version-b")),
        ("call_id", json!("call-b")),
        ("envelope_version", json!(2)),
        ("capability", json!("file.rollback")),
        ("tool_name", json!("local_file_rollback")),
        ("operation", json!("file.rollback")),
        ("capability_lease_id", json!("lease-b")),
        ("resource_refs", json!(["resource-b"])),
        ("arguments_digest", json!("changed-arguments")),
        ("target_snapshot_digest", json!("changed-target")),
        ("policy_snapshot_digest", json!("changed-policy")),
        ("nonce", json!("changed-nonce")),
        ("issued_at", json!(1.0)),
        ("expires_at", json!(4102444800.0)),
        ("platform_key_id", json!("platform-key-b")),
    ];
    a.verify(&Trusted, now()).unwrap();
    for (field, value) in values {
        assert!(
            matches!(
                mutate(&a, field, value).verify(&Trusted, now()),
                Err(Error::Denied(_))
            ),
            "{field}"
        );
    }
}
#[test]
fn forged_local_receipt_cannot_expand_platform_signature_and_valid_receipt_is_one_use() {
    let t = Temp::new();
    let v = approval(&t);
    let mut forged = approved(action());
    forged.approval.as_mut().unwrap().local_signature = "forged-local-signature".into();
    resign(&mut forged);
    forged.verify(&Trusted, now()).unwrap();
    assert!(matches!(
        forged.approve(&forged.target_snapshot_digest, &v),
        Err(Error::Denied(_))
    ));
    let valid = approved(action());
    valid.verify(&Trusted, now()).unwrap();
    valid.approve(&valid.target_snapshot_digest, &v).unwrap();
    assert!(matches!(
        valid.approve(&valid.target_snapshot_digest, &v),
        Err(Error::Denied(_))
    ));
}
#[test]
fn locally_signed_receipt_rejects_all_five_binding_mutations() {
    for field in [
        "action_id",
        "device_id",
        "arguments_digest",
        "target_snapshot_digest",
        "policy_snapshot_digest",
    ] {
        let t = Temp::new();
        let v = approval(&t);
        let mut a = serde_json::to_value(approved(action())).unwrap();
        a["approval"][field] = json!("different");
        let mut a: Action = serde_json::from_value(a).unwrap();
        resign(&mut a);
        a.verify(&Trusted, now()).unwrap();
        let result = a.approve(&a.target_snapshot_digest, &v);
        if field == "target_snapshot_digest" {
            assert!(
                matches!(result, Err(Error::Stale)),
                "source requires stale target classification"
            );
        } else {
            assert!(matches!(result, Err(Error::Denied(_))), "{field}");
        }
    }
}
#[test]
fn approval_exact_intent_action_mutations_preserve_error_classification() {
    for field in [
        "arguments_digest",
        "policy_snapshot_digest",
        "target_snapshot_digest",
    ] {
        let t = Temp::new();
        let v = approval(&t);
        let a = mutate(&approved(action()), field, json!("tampered"));
        let result = a.approve(&a.target_snapshot_digest, &v);
        if field == "target_snapshot_digest" {
            assert!(
                matches!(result, Err(Error::Stale)),
                "source requires stale target classification"
            );
        } else {
            assert!(matches!(result, Err(Error::Denied(_))));
        }
    }
}
#[test]
fn approval_expiry_missing_nonfinite_and_target_change_fail_closed() {
    let t = Temp::new();
    let v = approval(&t);
    let mut a = action();
    assert!(matches!(
        a.approve(&a.target_snapshot_digest, &v),
        Err(Error::Denied(_))
    ));
    a = approved(a);
    assert!(matches!(
        a.approve("human-edited-target", &v),
        Err(Error::Stale)
    ));
    for expiry in [0., f64::NAN] {
        a.approval.as_mut().unwrap().expires_at = expiry;
        assert!(matches!(
            a.approve(&a.target_snapshot_digest, &v),
            Err(Error::Denied(_))
        ));
    }
    a.expires_at = f64::NAN;
    assert!(matches!(a.verify(&Trusted, now()), Err(Error::Denied(_))));
}
#[test]
fn identity_fields_match_frozen_source_factory() {
    let a = action();
    assert_eq!(
        (
            &*a.tenant_id,
            &*a.user_id,
            &*a.device_id,
            &*a.session_id,
            &*a.run_id,
            &*a.agent_id,
            &*a.agent_version,
            &*a.call_id,
            a.envelope_version,
            &*a.capability_lease_id,
            &*a.nonce
        ),
        (
            "tenant-a",
            "user-a",
            "device-a",
            "session-a",
            "run-a",
            "agent-a",
            "agent-version-a",
            "call-a",
            1,
            "lease-a",
            "nonce-action-001"
        )
    );
}
#[test]
fn dispatch_is_durable_and_same_idempotency_has_one_policy_event() {
    let t = Temp::new();
    let path = t.0.join("ledger");
    let a = action();
    let mut l = Ledger::open(&path).unwrap();
    let (first, r) = l.begin(&a, &Trusted).unwrap();
    assert!(first);
    assert_eq!(r.status, "policy_check");
    assert_eq!(l.get(&a.action_id).unwrap(), Some(r.clone()));
    assert_eq!(r.envelope_digest, digest(&a.signed_payload()));
    assert_eq!(l.begin(&a, &Trusted).unwrap(), (false, r));
    l.verify().unwrap();
    drop(l);
    let l = Ledger::open(&path).unwrap();
    assert_eq!(l.get(&a.action_id).unwrap().unwrap().status, "policy_check");
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM events WHERE event_type='policy_check'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}
#[test]
fn ledger_state_machine_and_single_terminal_truth() {
    let t = Temp::new();
    let mut l = Ledger::open(&t.0.join("ledger")).unwrap();
    let a = action();
    l.begin(&a, &Trusted).unwrap();
    assert!(matches!(
        l.transition(&a.action_id, "running", None),
        Err(Error::Conflict)
    ));
    for s in ["awaiting_approval", "dispatched", "running", "observed"] {
        assert_eq!(l.transition(&a.action_id, s, None).unwrap().status, s);
    }
    let result = Some(json!({"after_digest":"after-v1"}));
    let r = l
        .transition(&a.action_id, "succeeded", result.clone())
        .unwrap();
    assert_eq!(l.transition(&a.action_id, "succeeded", result).unwrap(), r);
    assert!(matches!(
        l.transition(
            &a.action_id,
            "failed",
            Some(json!({"error_code":"late_error"}))
        ),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        l.transition(
            &a.action_id,
            "succeeded",
            Some(json!({"after_digest":"different"}))
        ),
        Err(Error::Conflict)
    ));
}
#[test]
fn disconnect_before_and_after_dispatch_preserves_unknown_and_never_replays() {
    for dispatched in [false, true] {
        let t = Temp::new();
        let mut l = Ledger::open(&t.0.join("ledger")).unwrap();
        let a = action();
        l.begin(&a, &Trusted).unwrap();
        for s in if dispatched {
            vec!["dispatched", "running"]
        } else {
            vec!["awaiting_approval"]
        } {
            l.transition(&a.action_id, s, None).unwrap();
        }
        assert_eq!(l.disconnect().unwrap(), vec![a.action_id.clone()]);
        let r = l.get(&a.action_id).unwrap().unwrap();
        assert_eq!(r.status, if dispatched { "unknown" } else { "interrupted" });
        assert_eq!(
            r.result,
            Some(
                json!({"error_code":"transport_disconnected","replay_allowed":false,"side_effect_unknown":dispatched})
            )
        );
        assert_eq!(l.begin(&a, &Trusted).unwrap(), (false, r));
    }
}
#[test]
fn ledger_scope_replay_rejects_all_14_resigned_mutations() {
    for field in [
        "tenant_id",
        "user_id",
        "device_id",
        "session_id",
        "run_id",
        "agent_id",
        "agent_version",
        "call_id",
        "capability",
        "capability_lease_id",
        "resource_refs",
        "target_snapshot_digest",
        "policy_snapshot_digest",
        "nonce",
    ] {
        let t = Temp::new();
        let mut l = Ledger::open(&t.0.join("ledger")).unwrap();
        let a = action();
        l.begin(&a, &Trusted).unwrap();
        let mut b = mutate(
            &a,
            field,
            if field == "resource_refs" {
                json!(["resource-b"])
            } else {
                json!("different")
            },
        );
        resign(&mut b);
        assert!(
            matches!(l.begin(&b, &Trusted), Err(Error::Conflict)),
            "{field}"
        );
    }
}
#[test]
fn changed_intent_reuses_no_idempotency_record() {
    let t = Temp::new();
    let mut l = Ledger::open(&t.0.join("ledger")).unwrap();
    let a = action();
    l.begin(&a, &Trusted).unwrap();
    let mut b = a.clone();
    b.action_id = "action-002".into();
    b.arguments_digest = digest(&json!({"path":"notes/other.txt","content_sha256":"changed"}));
    resign(&mut b);
    assert!(matches!(l.begin(&b, &Trusted), Err(Error::Conflict)));
}
#[test]
fn event_chain_detects_modify_delete_and_reorder() {
    for mode in ["modify", "delete", "reorder"] {
        let t = Temp::new();
        let path = t.0.join("ledger");
        let a = action();
        {
            let mut l = Ledger::open(&path).unwrap();
            l.begin(&a, &Trusted).unwrap();
            for s in ["dispatched", "running", "succeeded"] {
                l.transition(&a.action_id, s, None).unwrap();
            }
        }
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(match mode{"modify"=>"UPDATE events SET payload_json='{\"attacker\":\"rewrote-history\"}' WHERE seq=1","delete"=>"DELETE FROM events WHERE seq=2",_=>"UPDATE events SET seq=-seq WHERE seq IN (1,2);UPDATE events SET seq=CASE seq WHEN -1 THEN 2 WHEN -2 THEN 1 ELSE seq END WHERE seq<0"}).unwrap();
        drop(db);
        assert!(
            matches!(Ledger::open(&path), Err(Error::Integrity)),
            "{mode}"
        );
    }
}
#[test]
fn ledger_canary_redacted_in_database_wal_and_shm() {
    let t = Temp::new();
    let path = t.0.join("ledger");
    let mut l = Ledger::open(&path).unwrap();
    let canary = "sk-local-ledger-canary-123456789";
    let mut a = action();
    a.arguments_digest =
        digest(&json!({"path":"notes/output.txt","authorization":format!("Bearer {canary}")}));
    resign(&mut a);
    l.begin(&a, &Trusted).unwrap();
    l.transition(
        &a.action_id,
        "failed",
        Some(json!({"error_detail_safe":format!("provider failed with token={canary}")})),
    )
    .unwrap();
    for suffix in ["", "-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        if p.exists() {
            assert!(
                !std::fs::read(p)
                    .unwrap()
                    .windows(canary.len())
                    .any(|s| s == canary.as_bytes())
            );
        }
    }
}
#[derive(Default)]
struct Store(Mutex<BTreeMap<String, Vec<u8>>>);
impl SecureCredentialStore for Store {
    fn ready(&self) -> bool {
        true
    }
    fn load(&self, r: &str) -> ai_platform_local_node::Result<Option<Vec<u8>>> {
        Ok(self.0.lock().unwrap().get(r).cloned())
    }
    fn store(&self, r: &str, s: &[u8]) -> ai_platform_local_node::Result<()> {
        self.0.lock().unwrap().insert(r.into(), s.into());
        Ok(())
    }
}
#[test]
fn identity_private_metadata_no_key_and_no_storage_fallback() {
    let t = Temp::new();
    let store = Store::default();
    let dir = t.0.join("identity");
    let id = DeviceIdentity::load_or_create(&dir, &store).unwrap();
    let path = dir.join("device-identity.json");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let bytes = std::fs::read(path).unwrap();
    for key in store.0.lock().unwrap().values() {
        assert!(!bytes.windows(key.len()).any(|v| v == key));
    }
    assert_eq!(
        DeviceIdentity::load_or_create(&dir, &store)
            .unwrap()
            .device_id,
        id.device_id
    );
    assert!(matches!(
        DeviceIdentity::load_or_create(&t.0.join("unavailable"), &UnavailableStore),
        Err(Error::Denied(_))
    ));
}
#[test]
fn identity_symlink_and_plaintext_legacy_fail_closed() {
    let t = Temp::new();
    let store = Store::default();
    let outside = t.0.join("real.json");
    std::fs::write(&outside, b"{}").unwrap();
    let linked = t.0.join("linked");
    std::fs::create_dir(&linked).unwrap();
    symlink(&outside, linked.join("device-identity.json")).unwrap();
    assert!(DeviceIdentity::load_or_create(&linked, &store).is_err());
    let legacy = t.0.join("legacy");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(
        legacy.join("device-identity.json"),
        br#"{"device_id":"dev_legacy","key":"plaintext-secret"}"#,
    )
    .unwrap();
    assert!(DeviceIdentity::load_or_create(&legacy, &store).is_err());
}
#[test]
fn listener_and_outbound_scope_corpus() {
    for host in ["0.0.0.0", "::", "192.168.1.20", "localhost"] {
        assert!(
            LocalServiceBinding {
                host: host.into(),
                port: 8765
            }
            .validate()
            .is_err()
        );
    }
    for host in ["127.0.0.1", "::1"] {
        LocalServiceBinding {
            host: host.into(),
            port: 8765,
        }
        .validate()
        .unwrap();
    }
    for url in [
        "http://control.example.test",
        "ws://control.example.test",
        "https://user:password@control.example.test",
        "https://control.example.test/#secret",
    ] {
        assert!(OutboundControlPlane { url: url.into() }.validate().is_err());
    }
    OutboundControlPlane {
        url: "wss://control.example.test/local-node".into(),
    }
    .validate()
    .unwrap();
}
fn grant(t: &Temp) -> Grant {
    let root = t.0.join("granted");
    std::fs::create_dir(&root).unwrap();
    Grant::issue(
        "grant".into(),
        &root,
        "tenant-a".into(),
        "user-a".into(),
        &["list", "read", "search", "watch", "write", "rollback"],
        None,
    )
    .unwrap()
}
#[test]
fn lexical_and_sensitive_path_fixture_is_rejected() {
    let data: Value = serde_json::from_str(include_str!(
        "../../../../../tests/services/assistant/local_os_acceptance/fixtures/path_attacks.json"
    ))
    .unwrap();
    for key in ["lexical_escapes", "sensitive_paths"] {
        for path in data[key].as_array().unwrap() {
            assert!(safe_parts(path.as_str().unwrap()).is_err(), "{key}: {path}");
        }
    }
    assert!(safe_parts("cafe\u{301}.txt").is_err());
}
#[test]
fn symlink_hardlink_and_toctou_file_swaps_cannot_escape() {
    let t = Temp::new();
    let g = grant(&t);
    let root = t.0.join("granted");
    let outside = t.0.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), b"outside-secret").unwrap();
    symlink(outside.join("secret.txt"), root.join("file-link")).unwrap();
    symlink(&outside, root.join("dir-link")).unwrap();
    std::fs::hard_link(outside.join("secret.txt"), root.join("hardlink-secret.txt")).unwrap();
    for p in ["file-link", "dir-link/secret.txt", "hardlink-secret.txt"] {
        assert!(g.read(p, 100).is_err(), "{p}");
    }
    std::fs::write(root.join("proposal.txt"), b"approved-safe-content").unwrap();
    assert_eq!(
        g.read("proposal.txt", 100).unwrap(),
        b"approved-safe-content"
    );
    std::fs::remove_file(root.join("proposal.txt")).unwrap();
    symlink(outside.join("secret.txt"), root.join("proposal.txt")).unwrap();
    assert!(g.read("proposal.txt", 100).is_err());
}
#[test]
fn grant_scope_revocation_capability_and_root_replacement() {
    let t = Temp::new();
    let root = t.0.join("workspace");
    std::fs::create_dir(&root).unwrap();
    let mut store = DirectoryGrantStore::default();
    let id = store
        .issue(&root, &["read"], None, "tenant-a", "user-a", None)
        .unwrap()
        .id
        .clone();
    assert_eq!(store.get(&id, "read", "tenant-a", "user-a").unwrap().id, id);
    for (c, t, u) in [
        ("read", "tenant-b", "user-a"),
        ("read", "tenant-a", "user-b"),
        ("write", "tenant-a", "user-a"),
        ("network.upload", "tenant-a", "user-a"),
        ("credential.use", "tenant-a", "user-a"),
    ] {
        assert!(store.get(&id, c, t, u).is_err());
    }
    std::fs::rename(&root, t.0.join("workspace-old")).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(matches!(
        store.get(&id, "read", "tenant-a", "user-a"),
        Err(Error::Stale)
    ));
    store.revoke(&id);
    assert!(matches!(
        store.get(&id, "read", "tenant-a", "user-a"),
        Err(Error::Denied(_))
    ));
}

#[test]
fn legacy_unbound_rows_remain_readable_but_cannot_replay_signed_actions() {
    let t = Temp::new();
    let path = t.0.join("legacy.sqlite");
    let a = action();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TABLE actions(action_id TEXT PRIMARY KEY,idempotency_key TEXT UNIQUE NOT NULL,arguments_digest TEXT NOT NULL,status TEXT NOT NULL,result_json TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL)").unwrap();
    db.execute(
        "INSERT INTO actions VALUES(?,?,?,?,?,?,?)",
        rusqlite::params![
            a.action_id,
            a.idempotency_key,
            a.arguments_digest,
            "succeeded",
            r#"{"old":true}"#,
            1.0,
            1.0
        ],
    )
    .unwrap();
    drop(db);
    let mut ledger = Ledger::open(&path).unwrap();
    assert!(matches!(ledger.begin(&a, &Trusted), Err(Error::Conflict)));
    let record = ledger.get(&a.action_id).unwrap().unwrap();
    assert_eq!(record.envelope_digest, "legacy-unbound");
    assert_eq!(record.result, Some(json!({"old":true})));
    let mut fresh = action();
    fresh.action_id = "new-action".into();
    fresh.idempotency_key = "new-idem".into();
    resign(&mut fresh);
    assert!(ledger.begin(&fresh, &Trusted).unwrap().0);
    ledger.verify().unwrap();
    drop(ledger);
    Ledger::open(&path).unwrap().verify().unwrap();
}

#[tokio::test]
async fn process_network_and_secret_environment_fail_before_receipt_or_spawn() {
    use ai_platform_local_node::processes::{
        ProcessControl, ProcessPolicy, ProcessRequest, ProcessRunner,
    };
    for secret in [false, true] {
        let t = Temp::new();
        let g = grant(&t);
        let executable = std::env::current_exe().unwrap();
        let request = ProcessRequest {
            argv: vec![executable.to_str().unwrap().into()],
            grant_id: g.id.clone(),
            cwd: ".".into(),
            timeout_seconds: 5.,
            env: if secret {
                BTreeMap::from([("OPENAI_API_KEY".into(), "sk-secret-canary".into())])
            } else {
                BTreeMap::new()
            },
            network_policy: if secret { "inherit" } else { "deny" }.into(),
        };
        let mut a = action();
        a.capability = "process.run".into();
        a.operation = "process.run".into();
        a.tool_name = "local_process_run".into();
        a.arguments_digest = digest(&request.arguments());
        a.capability_lease_id = g.id.clone();
        a.resource_refs = vec![g.id.clone(), ".".into()];
        resign(&mut a);
        let runner = ProcessRunner {
            policy: ProcessPolicy {
                allowed_executables: [executable].into_iter().collect(),
                allowed_env_names: ["OPENAI_API_KEY".into()].into_iter().collect(),
                allow_inherited_network: secret,
                ..Default::default()
            },
            control: ProcessControl::default(),
        };
        let mut ledger = Ledger::open(&t.0.join("ledger")).unwrap();
        let v = approval(&t);
        let e = runner
            .run(&request, &g, &a, &mut ledger, &Trusted, &v)
            .await
            .err()
            .unwrap();
        assert!(
            matches!(e,Error::Denied(message) if if secret {message=="process environment"} else {message.contains("sandbox")})
        );
        assert!(ledger.get(&a.action_id).unwrap().is_none());
    }
}

#[test]
fn process_receipt_does_not_inherit_ambient_host_secret() {
    // Isolate the ambient canary in a child test process, avoiding global env mutation
    // and races with Rust's concurrently running tests.
    const CHILD: &str = "LOCAL_MIGRATION_PARITY_ENV_CHILD";
    const CANARY: &str = "sk-ambient-host-canary-123456";
    if std::env::var(CHILD).as_deref() != Ok("1") {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_receipt_does_not_inherit_ambient_host_secret",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("OPENAI_API_KEY", CANARY)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    assert_eq!(std::env::var("OPENAI_API_KEY").unwrap(), CANARY);
    use ai_platform_local_node::processes::{
        ProcessControl, ProcessPolicy, ProcessRequest, ProcessRunner,
    };
    use std::os::unix::fs::MetadataExt;
    let t = Temp::new();
    let g = grant(&t);
    let executable = PathBuf::from("/usr/bin/env");
    assert!(
        executable.is_file(),
        "source acceptance prerequisite: /usr/bin/env"
    );
    let request = ProcessRequest {
        argv: vec![executable.to_str().unwrap().into()],
        grant_id: g.id.clone(),
        cwd: ".".into(),
        timeout_seconds: 5.,
        env: BTreeMap::new(),
        network_policy: "inherit".into(),
    };
    let m = g.open_dir(".").unwrap().metadata().unwrap();
    let mut a = action();
    a.capability = "process.run".into();
    a.operation = "process.run".into();
    a.tool_name = "local_process_run".into();
    a.arguments_digest = digest(&request.arguments());
    a.capability_lease_id = g.id.clone();
    a.resource_refs = vec![g.id.clone(), ".".into()];
    a.target_snapshot_digest =
        digest(&json!({"grant_id":g.id,"cwd":".","device":m.dev(),"inode":m.ino()}));
    let a = approved(a);
    let runner = ProcessRunner {
        policy: ProcessPolicy {
            allowed_executables: [executable].into_iter().collect(),
            allow_inherited_network: true,
            ..Default::default()
        },
        control: ProcessControl::default(),
    };
    let path = t.0.join("ledger");
    let mut ledger = Ledger::open(&path).unwrap();
    let v = approval(&t);
    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(runner.run(&request, &g, &a, &mut ledger, &Trusted, &v))
        .unwrap();
    assert_eq!(result.status, "succeeded");
    assert!(!result.stdout.contains(CANARY));
    assert!(!result.stdout.contains("OPENAI_API_KEY"));
    let allowed = [
        format!("PATH={}", runner.policy.path),
        "LANG=C.UTF-8".into(),
        "LC_ALL=C.UTF-8".into(),
    ];
    assert!(
        result
            .stdout
            .lines()
            .all(|line| allowed.iter().any(|a| a == line))
    );
    assert!(
        !std::fs::read(path)
            .unwrap()
            .windows(CANARY.len())
            .any(|w| w == CANARY.as_bytes())
    );
}
