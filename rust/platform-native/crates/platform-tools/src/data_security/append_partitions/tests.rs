use super::*;
fn date(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
fn snapshot(name: &str, phase: &str) -> Snapshot {
    let spec = catalog::family(name).unwrap();
    let catalog = json!({"family":name,"clock_column":spec.clock,"cadence":if spec.cadence==Cadence::Day{"day"}else{"month"},"phase":phase,"revision":7,"compatibility_sha256":"c".repeat(64),"partitions":[],"foreign_keys":[]});
    Snapshot {
        schema: "agentforge-append-catalog/v1".into(),
        authority_snapshot_sha256: "a".repeat(64),
        catalog_sha256: sha256(&serde_json::to_vec(&catalog).unwrap()),
        catalog,
    }
}
fn refresh(s: &mut Snapshot) {
    s.catalog_sha256 = sha256(&serde_json::to_vec(&s.catalog).unwrap());
}
#[test]
fn all_planned_append_families_and_global_accounting_lease_contracts_are_present() {
    assert_eq!(catalog::ALL.len(), 22);
    for name in [
        "gateway.usage_records",
        "gateway.request_traces",
        "gateway.audit_logs",
        "gateway.billing_events",
        "gateway.agent_trace_spans",
        "gateway.agent_trace_events",
        "gateway.tool_audit_log",
        "gateway.login_audit",
        "gateway.service_health_records",
        "assistant.assistant_runtime_items",
        "assistant.assistant_audit_events",
        "assistant.assistant_capability_events",
        "knowledge.kb_document_progress_events",
        "assistant.assistant_runtime_model_calls",
        "assistant.assistant_runtime_model_leases",
        "gateway.knowledge_model_calls",
    ] {
        assert!(catalog::family(name).is_ok(), "{name}");
    }
    assert!(catalog::family("gateway.singleton_worker_leases").is_err());
}
#[test]
fn calendar_boundaries_handle_leap_days_years_and_reject_partial_windows() {
    let day = catalog::family("gateway.request_traces").unwrap();
    let month = catalog::family("gateway.usage_records").unwrap();
    assert_eq!(
        boundary(day, date("2028-02-28T00:00:00Z")).unwrap(),
        date("2028-02-29T00:00:00Z")
    );
    assert_eq!(
        boundary(month, date("2026-12-01T00:00:00Z")).unwrap(),
        date("2027-01-01T00:00:00Z")
    );
    assert!(boundary(month, date("2026-02-02T00:00:00Z")).is_err());
    assert!(boundary(day, date("2026-02-02T00:00:01Z")).is_err());
    assert_eq!(
        partition_name(day, date("2026-02-02T00:00:00Z")).unwrap(),
        "gateway.request_traces_p20260202"
    );
}
#[test]
fn snapshot_tampering_and_phase_skips_do_not_produce_mutation_plans() {
    let mut s = snapshot("gateway.usage_records", "registered");
    let op = Operation::Expand {
        boundary: date("2027-01-01T00:00:00Z"),
    };
    assert!(plan(&s, &op, date("2026-10-17T00:00:00Z")).is_ok());
    s.catalog["family"] = json!("gateway.audit_logs");
    assert!(validate_snapshot(&s).is_err());
    let s = snapshot("gateway.usage_records", "expanded");
    assert!(plan(&s, &Operation::ValidateLegacy, Utc::now()).is_err());
    assert!(plan(&s, &Operation::Backfill { limit: 10001 }, Utc::now()).is_err());
    let plan = plan(&s, &Operation::Backfill { limit: 10000 }, Utc::now()).unwrap();
    assert!(plan["sql"][0].as_str().unwrap().contains(",7,10000)"));
    assert_eq!(plan["effects"], false);
}
#[test]
fn resume_uses_catalog_pending_state_and_never_guesses_on_missing_acknowledgement() {
    let mut s = snapshot("gateway.usage_records", "active");
    s.catalog["partitions"] = json!([{"relation_name":"gateway.usage_records_p202601","state":"detaching","attached":true,"detach_pending":true}]);
    refresh(&mut s);
    let op = Operation::DetachResume {
        relation: "gateway.usage_records_p202601".into(),
    };
    let p = plan(&s, &op, Utc::now()).unwrap();
    assert!(p["sql"][0].as_str().unwrap().ends_with(" FINALIZE"));
    assert_eq!(p["nontransactional_steps_present"], true);
    s.catalog["partitions"][0]
        .as_object_mut()
        .unwrap()
        .remove("detach_pending");
    refresh(&mut s);
    assert_eq!(
        plan(&s, &op, Utc::now()).unwrap()["sql"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        plan(
            &s,
            &Operation::DetachResume {
                relation: "gateway.usage_records_p202601;DROP SCHEMA gateway CASCADE".into()
            },
            Utc::now()
        )
        .is_err()
    );
}
#[test]
fn retention_rejects_open_bounds_and_missing_real_evidence_before_detach() {
    let mut s = snapshot("assistant.assistant_runtime_model_leases", "active");
    s.catalog["partitions"] = json!([{"relation_name":"assistant.assistant_runtime_model_leases_p202601","relation_oid":123,"state":"active","upper_at":"2026-02-01T00:00:00Z"}]);
    refresh(&mut s);
    let op = Operation::RetentionBegin {
        relation: "assistant.assistant_runtime_model_leases_p202601".into(),
        cutoff: date("2026-03-01T00:00:00Z"),
        evidence: BTreeMap::new(),
    };
    assert!(plan(&s, &op, date("2026-01-20T00:00:00Z")).is_err());
    assert!(plan(&s, &op, date("2026-04-01T00:00:00Z")).is_err());
}
#[test]
fn drops_require_exact_verified_archive_and_cannot_be_resumed_after_missing_bytes() {
    let mut s = snapshot("gateway.audit_logs", "active");
    s.catalog["partitions"] = json!([{"relation_name":"gateway.audit_logs_p202601","state":"detached","archive_receipt":null}]);
    refresh(&mut s);
    let op = Operation::DropArchived {
        relation: "gateway.audit_logs_p202601".into(),
        archive_manifest_sha256: "b".repeat(64),
    };
    assert!(plan(&s, &op, Utc::now()).is_err());
    s.catalog["partitions"][0]["state"] = json!("archived");
    s.catalog["partitions"][0]["archive_receipt"] =
        json!({"archive_manifest_sha256":"b".repeat(64)});
    refresh(&mut s);
    let result = plan(&s, &op, Utc::now()).unwrap();
    assert!(!result["sql"][0].as_str().unwrap().contains("CASCADE"));
}
#[test]
fn evidence_bytes_are_verified_not_just_claimed_by_a_boolean() {
    let directory = std::env::temp_dir().join(format!("append-proof-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let file = directory.join("proof.json");
    std::fs::write(&file, b"isolated fixture proof").unwrap();
    let mut files = BTreeMap::from([(
        "pitr_restore_drill".into(),
        EvidenceFile {
            path: file,
            sha256: "a".repeat(64),
        },
    )]);
    assert!(verify_evidence(&files, &["pitr_restore_drill"]).is_err());
    files.get_mut("pitr_restore_drill").unwrap().sha256 = sha256(b"isolated fixture proof");
    assert!(verify_evidence(&files, &["pitr_restore_drill"]).is_ok());
    // Test fixture cleanup only; no service data or shared caches are touched.
    std::fs::remove_file(&files["pitr_restore_drill"].path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}

#[test]
fn scheduled_preparation_covers_all_families_and_preserves_inactive_blockers() {
    let mut snapshots = catalog::ALL
        .iter()
        .map(|f| snapshot(f.name, "active"))
        .collect::<Vec<_>>();
    let report = maintenance(&snapshots, date("2028-02-29T15:03:00Z"), 7, 2).unwrap();
    assert_eq!(report["plans"].as_array().unwrap().len(), 22);
    assert_eq!(report["effects"], false);
    assert!(report["plans"].as_array().unwrap().iter().any(|p| {
        p["sql"][0]
            .as_str()
            .unwrap()
            .contains("2028-02-29T00:00:00+00:00")
            && p["sql"][0].as_str().unwrap().ends_with(",8)")
    }));
    snapshots[0].catalog["phase"] = json!("registered");
    refresh(&mut snapshots[0]);
    let partial = maintenance(&snapshots, Utc::now(), 7, 2).unwrap();
    assert_eq!(
        partial["blocked_inactive_families"][0],
        catalog::ALL[0].name
    );
    assert_eq!(partial["plans"].as_array().unwrap().len(), 21);
    assert!(maintenance(&snapshots[..21], Utc::now(), 7, 2).is_err());
    assert!(maintenance(&snapshots, Utc::now(), 90, 2).is_err());
    snapshots[1] = snapshots[0].clone();
    assert!(maintenance(&snapshots, Utc::now(), 7, 2).is_err());
}
#[test]
fn retention_resume_never_uses_an_inactive_family_even_if_partition_state_is_forged() {
    let mut s = snapshot("gateway.audit_logs", "expanded");
    s.catalog["partitions"] = json!([{"relation_name":"gateway.audit_logs_p202601","state":"detaching","attached":true,"detach_pending":true}]);
    refresh(&mut s);
    assert!(
        plan(
            &s,
            &Operation::DetachResume {
                relation: "gateway.audit_logs_p202601".into()
            },
            Utc::now()
        )
        .is_err()
    );
}
