use super::*;
use std::{fs, path::PathBuf};
const SECRET: &str = "private-import-sentinel:@/+%中";
fn fixture() -> String {
    SUFFIXES.iter().map(|suffix| {
        let user = if *suffix == "ADMIN" { "postgres".into() } else { format!("ai_gateway_{}", suffix.to_ascii_lowercase()) };
        format!("{PREFIX}{suffix}_DSN='postgresql://{user}:private-import-sentinel%3A%40%2F%2B%25%E4%B8%AD@127.0.0.1:5432/gateway'\n")
    }).collect()
}
fn rejected(input: &str) {
    let error = parse(input, "gateway")
        .err()
        .expect("invalid input accepted");
    assert!(!error.contains("sentinel") && !error.contains("postgresql://"));
}
struct Private(PathBuf);
impl Private {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("data-import-test-{}", uuid::Uuid::new_v4()));
        material::private_directory(&root).unwrap();
        Self(root)
    }
    fn env(&self) -> PathBuf {
        let path = self.0.join("private.env");
        material::new_file(&path, fixture().as_bytes()).unwrap();
        path
    }
}
impl Drop for Private {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn six_existing_roles_and_bootstrap_decode_once_without_form_or_env_expansion() {
    let input = fixture() + "PROVIDER_API_KEY=never-export-provider\nBROKEN_PROVIDER_FIELD\n";
    let parsed = parse(&input, "gateway").unwrap();
    assert_eq!(parsed.passwords.len(), 6);
    assert_eq!(parsed.passwords[MIGRATOR], SECRET);
    assert_eq!(parsed.bootstrap, SECRET);
    assert!(!parsed.passwords.values().any(|v| v.contains("provider")));
    assert_eq!(decode("a+b%252F%24%7BHOME%7D").unwrap(), "a+b%2F${HOME}");
    assert_eq!(decode("%22%27%5C%3D").unwrap(), "\"'\\=");
    assert_eq!(parse(&(fixture() + "POSTGRES_PASSWORD='private-import-sentinel:@/+%中'\nLOCAL_DB_MIGRATOR_PASSWORD='private-import-sentinel:@/+%中'\n"), "gateway").unwrap().passwords[MIGRATOR], SECRET);
}
#[test]
fn missing_role_wrong_database_role_and_endpoint_are_rejected_without_secrets() {
    rejected(&fixture().lines().skip(1).collect::<Vec<_>>().join("\n"));
    rejected(&fixture().replace("ai_gateway_runtime:", "ai_gateway_gateway:"));
    rejected(&fixture().replacen("/gateway", "/other", 1));
    rejected(&fixture().replacen("127.0.0.1:5432", "127.0.0.1:5433", 1));
    rejected(&fixture().replace("127.0.0.1", "localhost"));
    rejected(&fixture().replace("127.0.0.1", "192.168.0.1"));
    rejected(&fixture().replace("127.0.0.1", "127.000.0.1"));
    rejected(&fixture().replace("127.0.0.1", "[127.0.0.1]"));
    rejected(&fixture().replace(":5432/", ":0/"));
    rejected(&fixture().replace(":5432/", ":+5432/"));
    assert!(parse(&fixture().replace("127.0.0.1", "[::1]"), "gateway").is_ok());
}
#[test]
fn duplicates_conflicting_compatibility_aliases_and_unknown_authority_fail_closed() {
    rejected(&(fixture() + fixture().lines().next().unwrap()));
    for alias in [
        "POSTGRES_PASSWORD=other",
        "POSTGRES_USER=ai_gateway_gateway",
        "POSTGRES_DB=other",
        "POSTGRES_HOST=::1",
        "POSTGRES_PORT=6432",
        "AI_GATEWAY_ROLE_PREFIX=other_",
        "LOCAL_DB_RUNTIME_PASSWORD=other",
        "AI_GATEWAY_DATABASE_OTHER_DSN=private",
        "LOCAL_DB_OTHER_PASSWORD=private",
        "GATEWAY_DATABASE__DSN=postgresql://ai_gateway_gateway:other@127.0.0.1:5432/gateway",
    ] {
        rejected(&(fixture() + alias + "\n"));
    }
    let gateway = fixture()
        .lines()
        .find(|l| l.starts_with("AI_GATEWAY_DATABASE_GATEWAY_DSN="))
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_owned();
    assert!(
        parse(
            &(fixture() + &format!("GATEWAY_DATABASE__DSN={gateway}\n")),
            "gateway"
        )
        .is_ok()
    );
}
#[test]
fn malformed_or_control_uri_encoding_query_and_ambiguous_authority_fail_closed() {
    for replacement in [
        "%",
        "%0",
        "%GG",
        "%FF",
        "%00",
        "%0A",
        "%09",
        "raw@ambiguous",
        "raw?query",
        "raw#fragment",
        "raw space",
    ] {
        rejected(&fixture().replace(
            "private-import-sentinel%3A%40%2F%2B%25%E4%B8%AD",
            replacement,
        ));
    }
    rejected(&fixture().replace("/gateway'", "/gateway?options=-crole=postgres'"));
    rejected(&fixture().replace("postgresql://", "mysql://"));
    rejected(&fixture().replace("=\'postgresql", "=\"postgresql"));
}
#[cfg(unix)]
#[test]
fn private_env_only_accepts_owned_single_link_regular_0400_or_0600_files() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let private = Private::new();
    let path = private.env();
    assert!(read_private(&path).is_ok());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
    assert!(read_private(&path).is_ok());
    for mode in [0o644, 0o640, 0o440, 0o700, 0o000] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert!(read_private(&path).is_err());
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let link = private.0.join("symlink.env");
    symlink(&path, &link).unwrap();
    assert!(read_private(&link).is_err());
    let hard = private.0.join("hardlink.env");
    fs::hard_link(&path, &hard).unwrap();
    assert!(read_private(&path).is_err());
    assert!(read_private(&private.0).is_err());
}
#[cfg(unix)]
#[test]
fn ancestor_symlinks_and_special_files_never_become_import_inputs() {
    use std::os::unix::fs::symlink;
    let private = Private::new();
    let original = private.env();
    let link = private.0.join("alias");
    symlink(&private.0, &link).unwrap();
    assert!(read_private(&link.join("private.env")).is_err());
    let fifo = private.0.join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(read_private(&fifo).is_err());
    assert_eq!(read_private(&original).unwrap(), fixture());
}
#[test]
fn validation_failure_writes_nothing_and_cannot_overwrite_existing_material() {
    let private = Private::new();
    let input = private.env();
    let output = private.0.join("new-release");
    fs::write(&input, fixture() + "POSTGRES_PASSWORD=conflict\n").unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let error = prepare(
        &root,
        &input,
        &output,
        "gateway",
        "10.71.10.0/24",
        "10.71.11.0/24",
        Path::new("/nonexistent-openssl"),
    )
    .unwrap_err();
    assert!(!error.contains("sentinel"));
    assert!(!output.exists());
    fs::write(&input, fixture()).unwrap();
    fs::create_dir(&output).unwrap();
    material::new_file(&output.join("existing"), b"preserved").unwrap();
    assert!(
        prepare(
            &root,
            &input,
            &output,
            "gateway",
            "10.71.10.0/24",
            "10.71.11.0/24",
            Path::new("/nonexistent-openssl")
        )
        .is_err()
    );
    assert_eq!(fs::read(output.join("existing")).unwrap(), b"preserved");
    assert!(material::new_file(&output.join("existing"), b"replacement").is_err());
}

