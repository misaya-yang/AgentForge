use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("local-env-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join(".env.dev"), "# private fixture\nPOSTGRES_USER=postgres\nPOSTGRES_PASSWORD=fixture_admin_password_12345\nPOSTGRES_DB=gateway\nPOSTGRES_PORT=5432\nREDIS_PORT=6379\nREDIS_PASSWORD=fixtureRedis1234\nDASHSCOPE_API_KEY=fixture-provider-secret\n").unwrap();
        fs::set_permissions(root.join(".env.dev"), fs::Permissions::from_mode(0o600)).unwrap();
        Self(root)
    }
    fn tool(&self, pass: bool) -> std::process::Output {
        let result = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(["local-dev-env", "--env-file"])
            .arg(self.0.join(".env.dev"))
            .arg("--repo-root")
            .arg(&self.0)
            .output()
            .unwrap();
        assert_eq!(
            result.status.success(),
            pass,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!String::from_utf8_lossy(&result.stdout).contains("fixture-provider-secret"));
        assert!(!String::from_utf8_lossy(&result.stderr).contains("fixture_admin_password"));
        result
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn prepares_distinct_roles_without_printing_values_and_is_byte_idempotent() {
    let fixture = Fixture::new();
    let first = fixture.tool(true);
    let first_json: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(first_json["database_mutated"], false);
    let path = fixture.0.join(".env.dev");
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# private fixture\n"));
    assert!(text.contains("DASHSCOPE_API_KEY=fixture-provider-secret\n"));
    let values: std::collections::BTreeMap<_, _> = dotenvy::from_read_iter(text.as_bytes())
        .map(Result::unwrap)
        .collect();
    let mut passwords = std::collections::BTreeSet::new();
    for principal in [
        "migrator",
        "gateway",
        "runtime",
        "capability_worker",
        "knowledge_api",
        "knowledge_worker",
    ] {
        let key = principal.to_ascii_uppercase();
        let password = &values[&format!("LOCAL_DB_{key}_PASSWORD")];
        assert_eq!(password.len(), 64);
        assert!(password.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(passwords.insert(password));
        let dsn = reqwest::Url::parse(&values[&format!("AI_GATEWAY_DATABASE_{key}_DSN")]).unwrap();
        assert_eq!(dsn.username(), format!("ai_gateway_{principal}"));
        assert!(!String::from_utf8_lossy(&first.stdout).contains(password));
    }
    for (application, authority) in [
        ("GATEWAY_DATABASE__DSN", "AI_GATEWAY_DATABASE_GATEWAY_DSN"),
        ("KNOWLEDGE_DATABASE__DSN", "AI_GATEWAY_DATABASE_KNOWLEDGE_API_DSN"),
    ] {
        let mut app = reqwest::Url::parse(&values[application]).unwrap();
        let login = reqwest::Url::parse(&values[authority]).unwrap();
        assert_eq!(login.query(), None);
        assert_eq!(
            app.query_pairs().collect::<Vec<_>>(),
            vec![(
                "options".into(),
                "-c statement_timeout=30000 -c idle_in_transaction_session_timeout=15000".into(),
            )]
        );
        app.set_query(None);
        assert_eq!(app, login);
    }
    assert_ne!(
        values["GATEWAY_DATABASE__DSN"],
        values["AI_GATEWAY_DATABASE_ADMIN_DSN"]
    );
    assert_eq!(values["COMPOSE_PROJECT_NAME"], "agentforge-dev-7801d4a9a4");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let second: Value = serde_json::from_slice(&fixture.tool(true).stdout).unwrap();
    assert_eq!(second["updated_keys"], serde_json::json!([]));
    assert_eq!(fs::read_to_string(path).unwrap(), text);
}

#[test]
fn conflicts_never_overwrite_an_existing_private_file() {
    for extra in [
        "COMPOSE_PROJECT_NAME=unrelated-project\n",
        "AI_PLATFORM_LOCAL_DEV_CREDENTIALS=false\n",
        "POSTGRES_HOST=db.production.invalid\n",
        "POSTGRES_HOST=localhost\n",
        "POSTGRES_HOST=::1\n",
        "AI_GATEWAY_DATABASE_RUNTIME_DSN=postgresql://other-secret@localhost/other\n",
        "POSTGRES_PORT=not-a-number\n",
    ] {
        let fixture = Fixture::new();
        let path = fixture.0.join(".env.dev");
        let mut original = fs::read_to_string(&path).unwrap();
        if extra.starts_with("POSTGRES_PORT=") {
            original = original.replace("POSTGRES_PORT=5432\n", "");
        }
        original.push_str(extra);
        fs::write(&path, &original).unwrap();
        fixture.tool(false);
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn insecure_permissions_and_symlinks_are_refused() {
    let fixture = Fixture::new();
    let path = fixture.0.join(".env.dev");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    fixture.tool(false);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::rename(&path, fixture.0.join("original")).unwrap();
    std::os::unix::fs::symlink("original", path).unwrap();
    fixture.tool(false);
}

#[test]
fn another_checkouts_private_env_is_refused_without_mutation() {
    let selected = Fixture::new();
    let other = Fixture::new();
    let path = other.0.join(".env.dev");
    let original = fs::read(&path).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
        .args(["local-dev-env", "--env-file"])
        .arg(&path)
        .arg("--repo-root")
        .arg(&selected.0)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read(path).unwrap(), original);
}

#[test]
fn tracked_env_files_never_receive_generated_secrets() {
    let fixture = Fixture::new();
    assert!(
        Command::new("git")
            .args(["init", "--quiet"])
            .arg(&fixture.0)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        Command::new("git")
            .arg("-C")
            .arg(&fixture.0)
            .args(["add", ".env.dev"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let path = fixture.0.join(".env.dev");
    let original = fs::read_to_string(&path).unwrap();
    fixture.tool(false);
    assert_eq!(fs::read_to_string(path).unwrap(), original);
}

#[test]
fn local_migration_orders_credentials_before_login_and_preserves_readonly_status() {
    for (flag, fail_init, marker_in_file, expected_exit, expected_calls) in [
        (
            "--auto",
            false,
            true,
            0,
            vec![
                "provision-roles",
                "initialize-local-credentials --expected-database gateway",
                "source-kind",
                "provision-extensions",
                "migrate",
            ],
        ),
        (
            "--auto",
            true,
            true,
            73,
            vec![
                "provision-roles",
                "initialize-local-credentials --expected-database gateway",
            ],
        ),
        ("--status", false, true, 0, vec!["status"]),
        (
            "--auto",
            false,
            false,
            0,
            vec![
                "provision-roles",
                "source-kind",
                "provision-extensions",
                "migrate",
            ],
        ),
    ] {
        let fixture = Fixture::new();
        fixture.tool(true);
        if !marker_in_file {
            let path = fixture.0.join(".env.dev");
            let text = fs::read_to_string(&path)
                .unwrap()
                .replace("AI_PLATFORM_LOCAL_DEV_CREDENTIALS=true\n", "");
            fs::write(path, text).unwrap();
        }
        let bin = fixture.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let executable = bin.join("database-authority");
        fs::write(&executable,"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$AUTHORITY_LOG\"\nif [ \"$1\" = initialize-local-credentials ] && [ \"$FAIL_LOCAL_INIT\" = 1 ]; then exit 73; fi\nif [ \"$1\" = source-kind ]; then echo empty; fi\n").unwrap();
        fs::set_permissions(executable, fs::Permissions::from_mode(0o755)).unwrap();
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../..")
            .canonicalize()
            .unwrap();
        let log = fixture.0.join("calls.log");
        let result = Command::new("/bin/bash")
            .arg(root.join("scripts/new/migrate.sh"))
            .args([flag, "--env"])
            .arg(fixture.0.join(".env.dev"))
            .env_clear()
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("AUTHORITY_LOG", &log)
            .env("FAIL_LOCAL_INIT", if fail_init { "1" } else { "0" })
            .env(
                "AI_PLATFORM_LOCAL_DEV_CREDENTIALS",
                if marker_in_file { "false" } else { "true" },
            )
            .output()
            .unwrap();
        assert_eq!(
            result.status.code(),
            Some(expected_exit),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let actual = fs::read_to_string(log).unwrap();
        assert_eq!(actual.lines().collect::<Vec<_>>(), expected_calls);
        assert!(!String::from_utf8_lossy(&result.stdout).contains("fixture_admin_password"));
    }
}
