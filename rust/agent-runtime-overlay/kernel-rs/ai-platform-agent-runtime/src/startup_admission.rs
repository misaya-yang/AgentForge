//! Source-bound startup admission. No privileged or direct-ledger fallback.
use serde_json::Value;
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};

const EXPECTED: &str = include_str!("schema_admission_expected.json");
#[path = "startup_admission_fresh.rs"]
mod fresh;

fn validate(snapshot: &Value) -> anyhow::Result<()> {
    let expected: Value = serde_json::from_str(EXPECTED)?;
    // Exact equality includes shape, sole baseline marker, complete ordered
    // change/checksum prefix, exact epoch window, and zero authority attempts.
    if snapshot != &expected {
        anyhow::bail!("schema admission refused: metadata differs from compiled source");
    }
    Ok(())
}

// Evaluated outside SECURITY DEFINER, on the login connection itself.
// Resolve the exact zero-argument reader and bind its owner to the schema
// owner; no name-only lookup, search_path lookup, or client-supplied prefix.
const PRINCIPAL_SQL: &str = r#"
SELECT count(*) = 1 AND COALESCE(bool_and(
    current_user = session_user
    AND caller.rolcanlogin
    AND NOT (caller.rolsuper OR caller.rolcreatedb OR caller.rolcreaterole
             OR caller.rolreplication OR caller.rolbypassrls OR caller.rolinherit)
    AND owner.rolname ~ '^[a-z][a-z0-9_]{0,20}_owner$'
    AND NOT (owner.rolcanlogin OR owner.rolsuper OR owner.rolcreatedb
             OR owner.rolcreaterole OR owner.rolreplication OR owner.rolbypassrls)
    AND reader.proowner = namespace.nspowner
    AND reader.prosecdef
    AND reader.prorettype = 'pg_catalog.jsonb'::pg_catalog.regtype
    AND caller.rolname = left(owner.rolname, length(owner.rolname) - length('owner')) || $1
), false)
FROM pg_catalog.pg_proc AS reader
JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = reader.pronamespace
JOIN pg_catalog.pg_roles AS owner ON owner.oid = reader.proowner
JOIN pg_catalog.pg_roles AS caller ON caller.rolname = current_user
WHERE namespace.nspname = 'assistant'
  AND reader.proname = 'platform_schema_admission_snapshot_v1'
  AND reader.pronargs = 0
"#;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const CLOSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

const SERVICE_ROLE_SUFFIX: &str = "runtime";

pub async fn require_compatible_schema(options: &PgConnectOptions) -> anyhow::Result<()> {
    let mut conn = tokio::time::timeout(CONNECT_TIMEOUT, PgConnection::connect_with(options))
        .await
        .map_err(|_| anyhow::anyhow!("schema admission connection timed out"))?
        .map_err(|_| anyhow::anyhow!("schema admission connection failed"))?;
    // Select from the actual connected database, never an environment flag or
    // a claimed metadata field. A missing/denied fresh reader cannot fall back.
    let database = tokio::time::timeout(
        QUERY_TIMEOUT,
        sqlx::query_scalar::<_, String>("SELECT current_database()::text").fetch_one(&mut conn),
    )
    .await
    .map_err(|_| anyhow::anyhow!("schema admission database check timed out"))?
    .map_err(|_| anyhow::anyhow!("schema admission database check failed"))?;
    let fresh = fresh::selected(&database);
    let principal_sql = if fresh {
        fresh::principal_sql(PRINCIPAL_SQL)
    } else {
        PRINCIPAL_SQL.to_owned()
    };
    let principal_valid = tokio::time::timeout(
        QUERY_TIMEOUT,
        // Audited SQLx0.9 boundary: this is PRINCIPAL_SQL or its closed,
        // source-only fresh::principal_sql transformation. No database value,
        // environment input or caller string contributes SQL text; the service
        // role remains a bound value below.
        sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(principal_sql.as_str()))
            .bind(SERVICE_ROLE_SUFFIX)
            .fetch_one(&mut conn),
    )
    .await
    .map_err(|_| anyhow::anyhow!("schema admission identity check timed out"))?
    .map_err(|_| anyhow::anyhow!("schema admission identity check failed"))?;
    if !principal_valid {
        anyhow::bail!("schema admission requires the canonical unprivileged service login role");
    }
    let snapshot = tokio::time::timeout(
        QUERY_TIMEOUT,
        sqlx::query_scalar::<_, Value>(if fresh {
            fresh::READER_SQL
        } else {
            "SELECT assistant.platform_schema_admission_snapshot_v1()"
        })
        .fetch_one(&mut conn),
    )
    .await
    .map_err(|_| anyhow::anyhow!("schema admission metadata query timed out"))?
    .map_err(|_| anyhow::anyhow!("schema admission metadata reader unavailable or denied"))?;
    if fresh {
        fresh::validate(&database, &snapshot)?;
        for query in fresh::privilege_queries()? {
            let admitted = tokio::time::timeout(
                QUERY_TIMEOUT,
                // Audited SQLx0.9 boundary: privilege_queries decodes only
                // include_str!("fresh_schema_privileges.json"). The two complete
                // source-frozen SELECTs contain no runtime interpolation.
                sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(query.as_str()))
                    .fetch_one(&mut conn),
            )
            .await
            .map_err(|_| anyhow::anyhow!("fresh schema admission privilege probe timed out"))?
            .map_err(|_| {
                anyhow::anyhow!("fresh schema admission privilege probe unavailable or denied")
            })?;
            if !admitted {
                anyhow::bail!("fresh schema admission exact service privileges refused");
            }
        }
    } else {
        validate(&snapshot)?;
    }
    tokio::time::timeout(CLOSE_TIMEOUT, conn.close())
        .await
        .map_err(|_| anyhow::anyhow!("schema admission connection close timed out"))?
        .map_err(|_| anyhow::anyhow!("schema admission connection close failed"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot() -> Value {
        serde_json::from_str(EXPECTED).unwrap()
    }
    #[test]
    fn exact_compiled_source_is_admitted() {
        assert!(validate(&snapshot()).is_ok());
    }
    #[test]
    fn baseline_ledger_window_checksums_and_attempts_fail_closed() {
        for pointer in [
            "/baselines",
            "/changes",
            "/epoch",
            "/schema_version",
            "/has_attempts",
            "/has_incomplete_attempts",
            "/baselines/0/baseline_id",
            "/baselines/0/manifest_sha256",
            "/baselines/0/structural_sha256",
            "/baselines/0/acl_sha256",
            "/baselines/0/extensions_sha256",
            "/baselines/0/reference_data_sha256",
            "/baselines/0/source_git_sha",
            "/changes/0/checksum_sha256",
        ] {
            let mut bad = snapshot();
            *bad.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(validate(&bad).is_err(), "{pointer}");
        }
        for epoch in [0, 10, 18, 21, 23] {
            let mut bad = snapshot();
            bad["epoch"] = epoch.into();
            assert!(validate(&bad).is_err());
        }
        let mut bad = snapshot();
        bad["changes"].as_array_mut().unwrap().remove(3);
        assert!(validate(&bad).is_err());
        let mut bad = snapshot();
        bad["changes"].as_array_mut().unwrap().swap(0, 1);
        assert!(validate(&bad).is_err());
        let mut bad = snapshot();
        bad["unexpected"] = true.into();
        assert!(validate(&bad).is_err());
    }
}
