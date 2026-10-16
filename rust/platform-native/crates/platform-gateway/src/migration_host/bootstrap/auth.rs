//! Shared current-account authority. Static keys retain source fallback semantics.
use super::{BootstrapError, DatabasePools};
use crate::migration_content::{
    ContentDatabase,
    access::{EndpointIdentityResolver, StaticKeyFallback},
};
use ai_platform_auth::{
    JwtKeyConfig, JwtSigningMode, PasswordEngine, TokenCodec, accounts::PgAccounts,
    admission::AdmissionService, service::AuthService, sessions::RedisSessions,
};
use axum::http::HeaderName;
use redis::aio::ConnectionManager;
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub struct AuthConfiguration {
    pub jwt_secret: String,
    pub jwt_mode: JwtSigningMode,
    pub jwt_active_kid: Option<String>,
    pub jwt_keys: Vec<JwtKeyConfig>,
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
        if config.jwt_mode == JwtSigningMode::LegacySingleHs256
            && !config.jwt_algorithms.is_empty()
            && config.jwt_algorithms != ["HS256"]
        {
            return Err(BootstrapError::Configuration("jwt_algorithms"));
        }
        if config.jwt_mode == JwtSigningMode::AsymmetricKeyring {
            let configured: std::collections::BTreeSet<_> =
                config.jwt_algorithms.iter().map(String::as_str).collect();
            let keyring_algorithms: std::collections::BTreeSet<_> = config
                .jwt_keys
                .iter()
                .map(|key| key.algorithm.as_str())
                .collect();
            if !configured.is_empty() && configured != keyring_algorithms {
                return Err(BootstrapError::Configuration("jwt_algorithms"));
            }
        }
        let header = HeaderName::from_bytes(config.api_key_header.as_bytes())
            .map_err(|_| BootstrapError::Configuration("api_key_header"))?;
        let codec = TokenCodec::from_keyring(
            config.jwt_mode,
            config.jwt_secret.as_bytes(),
            config.jwt_active_kid.as_deref(),
            &config.jwt_keys,
            config.jwt_audience.clone(),
            config.jwt_issuer.clone(),
        )
        .map_err(|_| BootstrapError::Configuration("jwt_keyring"))?;
        let introspection = Arc::new(
            super::super::credential_introspection::CredentialIntrospection {
                pool: pools.data.clone(),
                decoder: Arc::new(
                    super::super::jwt::JwtDecoder::from_keyring(
                        codec.keyring(),
                        config.jwt_audience.as_deref(),
                        config.jwt_issuer.as_deref(),
                    )
                    .map_err(|_| BootstrapError::Configuration("jwt_keyring"))?,
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
        let passwords = PasswordEngine::new(config.password_concurrency)
            .map_err(|_| BootstrapError::Configuration("password_concurrency"))?;
        let auth = Arc::new(AuthService::new(
            PgAccounts::new(pools.data.clone()),
            RedisSessions::new(redis),
            codec,
            passwords,
            config.account_domain,
        ));
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
