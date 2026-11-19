use super::*;
fn lock() -> Value {
    let mut artifacts = serde_json::Map::new();
    for &(id, binary) in ARTIFACTS {
        artifacts.insert(
            id.into(),
            json!({"binary":binary,"protocol":protocol(id),"candidate_start_allowed":false,
            "image_ref":null,"image_digest":null,"platforms":["linux/amd64"]}),
        );
    }
    json!({"source":{"upstream_sha":"a".repeat(40),"fork_sha":"a".repeat(40)},
        "build":{"overlay_sha256":"b".repeat(64),"capability_worker_schema_sha256":"c".repeat(64),"app_server_schema_sha256":"e".repeat(64)},
        "release_state":"local_source_locked","oci":{"artifacts":artifacts}})
}
fn inspection(id: &str) -> Value {
    let index = ARTIFACTS.iter().position(|(key, _)| *key == id).unwrap();
    let (family, schema) = if matches!(id, "capability_worker" | "python_sandbox") {
        ("capability-worker", "c")
    } else {
        ("agent-runtime", "e")
    };
    let prefix = format!("com.misaya.ai-platform.{family}");
    let mut labels = serde_json::Map::new();
    labels.insert(
        "org.opencontainers.image.revision".into(),
        json!(format!("{}+{}", "a".repeat(40), "b".repeat(12))),
    );
    labels.insert(format!("{prefix}.schema-sha256"), json!(schema.repeat(64)));
    labels.insert(format!("{prefix}.overlay-sha256"), json!("b".repeat(64)));
    labels.insert(format!("{prefix}.artifact"), json!(id));
    labels.insert(format!("{prefix}.binary"), json!(ARTIFACTS[index].1));
    json!({"Id":format!("sha256:{}", (index + 1).to_string().repeat(64)),"Os":"linux","Architecture":"amd64","Config":{"Labels":labels}})
}
fn release() -> (Value, Vec<(&'static str, Value)>) {
    let mut lock = lock();
    let mut inspections = Vec::new();
    for &(id, _, _) in RELEASE {
        let image = inspection(id);
        let digest = image["Id"].as_str().unwrap();
        let artifact = &mut lock["oci"]["artifacts"][id];
        artifact["candidate_start_allowed"] = json!(true);
        artifact["image_digest"] = json!(digest);
        artifact["image_ref"] = json!(format!("local-image://{digest}"));
        inspections.push((id, image));
    }
    lock["release_state"] = json!("local_image_locked");
    (lock, inspections)
}
#[test]
fn sandbox_has_its_own_required_binary_protocol_and_worker_label_namespace() {
    let source = lock();
    validate_artifacts(&source, false, None).unwrap();
    assert_eq!(
        source["oci"]["artifacts"]["python_sandbox"]["protocol"],
        "sandbox-job/v1"
    );
    let (digest, platform) =
        checked_image(&source, "python_sandbox", &inspection("python_sandbox")).unwrap();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(platform, "linux/amd64");
    let (release, images) = release();
    assert_eq!(
        validate_artifacts(&release, false, None).unwrap()["release_admission"],
        true
    );
    assert_eq!(
        verify_inspections(&release, &images).unwrap()["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}
#[test]
fn control_worker_runtime_and_sandbox_images_cannot_impersonate_each_other() {
    let source = lock();
    for requested in ["python_sandbox", "capability_worker", "agent_runtime"] {
        for supplied in ["python_sandbox", "capability_worker", "agent_runtime"] {
            assert_eq!(
                checked_image(&source, requested, &inspection(supplied)).is_ok(),
                requested == supplied
            );
        }
    }
    let mut renamed_control = inspection("capability_worker");
    renamed_control["Config"]["Labels"]["com.misaya.ai-platform.capability-worker.artifact"] =
        json!("python_sandbox");
    assert!(checked_image(&source, "python_sandbox", &renamed_control).is_err());
}
#[test]
fn sandbox_requires_exact_revision_full_overlay_schema_artifact_and_binary() {
    let source = lock();
    for key in [
        "org.opencontainers.image.revision",
        "com.misaya.ai-platform.capability-worker.overlay-sha256",
        "com.misaya.ai-platform.capability-worker.schema-sha256",
        "com.misaya.ai-platform.capability-worker.artifact",
        "com.misaya.ai-platform.capability-worker.binary",
    ] {
        for replacement in [Value::Null, json!("different"), json!("b".repeat(12))] {
            let mut image = inspection("python_sandbox");
            image["Config"]["Labels"][key] = replacement;
            assert!(
                checked_image(&source, "python_sandbox", &image).is_err(),
                "{key}"
            );
        }
    }
    for key in ["overlay_sha256", "capability_worker_schema_sha256"] {
        for replacement in [Value::Null, json!("b".repeat(12))] {
            let mut source = source.clone();
            source["build"][key] = replacement;
            assert!(
                checked_image(&source, "python_sandbox", &inspection("python_sandbox")).is_err()
            );
        }
    }
}
#[test]
fn immutable_image_id_and_nonempty_platform_remain_required() {
    for id in ["capability_worker", "python_sandbox"] {
        let mut wrong_overlay = inspection(id);
        wrong_overlay["Config"]["Labels"]["com.misaya.ai-platform.capability-worker.overlay-sha256"] =
            json!("wrong");
        assert!(checked_image(&lock(), id, &wrong_overlay).is_err());
        for (key, value) in [
            ("Id", json!("mutable:tag")),
            ("Id", json!(format!("sha256:{}", "A".repeat(64)))),
            ("Os", Value::Null),
            ("Architecture", json!("")),
        ] {
            let mut image = inspection(id);
            image[key] = value;
            assert!(checked_image(&lock(), id, &image).is_err());
        }
    }
    assert!(checked_image(&lock(), "unknown", &inspection("python_sandbox")).is_err());
}
#[test]
fn current_inventory_cannot_omit_or_corrupt_sandbox_even_for_source_checks() {
    let mut source = lock();
    source["oci"]["artifacts"]
        .as_object_mut()
        .unwrap()
        .remove("python_sandbox");
    assert!(validate_artifacts(&source, false, None).is_err());
    let mut extra = lock();
    extra["oci"]["artifacts"]["sandbox_alias"] =
        extra["oci"]["artifacts"]["python_sandbox"].clone();
    assert!(validate_artifacts(&extra, false, None).is_err());
    for (field, invalid) in [
        ("binary", json!("ai-platform-capability-worker")),
        ("protocol", Value::Null),
        ("protocol", json!("")),
        ("protocol", json!("wrong/v1")),
        ("platforms", json!([])),
        ("candidate_start_allowed", json!("true")),
    ] {
        let mut source = lock();
        source["oci"]["artifacts"]["python_sandbox"][field] = invalid;
        assert!(validate_artifacts(&source, false, None).is_err(), "{field}");
    }
}
#[test]
fn old_three_artifact_locks_are_audit_only_and_never_current_release_admission() {
    let (mut source, images) = release();
    source["source"]["upstream_sha"] = json!("092d3acd6bec3e3a14bdc7e7a2810ab628ab759d");
    source["oci"]["artifacts"]
        .as_object_mut()
        .unwrap()
        .remove("python_sandbox");
    let audit = validate_artifacts(&source, false, None).unwrap();
    assert_eq!(audit["historical_audit_only"], true);
    assert_eq!(audit["release_admission"], false);
    for required in ["agent_runtime", "capability_worker", "python_sandbox"] {
        assert!(validate_artifacts(&source, false, Some(required)).is_err());
    }
    assert!(validate_artifacts(&source, true, None).is_err());
    assert!(current_inventory(&source).is_err());
    assert!(verify_inspections(&source, &images).is_err());
}
#[test]
fn individual_recording_evidence_does_not_claim_complete_release() {
    let (mut source, images) = release();
    source["oci"]["artifacts"]["python_sandbox"]["candidate_start_allowed"] = json!(false);
    source["oci"]["artifacts"]["python_sandbox"]["image_ref"] = Value::Null;
    source["oci"]["artifacts"]["python_sandbox"]["image_digest"] = Value::Null;
    assert_eq!(
        validate_artifacts(&source, false, Some("agent_runtime")).unwrap()["release_admission"],
        false
    );
    assert!(validate_artifacts(&source, false, Some("python_sandbox")).is_err());
    assert!(validate_artifacts(&source, true, Some("agent_runtime")).is_err());
    assert!(verify_inspections(&source, &images).is_err());
    source["release_state"] = json!("published");
    for id in ["agent_runtime", "capability_worker"] {
        let digest = source["oci"]["artifacts"][id]["image_digest"]
            .as_str()
            .unwrap()
            .to_string();
        source["oci"]["artifacts"][id]["image_ref"] =
            json!(format!("example.invalid/{id}@{digest}"));
    }
    assert!(validate_artifacts(&source, false, None).is_err());
}
#[test]
fn three_actual_same_source_images_must_match_locked_digests_and_platforms() {
    let (source, images) = release();
    for index in 0..images.len() {
        let mut bad = images.clone();
        bad[index].1["Id"] = json!(format!("sha256:{}", "f".repeat(64)));
        assert!(verify_inspections(&source, &bad).is_err());
        let mut bad = images.clone();
        bad[index].1["Architecture"] = json!("arm64");
        assert!(verify_inspections(&source, &bad).is_err());
        let mut bad = images.clone();
        bad[index].1["Config"]["Labels"]["org.opencontainers.image.revision"] =
            json!(format!("{}+{}", "f".repeat(40), "b".repeat(12)));
        assert!(verify_inspections(&source, &bad).is_err());
    }
    let mut mixed_source = source.clone();
    let mut mixed = images.clone();
    mixed[2].1["Architecture"] = json!("arm64");
    mixed_source["oci"]["artifacts"]["python_sandbox"]["platforms"] = json!(["linux/arm64"]);
    assert!(verify_inspections(&mixed_source, &mixed).is_err());
}
#[test]
fn missing_duplicate_or_reused_inspections_cannot_satisfy_release() {
    let (mut source, images) = release();
    assert!(verify_inspections(&source, &images[..2]).is_err());
    let mut duplicate = images.clone();
    duplicate[2] = images[1].clone();
    assert!(verify_inspections(&source, &duplicate).is_err());
    let mut reused = images.clone();
    reused[2].1["Id"] = images[1].1["Id"].clone();
    source["oci"]["artifacts"]["python_sandbox"]["image_digest"] = images[1].1["Id"].clone();
    source["oci"]["artifacts"]["python_sandbox"]["image_ref"] = json!(format!(
        "local-image://{}",
        images[1].1["Id"].as_str().unwrap()
    ));
    assert!(verify_inspections(&source, &reused).is_err());
}
#[test]
fn published_sandbox_requires_its_own_matching_registry_digest() {
    let (mut source, mut images) = release();
    source["release_state"] = json!("published");
    for (id, image) in &mut images {
        let digest = format!(
            "sha256:{}",
            if *id == "python_sandbox" {
                "9"
            } else if *id == "agent_runtime" {
                "8"
            } else {
                "7"
            }
            .repeat(64)
        );
        let reference = format!("example.invalid/{id}@{digest}");
        source["oci"]["artifacts"][*id]["image_digest"] = json!(digest);
        source["oci"]["artifacts"][*id]["image_ref"] = json!(reference);
        image["RepoDigests"] = json!([reference]);
    }
    verify_inspections(&source, &images).unwrap();
    images[2].1["RepoDigests"] = images[1].1["RepoDigests"].clone();
    assert!(verify_inspections(&source, &images).is_err());
}
