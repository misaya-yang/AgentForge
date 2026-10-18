use super::tenant::TENANT_CONTEXT_SQL;
use super::*;
use std::{collections::BTreeMap, path::PathBuf};
fn config() -> ConnectionConfig {
    ConnectionConfig {
        role: DatabaseRole::Gateway,
        mode: ConnectionMode::Transaction,
        purpose: ConnectionPurpose::TenantTransactions,
        dsn_file: "/private/dsn".into(),
        password_file: None,
        tls: TlsFiles {
            ca: "/private/ca".into(),
            certificate: "/private/client.crt".into(),
            key: "/private/client.key".into(),
        },
        expected_host: "pgbouncer".into(),
        expected_database: "gateway_tx".into(),
        physical_database: "gateway".into(),
        max_connections: 4,
        replicas: 4,
        role_connection_limit: 64,
        pooled_backend_limit: 8,
        statement_timeout_ms: 30000,
    }
}
#[test]
fn pooling_cannot_break_backend_cancellation_identity() {
    let mut c = config();
    assert!(c.validate().is_ok());
    c.purpose = ConnectionPurpose::BackendPinned;
    assert_eq!(
        c.validate().unwrap_err(),
        DataError("transaction_pool_requires_scoped_operation")
    );
    c.mode = ConnectionMode::Session;
    c.expected_database = "gateway_session".into();
    assert!(c.validate().is_ok());
    c.purpose = ConnectionPurpose::CancellationControl;
    assert!(c.validate().is_err());
    c.mode = ConnectionMode::Direct;
    c.max_connections = 2;
    assert!(c.validate().is_ok());
}
#[test]
fn role_pool_alias_and_backend_budgets_are_enforced() {
    let mut c = config();
    c.expected_database = "runtime_tx".into();
    assert!(c.validate().is_err());
    c.expected_database = "gateway_tx".into();
    c.replicas = 64;
    assert!(c.validate().is_err());
    c.replicas = 4;
    c.pooled_backend_limit = 64;
    assert!(c.validate().is_err());
}
#[test]
fn missing_and_hostile_tenants_are_not_a_global_scope() {
    for tenant in ["", " ", "a,b", "a/b", "tenant';RESET ALL;--", "*", "日本"] {
        assert!(TenantScope::new(tenant).is_err());
    }
    assert_eq!(
        TenantScope::new("tenant_A-123").unwrap().as_str(),
        "tenant_A-123"
    );
    assert!(TENANT_CONTEXT_SQL.contains("'app.tenant_id',$1,true"));
    assert!(!TENANT_CONTEXT_SQL.contains("false"));
}
#[test]
fn errors_and_debug_do_not_render_credentials_or_private_paths() {
    let mut c = config();
    c.dsn_file = PathBuf::from("/private/SECRET-dsn");
    let printed = format!("{c:?}");
    assert!(!printed.contains("SECRET"));
    assert!(!format!("{}", DataError("dsn_invalid")).contains("postgres://"));
}
#[test]
fn environment_password_and_missing_tls_fail_closed() {
    let e = BTreeMap::from([("DATA_GATEWAY_PASSWORD".into(), "SECRET".into())]);
    let err = ConnectionConfig::from_environment(
        "DATA_GATEWAY",
        DatabaseRole::Gateway,
        ConnectionPurpose::TenantTransactions,
        &e,
    )
    .unwrap_err();
    assert_eq!(err, DataError("environment_secret_refused"));
    assert!(!err.to_string().contains("SECRET"));
}
#[test]
fn material_permissions_and_symlink_checks() {
    let root = std::env::temp_dir().join(format!("data-material-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let file = root.join("secret");
    std::fs::write(&file, b"nonsecret-fixture").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_material(&file, true).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(read_material(&file, true).is_ok());
        let link = root.join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(read_material(&link, true).is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn malformed_file_dsn_and_embedded_password_never_reach_a_socket() {
    let root = std::env::temp_dir().join(format!("data-dsn-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let path = root.join("dsn");
    let mut config = config();
    config.dsn_file = path.clone();
    for value in [
        "postgresql://ai_gateway_gateway:SECRET@pgbouncer:6432/gateway_tx",
        "postgresql://postgres@pgbouncer:6432/gateway_tx",
        "postgresql://ai_gateway_gateway@another-host:6432/gateway_tx",
        "postgresql://ai_gateway_gateway@pgbouncer:6432/gateway_tx?options=-c%20role%3Dpostgres",
    ] {
        std::fs::write(&path, value).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let err = config.connect_options().unwrap_err();
        assert!(!err.to_string().contains("SECRET"));
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn task_tenancy_is_restored_and_never_inherited_implicitly() {
    assert!(current_tenant_scope().is_err());
    with_tenant_scope(TenantScope::new("tenant-a").unwrap(), async {
        assert_eq!(current_tenant_scope().unwrap().as_str(), "tenant-a");
        let child = tokio::spawn(async { current_tenant_scope().is_err() })
            .await
            .unwrap();
        assert!(child);
        with_tenant_scope(TenantScope::new("tenant-b").unwrap(), async {
            assert_eq!(current_tenant_scope().unwrap().as_str(), "tenant-b");
        })
        .await;
        assert_eq!(current_tenant_scope().unwrap().as_str(), "tenant-a");
    })
    .await;
    assert!(current_tenant_scope().is_err());
}

#[test]
fn explicit_file_material_forces_mtls_without_creating_a_connection() {
    let root = std::env::temp_dir().join(format!("data-options-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let mut c = config();
    c.dsn_file = root.join("dsn");
    c.tls = TlsFiles {
        ca: root.join("ca"),
        certificate: root.join("cert"),
        key: root.join("key"),
    };
    for (path, bytes) in [
        (
            &c.dsn_file,
            b"postgresql://ai_gateway_gateway@pgbouncer:6432/gateway_tx\n".as_slice(),
        ),
        (
            &c.tls.ca,
            b"-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----".as_slice(),
        ),
        (
            &c.tls.certificate,
            b"-----BEGIN CERTIFICATE-----\nfixture\n-----END CERTIFICATE-----".as_slice(),
        ),
        (
            &c.tls.key,
            b"-----BEGIN PRIVATE KEY-----\nfixture\n-----END PRIVATE KEY-----".as_slice(),
        ),
    ] {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
    let options = c.connect_options().unwrap();
    assert_eq!(options.get_username(), "ai_gateway_gateway");
    assert_eq!(options.get_database(), Some("gateway_tx"));
    assert!(matches!(
        options.get_ssl_mode(),
        sqlx::postgres::PgSslMode::VerifyFull
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn authenticated_tenant_cannot_be_replaced_by_a_record_or_background_scope() {
    let a = TenantScope::new("tenant-a").unwrap();
    let b = TenantScope::new("tenant-b").unwrap();
    assert!(validate_tenant_binding(&b).is_ok());
    with_tenant_scope(a.clone(), async {
        assert!(validate_tenant_binding(&a).is_ok());
        assert_eq!(
            validate_tenant_binding(&b).unwrap_err(),
            DataError("request_tenant_mismatch")
        );
    })
    .await;
    assert!(validate_tenant_binding(&b).is_ok());
}
