//! Offline policy checks, bounded retention plans and explicit private material preparation.
pub mod local_env;
pub mod material;
pub mod retention;
use ai_platform_service_primitives::data_connection::DatabaseRole;
use std::{collections::BTreeMap, path::Path};
pub type Result<T> = std::result::Result<T, String>;
pub fn ensure(ok: bool, why: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(why.into()) }
}
pub fn parse_ini(input: &str) -> Result<BTreeMap<String, BTreeMap<String, String>>> {
    let mut result: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    for raw in input.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].into();
            ensure(
                section == section.to_ascii_lowercase(),
                "noncanonical INI section",
            )?;
            ensure(!result.contains_key(&section), "duplicate INI section")?;
            result.insert(section.clone(), BTreeMap::new());
            continue;
        }
        let (k, v) = line.split_once('=').ok_or("invalid INI assignment")?;
        ensure(
            !section.is_empty() && !k.trim().is_empty() && !v.trim().is_empty(),
            "invalid INI setting",
        )?;
        ensure(
            k.trim() == k.trim().to_ascii_lowercase(),
            "noncanonical INI key",
        )?;
        ensure(
            result
                .get_mut(&section)
                .unwrap()
                .insert(k.trim().into(), v.trim().into())
                .is_none(),
            "duplicate INI setting",
        )?;
    }
    Ok(result)
}
pub fn validate_ini(input: &str) -> Result<()> {
    let sections = parse_ini(input)?;
    ensure(sections.len() == 3, "unexpected INI authority")?;
    let settings = sections
        .get("pgbouncer")
        .ok_or("missing pooler configuration")?;
    for (k, v) in [
        ("pool_mode", "transaction"),
        ("auth_type", "cert"),
        ("client_tls_sslmode", "verify-full"),
        ("server_tls_sslmode", "verify-full"),
        ("max_prepared_statements", "200"),
        ("max_client_conn", "256"),
        ("max_db_connections", "8"),
        ("default_pool_size", "4"),
        ("min_pool_size", "0"),
        ("max_user_connections", "8"),
        ("reserve_pool_size", "0"),
        ("server_reset_query", "DISCARD ALL"),
        ("server_reset_query_always", "1"),
        ("query_wait_timeout", "10"),
        ("idle_transaction_timeout", "30"),
    ] {
        ensure(
            settings.get(k).map(String::as_str) == Some(v),
            "unsafe pooler policy",
        )?;
    }
    ensure(
        !settings.contains_key("auth_query")
            && !settings.contains_key("auth_user")
            && !settings.contains_key("admin_users")
            && !settings.contains_key("stats_users"),
        "pooler privilege escalation",
    )?;
    for key in [
        "auth_file",
        "client_tls_ca_file",
        "client_tls_cert_file",
        "client_tls_key_file",
        "server_tls_ca_file",
        "server_tls_cert_file",
        "server_tls_key_file",
    ] {
        ensure(
            settings.get(key).is_some_and(|v| v.starts_with("/run/")),
            "pooler file material missing",
        )?;
    }
    let databases = sections.get("databases").ok_or("missing role pools")?;
    let users = sections.get("users").ok_or("missing user pool caps")?;
    ensure(
        databases.len() == 10 && users.len() == 5,
        "pool inventory mismatch",
    )?;
    for role in DatabaseRole::ALL {
        ensure(
            users.get(role.name()).map(String::as_str)
                == Some("max_user_connections=8 pool_size=4 reserve_pool_size=0"),
            "unbounded role pool",
        )?;
        for mode in ["tx", "session"] {
            let entry = databases
                .get(&format!("{}_{mode}", role.alias()))
                .ok_or("role pool missing")?;
            let mut options = BTreeMap::new();
            for part in entry.split_whitespace() {
                let (key, value) = part.split_once('=').ok_or("invalid pool endpoint")?;
                ensure(
                    options.insert(key, value).is_none(),
                    "duplicate database endpoint option",
                )?;
            }
            ensure(
                options.len() == 4
                    && !options.contains_key("user")
                    && !options.contains_key("password")
                    && options.get("host") == Some(&"postgres-backend")
                    && options.get("port") == Some(&"5432")
                    && options
                        .get("dbname")
                        .is_some_and(|name| valid_identifier(name))
                    && options.get("pool_mode")
                        == Some(&if mode == "tx" {
                            "transaction"
                        } else {
                            "session"
                        }),
                "unsafe database alias",
            )?;
        }
    }
    Ok(())
}
pub fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
pub fn validate_repository(root: &Path) -> Result<serde_json::Value> {
    let ini = std::fs::read_to_string(root.join("deploy/data/pgbouncer/pgbouncer.ini"))
        .map_err(|_| "pooler config missing")?;
    validate_ini(&ini)?;
    let overlay =
        std::fs::read_to_string(root.join("deploy/compose/docker-compose.data-security.yml"))
            .map_err(|_| "data overlay missing")?;
    for required in [
        "POSTGRES_PASSWORD_FILE:",
        "POSTGRES_PASSWORD: !reset null",
        "ssl=on",
        "ssl_min_protocol_version=TLSv1.3",
        "hba_file=",
        "data-pgbouncer-backend",
        "create_host_path: false",
    ] {
        ensure(overlay.contains(required), "data overlay contract missing")?;
    }
    let pooler = std::fs::read_to_string(
        root.join("deploy/compose/docker-compose.data-session-pooling.yml"),
    )
    .map_err(|_| "session pooling overlay missing")?;
    for required in [
        "PGBOUNCER_IMAGE:?",
        "user:",
        "data-pgbouncer-backend",
        "dsn-session",
        "_MODE: session",
        "create_host_path: false",
    ] {
        ensure(
            pooler.contains(required),
            "session pooling deployment contract missing",
        )?;
    }
    let rls = std::fs::read_to_string(
        root.join("docs/design/proposals/data-platform/tenant-rls-complete.sql"),
    )
    .map_err(|_| "RLS proposal missing")?;
    for required in [
        "FORCE ROW LEVEL SECURITY",
        "AS RESTRICTIVE",
        "WITH CHECK",
        "current_setting(''app.tenant_id'',true)",
        "authority_maintenance",
        "pg_has_role",
    ] {
        ensure(rls.contains(required), "RLS guard contract missing")?;
    }
    ensure(
        !rls.contains("GRANT ALL") && !rls.contains("DISABLE ROW LEVEL SECURITY"),
        "RLS proposal broadens authority",
    )?;
    Ok(
        serde_json::json!({"status":"passed","scope":"source-policy-only","role_pools":10,"file_secrets":true,"client_and_backend_mtls":true,"runtime_acceptance":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reviewed_config_and_negative_tls_pool_identity_cases() {
        let source = include_str!("../../../../../deploy/data/pgbouncer/pgbouncer.ini");
        validate_ini(source).unwrap();
        for bad in [
            source.replace("auth_type = cert", "auth_type = trust"),
            source.replace(
                "server_tls_sslmode = verify-full",
                "server_tls_sslmode = require",
            ),
            source.replace(
                "max_prepared_statements = 200",
                "max_prepared_statements = 0",
            ),
            source.replace(
                "dbname=gateway pool_mode=transaction",
                "dbname=gateway user=postgres pool_mode=transaction",
            ),
            source.replace(
                "ai_gateway_runtime = max_user_connections=8",
                "ai_gateway_runtime = max_user_connections=99",
            ),
        ] {
            assert!(validate_ini(&bad).is_err());
        }
    }
    #[test]
    fn duplicate_and_wildcard_configs_are_not_accepted() {
        assert!(parse_ini("[x]\na=1\na=2").is_err());
        let source = include_str!("../../../../../deploy/data/pgbouncer/pgbouncer.ini");
        assert!(validate_ini(&source.replace("gateway_tx =", "* =")).is_err());
    }
}

pub mod append_partitions;

/// Structural policy validation from read-only authority exports.
pub mod rls;
