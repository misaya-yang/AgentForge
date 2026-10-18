//! Projection from src/config/settings.py and container/main constructor inputs.
//! The environment is an explicit caller-owned snapshot; no ambient reads occur.
use super::{
    BootstrapError, auth::AuthConfiguration, content::LangGraphEndpoint, core::CoreConfiguration,
    runtime::RuntimeConfiguration, storage::ObjectStorageConfiguration,
};
use crate::{
    capacity::CapacityResolver,
    capacity_admission::AdmissionOptions,
    dispatcher_rate_limiter::RateLimitConfig,
    load_shedder::{AdaptiveLoadShedder, LoadShedConfig},
    migration_agent::control::RuntimeOptions,
    migration_host::settings::HostSettings,
    session_manager::DatabaseSessionConfig,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::{Arc, Mutex},
};
pub struct SettingsProjection<'a> {
    pub settings: &'a HostSettings,
    pub environment: &'a BTreeMap<String, String>,
}
impl SettingsProjection<'_> {
    pub(super) fn env(&self, key: &str, default: &str) -> String {
        self.environment
            .get(key)
            .map(String::as_str)
            .unwrap_or(default)
            .to_owned()
    }
    pub(super) fn env_float(&self, key: &str, default: f64) -> f64 {
        self.environment
            .get(key)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    }
    pub(super) fn env_int(&self, key: &'static str, default: i64) -> Result<i64, BootstrapError> {
        self.environment
            .get(key)
            .map(|v| {
                v.trim()
                    .parse()
                    .map_err(|_| BootstrapError::Configuration(key))
            })
            .unwrap_or(Ok(default))
    }
    pub(super) fn enabled(&self, key: &str, default: bool) -> bool {
        self.environment
            .get(key)
            .map(|v| {
                matches!(
                    v.trim().to_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(default)
    }
    pub(super) fn section(&self, key: &'static str) -> Result<Value, BootstrapError> {
        match self.settings.service_sections.get(key) {
            None => Ok(json!({})),
            Some(Value::String(v)) => {
                serde_json::from_str(v).map_err(|_| BootstrapError::Configuration(key))
            }
            Some(v) => Ok(v.clone()),
        }
    }
    pub fn auth(&self) -> AuthConfiguration {
        let a = &self.settings.authentication;
        let role = self.env("GATEWAY_AUTHENTICATION__API_KEY__STATIC_ROLE", "user");
        let role = role.trim();
        AuthConfiguration {
            enterprise_policy_enabled: false,
            jwt_secret: a.jwt.secret.clone(),
            jwt_audience: a.jwt.audience.clone(),
            jwt_issuer: a.jwt.issuer.clone(),
            jwt_algorithms: a.jwt.algorithms.clone(),
            jwt_enabled: a.jwt.enabled,
            api_key_enabled: a.api_key.enabled,
            api_key_header: a.api_key.header_name.clone(),
            static_api_keys: a.api_key.keys.clone(),
            static_key_role: if ["user", "admin", "operator", "developer"].contains(&role) {
                role.into()
            } else {
                "user".into()
            },
            account_domain: self
                .env("AUTH_ALLOWED_EMAIL_DOMAIN", "example.com")
                .trim()
                .to_lowercase(),
            password_concurrency: 2,
            access_keys: None,
        }
    }
    pub fn core(&self) -> Result<CoreConfiguration, BootstrapError> {
        let sessions = self.section("session")?;
        let roles = self.section("rbac")?;
        let roles = match roles.get("roles") {
            None => source_roles(),
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|_| BootstrapError::Configuration("rbac.roles"))?,
        };
        let mut rates = self.section("rate_limits")?;
        coerce_rate_numbers(&mut rates)?;
        for key in ["tenant_limits", "user_limits", "service_limits"] {
            if rates.get(key) == Some(&Value::Null) {
                rates[key] = json!({});
            }
        }
        let rate_limits: RateLimitConfig = serde_json::from_value(rates)
            .map_err(|_| BootstrapError::Configuration("rate_limits"))?;
        let shed = AdaptiveLoadShedder::new(LoadShedConfig {
            window_seconds: self.env_float("ADAPTIVE_LOAD_SHEDDING_WINDOW_SECONDS", 30.0),
            low_threshold_ms: self.env_float("ADAPTIVE_LOAD_SHEDDING_LOW_THRESHOLD_MS", 5000.0),
            normal_threshold_ms: self
                .env_float("ADAPTIVE_LOAD_SHEDDING_NORMAL_THRESHOLD_MS", 10000.0),
            high_threshold_ms: self.env_float("ADAPTIVE_LOAD_SHEDDING_HIGH_THRESHOLD_MS", 20000.0),
            ..Default::default()
        })
        .map_err(|_| BootstrapError::Configuration("load_shedder"))?;
        Ok(CoreConfiguration {
            sessions: DatabaseSessionConfig {
                default_session_ttl: integer(&sessions, "authenticated_ttl_seconds", 604800)?,
                anonymous_session_ttl: integer(&sessions, "anonymous_ttl_seconds", 86400)?,
                ..Default::default()
            },
            roles,
            rate_limits,
            capacity: CapacityResolver::default(),
            admission: AdmissionOptions {
                per_tenant_default_share: self.env_float("ADMISSION_TENANT_SHARE_RATIO", 0.2),
                load_shedder: Some(Arc::new(Mutex::new(shed))),
                ..Default::default()
            },
            adapter_bulkhead_limits: self.env("ADAPTER_BULKHEAD_LIMITS", "").trim().into(),
            health_check_interval: self.settings.health_check_interval,
            durable_tasks: self.settings.task_delivery_mode == "durable",
            worker_concurrency: i64::try_from(self.settings.task_worker_concurrency)
                .map_err(|_| BootstrapError::Configuration("task_worker_concurrency"))?,
        })
    }
    /// Kernel revision is resolved by the runtime-assignment authority, never
    /// manufactured from a constant. Worker-write policy is an explicit input.
    pub fn runtime(
        &self,
        kernel_revision: String,
        worker_writes_enabled: bool,
    ) -> Result<RuntimeConfiguration, BootstrapError> {
        Ok(RuntimeConfiguration {
            options: RuntimeOptions {
                runtime_url: self
                    .env("AI_PLATFORM_AGENT_RUNTIME_URL", "http://agent-runtime:8094")
                    .trim()
                    .trim_end_matches('/')
                    .into(),
                runtime_internal_token: super::runtime_backends::transport_token(
                    self.environment,
                    "AI_PLATFORM_INTERNAL_TOKEN",
                )?,
                model_plane_base_url: self
                    .env(
                        "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_RUNTIME_BASE_URL",
                        "http://gateway:8080/internal/v1/agent-model-plane",
                    )
                    .trim()
                    .trim_end_matches('/')
                    .into(),
                kernel_revision,
                lease_ttl_seconds: self
                    .env_int("AI_PLATFORM_AGENT_RUNTIME_LEASE_TTL_SECONDS", 900)?
                    .clamp(30, 3600),
                max_model_calls: self
                    .env_int("AI_PLATFORM_AGENT_RUNTIME_MAX_MODEL_CALLS_PER_TURN", 24)?
                    .clamp(1, 128),
                max_cost_microusd: self
                    .env_int(
                        "AI_PLATFORM_AGENT_RUNTIME_MAX_COST_MICROUSD_PER_TURN",
                        5_000_000,
                    )?
                    .max(1),
                worker_writes_enabled,
            },
            lease_secret: self
                .env("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET", "")
                .trim()
                .into(),
            model_plane_internal_token: super::runtime_backends::transport_token(
                self.environment,
                "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
            )?,
            capability_worker_url: self
                .env(
                    "AI_PLATFORM_CAPABILITY_WORKER_URL",
                    "http://agent-capability-worker:8095",
                )
                .trim()
                .into(),
            knowledge_origin: self
                .env("KB_SERVICE_URL", "http://knowledge-service:8092")
                .trim()
                .into(),
            default_model: self.settings.default_model.clone(),
            web_search_configured: !self.env("TAVILY_API_KEY", "").trim().is_empty(),
            mcp_enabled: self.enabled("AGENT_STUDIO_MCP_ENABLED", true),
            management_enabled: self.enabled("AGENT_STUDIO_MANAGEMENT_ENABLED", true),
            publish_enabled: self.enabled("AGENT_STUDIO_PUBLISH_ENABLED", true),
            runtime_enabled: self.enabled("AGENT_STUDIO_RUNTIME_ENABLED", true),
        })
    }
    pub fn storage(&self) -> Result<ObjectStorageConfiguration, BootstrapError> {
        let s = self.section("storage")?;
        let prefix = text(&s, "key_prefix", "dev");
        let mode = self.env("AI_PLATFORM_DEPLOYMENT_MODE", "single-node");
        let backend = ai_platform_service_primitives::content::storage_policy::backend(
            &mode,
            s.get("backend").and_then(Value::as_str),
        )
        .map_err(|_| BootstrapError::Configuration("shared_storage"))?;
        match backend {
            "local" => Ok(ObjectStorageConfiguration::Local {
                root: PathBuf::from(text(&s, "local_base_path", "./data/images")),
                prefix,
            }),
            "s3" => {
                let c = &s["s3"];
                Ok(ObjectStorageConfiguration::S3 {
                    region: text(c, "region", "us-east-1"),
                    access_key: text(c, "access_key", ""),
                    secret_key: text(c, "secret_key", "").into_bytes(),
                    endpoint: c
                        .get("endpoint_url")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned),
                    bucket: text(c, "bucket", ""),
                    prefix,
                    public_base: String::new(),
                    encryption: self.env(
                        "GATEWAY_STORAGE__S3__SERVER_SIDE_ENCRYPTION",
                        &text(c, "server_side_encryption", ""),
                    ),
                    kms_key: self
                        .environment
                        .get("GATEWAY_STORAGE__S3__KMS_KEY_ID")
                        .filter(|v| !v.is_empty())
                        .cloned()
                        .or_else(|| {
                            c.get("kms_key_id")
                                .and_then(Value::as_str)
                                .filter(|v| !v.is_empty())
                                .map(str::to_owned)
                        }),
                })
            }
            "oss" => {
                let c = &s["oss"];
                Ok(ObjectStorageConfiguration::Oss {
                    endpoint: text(c, "endpoint", ""),
                    access_key: text(c, "access_key", ""),
                    secret_key: text(c, "secret_key", "").into_bytes(),
                    bucket: text(c, "bucket", ""),
                    prefix,
                    public_base: String::new(),
                })
            }
            _ => Err(BootstrapError::Configuration("storage_backend")),
        }
    }
    pub fn langgraph_endpoints(&self) -> Result<Vec<LangGraphEndpoint>, BootstrapError> {
        let s = self.section("langgraph")?;
        let enabled = match &s["enabled"] {
            Value::Bool(v) => *v,
            Value::String(v) => matches!(
                v.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            ),
            _ => false,
        };
        if !enabled {
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        for row in s["instances"].as_array().into_iter().flatten() {
            result.push(LangGraphEndpoint {
                id: text(row, "instance_id", ""),
                url: text(row, "url", ""),
                weight: integer(row, "weight", 1)? as f64,
            });
        }
        for (i, url) in text(&s, "instance_urls", "").split(',').enumerate() {
            if !url.trim().is_empty() {
                result.push(LangGraphEndpoint {
                    id: format!("langgraph-{i}"),
                    url: url.trim().into(),
                    weight: 1.0,
                });
            }
        }
        Ok(result)
    }
}
fn text(value: &Value, key: &str, default: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .into()
}
fn integer(value: &Value, key: &'static str, default: i64) -> Result<i64, BootstrapError> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(Value::String(v)) => v
            .trim()
            .parse()
            .map_err(|_| BootstrapError::Configuration(key)),
        Some(v) => v.as_i64().ok_or(BootstrapError::Configuration(key)),
    }
}
fn coerce_rate_numbers(v: &mut Value) -> Result<(), BootstrapError> {
    if let Value::Object(map) = v {
        for (k, v) in map {
            if matches!(k.as_str(), "requests" | "window" | "burst") {
                if let Value::String(s) = v {
                    *v = json!(
                        s.trim()
                            .parse::<i64>()
                            .map_err(|_| BootstrapError::Configuration("rate_limits"))?
                    );
                }
            } else {
                if let Value::String(s) = v {
                    if s.trim_start().starts_with('{') {
                        *v = serde_json::from_str(s)
                            .map_err(|_| BootstrapError::Configuration("rate_limits"))?;
                    }
                }
                coerce_rate_numbers(v)?;
            }
        }
    }
    Ok(())
}
fn source_roles() -> HashMap<String, Vec<String>> {
    serde_json::from_value(json!({
        "guest":["console:dashboard:view"],
        "user":["console:dashboard:view","conversation:playground:access","conversation:thread:create"],
        "manager":["console:dashboard:view","console:services:view","console:settings:view","conversation:playground:access","conversation:thread:create","conversation:thread:delete","knowledge:dataset:create","knowledge:dataset:edit","knowledge:dataset:view","user:list"],
        "cs_staff":["console:dashboard:view","conversation:playground:access","conversation:thread:create","conversation:thread:delete","knowledge:dataset:view"],
        "sales_staff":["console:dashboard:view","conversation:playground:access","conversation:thread:create","conversation:thread:delete","knowledge:dataset:view"],
        "developer":["console:dashboard:view","console:services:view","console:services:edit","console:settings:view","conversation:playground:access","conversation:thread:create","conversation:thread:delete","knowledge:dataset:create","knowledge:dataset:edit","knowledge:dataset:view"],"admin":["admin:*"]
    })).expect("source role constants")
}

