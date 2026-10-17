//! Shared current-account authority. Static keys retain source fallback semantics.
use super::{BootstrapError, DatabasePools};
use crate::migration_content::{
    ContentDatabase,
    access::{EndpointIdentityResolver, StaticKeyFallback},
};
use ai_platform_auth::{
    PasswordEngine, TokenCodec, accounts::PgAccounts, admission::AdmissionService,
    service::AuthService, sessions::RedisSessions,
};
use axum::http::HeaderName;
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub struct AuthConfiguration {
    pub jwt_secret: String,
    pub access_keys: Option<ai_platform_auth::access_tokens::EdDsaKeyRing>,
    pub jwt_audience: Option<String>,
    pub jwt_issuer: Option<String>,
    pub jwt_algorithms: Vec<String>,
    pub jwt_enabled: bool,
    pub api_key_enabled: bool,
    pub api_key_header: String,
    pub static_api_keys: Vec<String>,
    pub static_key_role: String,
    pub account_domain: String,
    pub password_concurrency: usize,
    pub enterprise_policy_enabled: bool,
}
pub struct AuthServices {
    pub auth: Arc<AuthService>,
    pub admission: Arc<AdmissionService>,
    pub identities: Arc<EndpointIdentityResolver>,
    pub introspection: Arc<super::super::credential_introspection::CredentialIntrospection>,
}
impl AuthServices {
    pub fn new(
        pools: &DatabasePools,
        redis: ConnectionManager,
        config: AuthConfiguration,
    ) -> Result<Self, BootstrapError> {
        // The existing algorithm setting owns legacy HS256 compatibility.
        // EdDSA issuance/rotation uses a separate explicit asymmetric key ring.
        if config.jwt_algorithms != ["HS256"] {
            return Err(BootstrapError::Configuration("jwt_algorithms"));
        }
        let header = HeaderName::from_bytes(config.api_key_header.as_bytes())
            .map_err(|_| BootstrapError::Configuration("api_key_header"))?;
        let legacy_decoder = Arc::new(
            super::super::jwt::JwtDecoder::new(
                &config.jwt_secret,
                &config.jwt_algorithms,
                config.jwt_audience.as_deref(),
                config.jwt_issuer.as_deref(),
            )
            .map_err(|_| BootstrapError::Configuration("jwt"))?,
        );
        let mut codec = TokenCodec::new(
            config.jwt_secret.as_bytes(),
            config.jwt_audience,
            config.jwt_issuer,
        )
        .map_err(|_| BootstrapError::Configuration("jwt"))?;
        if let Some(keys) = config.access_keys {
            codec = codec
                .with_eddsa(keys)
                .map_err(|_| BootstrapError::Configuration("eddsa_token_keys"))?;
        }
        let passwords = PasswordEngine::new(config.password_concurrency)
            .map_err(|_| BootstrapError::Configuration("password_concurrency"))?;
        let auth = Arc::new(
            AuthService::new(
                PgAccounts::new(pools.data.clone()),
                RedisSessions::new(redis.clone()),
                codec,
                passwords,
                config.account_domain,
            )
            .with_enterprise_policy_enabled(config.enterprise_policy_enabled),
        );
        let introspection = Arc::new(
            super::super::credential_introspection::CredentialIntrospection {
                pool: pools.data.clone(),
                decoder: Arc::new(
                    super::super::credential_introspection::RotatingIntrospectionDecoder::new(
                        legacy_decoder,
                        {
                            let auth = auth.clone();
                            Arc::new(move |token| {
                                auth.verify_access_claims(token)
                                    .and_then(|claims| {
                                        serde_json::to_value(claims).map_err(|_| {
                                            ai_platform_auth::service::ServiceError::Unavailable
                                        })
                                    })
                                    .map_err(|e| e.to_string())
                            })
                        },
                    )
                    .with_enterprise_policy_enabled(config.enterprise_policy_enabled),
                ),
                sessions: Some(RedisSessions::new(redis.clone())),
                jwt_enabled: config.jwt_enabled,
                api_key_enabled: config.api_key_enabled,
                api_key_header: header.clone(),
                static_key_hashes: config
                    .static_api_keys
                    .iter()
                    .map(|v| format!("{:x}", Sha256::digest(v.as_bytes())))
                    .collect(),
                static_key_role: config.static_key_role.clone(),
            },
        );
        let admission = Arc::new(
            AdmissionService::new(auth.clone(), pools.data.clone(), header.clone())
                .map_err(|_| BootstrapError::Configuration("admission"))?,
        );
        let static_keys = if config.static_api_keys.is_empty() {
            None
        } else {
            Some(StaticKeyFallback {
                database: Some(
                    ContentDatabase::new(pools.data.clone(), pools.control.clone())
                        .map_err(|_| BootstrapError::Construction("static_key_database"))?,
                ),
                key_hashes: config
                    .static_api_keys
                    .iter()
                    .map(|v| format!("{:x}", Sha256::digest(v.as_bytes())))
                    .collect(),
                role: config.static_key_role,
            })
        };
        let identities = Arc::new(EndpointIdentityResolver {
            admission: admission.clone(),
            jwt_enabled: config.jwt_enabled,
            api_key_enabled: config.api_key_enabled,
            api_key_header: header,
            static_keys,
        });
        Ok(Self {
            auth,
            admission,
            identities,
            introspection,
        })
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessKeyConfiguration {
    active_kid: String,
    private_key_file: String,
    public_keys: Vec<PublicAccessKey>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicAccessKey {
    kid: String,
    public_key_file: String,
    not_before: u64,
    not_after: u64,
}
pub(super) fn key_file(path: &str, private: bool) -> Result<String, BootstrapError> {
    use std::io::Read;
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return Err(BootstrapError::Configuration("auth_key_file_absolute_path"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options
        .open(path)
        .map_err(|_| BootstrapError::Configuration("auth_key_file"))?;
    let metadata = file
        .metadata()
        .map_err(|_| BootstrapError::Configuration("auth_key_file"))?;
    if !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Err(BootstrapError::Configuration("auth_key_file_type_or_size"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let forbidden = if private { 0o077 } else { 0o022 };
        if metadata.permissions().mode() & forbidden != 0
            || private && metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(BootstrapError::Configuration("auth_key_file_permissions"));
        }
    }
    #[cfg(not(unix))]
    if private {
        return Err(BootstrapError::Configuration(
            "auth_private_file_acl_required",
        ));
    }
    let mut bytes = Vec::new();
    file.take(16 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BootstrapError::Configuration("auth_key_file"))?;
    if bytes.len() > 16 * 1024 {
        return Err(BootstrapError::Configuration("auth_key_file_type_or_size"));
    }
    String::from_utf8(bytes).map_err(|_| BootstrapError::Configuration("auth_key_file_encoding"))
}

pub fn access_keys(
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<Option<ai_platform_auth::access_tokens::EdDsaKeyRing>, BootstrapError> {
    let Some(path) = environment.get("AI_PLATFORM_API_EDDSA_CONFIG_FILE") else {
        return Ok(None);
    };
    let config: AccessKeyConfiguration = serde_json::from_str(&key_file(path, false)?)
        .map_err(|_| BootstrapError::Configuration("eddsa_key_configuration"))?;
    let private = key_file(&config.private_key_file, true)?;
    let public = config
        .public_keys
        .into_iter()
        .map(|key| {
            Ok(ai_platform_auth::access_tokens::PublicKeyConfig {
                kid: key.kid,
                public_pem: key_file(&key.public_key_file, false)?,
                not_before: key.not_before,
                not_after: key.not_after,
            })
        })
        .collect::<Result<Vec<_>, BootstrapError>>()?;
    let keys =
        ai_platform_auth::access_tokens::EdDsaKeyRing::new(config.active_kid, &private, public)
            .map_err(|_| BootstrapError::Configuration("eddsa_key_configuration"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| BootstrapError::Configuration("auth_clock"))?
        .as_secs();
    keys.check_signing_window(now)
        .map_err(|_| BootstrapError::Configuration("eddsa_signing_window"))?;
    Ok(Some(keys))
}

#[cfg(all(test, unix))]
mod access_key_file_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn private_key_file_permissions_and_symlinks_fail_closed() {
        let path =
            std::env::temp_dir().join(format!("agentforge-key-file-test-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, "synthetic test material").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(key_file(path.to_str().unwrap(), true).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(key_file(path.to_str().unwrap(), true).is_ok());
        let link = path.with_extension("symlink");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(key_file(link.to_str().unwrap(), true).is_err());
        std::fs::remove_file(link).unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
