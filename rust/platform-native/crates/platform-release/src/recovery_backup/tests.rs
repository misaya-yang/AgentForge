//! Synthetic offline policy tests; never connect to configured endpoints.
use super::*;
fn config() -> Config {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let v = serde_json::from_slice(
        &fs::read(repo.join("deploy/backup/recovery.example.json")).unwrap(),
    )
    .unwrap();
    Config {
        v,
        repo: fs::canonicalize(repo).unwrap(),
    }
}
fn manifest(c: &Config) -> Value {
    json!({"schema":"agentforge-recovery-set/v1","target_id":c.v["target_id"],"database":c.v["database"],"baseline":c.v["baseline"],"role_prefix":c.v["role_prefix"],"backup_label":"20261017-120000F","restore_point":"af_recovery_fixture","restore_point_unix":now(),"restore_lsn":"0/1234ABCD","system_identifier":"1234567890","generations":[],"generation_sha256":hash(b"[]"),"objects":[],"snapshots":[]})
}
#[test]
fn public_template_policy_passes_without_secrets_or_network() {
    config().validate().unwrap();
}
#[test]
fn same_pg_port_and_qdrant_origin_are_refused() {
    let mut c = config();
    c.v["pg_drill_port"] = c.v["pg_source_port"].clone();
    assert!(c.validate().is_err());
    let mut c = config();
    c.v["qdrant_drill"] = c.v["qdrant_source"].clone();
    assert!(c.validate().is_err());
}
#[test]
fn equivalent_qdrant_origin_spellings_are_refused() {
    for (source, drill) in [
        ("http://127.0.0.1:6333", "http://127.0.0.1:6333/"),
        ("http://127.0.0.1", "http://127.0.0.1:80/"),
        ("http://127.0.0.1:6333", "http://127.1:6333/"),
    ] {
        let mut c = config();
        c.v["qdrant_source"] = json!(source);
        c.v["qdrant_drill"] = json!(drill);
        assert!(
            c.validate().is_err(),
            "same parsed origin: {source} / {drill}"
        );
    }
}
#[test]
fn writable_roots_cannot_be_inside_the_source_cluster() {
    for key in ["drill_root", "evidence_root"] {
        let mut c = config();
        c.v[key] = c.v["pg_source_path"].clone();
        assert!(c.validate().is_err(), "{key} cannot target source data");
        c.v[key] = json!(format!("{}/nested", c.value("pg_source_path").unwrap()));
        assert!(c.validate().is_err(), "{key} cannot be inside source data");
    }
}
#[cfg(unix)]
#[test]
fn source_cluster_aliases_cannot_be_writable_roots() {
    let root = std::env::temp_dir().join(format!("af-drill-roots-{}", uuid::Uuid::new_v4()));
    private_dir(&root).unwrap();
    let source = root.join("source");
    private_dir(&source).unwrap();
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&source, &alias).unwrap();
    for key in ["drill_root", "evidence_root"] {
        let mut c = config();
        c.v["pg_source_path"] = json!(source);
        c.v[key] = json!(alias.join("nested"));
        assert!(c.validate().is_err(), "{key} cannot alias source data");
    }
    fs::remove_file(alias).unwrap();
    fs::remove_dir(source).unwrap();
    fs::remove_dir(root).unwrap();
}
#[test]
fn production_bucket_and_shell_paths_are_refused() {
    let mut c = config();
    c.v["drill_bucket"] = c.v["bucket"].clone();
    assert!(c.validate().is_err());
    let mut c = config();
    c.v["pgbackrest_config"] = json!("/tmp/x;touch-pwned");
    assert!(c.validate().is_err());
}
#[test]
fn cross_scope_set_cannot_restore() {
    let c = config();
    let mut m = manifest(&c);
    drill::validate_set(&c, &m).unwrap();
    m["role_prefix"] = json!("foreign_");
    assert!(drill::validate_set(&c, &m).is_err());
}
#[test]
fn altered_generation_or_future_clock_is_refused() {
    let c = config();
    let mut m = manifest(&c);
    m["generations"] = json!([{"content_revision":7}]);
    assert!(drill::validate_set(&c, &m).is_err());
    let mut m = manifest(&c);
    m["restore_point_unix"] = json!(now() + 3600);
    assert!(drill::validate_set(&c, &m).is_err());
}
#[test]
fn foreign_object_prefix_and_restore_options_are_refused() {
    let c = config();
    let mut m = manifest(&c);
    m["objects"] = json!([{"key":"other-tenant/file"}]);
    assert!(drill::validate_set(&c, &m).is_err());
    let mut m = manifest(&c);
    m["backup_label"] = json!("--delta");
    assert!(drill::validate_set(&c, &m).is_err());
}
#[cfg(unix)]
#[test]
fn private_files_reject_world_readable_and_symlink() {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, symlink};
    let root = std::env::temp_dir().join(format!("af-recovery-policy-{}", uuid::Uuid::new_v4()));
    private_dir(&root).unwrap();
    let p = root.join("fixture");
    let mut f = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&p)
        .unwrap();
    f.write_all(b"synthetic-fixture").unwrap();
    assert!(secret(&p, &config().repo).is_ok());
    let link = root.join("link");
    symlink(&p, &link).unwrap();
    assert!(secret(&link, &config().repo).is_err());
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(secret(&p, &config().repo).is_err());
    fs::remove_file(link).unwrap();
    fs::remove_file(p).unwrap();
    fs::remove_dir(root).unwrap();
}
