//! Dedicated fresh-development admission. No legacy epoch/history is synthesized.
use serde_json::Value;

pub(super) const DATABASE: &str = "gateway_dev_fresh";
pub(super) const READER_SQL: &str = "SELECT assistant.platform_fresh_schema_snapshot_v1()";
const CONTRACT: &str = include_str!("fresh_schema_admission_expected.json");
const PRIVILEGES: &str = include_str!("fresh_schema_privileges.json");

pub(super) fn selected(database: &str) -> bool {
    database == DATABASE
}

pub(super) fn principal_sql(legacy: &str) -> String {
    // Preserve the original caller/session/owner/no-inherit checks and scope
    // the exact zero-argument SECURITY DEFINER reader to the fresh contract.
    legacy.replace(
        "platform_schema_admission_snapshot_v1",
        "platform_fresh_schema_snapshot_v1",
    ).replace(
        "AND caller.rolcanlogin",
        "AND caller.rolcanlogin AND NOT pg_catalog.has_table_privilege(current_user, 'pg_catalog.pg_authid', 'SELECT')",
    ).replace(
        "AND reader.prosecdef",
        "AND reader.prosecdef AND reader.prokind = 'f'",
    )
}

pub(super) fn validate(database: &str, snapshot: &Value) -> anyhow::Result<()> {
    if !selected(database) {
        anyhow::bail!("fresh schema admission requires the dedicated development database");
    }
    let mut expected: Value = serde_json::from_str(CONTRACT)?;
    let fingerprints = snapshot["fingerprints"]
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("fresh schema admission final fingerprints unavailable"))?;
    if fingerprints.len() != 4
        || ["structural", "acl", "extensions", "reference_data"]
            .iter()
            .any(|key| {
                fingerprints
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_none_or(|hash| {
                        hash.len() != 64
                            || !hash
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
            })
    {
        anyhow::bail!("fresh schema admission final fingerprints malformed");
    }
    // Values are measured during actual transactional initialization, never
    // copied from an historical database or fabricated as release evidence.
    // The owner-bound reader independently compares the entire live catalog.
    expected["fingerprints"] = Value::Object(fingerprints.clone());
    if snapshot != &expected {
        anyhow::bail!("fresh schema admission source, target, mode or final catalog mismatch");
    }
    Ok(())
}

pub(super) fn privilege_queries() -> anyhow::Result<Vec<String>> {
    let queries: Vec<String> = serde_json::from_str(PRIVILEGES)?;
    if queries.len() != 2 || queries.iter().any(|q| !q.starts_with("SELECT ")) {
        anyhow::bail!("fresh schema admission compiled privilege contract invalid");
    }
    Ok(queries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn snapshot() -> Value {
        let mut value: Value = serde_json::from_str(CONTRACT).unwrap();
        // Synthetic hashes exercise only payload validation. Actual startup
        // obtains measurements and catalog_matches from the trusted SQL reader.
        value["fingerprints"] = json!({"structural":"a".repeat(64),"acl":"b".repeat(64),"extensions":"c".repeat(64),"reference_data":"d".repeat(64)});
        value
    }
    #[test]
    fn exact_fresh_contract_is_valid_only_on_the_dedicated_database() {
        let value = snapshot();
        validate(DATABASE, &value).unwrap();
        for database in [
            "gateway",
            "gateway_dev",
            "gateway_dev_fresh ",
            "GATEWAY_DEV_FRESH",
            "",
        ] {
            assert!(!selected(database));
            assert!(validate(database, &value).is_err());
        }
        let historical: Value = serde_json::from_str(super::super::EXPECTED).unwrap();
        assert!(validate(DATABASE, &historical).is_err());
        assert!(super::super::validate(&value).is_err());
    }
    #[test]
    fn missing_extra_forged_and_historical_fields_fail_closed() {
        let value = snapshot();
        for key in value.as_object().unwrap().keys() {
            let mut bad = value.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(validate(DATABASE, &bad).is_err(), "missing {key}");
            let mut bad = value.clone();
            bad[key] = Value::Null;
            assert!(validate(DATABASE, &bad).is_err(), "null {key}");
        }
        for (key, wrong) in [
            ("catalog_matches", json!(false)),
            ("transactional_initialization", json!(false)),
            ("historical_upgrade_acceptance", json!(true)),
            ("database", json!("gateway")),
            ("schema_epoch", json!(29)),
            ("mode", json!("upgrade")),
            ("source_contract_sha256", json!("0".repeat(64))),
            (
                "schema_version",
                json!("ai-platform/schema-admission-snapshot/v1"),
            ),
            ("epoch", json!(35)),
            ("changes", json!([])),
        ] {
            let mut bad = value.clone();
            bad[key] = wrong;
            assert!(validate(DATABASE, &bad).is_err(), "forged {key}");
        }
        let mut extra = value;
        extra["unreviewed"] = json!(true);
        assert!(validate(DATABASE, &extra).is_err());
    }
    #[test]
    fn complete_measured_fingerprint_shape_is_required() {
        for key in ["structural", "acl", "extensions", "reference_data"] {
            for wrong in [
                json!("not measured"),
                json!("A".repeat(64)),
                json!("f".repeat(63)),
                json!("g".repeat(64)),
                json!(7),
                Value::Null,
            ] {
                let mut bad = snapshot();
                bad["fingerprints"][key] = wrong;
                assert!(validate(DATABASE, &bad).is_err(), "{key}");
            }
            let mut bad = snapshot();
            bad["fingerprints"].as_object_mut().unwrap().remove(key);
            assert!(validate(DATABASE, &bad).is_err());
        }
        let mut bad = snapshot();
        bad["fingerprints"]["unreviewed"] = json!("f".repeat(64));
        assert!(validate(DATABASE, &bad).is_err());
    }
    #[test]
    fn fresh_reader_retains_the_actual_login_owner_and_least_privilege_guards() {
        let sql = principal_sql(super::super::PRINCIPAL_SQL);
        assert!(sql.contains("reader.proname = 'platform_fresh_schema_snapshot_v1'"));
        assert!(!sql.contains("reader.proname = 'platform_schema_admission_snapshot_v1'"));
        for guard in [
            "current_user = session_user",
            "caller.rolcanlogin",
            "caller.rolsuper",
            "caller.rolcreatedb",
            "caller.rolcreaterole",
            "caller.rolreplication",
            "caller.rolbypassrls",
            "caller.rolinherit",
            "reader.proowner = namespace.nspowner",
            "reader.prosecdef",
            "reader.pronargs = 0",
            "namespace.nspname = 'assistant'",
        ] {
            assert!(sql.contains(guard), "{guard}");
        }
        assert!(sql.contains(
            "NOT pg_catalog.has_table_privilege(current_user, 'pg_catalog.pg_authid', 'SELECT')"
        ));
        assert!(sql.contains("reader.prokind = 'f'"));
        let queries = privilege_queries().unwrap();
        assert_eq!(queries[1], "SELECT partition_ops.schema_admitted()");
        assert!(queries[0].contains("has_table_privilege"));
        assert!(queries[0].contains("NOT"));
    }
    /// Optional read-only exercise of the exact startup function against the
    /// initialized database. Root supplies this binary's canonical login DSN;
    /// no test creates roles, schemas or replacement historical acceptance.
    #[tokio::test]
    #[ignore = "requires actual gateway_dev_fresh and this service's unprivileged login; root owns PostgreSQL acceptance"]
    async fn actual_fresh_database_admits_the_real_service_startup_guard() {
        use std::str::FromStr;
        let dsn = std::env::var("AI_PLATFORM_FRESH_SCHEMA_ADMISSION_TEST_DSN")
            .expect("explicit fresh admission test DSN required");
        let options = sqlx::postgres::PgConnectOptions::from_str(&dsn)
            .unwrap_or_else(|_| panic!("invalid fresh admission test DSN; value withheld"));
        assert_eq!(
            options.get_database(),
            Some(DATABASE),
            "dedicated database required"
        );
        super::super::require_compatible_schema(&options)
            .await
            .expect("actual fresh service startup admission failed");
    }
}
