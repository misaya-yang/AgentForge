//! Explicit local acceptance of production admission, without schema or role mutations.
use ai_platform_migration_authority::{
    application_guard::{ApplicationPrincipal, require_compatible_schema},
    commands, credential_bootstrap, credentials,
};
use serde_json::Value;
use sqlx::{Connection, PgConnection};
use std::time::Duration;

const SERVICES: &[(&str, ApplicationPrincipal)] = &[
    ("gateway", ApplicationPrincipal::Gateway),
    ("runtime", ApplicationPrincipal::Runtime),
    ("capability_worker", ApplicationPrincipal::CapabilityWorker),
    ("knowledge_api", ApplicationPrincipal::KnowledgeApi),
    ("knowledge_worker", ApplicationPrincipal::KnowledgeWorker),
];

fn denied(error: &sqlx::Error) -> bool {
    error.as_database_error().and_then(|e| e.code()).as_deref() == Some("42501")
}

#[tokio::test]
#[ignore = "explicit owned loopback PostgreSQL acceptance; set AGENTFORGE_ADMISSION_LIVE=1"]
async fn exact_service_roles_admit_and_every_other_login_is_refused() {
    assert_eq!(std::env::var("AGENTFORGE_ADMISSION_LIVE").as_deref(), Ok("1"));
    let prefix = credentials::role_prefix_from_env().expect("valid explicit role prefix required");
    let database = std::env::var("POSTGRES_DB").expect("explicit local database required");
    credential_bootstrap::verify_local_target(&commands::default_paths(), &prefix, &database)
        .await
        .unwrap_or_else(|_| panic!("owned local database preflight refused; details withheld"));

    let runtime_expected: Value = serde_json::from_str(include_str!(
        "../../../../agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/schema_admission_expected.json"
    ))
    .unwrap();
    let worker_expected: Value = serde_json::from_str(include_str!(
        "../../../../agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/schema_admission_expected.json"
    ))
    .unwrap();
    assert_eq!(runtime_expected, worker_expected);
    assert_eq!(runtime_expected["epoch"], 24);

    let mut allowed = 0;
    let mut refused = 0;
    let mut privilege_denials = 0;
    for actual in credentials::ALL_PRINCIPALS {
        let dsn = credentials::dsn_for_principal(actual)
            .unwrap_or_else(|_| panic!("explicit role credential missing: {actual}"));
        let mut connection = tokio::time::timeout(Duration::from_secs(10), PgConnection::connect(&dsn))
            .await
            .unwrap_or_else(|_| panic!("connection deadline: {actual}"))
            .unwrap_or_else(|_| panic!("role connection refused: {actual}; details withheld"));
        for (expected, profile) in SERVICES {
            let result = tokio::time::timeout(
                Duration::from_secs(10),
                require_compatible_schema(&mut connection, *profile),
            )
            .await
            .unwrap_or_else(|_| panic!("admission deadline: actual={actual}, expected={expected}"));
            assert_eq!(
                result.is_ok(),
                actual == expected,
                "production role admission mismatch: actual={actual}, expected={expected}; details withheld"
            );
            if result.is_ok() {
                allowed += 1;
            } else {
                refused += 1;
            }
        }
        if matches!(*actual, "runtime" | "capability_worker") {
            let snapshot: Value = sqlx::query_scalar(
                "SELECT assistant.platform_schema_admission_snapshot_v1()",
            )
            .fetch_one(&mut connection)
            .await
            .unwrap_or_else(|_| panic!("own-role metadata reader failed: {actual}"));
            assert_eq!(snapshot, runtime_expected, "exact compiled source snapshot: {actual}");
        }
        if credentials::APPLICATION_PRINCIPALS.contains(actual) {
            // The metadata reader must never require widening the ledger,
            // password catalog or owner-role authority of any application.
            for query in [
                "SELECT count(*) FROM public.platform_schema_changes".to_string(),
                "SELECT rolpassword FROM pg_catalog.pg_authid LIMIT 1".to_string(),
                format!("SET ROLE {prefix}owner"),
            ] {
                let result = sqlx::query(&query).execute(&mut connection).await;
                assert!(
                    result.as_ref().is_err_and(denied),
                    "application privilege boundary changed: {actual}; values withheld"
                );
                privilege_denials += 1;
            }
        }
        connection.close().await.expect("close local test connection");
    }
    assert_eq!(allowed, 5);
    assert_eq!(refused, 30);
    assert_eq!(privilege_denials, 15);
}
