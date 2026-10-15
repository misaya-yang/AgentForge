//! Pure projection of resolved settings and a caller-captured environment.
//! No environment/file reads, secret logging, connections or workers occur here.
use super::{
    BootstrapError,
    foundation::{FoundationConfiguration, FoundationSharedConfiguration},
    foundation_backends::NativeFoundationConfiguration,
    settings_projection::SettingsProjection,
    timestamps::DatabaseTimestampPolicy,
};
use crate::{
    assistant_catalog::CapabilityCatalog,
    auth_config_api::AuthConfigSettings,
    client_ip::TrustedProxies,
    connector_user_types::ConnectorEnvironment,
    migration_host::settings::HostSettings,
    rate_config_api::{boolean, integer},
    system_status_api::SystemStatusSettingsSnapshot,
    usage_parser::python_whitespace,
    users_api::UsersConfiguration,
};
use chrono::FixedOffset;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

/// Native scheduling and presentation inputs, distinct from Python settings.
/// Python's usage error enumerates a set, so its ordering varies by process.
pub struct NativeFoundationInputs {
    pub password_concurrency: usize,
    pub usage_dimension_order: [String; 5],
}
impl Default for NativeFoundationInputs {
    fn default() -> Self {
        Self {
            // Match the shared native authentication projection's bounded pool.
            password_concurrency: 2,
            usage_dimension_order: ["model", "user", "assistant", "service", "provider"]
                .map(str::to_owned),
        }
    }
}

pub fn project(
    settings: &HostSettings,
    environment: &BTreeMap<String, String>,
    local_offset: FixedOffset,
) -> Result<
    (
        FoundationConfiguration,
        FoundationSharedConfiguration,
        NativeFoundationConfiguration,
    ),
    BootstrapError,
> {
    project_with_inputs(
        settings,
        environment,
        local_offset,
        NativeFoundationInputs::default(),
    )
}

pub fn project_with_inputs(
    settings: &HostSettings,
    environment: &BTreeMap<String, String>,
    local_offset: FixedOffset,
    native: NativeFoundationInputs,
) -> Result<
    (
        FoundationConfiguration,
        FoundationSharedConfiguration,
        NativeFoundationConfiguration,
    ),
    BootstrapError,
