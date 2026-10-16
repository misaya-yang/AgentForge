//! Explicit host configuration inputs. The caller loads its authorized environment;
//! this module never opens dotenv files or prints credentials.
use super::{
    cors::CorsConfig,
    identity::IdentityConfig,
    jwt::JwtDecoder,
    middleware::MiddlewareConfig,
    resources::{DatabaseSettings, RedisSettings},
};
use ai_platform_auth::{JwtKeyConfig, JwtSigningMode};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct JwtSettings {
    pub enabled: bool,
    pub mode: JwtSigningMode,
    pub secret: String,
    pub algorithms: Vec<String>,
    pub audience: Option<String>,
    pub issuer: Option<String>,
    pub active_kid: Option<String>,
    pub keyring: Vec<JwtKeySettings>,
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct JwtKeySettings {
    pub kid: String,
    pub algorithm: String,
    pub public_jwk: Value,
    /// Name of the environment variable holding the active private PEM.
    /// Retired verification keys must not have this set.
    pub private_key_env: Option<String>,
    /// Absolute Unix time after which this retired key is neither accepted nor
    /// included in JWKS. The active signing key leaves this unset.
    pub verify_until: Option<u64>,
}
impl Default for JwtKeySettings {
    fn default() -> Self {
        Self {
            kid: String::new(),
            algorithm: String::new(),
            public_jwk: Value::Null,
            private_key_env: None,
            verify_until: None,
        }
    }
}
impl Default for JwtSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: JwtSigningMode::AsymmetricKeyring,
            secret: String::new(),
            algorithms: vec![],
            audience: None,
            issuer: None,
            active_kid: None,
            keyring: vec![],
        }
    }
}
impl JwtSettings {
    pub(crate) fn key_configs(
        &self,
        environment: &BTreeMap<String, String>,
        include_private_key: bool,
    ) -> Vec<JwtKeyConfig> {
        self.keyring
            .iter()
            .map(|key| JwtKeyConfig {
                kid: key.kid.clone(),
                algorithm: key.algorithm.clone(),
                public_jwk: key.public_jwk.clone(),
                private_key_pem: if include_private_key {
                    key.private_key_env
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .and_then(|name| environment.get(name))
                        .map(|value| value.replace("\\n", "\n"))
                } else {
                    None
                },
                verify_until: key.verify_until,
            })
            .collect()
    }

