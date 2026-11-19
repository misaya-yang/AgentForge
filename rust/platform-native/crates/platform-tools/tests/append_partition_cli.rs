use std::process::Command;
#[test]
fn append_catalog_is_complete_and_no_database_execution_or_dsn_flag_exists() {
    let out = Command::new(env!("CARGO_BIN_EXE_platform-append-partitions"))
        .arg("catalog")
        .output()
        .unwrap();
    assert!(out.status.success());
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["families"].as_array().unwrap().len(), 22);
    assert_eq!(value["effects"], false);
    for args in [
        vec!["execute"],
        vec!["catalog", "--dsn", "postgresql://forbidden"],
        vec!["plan"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_platform-append-partitions"))
            .args(args)
            .output()
            .unwrap();
        assert!(!out.status.success());
    }
}

#[test]
fn maintenance_cannot_silently_cover_only_one_family() {
    let file =
        std::env::temp_dir().join(format!("append-maintenance-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&file, "[]").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_platform-append-partitions"))
        .args(["maintenance", "--snapshots"])
        .arg(&file)
        .args([
            "--now",
            "2026-10-18T00:00:00Z",
            "--daily-ahead",
            "7",
            "--monthly-ahead",
            "2",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("complete 22-family"));
    std::fs::remove_file(file).unwrap();
}
