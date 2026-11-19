use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT_ROOT_ID: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let root_id = NEXT_ROOT_ID.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "sdk-parity-{}-{}-{root_id}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&p).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        Self(p)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn restart_marker_precedes_receipt_and_blocks_deletion_sweep() {
    let root = Root::new();
    let id = "a".repeat(64);
    let session = acquire(&root.0, &id).unwrap();
    let path = session.path.clone();
    atomic(&path.join("pending.json"), &json!({"request_id":"r"})).unwrap();
    atomic(
        &path.join("run-receipt.json"),
        &json!({"fingerprint":"synthetic","result":{"event":"completed"}}),
    )
    .unwrap();
    begin_cleanup(&session).unwrap();
    drop(session);
    let retry = acquire(&root.0, &id);
    assert!(retry
        .as_ref()
        .err()
        .is_some_and(|code| *code == "sdk_session_reconciliation_required"));
    drop(retry); // Explicitly close any unexpected successful Session before probing delete.
    assert_eq!(
        remove(&root.0, &id, "deleted", None),
        Err("sdk_session_reconciliation_required")
    );
    File::open(path.join(".activity"))
        .unwrap()
        .set_modified(UNIX_EPOCH)
        .unwrap();
    Sweeper::new().tick(&root.0, 3600).unwrap();
    assert!(path.exists());
    assert!(!marker(&root.0, ".tombstones", &id).exists());
    fs::remove_file(path.join("pending.json")).unwrap();
    assert!(matches!(
        acquire(&root.0, &id),
        Err("sdk_session_reconciliation_required")
    ));
}
#[test]
fn legacy_unknown_deletion_requires_observed_cleanup() {
    for observed in [false, true] {
        let root = Root::new();
        let id = "b".repeat(64);
        let session = acquire(&root.0, &id).unwrap();
        let path = session.path.clone();
        let fingerprint = "b".repeat(64);
        atomic(&path.join("pending.json"), &json!({"request_id":"r","fingerprint":fingerprint})).unwrap();
        if observed {
            begin_cleanup(&session).unwrap();
            complete_cleanup(&session,"r",&fingerprint).unwrap();
        }
        drop(session);
        if observed {
            assert!(remove(&root.0, &id, "deleted", None).unwrap());
            assert_eq!(
                read(&marker(&root.0, ".tombstones", &id)).unwrap()["reason"],
                "deleted_unknown"
            );
        } else {
            assert_eq!(
                remove(&root.0, &id, "deleted", None),
                Err("sdk_session_reconciliation_required")
            );
            assert!(path.exists());
        }
    }
}
#[test]
fn completed_proof_never_overrides_active_cleanup_marker() {
    let root = Root::new();
    let id = "c".repeat(64);
    let session = acquire(&root.0, &id).unwrap();
    begin_cleanup(&session).unwrap();
    fs::write(session.path.join(".cleanup-complete"), "sdk-cleanup/v1\n").unwrap();
    drop(session);
    assert!(matches!(
        acquire(&root.0, &id),
        Err("sdk_session_reconciliation_required")
    ));
    assert_eq!(
        remove(&root.0, &id, "deleted", None),
        Err("sdk_session_reconciliation_required")
    );
}
#[test]
fn active_scope_blocks_reentry_and_delete_without_blocking_another_scope() {
    let root = Root::new();
    let id = "d".repeat(64);
    let session = acquire(&root.0, &id).unwrap();
    assert!(matches!(acquire(&root.0, &id), Err("sdk_session_busy")));
    assert_eq!(
        remove(&root.0, &id, "deleted", None),
        Err("sdk_session_busy")
    );
    let other = acquire(&root.0, &"e".repeat(64)).unwrap();
    assert_ne!(session.path, other.path);
    drop(other);
    drop(session);
    assert!(remove(&root.0, &id, "deleted", None).unwrap());
    assert!(matches!(acquire(&root.0, &id), Err("sdk_session_deleted")));
}
