use super::*;

fn principal_fixture(principal: ApplicationPrincipal) -> Value {
    let role = format!("fixture_{}", principal_role_suffix(principal));
    serde_json::json!({
        "current_user": role, "session_user": role,
        "login": true, "inherit": false, "superuser": false,
        "createdb": false, "createrole": false, "replication": false,
        "bypassrls": false, "owner": "fixture_owner",
        "canonical_owner": true, "metadata_reader": true,
    })
}

#[test]
fn only_matching_unprivileged_service_login_is_admitted() {
    let principals = [
        ApplicationPrincipal::Gateway,
        ApplicationPrincipal::GatewayDurable,
        ApplicationPrincipal::KnowledgeApi,
        ApplicationPrincipal::KnowledgeWorker,
        ApplicationPrincipal::Runtime,
        ApplicationPrincipal::CapabilityWorker,
    ];
    for principal in principals {
        let identity = principal_fixture(principal);
        assert!(validate_principal_identity(&identity, principal).is_ok());
        for key in [
            "inherit",
            "superuser",
            "createdb",
            "createrole",
            "replication",
            "bypassrls",
        ] {
            let mut bad = identity.clone();
            bad[key] = true.into();
            assert!(
                validate_principal_identity(&bad, principal).is_err(),
                "{key}"
            );
            bad[key] = Value::Null;
            assert!(
                validate_principal_identity(&bad, principal).is_err(),
                "missing {key}"
            );
        }
        for key in ["login", "canonical_owner", "metadata_reader"] {
            let mut bad = identity.clone();
            bad[key] = false.into();
            assert!(
                validate_principal_identity(&bad, principal).is_err(),
                "{key}"
            );
        }
        for key in ["current_user", "session_user"] {
            for role in [
                "postgres",
                "fixture_owner",
                "fixture_migrator",
                "foreign_gateway",
            ] {
                let mut bad = identity.clone();
                bad[key] = role.into();
                assert!(
                    validate_principal_identity(&bad, principal).is_err(),
                    "{key} {role}"
                );
            }
        }
        for other in principals {
            let other_identity = principal_fixture(other);
            assert_eq!(
                validate_principal_identity(&other_identity, principal).is_ok(),
                principal_role_suffix(other) == principal_role_suffix(principal)
            );
        }
        for owner in [
            "postgres",
            "Owner_owner",
            "_owner",
            "bad-prefix_owner",
            "aaaaaaaaaaaaaaaaaaaaaa_owner",
            "foreign_owner",
        ] {
            let mut bad = identity.clone();
            bad["owner"] = owner.into();
            assert!(
                validate_principal_identity(&bad, principal).is_err(),
                "{owner}"
            );
        }
    }
    assert!(validate_principal_identity(&Value::Null, ApplicationPrincipal::Gateway).is_err());
}

#[test]
fn runtime_and_worker_require_source_024_and_reject_bad_ledger() {
    let contract = fixture_contract(24);
    for principal in [
        ApplicationPrincipal::Runtime,
        ApplicationPrincipal::CapabilityWorker,
    ] {
        for epoch in [10, 18, 21, 22, 23, 25] {
            assert!(
                validate_snapshot_value(&snapshot(&contract, epoch), principal, &contract).is_err()
            );
        }
        assert!(validate_snapshot_value(&snapshot(&contract, 24), principal, &contract).is_ok());
        let mut bad = snapshot(&contract, 24);
        bad["changes"][0]["checksum_sha256"] = Value::Null;
        assert!(validate_snapshot_value(&bad, principal, &contract).is_err());
        assert!(validate_reader_result(Err(ReaderFailure::Denied), principal, &contract).is_err());
    }
}

#[test]
fn current_gateway_requires_turn_persistence_successor025() {
    let contract = fixture_contract(25);
    for principal in [
        ApplicationPrincipal::Gateway,
        ApplicationPrincipal::GatewayDurable,
    ] {
        for epoch in [18, 20, 23, 24, 26] {
            assert!(
                validate_snapshot_value(&snapshot(&contract, epoch), principal, &contract).is_err()
            );
        }
        assert!(validate_snapshot_value(&snapshot(&contract, 25), principal, &contract).is_ok());
    }
}

