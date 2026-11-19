//! Authority-owned file credentials. Secrets stay in memory; never argv, env or diagnostics.
use crate::{AuthorityError, Result, credentials};
use sha2::{Digest, Sha256};
use sqlx::{
    ConnectOptions,
    postgres::{PgConnectOptions, PgSslMode},
};
use std::{collections::BTreeMap, io::Read, path::Path, str::FromStr};

pub struct PrincipalConnection {
    pub(crate) dsn: String,
    pub(crate) options: PgConnectOptions,
    pub(crate) tls_key_identity: Option<String>,
    pub(crate) password_identity: Option<String>,
}
impl std::fmt::Debug for PrincipalConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrincipalConnection { credentials: [withheld] }")
    }
}
fn denied(code: &str) -> AuthorityError {
    AuthorityError::Refused(format!("authority credential {code}; values withheld"))
}
fn present<'a>(e: &'a BTreeMap<String, String>, key: &str) -> Option<&'a str> {
    e.get(key).map(String::as_str).filter(|s| !s.is_empty())
}
fn required<'a>(e: &'a BTreeMap<String, String>, prefix: &str, name: &str) -> Result<&'a str> {
    present(e, &format!("{prefix}_{name}")).ok_or_else(|| denied("file configuration incomplete"))
}
/// Same 0400/0440/0600/0640 policy as common ConnectionConfig, also applied to
/// authority CA/cert files. Projected secret final symlinks must use subPath.
pub(crate) fn private_bytes(path: &Path) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        return Err(denied("file path must be absolute"));
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| denied("file unavailable"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > 65536
    {
        return Err(denied("file shape refused"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !matches!(
            metadata.permissions().mode() & 0o7777,
            0o400 | 0o440 | 0o600 | 0o640
        ) {
            return Err(denied("file permissions refused"));
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|_| denied("file unavailable"))?
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| denied("file unavailable"))?;
    if bytes.is_empty() || bytes.len() > 65536 {
        return Err(denied("file size refused"));
    }
    Ok(bytes)
}
fn text(path: &str) -> Result<String> {
    let raw = String::from_utf8(private_bytes(Path::new(path))?)
        .map_err(|_| denied("file encoding refused"))?;
    let value = raw
        .strip_suffix("\n")
        .unwrap_or(&raw)
        .strip_suffix('\r')
        .unwrap_or(raw.strip_suffix("\n").unwrap_or(&raw));
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(denied("file text shape refused"));
    }
    Ok(value.into())
}
fn alias(principal: &str) -> &'static str {
    match principal {
        "admin" => "DATA_ADMIN",
        "migrator" => "DATA_MIGRATION",
        "gateway" => "DATA_GATEWAY",
        "runtime" => "DATA_RUNTIME",
        "capability_worker" => "DATA_CAPABILITY_WORKER",
        "knowledge_api" => "DATA_KNOWLEDGE",
        "knowledge_worker" => "DATA_KNOWLEDGE_WORKER",
        _ => "",
    }
}
pub fn resolve(
    e: &BTreeMap<String, String>,
    principal: &str,
    role_prefix: &str,
) -> Result<PrincipalConnection> {
    if !credentials::ALL_PRINCIPALS.contains(&principal) {
        return Err(denied("unknown principal"));
    }
    credentials::validate_role_prefix(role_prefix)?;
    let canonical = format!("AI_GATEWAY_DATABASE_{}", principal.to_ascii_uppercase());
    let common = alias(principal);
    let file_prefixes: Vec<_> = [canonical.as_str(), common]
        .into_iter()
        .filter(|p| present(e, &format!("{p}_DSN_FILE")).is_some())
        .collect();
    if file_prefixes.len() > 1 {
        return Err(denied("ambiguous file prefixes"));
    }
    for p in [canonical.as_str(), common] {
        if present(e, &format!("{p}_PASSWORD")).is_some()
            || (p == common && present(e, &format!("{p}_DSN")).is_some())
        {
            return Err(denied("plaintext environment secret refused"));
        }
    }
    let legacy = present(e, &format!("{canonical}_DSN"));
    let Some(prefix) = file_prefixes.first().copied() else {
        let dsn = legacy.ok_or_else(|| denied("dedicated principal DSN required"))?;
        // Preserve the explicitly selected legacy local interface; no inferred/shared DSN.
        let options = PgConnectOptions::from_str(dsn)
            .map_err(|_| denied("DSN invalid"))?
            .disable_statement_logging();
        return Ok(PrincipalConnection {
            dsn: dsn.into(),
            options,
            tls_key_identity: None,
            password_identity: None,
        });
    };
    if legacy.is_some() {
        return Err(denied("file and environment DSN conflict"));
    }
    if present(e, &format!("{prefix}_MODE")).is_some_and(|s| s != "direct") {
        return Err(denied("authority requires direct backend connection"));
    }
    let dsn = text(required(e, prefix, "DSN_FILE")?)?;
    let mut url = url::Url::parse(&dsn).map_err(|_| denied("file DSN invalid"))?;
    let expected_host = required(e, prefix, "EXPECTED_HOST")?;
    let database = required(e, prefix, "DATABASE_ALIAS")?;
    let physical = required(e, prefix, "PHYSICAL_DATABASE")?;
    let role = if principal == "admin" {
        required(e, prefix, "EXPECTED_ROLE")?.to_owned()
    } else {
        format!("{role_prefix}{principal}")
    };
    if present(e, &format!("{prefix}_EXPECTED_ROLE")).is_some_and(|v| v != role)
        || database != physical
        || !matches!(url.scheme(), "postgres" | "postgresql")
        || url.host_str() != Some(expected_host)
        || url.path() != format!("/{database}")
        || url.username() != role
        || url.port().is_none_or(|v| v == 0)
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(denied("file DSN role or direct endpoint refused"));
    }
    let password = present(e, &format!("{prefix}_PASSWORD_FILE"))
        .map(text)
        .transpose()?
        .unwrap_or_default();
    let ca = private_bytes(Path::new(required(e, prefix, "TLS_CA_FILE")?))?;
    let cert = private_bytes(Path::new(required(e, prefix, "TLS_CERT_FILE")?))?;
    let key = private_bytes(Path::new(required(e, prefix, "TLS_KEY_FILE")?))?;
    if ca.windows(11).any(|v| v == b"PRIVATE KEY")
        || cert.windows(11).any(|v| v == b"PRIVATE KEY")
        || !ca.windows(27).any(|v| v == b"-----BEGIN CERTIFICATE-----")
        || !cert
            .windows(27)
            .any(|v| v == b"-----BEGIN CERTIFICATE-----")
        || !key.windows(11).any(|v| v == b"PRIVATE KEY")
    {
        return Err(denied("TLS PEM shape refused"));
    }
    let options = PgConnectOptions::from_str(&dsn).map_err(|_| denied("file DSN invalid"))?;
    if options.get_options().is_some() || options.get_username() != role {
        return Err(denied("startup identity override refused"));
    }
    let options = options
        .password(&password)
        .ssl_mode(PgSslMode::VerifyFull)
        .ssl_root_cert_from_pem(ca)
        .ssl_client_cert_from_pem(cert)
        .ssl_client_key_from_pem(key.clone())
        .disable_statement_logging();
    if !password.is_empty() {
        url.set_password(Some(&password))
            .map_err(|_| denied("password binding failed"))?;
    }
    Ok(PrincipalConnection {
        dsn: url.to_string(),
        options,
        // PEM line wrapping/CRLF must not make reused client key material look
        // distinct. The TLS backend still parses and authenticates the key.
        tls_key_identity: Some(format!(
            "{:x}",
            Sha256::digest(
                key.into_iter()
                    .filter(|b| !b.is_ascii_whitespace())
                    .collect::<Vec<_>>()
            )
        )),
        password_identity: (!password.is_empty())
            .then(|| format!("{:x}", Sha256::digest(password.as_bytes()))),
    })
}
/// Bootstrap's existing pure policy consumes canonical DSNs from an in-memory
/// snapshot. No set_var, subprocess, shell expansion or plaintext log occurs.
pub(crate) fn bootstrap_values(
    e: &BTreeMap<String, String>,
    prefix: &str,
) -> Result<BTreeMap<String, String>> {
    let mut result = e.clone();
    let mut keys = std::collections::BTreeSet::new();
    for principal in credentials::ALL_PRINCIPALS {
        let connection = resolve(e, principal, prefix)?;
        if let Some(identity) = connection.tls_key_identity {
            if !keys.insert(identity) {
                return Err(denied("shared TLS client key refused"));
            }
        }
        result.insert(
            format!("AI_GATEWAY_DATABASE_{}_DSN", principal.to_ascii_uppercase()),
            connection.dsn,
        );
    }
    if !keys.is_empty() && keys.len() != credentials::ALL_PRINCIPALS.len() {
        return Err(denied(
            "file TLS bootstrap cannot mix legacy plaintext connections",
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Files(std::path::PathBuf);
    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    impl Files {
        fn new() -> Self {
            let root =
                std::env::temp_dir().join(format!("authority-file-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
        fn put(&self, name: &str, text: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            path.to_string_lossy().into_owned()
        }
        fn settings(&self, prefix: &str, role: &str) -> BTreeMap<String, String> {
            let mut e = BTreeMap::new();
            for (name, value) in [
                (
                    "DSN_FILE",
                    self.put(
                        "dsn",
                        &format!("postgresql://{role}@postgres:5432/gateway\n"),
                    ),
                ),
                (
                    "PASSWORD_FILE",
                    self.put("password", "private-file-only-sentinel-0000000000\n"),
                ),
                (
                    "TLS_CA_FILE",
                    self.put(
                        "ca",
                        "-----BEGIN CERTIFICATE-----\nCA\n-----END CERTIFICATE-----",
                    ),
                ),
                (
                    "TLS_CERT_FILE",
                    self.put(
                        "cert",
                        "-----BEGIN CERTIFICATE-----\nCERT\n-----END CERTIFICATE-----",
                    ),
                ),
                (
                    "TLS_KEY_FILE",
                    self.put(
                        "key",
                        "-----BEGIN PRIVATE KEY-----\nKEY\n-----END PRIVATE KEY-----",
                    ),
                ),
                ("MODE", "direct".into()),
                ("EXPECTED_HOST", "postgres".into()),
                ("DATABASE_ALIAS", "gateway".into()),
                ("PHYSICAL_DATABASE", "gateway".into()),
                ("EXPECTED_ROLE", role.into()),
            ] {
                e.insert(format!("{prefix}_{name}"), value);
            }
            e
        }
    }
    #[test]
    fn canonical_and_common_authority_aliases_bind_explicit_roles_without_diagnostics() {
        for (prefix, principal, role) in [
            (
                "AI_GATEWAY_DATABASE_MIGRATOR",
                "migrator",
                "custom_migrator",
            ),
            ("DATA_MIGRATION", "migrator", "custom_migrator"),
            ("DATA_ADMIN", "admin", "bootstrap_admin"),
        ] {
            let files = Files::new();
            let e = files.settings(prefix, role);
            let result = resolve(&e, principal, "custom_").unwrap();
            assert_eq!(result.options.get_username(), role);
            assert_eq!(result.options.get_database(), Some("gateway"));
            assert_eq!(result.options.get_host(), "postgres");
            assert_eq!(result.options.get_port(), 5432);
            assert!(!format!("{result:?}").contains("sentinel"));
            assert!(result.tls_key_identity.is_some());
        }
    }
    #[test]
    fn file_contract_refuses_override_identity_missing_tls_embedded_and_environment_secrets() {
        let files = Files::new();
        let original = files.settings("DATA_MIGRATION", "ai_gateway_migrator");
        for (key, value) in [
            ("DATA_MIGRATION_MODE", "transaction"),
            ("DATA_MIGRATION_PHYSICAL_DATABASE", "other"),
            ("DATA_MIGRATION_EXPECTED_HOST", "foreign"),
            ("DATA_MIGRATION_EXPECTED_ROLE", "postgres"),
            ("DATA_MIGRATION_PASSWORD", "private-sentinel"),
            ("AI_GATEWAY_DATABASE_MIGRATOR_DSN", "private-sentinel"),
        ] {
            let mut e = original.clone();
            e.insert(key.into(), value.into());
            let error = resolve(&e, "migrator", "ai_gateway_")
                .unwrap_err()
                .to_string();
            assert!(!error.contains("private-sentinel"));
        }
        for dsn in [
            "postgresql://postgres@postgres:5432/gateway",
            "postgresql://ai_gateway_migrator:private-sentinel@postgres:5432/gateway",
            "postgresql://ai_gateway_migrator@postgres:5432/gateway?options=-crole=postgres",
            "postgresql://ai_gateway_migrator@postgres/gateway",
        ] {
            std::fs::write(&original["DATA_MIGRATION_DSN_FILE"], dsn).unwrap();
            assert!(resolve(&original, "migrator", "ai_gateway_").is_err());
        }
        let mut e = files.settings("DATA_MIGRATION", "ai_gateway_migrator");
        e.remove("DATA_MIGRATION_TLS_KEY_FILE");
        assert!(resolve(&e, "migrator", "ai_gateway_").is_err());
        let mut e = original.clone();
        e.insert(
            "AI_GATEWAY_DATABASE_MIGRATOR_DSN_FILE".into(),
            original["DATA_MIGRATION_DSN_FILE"].clone(),
        );
        assert!(resolve(&e, "migrator", "ai_gateway_").is_err());
    }
    #[cfg(unix)]
    #[test]
    fn private_file_shape_permissions_and_symlinks_fail_closed() {
        use std::os::unix::fs::PermissionsExt;
        let files = Files::new();
        let path = std::path::PathBuf::from(files.put("secret", "private-sentinel"));
        for mode in [0o644, 0o620, 0o601, 0o700] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(private_bytes(&path).is_err());
        }
        for mode in [0o400, 0o440, 0o600, 0o640] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(private_bytes(&path).is_ok());
        }
        let link = files.0.join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(private_bytes(&link).is_err());
        assert!(private_bytes(&files.0).is_err());
        assert!(private_bytes(Path::new("relative-secret")).is_err());
    }
}
