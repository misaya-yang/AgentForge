use super::*;
fn golden() -> Golden {
    let request = json!({"query":[0.1,0.2,0.3],"limit":20,"with_payload":true,"filter":{"must":[{"key":"tenant_id","match":{"value":"t1"}},{"key":"dataset_id","match":{"value":"d1"}}]}});
    let mut negative_request = request.clone();
    negative_request["filter"]["must"][0]["match"]["value"] = json!("t2");
    Golden {
        schema: "qdrant-retrieval-golden/v1".into(),
        corpus_id: "unit-fixture".into(),
        corpus_kind: "fixture".into(),
        model_revision: "fixture-frozen-dense-3".into(),
        index_generation: "fixture-generation".into(),
        cases: vec![
            Case {
                id: "positive".into(),
                collection: "knowledge".into(),
                tenant_id: "t1".into(),
                dataset_id: "d1".into(),
                request: request.clone(),
                expected_ids: vec![json!(1)],
                relevance: BTreeMap::from([("1".into(), 3)]),
                negative: false,
            },
            Case {
                id: "negative".into(),
                collection: "knowledge".into(),
                tenant_id: "t2".into(),
                dataset_id: "d1".into(),
                request: negative_request,
                expected_ids: vec![],
                relevance: BTreeMap::new(),
                negative: true,
            },
        ],
    }
}
fn results() -> BTreeMap<String, Vec<Hit>> {
    BTreeMap::from([
        (
            "positive".into(),
            vec![Hit {
                id: json!(1),
                score: 0.7,
                payload_sha256: "fixture-payload".into(),
            }],
        ),
        ("negative".into(), vec![]),
    ])
}
struct Fixture {
    base: PathBuf,
    spec: Spec,
}
impl Fixture {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "agentforge-qdrant-upgrade-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&base).unwrap();
        let root = base.join("repository");
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let compose = root.join("compose.yml");
        write_new(&compose, &json!({"fixture":true})).unwrap();
        let env = base.join("env");
        write_new(&env, &json!({"fixture":true})).unwrap();
        let artifacts = base.join("artifacts");
        std::fs::create_dir(&artifacts).unwrap();
        let golden_file = base.join("golden.json");
        write_new(&golden_file, &golden()).unwrap();
        let endpoint = |port| Endpoint {
            url: format!("https://localhost:{port}"),
            api_key_file: base.join("key"),
            ca_file: base.join("ca"),
        };
        let spec = Spec {
            schema: "qdrant-upgrade/v1".into(),
            run_id: "fixture-run".into(),
            repository: root,
            artifacts,
            source: endpoint(6333),
            canary: endpoint(16333),
            rollback: endpoint(26333),
            deployment: deployment::Deployment::Compose {
                project: "fixture-project".into(),
                files: vec![compose],
                env_file: env,
                source_service: "qdrant".into(),
                canary_service: "qdrant-upgrade-canary".into(),
                rollback_service: "qdrant-upgrade-rollback".into(),
                source_volume: "source-volume".into(),
                canary_volume: "fixture-run-canary".into(),
                rollback_volume: "fixture-run-rollback".into(),
            },
            golden_file,
            corpus_kind: "fixture".into(),
            max_snapshot_bytes: 1024,
        };
        Self { base, spec }
    }
    fn snapshot(&self) -> Receipt {
        write_new(
            &self.spec.receipt("metadata"),
            &json!({"fixture":"not_official_metadata"}),
        )
        .unwrap();
        let file = self.spec.artifacts.join("knowledge.snapshot");
        write_new(&file, &json!({"fixture":"not_a_real_snapshot"})).unwrap();
        let (hash, bytes) = digest_file(&file).unwrap();
        let snapshot = Snapshot {
            collection: "knowledge".into(),
            file: "knowledge.snapshot".into(),
            sha256: hash,
            bytes,
            count: 1,
            config: json!({"params":{"vectors":{"size":3}}}),
            payload_schema: json!({}),
        };
        receipt(
            &self.spec,
            "snapshot",
            None,
            vec![snapshot],
            json!([]),
            results(),
        )
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}
#[test]
fn enforces_one_minor_and_no_downgrade_or_skip() {
    assert!(metadata::one_minor("1.18.2", "1.19.2"));
    for (a, b) in [
        ("1.18.2", "1.20.0"),
        ("1.19.2", "1.18.2"),
        ("1.18.2", "2.19.2"),
        ("1.18.2", "1.19.2-rc"),
        ("1.18", "1.19.2"),
    ] {
        assert!(!metadata::one_minor(a, b));
    }
}
#[test]
fn oci_verification_binds_raw_bytes_header_and_both_architectures() {
    let index = json!({"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[{"platform":{"os":"linux","architecture":"amd64"},"digest":format!("sha256:{}","a".repeat(64))},{"platform":{"os":"linux","architecture":"arm64"},"digest":format!("sha256:{}","b".repeat(64))}]});
    let bytes = serde_json::to_vec(&index).unwrap();
    let digest = format!("sha256:{}", sha(&bytes));
    assert_eq!(metadata::index(&bytes, &digest, &digest).unwrap().len(), 2);
    assert!(metadata::index(&bytes, "sha256:wrong", &digest).is_err());
    let mut corrupted = bytes.clone();
    corrupted.push(b' ');
    assert!(metadata::index(&corrupted, &digest, &digest).is_err());
    let mut missing = index;
    missing["manifests"].as_array_mut().unwrap().pop();
    let raw = serde_json::to_vec(&missing).unwrap();
    let digest = format!("sha256:{}", sha(&raw));
    assert!(metadata::index(&raw, &digest, &digest).is_err());
}
#[test]
fn private_https_endpoint_rejects_provider_or_url_escape() {
    for url in [
        "http://localhost:6333",
        "https://provider.example",
        "https://user:secret@qdrant:6333",
        "https://qdrant:6333/path",
        "https://qdrant:6333/?x=1",
        "https://8.8.8.8:6333",
    ] {
        assert!(transport::endpoint(url).is_err(), "{url}");
    }
    assert!(transport::endpoint("https://qdrant:6333").is_ok());
}
#[test]
fn golden_rejects_missing_filter_unfrozen_inference_and_missing_negatives() {
    let f = Fixture::new();
    let mut g = golden();
    assert!(g.validate(&f.spec).is_ok());
    g.cases[0].request["filter"] = Value::Null;
    assert!(g.validate(&f.spec).is_err());
    let mut g = golden();
    g.cases[0].request["query"] =
        json!({"text":"no model inference during upgrade","model":"provider"});
    assert!(g.validate(&f.spec).is_err());
    let mut g = golden();
    g.cases.pop();
    assert!(g.validate(&f.spec).is_err());
}
#[test]
fn parity_rejects_reorder_score_payload_missing_case_and_negative_leak() {
    let g = golden();
    let baseline = results();
    assert!(compare(&g, &baseline, &baseline).is_ok());
    for kind in 0..5 {
        let mut changed = baseline.clone();
        match kind {
            0 => changed.get_mut("positive").unwrap()[0].id = json!(2),
            1 => changed.get_mut("positive").unwrap()[0].score += 0.001,
            2 => changed.get_mut("positive").unwrap()[0].payload_sha256 = "changed".into(),
            3 => {
                changed.remove("negative");
            }
            _ => {
                changed
                    .get_mut("negative")
                    .unwrap()
                    .push(baseline["positive"][0].clone());
            }
        }
        assert!(compare(&g, &baseline, &changed).is_err(), "{kind}");
    }
}
#[test]
fn response_validation_rejects_cross_tenant_payload_and_duplicate_points() {
    let c = golden().cases.remove(0);
    let row = json!({"id":1,"score":0.7,"payload":{"tenant_id":"t1","dataset_id":"d1"}});
    assert!(transport::parse_hits(&c, &json!({"result":{"points":[row.clone()]}})).is_ok());
    let mut wrong = row.clone();
    wrong["payload"]["tenant_id"] = json!("t2");
    assert!(transport::parse_hits(&c, &json!({"result":{"points":[wrong]}})).is_err());
    assert!(transport::parse_hits(&c, &json!({"result":{"points":[row.clone(),row]}})).is_err());
}
#[test]
fn snapshot_tamper_and_duplicate_evidence_fail_closed() {
    let f = Fixture::new();
    let r = f.snapshot();
    write_new(&f.spec.receipt("snapshot"), &r).unwrap();
    assert!(read_receipt(&f.spec, "snapshot", &golden()).is_ok());
    assert!(write_new(&f.spec.receipt("snapshot"), &r).is_err());
    std::fs::write(f.spec.artifacts.join("knowledge.snapshot"), b"tampered").unwrap();
    assert!(read_receipt(&f.spec, "snapshot", &golden()).is_err());
}
#[test]
fn deployment_config_content_change_invalidates_existing_receipt() {
    let f = Fixture::new();
    let r = f.snapshot();
    write_new(&f.spec.receipt("snapshot"), &r).unwrap();
    assert!(read_receipt(&f.spec, "snapshot", &golden()).is_ok());
    if let deployment::Deployment::Compose { files, .. } = &f.spec.deployment {
        std::fs::write(&files[0], b"changed compose routing").unwrap();
    }
    assert!(read_receipt(&f.spec, "snapshot", &golden()).is_err());
}
#[test]
fn same_source_volume_endpoint_and_in_tree_artifacts_are_refused() {
    let mut f = Fixture::new();
    assert!(f.spec.validate().is_ok());
    f.spec.canary.url = f.spec.source.url.clone();
    assert!(f.spec.validate().is_err());
    f.spec.canary.url = "https://localhost:16333".into();
    if let deployment::Deployment::Compose {
        source_volume,
        canary_volume,
        ..
    } = &mut f.spec.deployment
    {
        *canary_volume = source_volume.clone();
    }
    assert!(f.spec.validate().is_err());
    f.spec.artifacts = f.spec.repository.join("artifacts");
    assert!(f.spec.validate().is_err());
}
#[test]
fn explicit_apply_is_required_and_fixture_receipt_never_claims_product_acceptance() {
    assert!(apply(&Args::default()).is_err());
    let f = Fixture::new();
    assert_eq!(f.snapshot().product_acceptance, "not_run_fixture_only");
}
#[test]
fn config_parity_allows_only_new_defaults_not_changed_old_values() {
    let old = json!({"params":{"vectors":{"size":1024},"on_disk_payload":false}});
    assert!(transport::retained(
        &old,
        &json!({"params":{"vectors":{"size":1024},"on_disk_payload":false,"new_default":null}})
    ));
    assert!(!transport::retained(
        &old,
        &json!({"params":{"vectors":{"size":384},"on_disk_payload":false}})
    ));
}
#[test]
fn path_components_cannot_escape_snapshot_directory() {
    for s in [
        "..",
        ".",
        "../snapshot",
        "/snapshot",
        "c?x=y",
        "c\\snapshot",
        "c%2fsnapshot",
    ] {
        assert!(!component(s));
    }
    assert!(component("collection-2026-10-17.snapshot"));
}

#[test]
fn changed_previous_stage_or_missing_upgrade_cannot_authorize_next_stage() {
    let f = Fixture::new();
    let snapshot = f.snapshot();
    write_new(&f.spec.receipt("snapshot"), &snapshot).unwrap();
    let restored = receipt(
        &f.spec,
        "restore",
        Some("snapshot"),
        snapshot.snapshots.clone(),
        snapshot.aliases.clone(),
        snapshot.results.clone(),
    )
    .unwrap();
    write_new(&f.spec.receipt("restore"), &restored).unwrap();
    assert!(read_receipt(&f.spec, "restore", &golden()).is_ok());
    assert!(read_receipt(&f.spec, "golden", &golden()).is_err());
    let mut changed = snapshot;
    changed.snapshots[0].count += 1;
    std::fs::write(
        f.spec.receipt("snapshot"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(read_receipt(&f.spec, "restore", &golden()).is_err());
}
#[test]
fn fixture_cannot_render_even_at_the_final_emission_boundary() {
    let f = Fixture::new();
    assert!(deployment::render(&f.spec, "fixture-receipt").is_err());
    assert!(!f.spec.artifacts.join("admission.json").exists());
}
#[cfg(unix)]
#[test]
fn private_inputs_reject_world_readable_files_and_symlinks() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let f = Fixture::new();
    let input = f.base.join("secret");
    write_new(&input, &json!({"private":true})).unwrap();
    assert!(private_read(&input, 1024).is_ok());
    std::fs::set_permissions(&input, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(private_read(&input, 1024).is_err());
    let link = f.base.join("link");
    symlink(input, &link).unwrap();
    assert!(private_read(&link, 1024).is_err());
}

#[cfg(unix)]
#[test]
fn artifact_parent_symlink_cannot_store_private_snapshot_inside_repository() {
    let mut f = Fixture::new();
    let link = f.base.join("looks-external");
    std::os::unix::fs::symlink(&f.spec.repository, &link).unwrap();
    f.spec.artifacts = link.join("snapshots");
    assert!(f.spec.validate().is_err());
}