#[test]
fn current_source026_rejects_missing_new_contracts_and_future_epochs() {
    let contract = fixture_contract(26);
    for principal in [
        ApplicationPrincipal::Gateway,
        ApplicationPrincipal::GatewayDurable,
        ApplicationPrincipal::KnowledgeApi,
        ApplicationPrincipal::KnowledgeWorker,
        ApplicationPrincipal::Runtime,
        ApplicationPrincipal::CapabilityWorker,
    ] {
        for epoch in [18, 20, 21, 24, 25, 27] {
            assert!(
                validate_snapshot_value(&snapshot(&contract, epoch), principal, &contract).is_err()
            );
        }
        let current = snapshot(&contract, 26);
        assert!(validate_snapshot_value(&current, principal, &contract).is_ok());
        let mut bad = current.clone();
        bad["changes"][25]["checksum_sha256"] = "0".repeat(64).into();
        assert!(validate_snapshot_value(&bad, principal, &contract).is_err());
    }
}

#[test]
fn current_source027_requires_the_model_plane_metering_and_identity_contracts() {
    let contract = fixture_contract(27);
    for principal in [
        ApplicationPrincipal::Gateway,
        ApplicationPrincipal::GatewayDurable,
        ApplicationPrincipal::KnowledgeApi,
        ApplicationPrincipal::KnowledgeWorker,
        ApplicationPrincipal::Runtime,
        ApplicationPrincipal::CapabilityWorker,
    ] {
        for epoch in [18, 20, 24, 25, 26, 28] {
            assert!(
                validate_snapshot_value(&snapshot(&contract, epoch), principal, &contract).is_err()
            );
        }
        let current = snapshot(&contract, 27);
        assert!(validate_snapshot_value(&current, principal, &contract).is_ok());
        let mut bad = current;
        bad["changes"][26]["checksum_sha256"] = "0".repeat(64).into();
        assert!(validate_snapshot_value(&bad, principal, &contract).is_err());
    }
}

#[test]
fn overlay_snapshots_are_exact_native_source_exports() {
    let expected = compiled_admission_snapshot().expect("valid source");
    for bytes in [
        include_bytes!("../../../../../agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/schema_admission_expected.json").as_slice(),
        include_bytes!("../../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/schema_admission_expected.json").as_slice(),
    ] {
        let actual: Value = serde_json::from_slice(bytes).expect("valid compiled snapshot");
        assert_eq!(actual, expected);
    }
}

fn fixture_contract(source_epoch: i32) -> AdmissionContract {
    let mut contract = compiled_contract(BASELINE_MANIFEST_BYTES, EPOCH_MANIFEST_BYTES)
        .expect("checked-in frozen source manifests parse");
    while contract.changes.len() < source_epoch as usize {
        let sequence = contract.changes.len() as i32 + 1;
        contract.changes.push(ExpectedChange {
            sequence,
            name: format!("test_successor_{sequence}"),
            checksum_sha256: format!("{:064x}", sequence),
        });
    }
    contract.changes.truncate(source_epoch as usize);
    contract.source_epoch = source_epoch;
    contract
}

fn snapshot(contract: &AdmissionContract, epoch: i32) -> Value {
    snapshot_value(contract, epoch)
}

fn measured_contract() -> AdmissionContract {
    fixture_contract(20)
}

#[test]
fn memory_knowledge_and_durable_profiles_have_distinct_windows() {
    let contract = measured_contract();
    for epoch in 18..=20 {
        assert!(
            validate_snapshot_value(
                &snapshot(&contract, epoch),
                ApplicationPrincipal::Gateway,
                &contract,
            )
            .is_ok()
        );
    }
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 18),
            ApplicationPrincipal::KnowledgeApi,
            &contract,
        )
        .is_err()
    );
    for epoch in 19..=20 {
        for principal in [
            ApplicationPrincipal::KnowledgeApi,
            ApplicationPrincipal::KnowledgeWorker,
        ] {
            assert!(
                validate_snapshot_value(&snapshot(&contract, epoch), principal, &contract).is_ok()
            );
        }
    }
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 19),
            ApplicationPrincipal::GatewayDurable,
            &contract,
        )
        .is_err()
    );
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 20),
            ApplicationPrincipal::GatewayDurable,
            &contract,
        )
        .is_ok()
    );
}

#[test]
fn frozen_baseline_marker_and_change_checksums_are_exact() {
    let contract = measured_contract();
    let mut value = snapshot(&contract, 20);
    value["baselines"][0]["structural_sha256"] = Value::String("f".repeat(64));
    assert!(validate_snapshot_value(&value, ApplicationPrincipal::Gateway, &contract).is_err());

    let mut value = snapshot(&contract, 20);
    value["changes"][17]["checksum_sha256"] = Value::String("e".repeat(64));
    assert!(validate_snapshot_value(&value, ApplicationPrincipal::Gateway, &contract).is_err());
}

