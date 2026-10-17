//! Explicit live acceptance against the owned local project after `make dev-setup`.
//! No roles, passwords, tables or volumes are created/reset by this test.
use ai_platform_migration_authority::credentials::APPLICATION_PRINCIPALS;
use serde_json::Value;
use sqlx::{ConnectOptions, Connection, PgConnection, postgres::PgConnectOptions};
use std::{collections::BTreeMap, path::PathBuf, str::FromStr, time::Duration};

fn configuration() -> BTreeMap<String, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .canonicalize()
        .unwrap();
    let path = PathBuf::from(std::env::var_os("ENV_FILE").expect("explicit ENV_FILE required"))
        .canonicalize()
        .expect("private local ENV_FILE unavailable");
    assert!(
        path == root.join(".env.dev"),
        "selected local .env.dev required"
    );
    let values: BTreeMap<_, _> = dotenvy::from_path_iter(path)
        .expect("local config unreadable")
        .map(|v| v.expect("invalid private config syntax"))
        .collect();
    assert!(
        values
            .get("AI_PLATFORM_LOCAL_DEV_CREDENTIALS")
            .map(String::as_str)
            == Some("true")
    );
    values
}

async fn cli(values: &BTreeMap<String, String>) -> std::process::Output {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_database-authority"))
            .args(["initialize-local-credentials", "--expected-database"])
            .arg(&values["POSTGRES_DB"])
            .envs(values)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("local credential command timed out")
    .expect("local credential command unavailable");
    for (key, value) in values {
        if value.len() >= 16
            && ["PASSWORD", "SECRET", "TOKEN", "API_KEY", "DSN"]
                .iter()
                .any(|part| key.contains(part))
        {
            assert!(!String::from_utf8_lossy(&output.stdout).contains(value));
            assert!(!String::from_utf8_lossy(&output.stderr).contains(value));
        }
    }
    output
}

async fn connect(dsn: &str) -> PgConnection {
    let options = PgConnectOptions::from_str(dsn)
        .expect("invalid private DSN")
        .disable_statement_logging();
    tokio::time::timeout(
        Duration::from_secs(10),
        PgConnection::connect_with(&options),
    )
    .await
    .expect("local role connection timed out")
    .unwrap_or_else(|_| panic!("local role authentication failed; details withheld"))
}

async fn snapshot(admin: &mut PgConnection, roles: &[String]) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM pg_catalog.pg_authid r WHERE rolname=ANY($1) ORDER BY rolname",
    )
    .bind(roles)
    .fetch_all(admin)
    .await
    .unwrap_or_else(|_| panic!("private role snapshot failed"))
}

#[tokio::test]
#[ignore = "requires explicitly selected, initialized owned local Compose PostgreSQL; run with --ignored"]
async fn local_credentials_preserve_catalog_and_refuse_rotation_with_real_role_denials() {
    let values = configuration();
    // The CLI verifies Compose ownership before this test opens any database connection.
    let output = cli(&values).await;
    assert!(
        output.status.success(),
        "local ownership or initialization gate failed"
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).expect("missing safe receipt");
    assert_eq!(receipt["initialized"], 0);
    assert_eq!(receipt["preserved"], 6);
    assert_eq!(receipt["verified_connections"], 7);
    assert_eq!(receipt["passwords_rotated"], false);

    let prefix = values
        .get("AI_GATEWAY_ROLE_PREFIX")
        .map(String::as_str)
        .unwrap_or("ai_gateway_");
    let roles: Vec<_> = ["migrator"]
        .into_iter()
        .chain(APPLICATION_PRINCIPALS.iter().copied())
        .map(|p| format!("{prefix}{p}"))
        .collect();
    let mut admin = connect(&values["AI_GATEWAY_DATABASE_ADMIN_DSN"]).await;
    let before = snapshot(&mut admin, &roles).await;
    assert_eq!(before.len(), 6);

    let mut wrong = values.clone();
    let key = "AI_GATEWAY_DATABASE_KNOWLEDGE_WORKER_DSN";
    let mut dsn = url::Url::parse(&wrong[key]).expect("invalid private DSN");
    dsn.set_password(Some(&uuid::Uuid::new_v4().simple().to_string().repeat(2)))
        .unwrap();
    wrong.insert(key.into(), dsn.into());
    let rejected = cli(&wrong).await;
    assert!(
        !rejected.status.success(),
        "mismatched password must never rotate an existing role"
    );
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("no passwords rotated"));
    assert!(
        snapshot(&mut admin, &roles).await == before,
        "role catalog must stay byte equivalent"
    );

    let mut redirected = values.clone();
    redirected.insert(
        "AI_GATEWAY_DATABASE_DIR".into(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(4)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let rejected = cli(&redirected).await;
    assert!(
        !rejected.status.success(),
        "artifact overrides cannot redefine local ownership"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("this binary's repository database source")
    );

    for principal in APPLICATION_PRINCIPALS {
        let mut conn = connect(
            &values[&format!("AI_GATEWAY_DATABASE_{}_DSN", principal.to_ascii_uppercase())],
        )
        .await;
        let selected: i32 = sqlx::query_scalar("SELECT 1")
            .fetch_one(&mut conn)
            .await
            .unwrap();
        assert_eq!(selected, 1);
        if ["gateway", "knowledge_api", "knowledge_worker"].contains(principal) {
            let profile = match *principal {
                "gateway" => ai_platform_migration_authority::application_guard::ApplicationPrincipal::Gateway,
                "knowledge_api" => ai_platform_migration_authority::application_guard::ApplicationPrincipal::KnowledgeApi,
                _ => ai_platform_migration_authority::application_guard::ApplicationPrincipal::KnowledgeWorker,
            };
            ai_platform_migration_authority::application_guard::require_compatible_schema(
                &mut conn, profile,
            )
            .await
            .expect("scoped metadata-only admission must pass");
        }
        for forbidden in [
            "SELECT rolpassword FROM pg_catalog.pg_authid LIMIT 1".to_owned(),
            "SELECT count(*) FROM public.platform_schema_baselines".to_owned(),
            format!("SET ROLE \"{prefix}owner\""),
        ] {
            let error = sqlx::raw_sql(&forbidden)
                .execute(&mut conn)
                .await
                .expect_err("application role unexpectedly gained administrator/owner access");
            assert!(error.as_database_error().and_then(|e| e.code()).as_deref() == Some("42501"));
        }
        conn.close().await.unwrap();
    }
    let repeated = cli(&values).await;
    assert!(
        repeated.status.success(),
        "original credentials must still authenticate"
    );
    assert!(
        snapshot(&mut admin, &roles).await == before,
        "repeat must preserve all role attributes and SCRAM salts"
    );
    admin.close().await.unwrap();
}
