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
        // TokenCodec supports HS256; never silently reinterpret other algorithms.
        if config.jwt_algorithms != ["HS256"] {
            return Err(BootstrapError::Configuration("jwt_algorithms"));
        }
        let header = HeaderName::from_bytes(config.api_key_header.as_bytes())
            .map_err(|_| BootstrapError::Configuration("api_key_header"))?;
        let introspection = Arc::new(
            super::super::credential_introspection::CredentialIntrospection {
                pool: pools.data.clone(),
                decoder: Arc::new(
                    super::super::jwt::JwtDecoder::new(
                        &config.jwt_secret,
                        &config.jwt_algorithms,
                        config.jwt_audience.as_deref(),
                        config.jwt_issuer.as_deref(),
                    )
                    .map_err(|_| BootstrapError::Configuration("jwt"))?,
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
        let codec = TokenCodec::new(
            config.jwt_secret.as_bytes(),
            config.jwt_audience,
            config.jwt_issuer,
        )
        .map_err(|_| BootstrapError::Configuration("jwt"))?;
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