#[test]
fn malformed_sparse_future_and_attempt_snapshots_fail_closed() {
    let contract = measured_contract();
    let mut malformed = snapshot(&contract, 20);
    malformed["changes"] = Value::Null;
    assert!(validate_snapshot_value(&malformed, ApplicationPrincipal::Gateway, &contract).is_err());

    let mut sparse = snapshot(&contract, 20);
    sparse["changes"].as_array_mut().unwrap().remove(4);
    assert!(validate_snapshot_value(&sparse, ApplicationPrincipal::Gateway, &contract).is_err());

    let mut future = snapshot(&contract, 20);
    future["epoch"] = serde_json::json!(21);
    assert!(validate_snapshot_value(&future, ApplicationPrincipal::Gateway, &contract).is_err());

    let mut attempts = snapshot(&contract, 20);
    attempts["has_attempts"] = Value::Bool(true);
    assert!(validate_snapshot_value(&attempts, ApplicationPrincipal::Gateway, &contract).is_err());

    let mut incomplete = snapshot(&contract, 20);
    incomplete["has_incomplete_attempts"] = Value::Bool(true);
    assert!(
        validate_snapshot_value(&incomplete, ApplicationPrincipal::Gateway, &contract).is_err()
    );
}

#[test]
fn denied_reader_never_falls_back_to_direct_ledger_access() {
    let contract = measured_contract();
    assert!(matches!(
        validate_reader_result(
            Err(ReaderFailure::Denied),
            ApplicationPrincipal::Gateway,
            &contract,
        ),
        Err(AuthorityError::Refused(_))
    ));
}

#[test]
fn inactive_or_partial_source_manifest_cannot_activate_new_profiles() {
    let contract = fixture_contract(17);
    let snapshot = snapshot(&contract, 17);
    assert!(validate_snapshot_value(&snapshot, ApplicationPrincipal::Gateway, &contract).is_err());
    assert!(
        validate_snapshot_value(&snapshot, ApplicationPrincipal::KnowledgeApi, &contract).is_err()
    );
}

#[test]
fn compiled_021_requires_knowledge_worker_at_exactly_021() {
    let contract = fixture_contract(21);
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 20),
            ApplicationPrincipal::KnowledgeWorker,
            &contract,
        )
        .is_err()
    );
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 21),
            ApplicationPrincipal::KnowledgeWorker,
            &contract,
        )
        .is_ok()
    );
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 22),
            ApplicationPrincipal::Gateway,
            &contract,
        )
        .is_err()
    );
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 20),
            ApplicationPrincipal::Gateway,
            &contract,
        )
        .is_ok()
    );
}

#[test]
fn compiled_022_preserves_durable_minimum_and_rejects_future_epochs() {
    let contract = fixture_contract(22);
    for epoch in 20..=22 {
        assert!(
            validate_snapshot_value(
                &snapshot(&contract, epoch),
                ApplicationPrincipal::GatewayDurable,
                &contract,
            )
            .is_ok()
        );
    }
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 23),
            ApplicationPrincipal::GatewayDurable,
            &contract,
        )
        .is_err()
    );
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 21),
            ApplicationPrincipal::KnowledgeWorker,
            &contract,
        )
        .is_ok()
    );
    assert!(
        validate_snapshot_value(
            &snapshot(&contract, 22),
            ApplicationPrincipal::KnowledgeWorker,
            &contract,
        )
        .is_ok()
    );
}

#[test]
fn compiled_028_requires_the_request_meter_contract() {
    let contract = fixture_contract(28);
    for principal in [ApplicationPrincipal::Gateway, ApplicationPrincipal::GatewayDurable,
        ApplicationPrincipal::KnowledgeApi, ApplicationPrincipal::KnowledgeWorker,
        ApplicationPrincipal::Runtime, ApplicationPrincipal::CapabilityWorker] {
        assert!(validate_snapshot_value(&snapshot(&contract, 28), principal, &contract).is_ok());
        for rejected in [25, 26, 27, 29] {
            assert!(validate_snapshot_value(&snapshot(&contract, rejected), principal, &contract).is_err());
        }
    }
}
