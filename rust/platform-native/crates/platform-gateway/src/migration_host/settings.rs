//! Explicit host configuration inputs. The caller loads its authorized environment;
//! this module never opens dotenv files or prints credentials.
use super::{
    cors::CorsConfig,
    identity::IdentityConfig,
    jwt::JwtDecoder,
    middleware::MiddlewareConfig,
    resources::{DatabaseSettings, RedisSettings},
};
use serde::Deserialize;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct JwtSettings {
    pub enabled: bool,
    pub secret: String,
    pub algorithms: Vec<String>,
    pub audience: Option<String>,
    pub issuer: Option<String>,
}
impl Default for JwtSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            secret: String::new(),
            algorithms: vec!["HS256".into()],
            audience: None,
            issuer: None,
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
            task_delivery_mode: "schema_admitted".into(),
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
        if let Some(v) = environment.get("DEFAULT_MODEL") {
            settings.default_model = v.clone();
        }
        if let Some(v) = environment.get("MODEL_SETUP_MODE") {
            settings.model_setup_mode = v.clone();
        }
        if !matches!(settings.model_setup_mode.as_str(), "ui" | "environment") {
            return Err("model_setup_mode must be one of: ui, environment".into());
        }
        if !matches!(settings.task_delivery_mode.as_str(), "memory" | "durable" | "schema_admitted") {
            return Err("task_delivery_mode must be one of: memory, durable, schema_admitted".into());
        }
        if settings.task_delivery_mode != "memory" && settings.task_worker_concurrency == 0 {
            return Err("durable tasks require positive task_worker_concurrency".into());
        }
        if environment
            .get("GATEWAY_ENV")
            .is_some_and(|v| v.to_lowercase() == "production")
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
        let a = &self.authentication;
        !(a.jwt.enabled && (a.jwt.secret.trim().is_empty() || a.jwt.algorithms.is_empty())
            || a.api_key.enabled && a.api_key.keys.is_empty())
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
        if a.jwt.enabled && !a.jwt.secret.is_empty() {
            let insecure = [
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
            if !insecure || dev {
                if let Ok(decoder) = JwtDecoder::new(
                    &a.jwt.secret,
                    &a.jwt.algorithms,
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
    fn task_delivery_mode_defaults_to_schema_admitted_and_preserves_explicit_durable() {
        assert_eq!(
            HostSettings::from_environment(&BTreeMap::new())
                .unwrap()
                .task_delivery_mode,
            "schema_admitted"
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
