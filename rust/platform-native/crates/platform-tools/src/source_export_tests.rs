use super::*;

struct Fixture {
    root: PathBuf,
    source: PathBuf,
    schema: PathBuf,
    evidence: Value,
    unit: Value,
    runs: Vec<Value>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("docker-source-export-{}", uuid::Uuid::new_v4()));
        let source = root.join("composed");
        let schema = root.join("schema");
        std::fs::create_dir_all(source.join("codex-rs/crate")).unwrap();
        std::fs::create_dir_all(&schema).unwrap();
        std::fs::write(
            source.join("codex-rs/crate/Cargo.toml"),
            "[package]\nname='fixture'\n",
        )
        .unwrap();
        std::fs::write(source.join("codex-rs/Cargo.lock"), "locked source").unwrap();
        let source = source.canonicalize().unwrap();
        let payload = snapshot(&source).unwrap();
        for name in ["source_inventory_before", "source_inventory_after"] {
            std::fs::write(root.join(name), serde_json::to_vec(&payload).unwrap()).unwrap();
        }
        std::fs::write(root.join("schema_stdout"), "").unwrap();
        let unit = json!({"target":"a".repeat(40),"target_tree":"b".repeat(40),"resolved_lock_sha256":"c".repeat(64)});
        let binding = json!({"target_commit":unit["target"],"target_tree":unit["target_tree"],"source_unit_sha256":"d".repeat(64),
            "resolved_lock_sha256":unit["resolved_lock_sha256"],"source_inventory_sha256":digest(&root.join("source_inventory_before")).unwrap()});
        let evidence = json!({"schema":"ai-platform/composed-source-export/v2","execution":{
            "kind":"docker","container_source_root":"/source","container_cwd":"/source/codex-rs","container_schema_dir":"/export/schema",
            "host_source_root":source,"host_cwd":source.join("codex-rs"),"source_binding":binding}});
        let run = json!({"command":["cargo"],"cwd":"/source/codex-rs","source_binding":binding,"stdout_sha256":hash(b""),"exit_code":0,"stop_reason":null});
        Self {
            root,
            source,
            schema,
            evidence,
            unit,
            runs: vec![run.clone(), run],
        }
    }
    fn check(&self) -> Result<ExportPaths> {
        paths(
            &self.evidence,
            &self.unit,
            &"d".repeat(64),
            &self.source,
            &self.schema,
            &|name| Ok(self.root.join(name)),
            &[&self.runs[0], &self.runs[1]],
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn docker_mapping_preserves_real_workspace_and_rejects_escape_paths() {
    let fixture = Fixture::new();
    let mapping = fixture.check().unwrap();
    assert!(mapping.workspace_matches("/source/codex-rs").unwrap());
    assert!(!mapping.workspace_matches("/unrelated/codex-rs").unwrap());
    assert_eq!(
        mapping
            .manifest("/source/codex-rs/crate/Cargo.toml")
            .unwrap(),
        fixture.source.join("codex-rs/crate/Cargo.toml")
    );
    for raw in [
        "/source-extra/codex-rs/crate/Cargo.toml",
        "/source/codex-rs/../Cargo.toml",
        "/source//codex-rs/crate/Cargo.toml",
        "/other/crate/Cargo.toml",
        "/source/codex-rs/Cargo.lock",
    ] {
        assert!(mapping.manifest(raw).is_err(), "{raw}");
    }
    std::os::unix::fs::symlink(
        fixture.source.join("codex-rs/crate"),
        fixture.source.join("alias"),
    )
    .unwrap();
    assert!(mapping.manifest("/source/alias/Cargo.toml").is_err());
}
#[test]
fn docker_mapping_requires_every_explicit_path_and_source_binding() {
    for field in [
        "kind",
        "container_source_root",
        "container_cwd",
        "container_schema_dir",
        "host_source_root",
        "host_cwd",
        "source_binding",
    ] {
        let mut fixture = Fixture::new();
        fixture.evidence["execution"][field] = json!("arbitrary-alias");
        assert!(fixture.check().is_err(), "{field}");
    }
    for field in ["target", "target_tree", "resolved_lock_sha256"] {
        let mut fixture = Fixture::new();
        fixture.unit[field] = json!("different source");
        assert!(fixture.check().is_err(), "{field}");
    }
}
#[test]
fn inventory_must_cover_the_entire_unchanged_composed_source() {
    for mode in ["changed", "added", "removed", "after", "mode"] {
        let fixture = Fixture::new();
        let file = fixture.source.join("codex-rs/crate/Cargo.toml");
        match mode {
            "changed" => std::fs::write(file, "different").unwrap(),
            "added" => std::fs::write(fixture.source.join("unlisted.rs"), "extra").unwrap(),
            "removed" => std::fs::remove_file(file).unwrap(),
            "after" => std::fs::write(fixture.root.join("source_inventory_after"), "{}").unwrap(),
            "mode" => {
                std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap()
            }
            _ => unreachable!(),
        }
        assert!(fixture.check().is_err(), "{mode}");
    }
}
#[test]
fn source_links_are_hashed_as_links_without_dereferencing() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink("not-present", fixture.source.join("link")).unwrap();
    let inventory = snapshot(&fixture.source).unwrap();
    let link = inventory["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["path"] == "link")
        .unwrap();
    assert_eq!(link["file_mode"], "120000");
    assert_eq!(link["sha256"], hash(b"not-present"));
}
#[test]
fn command_receipts_cannot_invent_source_bindings_or_schema_stdout() {
    for which in 0..2 {
        let mut fixture = Fixture::new();
        fixture.runs[which]["source_binding"]["target_commit"] = json!("f".repeat(40));
        assert!(fixture.check().is_err());
    }
    let mut fixture = Fixture::new();
    fixture.runs[0]["command"][0] = json!("echo");
    assert!(fixture.check().is_err());
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("schema_stdout"), "unbound output").unwrap();
    assert!(fixture.check().is_err());
}
#[test]
fn failed_stopped_or_foreign_cwd_commands_are_rejected() {
    let fixture = Fixture::new();
    let mapping = fixture.check().unwrap();
    mapping
        .validate_runs(&[&fixture.runs[0], &fixture.runs[1]])
        .unwrap();
    for (field, invalid) in [
        ("exit_code", json!(1)),
        ("stop_reason", json!("interrupted")),
        ("cwd", json!("/arbitrary/alias")),
        ("command", json!("cargo")),
    ] {
        let mut run = fixture.runs[0].clone();
        run[field] = invalid;
        assert!(
            mapping.validate_runs(&[&run, &fixture.runs[1]]).is_err(),
            "{field}"
        );
    }
}
#[test]
fn legacy_host_exports_cannot_opt_into_arbitrary_path_aliases() {
    let mut fixture = Fixture::new();
    fixture.evidence["schema"] = json!("ai-platform/composed-source-export/v1");
    assert!(fixture.check().is_err());
    fixture
        .evidence
        .as_object_mut()
        .unwrap()
        .remove("execution");
    let paths = fixture.check().unwrap();
    assert!(
        paths
            .workspace_matches(fixture.source.join("codex-rs").to_str().unwrap())
            .unwrap()
    );
}
