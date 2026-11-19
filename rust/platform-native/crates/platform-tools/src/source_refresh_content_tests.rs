use super::*;

struct Fixture {
    root: PathBuf,
    revision: String,
    lock: Value,
    cargo_lock: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("content-refresh-{}", uuid::Uuid::new_v4()));
        let put = |path: &str, bytes: &[u8]| {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        };
        put(
            "rust/agent-runtime-overlay/kernel-rs/Cargo.toml",
            b"[workspace]\n",
        );
        put(
            "rust/agent-runtime-overlay/kernel-rs/Cargo.lock",
            b"version = 4\n",
        );
        put(
            "rust/agent-runtime-overlay/kernel-rs/snapshot.json",
            b"{}\n",
        );
        put(
            "rust/agent-runtime-overlay/kernel-rs/module.rs",
            b"pub mod child;\n",
        );
        put(
            "rust/agent-runtime-overlay/kernel-rs/module/child.rs",
            b"// content\n",
        );
        let identity = bundle_identity(&root.join(OVERLAY), true).unwrap();
        let cargo_lock = digest(&root.join(OVERLAY).join("kernel-rs/Cargo.lock")).unwrap();
        let source = json!({"upstream_sha":"a".repeat(40),"fork_sha":"a".repeat(40),"git_tree_sha":"b".repeat(40),"upstream_url":"https://github.com/openai/codex.git"});
        let metadata = json!({"packages":[{"id":"worker","name":"ai-platform-capability-worker","version":"0.0.0","source":null}],"resolve":{"nodes":[{"id":"worker","dependencies":[]}]}});
        let schema = "c".repeat(64);
        let mut sbom = worker_sbom(&metadata, &identity, &schema, &cargo_lock, &source).unwrap();
        // The pinned old producer's UUID seed is opaque; metadata/hash binding
        // remains exact and the refreshed identity uses the current generator.
        sbom["serialNumber"] = json!("urn:uuid:e5c493f9-0c88-5de7-a6a7-5d39143b5d97");
        let sbom_path = "deploy/agent-runtime-source/capability-worker-sbom.cdx.json";
        atomic(&root.join(sbom_path), &sbom).unwrap();
        let sbom_sha = digest(&root.join(sbom_path)).unwrap();
        let manifest_path = "rust/agent-runtime-overlay/manifest.json";
        let manifest = json!({"schema_version":"ai-platform/agent-runtime-overlay/v2","sha256":identity["sha256"],"file_count":identity["file_count"],"cargo_lock_sha256":cargo_lock});
        atomic(&root.join(manifest_path), &manifest).unwrap();
        let manifest_sha = digest(&root.join(manifest_path)).unwrap();
        let receipt_path = "deploy/agent-runtime-source/source-receipt.json";
        let receipt = json!({"source":source,"overlay":{"sha256":identity["sha256"],"file_count":identity["file_count"],"cargo_lock_sha256":cargo_lock,"manifest_sha256":manifest_sha},"capability_worker_sbom":{"sha256":sbom_sha,"component_count":1}});
        atomic(&root.join(receipt_path), &receipt).unwrap();
        let lock = json!({"source":source,"build":{"source_receipt":receipt_path,"source_receipt_sha256":digest(&root.join(receipt_path)).unwrap(),"capability_worker_sbom":sbom_path,"capability_worker_sbom_sha256":sbom_sha,"capability_worker_schema_sha256":schema,"overlay_manifest_sha256":manifest_sha,"overlay_sha256":identity["sha256"],"overlay_file_count":identity["file_count"],"overlay_cargo_lock_sha256":cargo_lock},"oci":{"platforms":["linux/arm64"]}});
        atomic(&root.join(LOCK), &lock).unwrap();
        git(&root, &["init", "--quiet"]).unwrap();
        git(&root, &["add", "."]).unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "locked baseline",
            ],
        )
        .unwrap();
        let revision = String::from_utf8(git(&root, &["rev-parse", "HEAD"]).unwrap())
            .unwrap()
            .trim()
            .to_owned();
        Self {
            root,
            revision,
            lock,
            cargo_lock,
        }
    }
    fn refresh(&self) -> Result<Value> {
        reuse_worker_sbom(
            &self.root,
            &self.revision,
            &self.root.join(LOCK),
            &self.lock,
            &bundle_identity(&self.root.join(OVERLAY), true)?,
            "new-schema",
            &self.cargo_lock,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn content_change_reuses_exact_graph_and_rebinds_with_the_normal_generator() {
    let fixture = Fixture::new();
    let old = object(
        &fixture
            .root
            .join("deploy/agent-runtime-source/capability-worker-sbom.cdx.json"),
    )
    .unwrap();
    std::fs::write(
        fixture.root.join(OVERLAY).join("kernel-rs/snapshot.json"),
        b"{\"epoch\":32}\n",
    )
    .unwrap();
    let new = fixture.refresh().unwrap();
    assert_eq!(old["components"], new["components"]);
    assert_eq!(old["dependencies"], new["dependencies"]);
    assert_ne!(old["serialNumber"], new["serialNumber"]);
    assert_ne!(old["metadata"], new["metadata"]);
}

#[test]
fn changed_or_added_dependency_definitions_require_actual_metadata() {
    for path in [
        "kernel-rs/Cargo.toml",
        "kernel-rs/Cargo.lock",
        "kernel-rs/new/Cargo.toml",
        "kernel-rs/.cargo/config.toml",
        "kernel-rs/rust-toolchain.toml",
    ] {
        let fixture = Fixture::new();
        let path = fixture.root.join(OVERLAY).join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"changed dependency input\n").unwrap();
        assert!(fixture.refresh().is_err());
    }
}

#[test]
fn mutated_baseline_evidence_and_mutable_refs_are_refused() {
    for path in [
        LOCK,
        "rust/agent-runtime-overlay/manifest.json",
        "deploy/agent-runtime-source/source-receipt.json",
        "deploy/agent-runtime-source/capability-worker-sbom.cdx.json",
    ] {
        let fixture = Fixture::new();
        let path = fixture.root.join(path);
        let mut value = object(&path).unwrap();
        value["tampered"] = json!(true);
        atomic(&path, &value).unwrap();
        assert!(fixture.refresh().is_err());
    }
    let mut fixture = Fixture::new();
    fixture.revision = "HEAD".into();
    assert!(fixture.refresh().is_err());
}
