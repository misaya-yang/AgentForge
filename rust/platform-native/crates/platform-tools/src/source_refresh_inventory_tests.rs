use super::*;

struct Fixture {
    directory: PathBuf,
    checkout: PathBuf,
    kernel: PathBuf,
    sha: String,
    upstream_blob: String,
}
impl Fixture {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("refresh-preimages-{}", uuid::Uuid::new_v4()));
        let checkout = directory.join("upstream");
        let kernel = directory.join("overlay/kernel-rs");
        std::fs::create_dir_all(checkout.join("codex-rs")).unwrap();
        std::fs::create_dir_all(&kernel).unwrap();
        std::fs::write(checkout.join("codex-rs/shared.rs"), "upstream preimage\n").unwrap();
        std::fs::write(
            checkout.join("codex-rs/new-upstream.rs"),
            "existing upstream\n",
        )
        .unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "fixture",
            ],
        ] {
            git(&checkout, &args).unwrap();
        }
        let sha = String::from_utf8(git(&checkout, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_string();
        let upstream_blob =
            String::from_utf8(git(&checkout, &["rev-parse", "HEAD:codex-rs/shared.rs"]).unwrap())
                .unwrap()
                .trim()
                .to_string();
        std::fs::write(kernel.join("shared.rs"), "platform modification\n").unwrap();
        std::fs::write(kernel.join("platform.rs"), "platform only\n").unwrap();
        Self {
            directory,
            checkout,
            kernel,
            sha,
            upstream_blob,
        }
    }
    fn source(&self) -> Value {
        json!({"upstream_sha":self.sha,"fork_sha":self.sha})
    }
    fn manifest(&self) -> Value {
        json!({"schema_version":"ai-platform/agent-runtime-overlay/v2", "upstream_sha":self.sha,
            "source_revision":self.sha,"upstream_blobs":{"shared.rs":self.upstream_blob,"platform.rs":null}})
    }
    fn refresh(&self, manifest: &mut Value, source: Option<&Path>) -> Result<()> {
        refresh_preimages(manifest, &self.source(), &self.kernel, source)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn additions_and_removals_are_classified_against_actual_upstream_tree() {
    let fixture = Fixture::new();
    let mut manifest = fixture.manifest();
    std::fs::remove_file(fixture.kernel.join("platform.rs")).unwrap();
    std::fs::write(fixture.kernel.join("new-platform.rs"), "local addition").unwrap();
    std::fs::write(fixture.kernel.join("new-upstream.rs"), "local override").unwrap();
    fixture
        .refresh(&mut manifest, Some(&fixture.checkout))
        .unwrap();
    let blobs = manifest["upstream_blobs"].as_object().unwrap();
    assert!(!blobs.contains_key("platform.rs"));
    assert_eq!(blobs["new-platform.rs"], Value::Null);
    let expected = String::from_utf8(
        git(
            &fixture.checkout,
            &["rev-parse", "HEAD:codex-rs/new-upstream.rs"],
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(blobs["new-upstream.rs"], expected.trim());
    assert_eq!(blobs["shared.rs"], fixture.upstream_blob);
    let before = manifest.clone();
    fixture.refresh(&mut manifest, None).unwrap();
    assert_eq!(manifest, before);
}

#[test]
fn unchanged_inventory_preserves_old_no_source_interface() {
    let fixture = Fixture::new();
    let mut manifest = fixture.manifest();
    let before = manifest.clone();
    fixture.refresh(&mut manifest, None).unwrap();
    assert_eq!(manifest, before);
}

#[test]
fn inventory_changes_without_checkout_cannot_invent_null_preimages() {
    let fixture = Fixture::new();
    for addition in [true, false] {
        let mut manifest = fixture.manifest();
        let before = manifest.clone();
        if addition {
            std::fs::write(fixture.kernel.join("new-upstream.rs"), "override").unwrap();
        } else {
            std::fs::remove_file(fixture.kernel.join("platform.rs")).unwrap();
        }
        assert!(fixture.refresh(&mut manifest, None).is_err());
        assert_eq!(manifest, before);
    }
}

#[test]
fn existing_upstream_platform_and_removed_preimage_tampering_is_refused() {
    let fixture = Fixture::new();
    for (path, replacement) in [
        ("shared.rs", Value::Null),
        ("shared.rs", json!("f".repeat(40))),
        ("platform.rs", json!(fixture.upstream_blob)),
        ("shared.rs", json!(17)),
    ] {
        let mut manifest = fixture.manifest();
        manifest["upstream_blobs"][path] = replacement;
        let before = manifest.clone();
        assert!(
            fixture
                .refresh(&mut manifest, Some(&fixture.checkout))
                .is_err()
        );
        assert_eq!(manifest, before);
    }
    let mut manifest = fixture.manifest();
    manifest["upstream_blobs"]["shared.rs"] = Value::Null;
    std::fs::remove_file(fixture.kernel.join("shared.rs")).unwrap();
    assert!(
        fixture
            .refresh(&mut manifest, Some(&fixture.checkout))
            .is_err()
    );
}

#[test]
fn missing_dirty_foreign_and_nested_checkouts_are_refused() {
    let fixture = Fixture::new();
    for checkout in [
        fixture.directory.join("missing"),
        fixture.checkout.join("codex-rs"),
    ] {
        assert!(
            fixture
                .refresh(&mut fixture.manifest(), Some(&checkout))
                .is_err()
        );
    }
    let mut wrong_source = fixture.source();
    wrong_source["upstream_sha"] = json!("f".repeat(40));
    let mut manifest = fixture.manifest();
    manifest["upstream_sha"] = wrong_source["upstream_sha"].clone();
    assert!(
        refresh_preimages(
            &mut manifest,
            &wrong_source,
            &fixture.kernel,
            Some(&fixture.checkout)
        )
        .is_err()
    );
    std::fs::write(
        fixture.checkout.join("codex-rs/shared.rs"),
        "dirty upstream",
    )
    .unwrap();
    assert!(
        fixture
            .refresh(&mut fixture.manifest(), Some(&fixture.checkout))
            .is_err()
    );
}

#[test]
fn revision_drift_and_invalid_inventories_are_refused_even_without_checkout() {
    let fixture = Fixture::new();
    for field in ["source_revision", "upstream_sha"] {
        let mut manifest = fixture.manifest();
        manifest[field] = json!("f".repeat(40));
        assert!(fixture.refresh(&mut manifest, None).is_err());
    }
    let mut manifest = fixture.manifest();
    manifest["upstream_blobs"] = Value::Null;
    assert!(fixture.refresh(&mut manifest, None).is_err());
    let mut manifest = fixture.manifest();
    manifest["upstream_blobs"]["../escape"] = Value::Null;
    assert!(
        fixture
            .refresh(&mut manifest, Some(&fixture.checkout))
            .is_err()
    );
}

#[test]
fn checkout_head_drift_is_refused_even_when_original_commit_still_exists() {
    let fixture = Fixture::new();
    std::fs::write(fixture.checkout.join("codex-rs/shared.rs"), "next commit").unwrap();
    git(&fixture.checkout, &["add", "."]).unwrap();
    git(
        &fixture.checkout,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "foreign revision",
        ],
    )
    .unwrap();
    assert!(git(&fixture.checkout, &["cat-file", "-e", &fixture.sha]).is_ok());
    let mut manifest = fixture.manifest();
    let before = manifest.clone();
    assert!(
        fixture
            .refresh(&mut manifest, Some(&fixture.checkout))
            .is_err()
    );
    assert_eq!(manifest, before);
}

#[test]
fn source_refresh_adds_sandbox_with_nonempty_protocol_and_revokes_every_image() {
    let mut lock = json!({"release_state":"local_image_locked","oci":{"artifacts":{
        "agent_runtime":{"binary":"old","protocol":"old/v1","candidate_start_allowed":true,"image_ref":"old","image_digest":"old","platforms":["linux/amd64"]},
        "capability_worker":{"candidate_start_allowed":true,"image_ref":"old","image_digest":"old"}
    }}});
    invalidate(&mut lock);
    assert_eq!(lock["release_state"], "local_source_locked");
    assert_eq!(lock["oci"]["artifacts"].as_object().unwrap().len(), 4);
    for &(id, binary) in ARTIFACTS {
        let artifact = &lock["oci"]["artifacts"][id];
        assert_eq!(artifact["binary"], binary);
        assert_eq!(artifact["protocol"].as_str(), images::protocol(id));
        assert!(
            artifact["protocol"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert_eq!(artifact["candidate_start_allowed"], false);
        assert!(artifact["image_ref"].is_null());
        assert!(artifact["image_digest"].is_null());
    }
    assert_eq!(
        lock["oci"]["artifacts"]["python_sandbox"]["protocol"],
        "sandbox-job/v1"
    );
    assert_eq!(
        lock["oci"]["artifacts"]["agent_runtime"]["platforms"],
        json!(["linux/amd64"])
    );
}