#[cfg(test)]
mod required_runtime_projection_tests {
    use super::*;
    #[test]
    fn actual_runtime_projection_keeps_absent_bearers_and_original_domain_settings() {
        let settings = HostSettings::default();
        let mut environment = BTreeMap::from([
            ("AI_PLATFORM_WORKLOAD_MTLS_MODE".into(), "required".into()),
            (
                "AI_PLATFORM_AGENT_RUNTIME_URL".into(),
                "https://agent-runtime:8454".into(),
            ),
            (
                "AI_PLATFORM_CAPABILITY_WORKER_URL".into(),
                "https://agent-capability-worker:8455".into(),
            ),
            (
                "KB_SERVICE_URL".into(),
                "https://knowledge-service:8452".into(),
            ),
            (
                "INTERNAL_AUTH_KEYS".into(),
                "domain:independent-domain-signing-root".into(),
            ),
        ]);
        let project = |environment: &BTreeMap<String, String>| {
            SettingsProjection {
                settings: &settings,
                environment,
            }
            .runtime("actual-kernel-revision".into(), false)
        };
        let config = project(&environment).unwrap();
        assert!(config.options.runtime_internal_token.is_empty());
        assert!(config.model_plane_internal_token.is_empty());
        assert_eq!(config.options.runtime_url, "https://agent-runtime:8454");
        assert_eq!(
            config.capability_worker_url,
            "https://agent-capability-worker:8455"
        );
        assert_eq!(config.knowledge_origin, "https://knowledge-service:8452");
        assert_eq!(config.options.kernel_revision, "actual-kernel-revision");
        for key in [
            "AI_PLATFORM_INTERNAL_TOKEN",
            "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
        ] {
            environment.insert(key.into(), "independent-domain-signing-root".into());
            assert!(project(&environment).is_err());
            environment.remove(key);
        }
    }
}