> {
    if native.password_concurrency == 0 {
        return Err(BootstrapError::Configuration("password_concurrency"));
    }
    let mut dimensions = native.usage_dimension_order.each_ref().map(String::as_str);
    dimensions.sort_unstable();
    if dimensions != ["assistant", "model", "provider", "service", "user"] {
        return Err(BootstrapError::Configuration("usage_dimension_order"));
    }
    let projection = SettingsProjection {
        settings,
        environment,
    };
    // An explicit source override cannot silently select the bundled catalog.
    // Callers needing a file must load authorized bytes separately and replace
    // FoundationConfiguration.catalog with CapabilityCatalog::from_bytes.
    if environment
        .get("AI_PLATFORM_CAPABILITY_CATALOG_PATH")
        .is_some_and(|value| !value.trim_matches(python_whitespace).is_empty())
    {
        return Err(BootstrapError::Configuration(
            "capability_catalog_path_requires_explicit_bytes",
        ));
    }
    let balancer = object_section(&projection, "load_balancer")?;
    let proxy = object_section(&projection, "proxy")?;
    let metrics = object_section(&projection, "metrics")?;
    let rates = object_section(&projection, "rate_limits")?;
    let strategy = match balancer.get("strategy") {
        None => "round_robin".to_owned(),
        Some(Value::String(value)) => value.clone(),
        _ => return Err(BootstrapError::Configuration("load_balancer.strategy")),
    };
    let cache_seconds = float_field(&proxy, "config_cache_ttl", 60.0)
        .ok_or(BootstrapError::Configuration("proxy.config_cache_ttl"))?;
    // Negative source TTLs expire immediately. Rust Duration cannot represent
    // nonfinite or out-of-range TTLs; reject rather than panic or invent a TTL.
    let proxy_cache_ttl = if cache_seconds.is_finite() {
        Duration::try_from_secs_f64(cache_seconds.max(0.0)).ok()
    } else {
        None
    }
    .ok_or(BootstrapError::Configuration("proxy.config_cache_ttl"))?;
    let service_access_ttl = proxy
        .get("constraint_cache_ttl_seconds")
        .cloned()
        .unwrap_or_else(|| json!(30.0));
    let latency_sample_cap = metrics
        .get("latency_sample_cap")
        .map(integer)
        .transpose()
        .map_err(|_| BootstrapError::Configuration("metrics.latency_sample_cap"))?
        .unwrap_or(10_000);
    let rate_limits_enabled = rates
        .get("enabled")
        .map(boolean)
        .transpose()
        .map_err(|_| BootstrapError::Configuration("rate_limits.enabled"))?
        .unwrap_or(false);
    let mcp_enabled = projection.enabled("AGENT_STUDIO_MCP_ENABLED", true);
    // These conversions in main.py are strict; invalid values abort startup.
    let mcp_client_ttl_seconds = environment
        .get("ASSISTANT_MCP_CLIENT_CACHE_TTL_SECONDS")
        .map(|value| value.trim_matches(python_whitespace).parse::<f64>())
        .transpose()
        .map_err(|_| BootstrapError::Configuration("ASSISTANT_MCP_CLIENT_CACHE_TTL_SECONDS"))?
        .unwrap_or(60.0);
    let mcp_client_max_entries = projection
        .env_int("ASSISTANT_MCP_CLIENT_CACHE_MAX_ENTRIES", 100)?
        .clamp(1, 1000) as usize;
    let gateway_encryption_key = projection.env("GATEWAY_ENCRYPTION_KEY", "");
    let console_public_base_url = projection
        .env("CONSOLE_PUBLIC_BASE_URL", "")
        .trim_matches(python_whitespace)
        .to_owned();
    let trusted_proxies = match environment
        .get("GATEWAY_TRUSTED_PROXY_CIDRS")
        .map(|value| value.trim_matches(python_whitespace))
        .filter(|value| !value.is_empty())
    {
        Some(value) => TrustedProxies::parse(value),
        None => TrustedProxies::default(),
    };
    let authentication = &settings.authentication;
    Ok((
        FoundationConfiguration {
            timestamps: DatabaseTimestampPolicy {
                naive_offset: local_offset,
            },
            auth: AuthConfigSettings {
                jwt_enabled: authentication.jwt.enabled,
                jwt_algorithms: authentication.jwt.algorithms.clone(),
                jwt_issuer: authentication.jwt.issuer.clone(),
                jwt_audience: authentication.jwt.audience.clone(),
                api_key_enabled: authentication.api_key.enabled,
                api_key_header: authentication.api_key.header_name.clone(),
                api_key_count: authentication.api_key.keys.len(),
            },
            system: SystemStatusSettingsSnapshot {
                database_enabled: settings.database.enabled,
                redis_enabled: settings.redis.enabled,
                jwt_enabled: authentication.jwt.enabled,
                api_key_enabled: authentication.api_key.enabled,
                rate_limits_enabled,
            },
            balancer_strategy: strategy.clone(),
            connector_environment: ConnectorEnvironment {
                // main.py does not inject an alternate connector key.
                injected_encryption_key: None,
                gateway_encryption_key: gateway_encryption_key.clone(),
                console_public_base_url: console_public_base_url.clone(),
            },
            connector_encryption_key: gateway_encryption_key,
            console_public_base_url,
            // Connector-principal routes use mcp._get_repository's same gate.
            principal_enabled: mcp_enabled,
            mcp_enabled,
            mcp_client_ttl_seconds,
            mcp_client_max_entries,
            users: UsersConfiguration::new(
                projection.env("DEFAULT_USER_PASSWORD", ""),
                projection.env("AUTH_ALLOWED_EMAIL_DOMAIN", "example.com"),
            ),
            password_concurrency: native.password_concurrency,
            service_access_ttl,
            service_access_database_module: "ai_gateway_core.persistence.database".into(),
            catalog: CapabilityCatalog::bundled().map(Arc::new),
            usage_dimension_order: native.usage_dimension_order,
        },
        FoundationSharedConfiguration {
            // Runtime admin rules begin empty, independently of configured rates.
            runtime_rate_rules: vec![],
            balancer_strategy: strategy,
            latency_sample_cap,
            trusted_proxies,
        },
        NativeFoundationConfiguration {
            local_offset,
            proxy_cache_ttl,
            response_cache_enabled: settings.database.enabled,
        },
    ))
}

fn object_section(
    projection: &SettingsProjection<'_>,
    name: &'static str,
) -> Result<Value, BootstrapError> {
    let value = projection.section(name)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(BootstrapError::Configuration(name))
    }
}
fn float_field(value: &Value, key: &str, default: f64) -> Option<f64> {
    match value.get(key) {
        None => Some(default),
        Some(Value::String(value)) => value.trim_matches(python_whitespace).parse().ok(),
        Some(Value::Bool(value)) => Some(u8::from(*value) as f64),
        Some(value) => value.as_f64(),
    }
}