#[test]
fn empty_oversized_non_utf8_private_inputs_and_incomplete_imports_are_rejected() {
    let private = Private::new();
    let input = private.env();
    for bytes in [Vec::new(), vec![0xff, 0xfe], vec![b'a'; 1024 * 1024 + 1]] {
        fs::write(&input, bytes).unwrap();
        assert!(read_private(&input).is_err());
    }
    material::new_file(&private.0.join("INCOMPLETE"), b"unfinished").unwrap();
    assert!(validate(&private.0).is_err());
    assert!(private.0.join("INCOMPLETE").is_file());
}

#[test]
fn provider_multiline_contents_cannot_supply_database_credentials() {
    rejected(&("PROVIDER_PROMPT='\n".to_owned() + &fixture() + "'\n"));
    rejected(&("PROVIDER_PROMPT=continued\\\n".to_owned() + &fixture()));
    rejected(&(fixture() + "PROVIDER_PROMPT=\"unclosed\n"));
    rejected(&("PROVIDER_PROMPT=\"escaped\\\"\n".to_owned() + &fixture()));
}

#[test]
fn explicit_fresh_target_reuses_one_validated_legacy_source_without_requiring_new_dsn() {
    let input = fixture() + "POSTGRES_DB=gateway\n";
    let imported = parse(&input, "gateway_dev_fresh").unwrap();
    assert_eq!(imported.source_database, "gateway");
    assert_eq!(imported.passwords[MIGRATOR], SECRET);
    assert_eq!(imported.passwords.len(), 6);
    let other_source = input
        .replace("/gateway'", "/other_source'")
        .replace("POSTGRES_DB=gateway", "POSTGRES_DB=other_source");
    assert_eq!(
        parse(&other_source, "gateway_dev_fresh")
            .unwrap()
            .source_database,
        "other_source"
    );
    assert!(
        parse(
            &input.replacen("/gateway'", "/other_source'", 1),
            "gateway_dev_fresh"
        )
        .is_err()
    );
    assert!(parse(&input, "gateway;drop database").is_err());
    let projected = connection_record(
        Path::new("/private/fresh/authority/migrator"),
        MIGRATOR,
        "gateway_dev_fresh",
    );
    assert_eq!(projected["physical_database"], "gateway_dev_fresh");
}