    fn configuration_shape_ready(&self) -> bool {
        match self.mode {
            JwtSigningMode::LegacySingleHs256 => {
                self.secret.trim().len() >= 32
                    && self.keyring.is_empty()
                    && (self.algorithms.is_empty() || self.algorithms.as_slice() == ["HS256"])
            }
            JwtSigningMode::AsymmetricKeyring => {
                if !self.secret.trim().is_empty()
                    || self.active_kid.as_deref().is_none_or(str::is_empty)
                    || self.keyring.is_empty()
                {
                    return false;
                }
                let Some(active) = self
                    .keyring
                    .iter()
                    .find(|key| Some(key.kid.as_str()) == self.active_kid.as_deref())
                else {
                    return false;
                };
                if active
                    .private_key_env
                    .as_deref()
                    .is_none_or(|name| name.trim().is_empty())
                    || active.verify_until.is_some()
                    || self.keyring.iter().any(|key| {
                        key.kid.is_empty()
                            || (key.kid != active.kid
                                && (key.private_key_env.is_some() || key.verify_until.is_none()))
                    })
                    || self.keyring.iter().enumerate().any(|(index, key)| {
                        self.keyring[..index]
                            .iter()
                            .any(|previous| previous.kid == key.kid)
                    })
                {
                    return false;
                }
                self.algorithms.is_empty()
                    || (self.algorithms.iter().all(|algorithm| {
                        self.keyring.iter().any(|key| key.algorithm == *algorithm)
                    }) && self
                        .keyring
                        .iter()
                        .all(|key| self.algorithms.contains(&key.algorithm)))
            }
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct ApiKeySettings {
    pub enabled: bool,
    pub header_name: String,
    pub keys: Vec<String>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DatabaseApiKeyVerifierAvailability {
    Unavailable,
    Available,
}
impl Default for ApiKeySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            header_name: ai_platform_gateway_foundation::native_contract::API_KEY_HEADER_DEFAULT
                .into(),
            keys: vec![],
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct AuthenticationSettings {
    pub jwt: JwtSettings,
    pub api_key: ApiKeySettings,
    pub guest_session_enabled: bool,
}
impl Default for AuthenticationSettings {
    fn default() -> Self {
        Self {
            jwt: JwtSettings::default(),
            api_key: ApiKeySettings::default(),
            guest_session_enabled: true,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct AnonymousSettings {
    pub enabled: bool,
    pub cookie_name: String,
    pub header_name: String,
    pub ttl_days: i64,
    pub same_site: String,
}
impl Default for AnonymousSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            cookie_name: "ag_anon_id".into(),
            header_name: "X-AG-Anonymous-Id".into(),
            ttl_days: 30,
            same_site: "lax".into(),
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct CorsSettings {
    pub allow_origins: Vec<String>,
    pub allow_credentials: bool,
    pub allow_methods: Vec<String>,
    pub allow_headers: Vec<String>,
    pub expose_headers: Vec<String>,
    pub max_age: u64,
    pub allow_origin_regex: Option<String>,
}
impl Default for CorsSettings {
    fn default() -> Self {
        Self {
            allow_origins: vec![
                "http://localhost:3000".into(),
                "http://127.0.0.1:3000".into(),
            ],
            allow_credentials: true,
            allow_methods: ["GET", "POST", "PUT", "DELETE", "OPTIONS"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            allow_headers: vec!["*".into()],
            expose_headers: vec![],
            max_age: 600,
            allow_origin_regex: None,
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct HostSettings {
    pub host: String,
    pub port: u16,
    pub services_path: String,
    pub default_model: String,
    pub model_setup_mode: String,
    pub health_check_interval: i64,
    pub task_worker_concurrency: usize,
    pub task_delivery_mode: String,
    pub database: DatabaseSettings,
    pub redis: RedisSettings,
    pub authentication: AuthenticationSettings,
    pub anonymous: AnonymousSettings,
    pub cors: CorsSettings,
    pub request_governance:
        ai_platform_gateway_foundation::request_governance::RequestGovernanceConfig,
    #[serde(flatten)]
    pub service_sections: BTreeMap<String, Value>,
}
impl Default for HostSettings {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 8080,
            services_path: "services".into(),
            default_model: "qwen3.8-flash".into(),
            model_setup_mode: "ui".into(),
            health_check_interval: 30,
            task_worker_concurrency: 2,
            task_delivery_mode: "memory".into(),
            database: DatabaseSettings::default(),
            redis: RedisSettings::default(),
            authentication: AuthenticationSettings::default(),
            anonymous: AnonymousSettings::default(),
            cors: CorsSettings::default(),
            request_governance: Default::default(),
            service_sections: BTreeMap::new(),
        }
    }
}
impl HostSettings {
    pub fn from_resolved_json(
        value: Value,
        environment: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        let mut settings: Self =
            serde_json::from_value(value).map_err(|_| "Invalid gateway settings".to_owned())?;
        if let Some(mode) = environment
            .get("GATEWAY_AUTHENTICATION__JWT__MODE")
            .or_else(|| environment.get("GATEWAY_JWT_MODE"))
        {
            settings.authentication.jwt.mode = match mode.trim() {
                "asymmetric_keyring" => JwtSigningMode::AsymmetricKeyring,
                "legacy_single_hs256" => JwtSigningMode::LegacySingleHs256,
                _ => {
                    return Err(
                        "GATEWAY_JWT_MODE must be asymmetric_keyring or legacy_single_hs256"
                            .to_owned(),
                    );
                }
            };
        }
        if let Some(v) = environment.get("DEFAULT_MODEL") {
            settings.default_model = v.clone();
        }
        if let Some(v) = environment.get("MODEL_SETUP_MODE") {
            settings.model_setup_mode = v.clone();
        }
        if !matches!(settings.model_setup_mode.as_str(), "ui" | "environment") {
            return Err("model_setup_mode must be one of: ui, environment".into());
        }
        if !matches!(settings.task_delivery_mode.as_str(), "memory" | "durable") {
            return Err("task_delivery_mode must be one of: memory, durable".into());
        }
        if settings.task_delivery_mode == "durable" && settings.task_worker_concurrency == 0 {
            return Err("durable tasks require positive task_worker_concurrency".into());
        }
        if environment
            .get("GATEWAY_ENV")
            .is_some_and(|v| v.to_lowercase() == "production")
            && settings.authentication.jwt.mode == JwtSigningMode::LegacySingleHs256
            && [
                "",
                "default-secret-change-me",
                "your-jwt-secret-key",
                "your-jwt-secret-key-change-in-production",
                "change-me-in-production-use-strong-secret",
                "secret",
                "changeme",
            ]
            .contains(&settings.authentication.jwt.secret.as_str())
        {
            return Err("JWT secret is empty or insecure".into());
        }
        Ok(settings)
    }
    pub fn auth_ready(&self) -> bool {
        self.auth_ready_with_database_api_key_verifier(
            DatabaseApiKeyVerifierAvailability::Unavailable,
        )
    }
    pub fn auth_ready_with_database_api_key_verifier(
        &self,
        database_verifier: DatabaseApiKeyVerifierAvailability,
    ) -> bool {
        let a = &self.authentication;
        !(a.jwt.enabled && !a.jwt.configuration_shape_ready()
            || a.api_key.enabled
                && a.api_key.keys.is_empty()
                && database_verifier == DatabaseApiKeyVerifierAvailability::Unavailable)
    }
    pub fn middleware(&self) -> MiddlewareConfig {
        MiddlewareConfig {
            request_governance: self.request_governance.clone(),
            anonymous_enabled: self.anonymous.enabled,
            anonymous_header: self.anonymous.header_name.clone(),
            anonymous_cookie: self.anonymous.cookie_name.clone(),
            anonymous_ttl_days: self.anonymous.ttl_days,
            anonymous_same_site: self.anonymous.same_site.clone(),
            ..Default::default()
        }
    }
    pub fn identity(&self, environment: &BTreeMap<String, String>) -> IdentityConfig {
        let a = &self.authentication;
        let mut config = IdentityConfig {
            jwt_enabled: a.jwt.enabled,
            api_key_enabled: a.api_key.enabled,
            api_keys: a.api_key.keys.clone(),
            guest_session_enabled: a.guest_session_enabled,
            ..Default::default()
        };
        // main.py does not forward AuthAPIKeySettings.header_name to the streaming config.
        if let Some(cidrs) = environment
            .get("GATEWAY_TRUSTED_PROXY_CIDRS")
            .filter(|v| !v.trim().is_empty())
        {
            config.trusted_proxy_cidrs = cidrs.split(',').map(str::to_owned).collect();
        }
        if a.jwt.enabled {
            let insecure_legacy_secret = a.jwt.mode == JwtSigningMode::LegacySingleHs256
                && [
                    "",
                    "default-secret-change-me",
                    "your-jwt-secret-key",
                    "secret",
                    "changeme",
                ]
                .contains(&a.jwt.secret.as_str());
            let dev = environment
                .get("GATEWAY_DEV_MODE")
                .is_some_and(|v| matches!(v.to_lowercase().as_str(), "1" | "true"));
            let keys = a.jwt.key_configs(environment, false);
            if !insecure_legacy_secret || dev {
                if let Ok(decoder) = JwtDecoder::from_keyring_config(
                    a.jwt.mode,
                    &a.jwt.secret,
                    a.jwt.active_kid.as_deref(),
                    &keys,
                    a.jwt.audience.as_deref(),
                    a.jwt.issuer.as_deref(),
                ) {
                    config.verify_jwt = Some(Arc::new(move |token| decoder.decode(token)));
                }
            }
        }
        config
    }
    pub fn cors(&self) -> Result<CorsConfig, String> {
        let c = &self.cors;
        Ok(CorsConfig {
            allow_origins: c.allow_origins.clone(),
            allow_credentials: c.allow_credentials,
            allow_methods: c.allow_methods.clone(),
            allow_headers: c.allow_headers.clone(),
            expose_headers: c.expose_headers.clone(),
            max_age: c.max_age,
            allow_origin_regex: c
                .allow_origin_regex
                .as_deref()
                .map(regex::Regex::new)
                .transpose()
                .map_err(|_| "Invalid CORS origin expression")?,
        }
        .normalized())
    }
}

impl HostSettings {
    /// Build the host-owned settings from an explicitly supplied environment
    /// snapshot. Dotenv loading, if desired by an executable, is an outer boundary.
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Result<Self, String> {
        let mut root = serde_json::Map::new();
        if let Some(origins) = environment.get("CORS_ALLOWED_ORIGINS") {
            root.insert("cors".into(), serde_json::json!({"allow_origins": origins.split(',').map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()}));
        }
        // Top-level JSON settings are applied before nested fields, as in the
        // canonical env_nested_delimiter="__" resolution.
        let entries: Vec<(String, &String)> = environment
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .filter(|(k, _)| k.starts_with("gateway_"))
            .collect();
        for nested in [false, true] {
            for (key, raw) in &entries {
                let key = &key[8..];
                if key.contains("__") != nested {
                    continue;
                }
                let path: Vec<&str> = key.split("__").collect();
                let first = path[0];
                if ![
                    "host",
                    "port",
                    "services_path",
                    "database",
                    "redis",
                    "authentication",
                    "rbac",
                    "rate_limits",
                    "cors",
                    "anonymous",
                    "session",
                    "load_balancer",
                    "langgraph",
                    "knowledge",
                    "image_signing",
                    "proxy",
                    "storage",
                    "metrics",
                    "health_check_interval",
                    "task_worker_concurrency",
                    "task_delivery_mode",
                ]
                .contains(&first)
                {
                    continue;
                }
                let value = if path.len() == 1
                    && ![
                        "host",
                        "port",
                        "services_path",
                        "health_check_interval",
                        "task_worker_concurrency",
                        "task_delivery_mode",
                    ]
                    .contains(&first)
                {
                    serde_json::from_str::<Value>(raw)
                        .map_err(|_| format!("Invalid JSON configuration section: {first}"))?
                } else {
                    parse_host_env_leaf(&path, raw)?
                };
                insert_setting(&mut root, &path, value)?;
            }
        }
        Self::from_resolved_json(Value::Object(root), environment)
    }
}
fn insert_setting(
    root: &mut serde_json::Map<String, Value>,
    path: &[&str],
    value: Value,
) -> Result<(), String> {
    let Some((first, rest)) = path.split_first() else {
        return Err("Empty configuration path".into());
    };
    if rest.is_empty() {
        root.insert((*first).into(), value);
        return Ok(());
    }
    let branch = root
        .entry((*first).to_owned())
        .or_insert_with(|| Value::Object(Default::default()));
    if branch.is_null() {
        *branch = Value::Object(Default::default());
    }
    let object = branch
        .as_object_mut()
        .ok_or_else(|| format!("Nested configuration is not an object: {first}"))?;
    insert_setting(object, rest, value)
}
fn parse_host_env_leaf(path: &[&str], raw: &str) -> Result<Value, String> {
    let name = path.last().copied().unwrap_or("");
    let host_owned = matches!(
        path.first().copied(),
        Some("database" | "redis" | "authentication" | "cors" | "anonymous")
    );
    if host_owned
        && matches!(
            name,
            "enabled" | "auto_init" | "guest_session_enabled" | "allow_credentials"
        )
    {
        return match raw.to_lowercase().as_str() {
            "1" | "true" | "t" | "yes" | "y" | "on" => Ok(Value::Bool(true)),
            "0" | "false" | "f" | "no" | "n" | "off" => Ok(Value::Bool(false)),
            _ => Err(format!("Invalid boolean configuration: {name}")),
        };
    }
    if matches!(
        name,
        "port" | "health_check_interval" | "task_worker_concurrency"
    ) || host_owned
        && matches!(
            name,
            "permission_cache_ttl_seconds"
                | "pool_min_size"
                | "pool_max_size"
                | "api_key_usage_flush_interval_seconds"
                | "api_key_usage_flush_batch_size"
                | "ttl_days"
                | "max_age"
        )
    {
        return raw
            .trim()
            .parse::<i64>()
            .map(|n| Value::Number(n.into()))
            .map_err(|_| format!("Invalid integer configuration: {name}"));
    }
    if host_owned
        && matches!(
            name,
            "algorithms"
                | "keyring"
                | "keys"
                | "allow_origins"
                | "allow_methods"
                | "allow_headers"
                | "expose_headers"
        )
    {
        return serde_json::from_str::<Value>(raw)
            .map_err(|_| format!("Invalid JSON configuration: {name}"));
    }
    // Service-specific sections retain explicit strings for their own canonical
    // schema conversion; secrets are never heuristically parsed or displayed.
    Ok(Value::String(raw.into()))
}

#[cfg(test)]
mod task_delivery_mode_tests {
    use super::*;
    #[test]
    fn task_delivery_mode_defaults_to_memory_and_requires_explicit_durable() {
        assert_eq!(
            HostSettings::from_environment(&BTreeMap::new())
                .unwrap()
                .task_delivery_mode,
            "memory"
        );
        let env = BTreeMap::from([("GATEWAY_TASK_DELIVERY_MODE".into(), "durable".into())]);
        assert_eq!(
            HostSettings::from_environment(&env)
                .unwrap()
                .task_delivery_mode,
            "durable"
        );
    }
    #[test]
    fn task_delivery_mode_rejects_unknown_and_zero_durable_concurrency() {
        for mode in ["typo", "auto", ""] {
            let env = BTreeMap::from([("GATEWAY_TASK_DELIVERY_MODE".into(), mode.into())]);
            assert!(HostSettings::from_environment(&env).is_err());
        }
        let env = BTreeMap::from([
            ("GATEWAY_TASK_DELIVERY_MODE".into(), "durable".into()),
            ("GATEWAY_TASK_WORKER_CONCURRENCY".into(), "0".into()),
        ]);
        assert!(HostSettings::from_environment(&env).is_err());
    }
}

#[cfg(test)]
mod jwt_mode_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn asymmetric_keyring_is_the_default_and_hmac_requires_explicit_compatibility() {
        let mut jwt = JwtSettings::default();
        assert_eq!(jwt.mode, JwtSigningMode::AsymmetricKeyring);
        assert!(!jwt.configuration_shape_ready());

        jwt.secret = "synthetic-legacy-secret-material-32-bytes".into();
        assert!(!jwt.configuration_shape_ready());
        jwt.mode = JwtSigningMode::LegacySingleHs256;
        assert!(jwt.configuration_shape_ready());
        jwt.algorithms = vec!["RS256".into()];
        assert!(!jwt.configuration_shape_ready());
    }

    #[test]
    fn environment_selects_legacy_mode_only_when_explicit() {
        let environment =
            BTreeMap::from([("GATEWAY_JWT_MODE".into(), "legacy_single_hs256".into())]);
        let settings = HostSettings::from_resolved_json(json!({}), &environment).unwrap();
        assert_eq!(
            settings.authentication.jwt.mode,
            JwtSigningMode::LegacySingleHs256
        );
    }

    #[test]
    fn asymmetric_keyring_can_be_supplied_as_nested_json_configuration() {
        let environment = BTreeMap::from([
            (
                "GATEWAY_AUTHENTICATION__JWT__ENABLED".into(),
                "true".into(),
            ),
            (
                "GATEWAY_AUTHENTICATION__JWT__MODE".into(),
                "asymmetric_keyring".into(),
            ),
            (
                "GATEWAY_AUTHENTICATION__JWT__ACTIVE_KID".into(),
                "current".into(),
            ),
            (
                "GATEWAY_AUTHENTICATION__JWT__ALGORITHMS".into(),
                r#"["EdDSA"]"#.into(),
            ),
            (
                "GATEWAY_AUTHENTICATION__JWT__KEYRING".into(),
                r#"[{"kid":"current","algorithm":"EdDSA","private_key_env":"JWT_CURRENT_PRIVATE_PEM","public_jwk":{"kty":"OKP","crv":"Ed25519","alg":"EdDSA","kid":"current","x":"fixture"}}]"#.into(),
            ),
        ]);
        let settings = HostSettings::from_environment(&environment).unwrap();
        assert!(settings.auth_ready());
        assert_eq!(settings.authentication.jwt.keyring.len(), 1);
        assert_eq!(
            settings.authentication.jwt.keyring[0]
                .private_key_env
                .as_deref(),
            Some("JWT_CURRENT_PRIVATE_PEM")
        );
    }

    #[test]
    fn private_key_material_resolves_only_from_explicit_environment_snapshot() {
        let pem = "synthetic-line-one\nsynthetic-line-two\n";
        let escaped = pem.replace('\n', "\\n");
        let settings = JwtSettings {
            mode: JwtSigningMode::AsymmetricKeyring,
            active_kid: Some("current".into()),
            keyring: vec![JwtKeySettings {
                kid: "current".into(),
                algorithm: "EdDSA".into(),
                public_jwk: json!({}),
                private_key_env: Some("JWT_CURRENT_PRIVATE_PEM".into()),
                verify_until: None,
            }],
            ..Default::default()
        };
        let environment = BTreeMap::from([("JWT_CURRENT_PRIVATE_PEM".into(), escaped)]);
        let signing_keys = settings.key_configs(&environment, true);
        let verification_keys = settings.key_configs(&environment, false);

        assert!(signing_keys[0].private_key_pem.as_deref() == Some(pem));
        assert!(verification_keys[0].private_key_pem.is_none());
    }
}

#[cfg(test)]
mod auth_readiness_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn empty_static_keys_require_an_available_database_verifier() {
        let settings = HostSettings::from_resolved_json(
            json!({"authentication":{"api_key":{"enabled":true,"keys":[]}}}),
            &BTreeMap::new(),
        )
        .unwrap();

        assert!(!settings.auth_ready());
        assert!(!settings.auth_ready_with_database_api_key_verifier(
            DatabaseApiKeyVerifierAvailability::Unavailable
        ));
        assert!(settings.auth_ready_with_database_api_key_verifier(
            DatabaseApiKeyVerifierAvailability::Available
        ));
    }

    #[test]
    fn database_api_key_readiness_does_not_override_invalid_jwt_configuration() {
        let settings = HostSettings::from_resolved_json(
            json!({"authentication":{"jwt":{"enabled":true},"api_key":{"enabled":true,"keys":[]}}}),
            &BTreeMap::new(),
        )
        .unwrap();

        assert!(!settings.auth_ready_with_database_api_key_verifier(
            DatabaseApiKeyVerifierAvailability::Available
        ));
    }

    #[test]
    fn flat_jwt_mode_is_projected_before_auth_readiness() {
        let environment = BTreeMap::from([("GATEWAY_JWT_MODE".into(), "legacy_single_hs256".into())]);
        let settings = HostSettings::from_resolved_json(
            json!({"authentication":{"jwt":{"enabled":true,"secret":"synthetic-legacy-secret-material-32-bytes","algorithms":["HS256"]}}}),
            &environment,
        )
        .unwrap();

        assert_eq!(settings.authentication.jwt.mode, JwtSigningMode::LegacySingleHs256);
        assert!(settings.auth_ready());
    }
}
