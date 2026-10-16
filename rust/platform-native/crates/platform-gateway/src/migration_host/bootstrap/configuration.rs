//! Config-only executable input. The process reads one explicitly supplied JSON
//! document; secrets are never discovered from files, ambient environment or ADC.
use super::{BootstrapError, graph::GatewayConfiguration, settings_projection::SettingsProjection};
use crate::migration_host::{pdf_backend::PopplerPdfConfig, settings::HostSettings};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

pub(super) fn provider_environment(
    environment: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    environment
        .iter()
        .filter(|(name, _)| !name.starts_with("AI_PLATFORM_LOCAL_NODE_"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayDocument {
    /// Already-resolved source settings (same shape as Settings.model_dump()).
    pub settings: Value,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    pub native: NativeDeployment,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDeployment {
    /// Revision supplied by the registered Runtime assignment authority.
    pub kernel_revision: String,
    pub worker_writes_enabled: bool,
    pub local_utc_offset_seconds: i32,
    pub allow_private_knowledge_http: bool,
    #[serde(default)]
    pub sandbox_origin: String,
    #[serde(default)]
    pub sandbox_token: String,
    pub pdf: PdfDeployment,
    #[serde(default)]
    pub google_application_default_credentials: Option<Value>,
    #[serde(default)]
    pub google_default_project: Option<String>,
    /// Metadata credentials are disabled unless an explicit origin is supplied.
    #[serde(default)]
    pub google_metadata_origin: Option<String>,
    pub local_channel: String,
    pub confluence_registration: String,
    pub model_publication: String,
    pub provider_publication: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PdfDeployment {
    pub pdfinfo: PathBuf,
    pub pdftotext: PathBuf,
    pub pdftoppm: Option<PathBuf>,
    pub scratch_root: PathBuf,
    pub allow_attachment_alternative: bool,
}
impl GatewayDocument {
    pub fn parse(bytes: &[u8]) -> Result<Self, BootstrapError> {
        serde_json::from_slice(bytes).map_err(|_| BootstrapError::Configuration("gateway_document"))
    }
    pub fn settings(&self) -> Result<HostSettings, BootstrapError> {
        HostSettings::from_resolved_json(self.settings.clone(), &self.environment)
            .map_err(|_| BootstrapError::Configuration("source_settings"))
    }
    pub fn policies(&self) -> Result<super::native_policies::NativePolicyServices, BootstrapError> {
        use super::native_policies::*;
        let required = |value: &str, name: &'static str| match value {
            "disabled" => Ok(false),
            "required" => Ok(true),
            _ => Err(BootstrapError::Configuration(name)),
        };
        let publisher = |value: &str, name: &'static str| match value {
            "absent" => Ok(PublisherMode::Absent),
            "required" => Ok(PublisherMode::Required),
            _ => Err(BootstrapError::Configuration(name)),
        };
        NativePolicyServices::new(NativePolicyConfiguration {
            architecture: ArchitectureConfiguration {
                deployment_mode: self.environment.get("AI_PLATFORM_TOPOLOGY_MODE").cloned(),
                image_versions: self.environment.clone(),
            },
            local_channel: if required(&self.native.local_channel, "local_channel")? {
                LocalChannelMode::Required
            } else {
                LocalChannelMode::Disabled
            },
            confluence_registration: if required(
                &self.native.confluence_registration,
                "confluence_registration",
            )? {
                ConfluenceRegistrarMode::Required
            } else {
                ConfluenceRegistrarMode::Disabled
            },
            model_publication: publisher(&self.native.model_publication, "model_publication")?,
            provider_publication: publisher(
                &self.native.provider_publication,
                "provider_publication",
            )?,
        })
    }
    pub fn project(&self, settings: &HostSettings) -> Result<GatewayConfiguration, BootstrapError> {
        let p = SettingsProjection {
            settings,
            environment: &self.environment,
        };
        let offset = chrono::FixedOffset::east_opt(self.native.local_utc_offset_seconds)
            .ok_or(BootstrapError::Configuration("local_utc_offset_seconds"))?;
        if self.native.kernel_revision.trim().is_empty() {
            return Err(BootstrapError::Configuration("kernel_revision"));
        }
        let (foundation, foundation_shared, native_foundation) =
            super::foundation_settings::project(settings, &self.environment, offset)?;
        let runtime = p.runtime(
            self.native.kernel_revision.clone(),
            self.native.worker_writes_enabled,
        )?;
        let signing = super::runtime_backends::InternalSigningConfiguration::from_environment(
            &self.environment,
        )?;
        let internal = signing.primary_secret.clone();
        let proof = p
            .env("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
            .into_bytes();
        let mut proxy = crate::migration_host::knowledge_proxy::ProxyConfig::new(super::endpoint(
            &runtime.knowledge_origin,
            "knowledge_origin",
        )?);
        proxy.allow_private_http = self.native.allow_private_knowledge_http;
        let rpc_origin = p.env("GATEWAY_KNOWLEDGE_RPC_URL", "");
        if !rpc_origin.trim().is_empty() {
            let timeout = p.env_int("GATEWAY_KNOWLEDGE_RPC_TIMEOUT_MS", 30_000)?;
            let capacity = p.env_int("GATEWAY_KNOWLEDGE_RPC_MAX_IN_FLIGHT", 32)?;
            if !(100..=120_000).contains(&timeout) || !(1..=256).contains(&capacity) {
                return Err(BootstrapError::Configuration("knowledge_rpc_bounds"));
            }
            let rpc = ai_platform_rpc::RpcConfig {
                request_timeout: Duration::from_millis(timeout as u64),
                max_in_flight: capacity as usize,
                channel_buffer: capacity as usize,
                ..Default::default()
            };
            rpc.validate()
                .map_err(|_| BootstrapError::Configuration("knowledge_rpc_bounds"))?;
            proxy.retrieval_rpc = Some((rpc_origin.trim().to_owned(), rpc));
        }
        let history = if self.native.allow_private_knowledge_http {
            crate::knowledge::KnowledgeConfig::for_private_http(
                &runtime.knowledge_origin,
                signing.active_key_id.clone(),
                signing.active_secret.clone(),
            )
        } else {
            crate::knowledge::KnowledgeConfig::new(
                &runtime.knowledge_origin,
                signing.active_key_id.clone(),
                signing.active_secret.clone(),
            )
        }
        .map_err(|_| BootstrapError::Configuration("history_knowledge"))?;
        let mut pdf = PopplerPdfConfig::new(
            self.native.pdf.pdfinfo.clone(),
            self.native.pdf.pdftotext.clone(),
            self.native.pdf.pdftoppm.clone(),
            self.native.pdf.scratch_root.clone(),
        );
        pdf.allow_attachment_alternative = self.native.pdf.allow_attachment_alternative;
        let uploads = PathBuf::from(p.env("FILE_STORAGE_PATH", "./uploads"));
        let max_upload = p
            .env_int("MAX_FILE_SIZE_MB", 50)?
            .try_into()
            .ok()
            .and_then(|v: usize| v.checked_mul(1024 * 1024))
            .ok_or(BootstrapError::Configuration("MAX_FILE_SIZE_MB"))?;
        let langgraph = p.section("langgraph")?;
        let proxy_settings = p.section("proxy")?;
        let cleanup = crate::migration_content::files::CleanupConfig {
            storage_path: uploads.clone(),
            file_ttl_days: p.env_int("FILE_TTL_DAYS", 7)?,
            user_quota_mb: p
                .env_int("FILE_USER_QUOTA_MB", 500)?
                .try_into()
                .map_err(|_| BootstrapError::Configuration("FILE_USER_QUOTA_MB"))?,
            cleanup_interval_hours: p
                .env_int("FILE_CLEANUP_INTERVAL_HOURS", 6)?
                .try_into()
                .map_err(|_| BootstrapError::Configuration("FILE_CLEANUP_INTERVAL_HOURS"))?,
            enabled: p.env("FILE_CLEANUP_ENABLED", "true").to_lowercase() == "true",
        };
        let watermark = include_bytes!("../../../assets/watermark.png");
        let content = super::content::ContentRuntimeConfiguration {
            artifacts: p.storage()?,
            uploads: p.storage()?,
            attachment_fallbacks: vec![p.storage()?],
            content: crate::migration_host::content::ContentConfiguration {
                uploads_path: uploads,
                maximum_upload_bytes: max_upload,
                file_admins: p
                    .env("FILE_ADMIN_USER_IDS", "")
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_owned)
                    .collect(),
                cleanup,
                images: project_images(&p),
                watermark_png: watermark.to_vec(),
            },
            langgraph: super::content::LangGraphConfiguration {
                instances: p.langgraph_endpoints()?,
                strategy: langgraph["load_balancer_strategy"]
                    .as_str()
                    .unwrap_or("least_connections")
                    .into(),
                token: langgraph["auth_token"].as_str().unwrap_or("").into(),
                governance: crate::migration_compat::langgraph::LangGraphGovernance {
                    quota: None,
                    rpm: Arc::new(crate::rate_limits::QuotaRpmLimiter::default()),
                    runtime_snapshot: None,
                    live_resolution: None,
                    live_services: None,
                    services: HashMap::new(),
                    allowed_models: vec![],
                    quota_alert_dedupe_ttl_seconds:
                        proxy_settings["quota_alert_dedupe_ttl_seconds"]
                            .as_f64()
                            .unwrap_or(60.0),
                    security: None,
                    quota_fail_closed: proxy_settings["quota_check_failure_mode"].as_str()
                        == Some("fail_closed"),
                },
            },
            guests: crate::guest_session_manager::GuestSessionConfig::default(),
            sandbox_url: self.native.sandbox_origin.clone(),
            sandbox_token: self.native.sandbox_token.clone(),
            capability_internal_token: internal.clone(),
            capability_proof_secret: proof.clone(),
            tavily_api_key: p.env("TAVILY_API_KEY", ""),
        };
        let (rate_config, _) = project_rates(&p);
        Ok(GatewayConfiguration {
            auth: p.auth(),
            core: p.core()?,
            callback_allowlist: p
                .env("CALLBACK_URL_ALLOWLIST", "")
                .split(',')
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_owned)
                .collect(),
            native_foundation,
            provider_encryption_key: p.env("GATEWAY_ENCRYPTION_KEY", ""),
            provider_environment: provider_environment(&self.environment),
            pricing_cache_ttl_seconds: 300.0,
            google_auth: crate::migration_host::google_auth::GoogleAuthConfig {
                application_default_credentials: self
                    .native
                    .google_application_default_credentials
                    .clone(),
                default_project: self.native.google_default_project.clone(),
                metadata_server: self.native.google_metadata_origin.as_ref().map(|url| {
                    crate::migration_host::google_auth::GoogleMetadataConfig {
                        root_url: url.clone(),
                        service_account: "default".into(),
                    }
                }),
            },
            runtime_configured_providers: Vec::new(),
            runtime,
            content,
            pdf_backend: pdf,
            compat: super::compat::CompatConfiguration {
                internal_token: internal.clone(),
                proof_secret: proof,
                ragas_origin: proxy.origin.to_string(),
                ragas_secret: signing.active_secret.into_bytes(),
                ragas_key_id: signing.active_key_id,
                default_model: settings.default_model.clone(),
                confluence_enabled: settings.database.enabled
                    && p.enabled("AGENT_STUDIO_MCP_ENABLED", true),
            },
            evaluation: super::evaluation::EvaluationConfiguration::from_settings(
                settings,
                &self.environment,
            )?,
            foundation,
            foundation_shared,
            rate_config,
            usage: crate::usage_recorder::RecorderConfig {
                buffer_size: proxy_settings["billing_buffer_size"]
                    .as_u64()
                    .unwrap_or(100)
                    .try_into()
                    .map_err(|_| BootstrapError::Configuration("billing_buffer_size"))?,
                flush_interval: duration(
                    proxy_settings["billing_flush_interval"]
                        .as_f64()
                        .unwrap_or(5.0),
                    "billing_flush_interval",
                )?,
                flushed_ids_max: 5000,
            },
            retention: crate::migration_trace::retention::RetentionConfig::default(),
            knowledge_proxy: proxy,
            history_knowledge: history,
            public: Some(super::graph::PublicConfiguration {
                embed_secret: first(
                    &p,
                    &["AGENT_RUNTIME_TOKEN_SIGNING_KEY", "GATEWAY_ENCRYPTION_KEY"],
                ),
                max_attachment_bytes: max_upload,
            }),
        })
    }
    pub fn authorities(
        &self,
        redis: redis::aio::ConnectionManager,
    ) -> Result<super::graph::KnowledgeAuthorities, BootstrapError> {
        use crate::migration_knowledge_query::auth::{GatewayAuth, ReplayStore};
        let signing = super::runtime_backends::InternalSigningConfiguration::from_environment(
            &self.environment,
        )?;
        // GatewayAuth signs with its first map entry. Give the outbound signer
        // exactly the selected active key; retain all rotated keys for verification.
        let signer = GatewayAuth::new(
            signing.primary_secret.clone(),
            BTreeMap::from([(signing.active_key_id, signing.active_secret)]),
            "v2".into(),
            ReplayStore::Redis(redis.clone()),
        )
        .map_err(|_| BootstrapError::Configuration("knowledge_signer"))?;
        let mut verifier = GatewayAuth::new(
            signing.primary_secret,
            signing.keys,
            "v2".into(),
            ReplayStore::Redis(redis),
        )
        .map_err(|_| BootstrapError::Configuration("knowledge_actor_verifier"))?;
        verifier.caller_service = "knowledge-service".into();
        verifier.audience = "gateway".into();
        verifier.allowed_path_prefixes =
            vec![crate::migration_knowledge_query::actor::ACTOR_PATH.into()];
        Ok(super::graph::KnowledgeAuthorities {
            signer: Arc::new(signer),
            actor_verifier: Arc::new(verifier),
        })
    }
}
fn duration(value: f64, name: &'static str) -> Result<Duration, BootstrapError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(BootstrapError::Configuration(name));
    }
    Duration::try_from_secs_f64(value).map_err(|_| BootstrapError::Configuration(name))
}
fn first(p: &SettingsProjection<'_>, keys: &[&str]) -> String {
    keys.iter()
        .map(|k| p.env(k, ""))
        .find(|v| !v.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .into()
}
fn project_images(
    p: &SettingsProjection<'_>,
) -> crate::migration_content::image_provider::ImageGenerationConfig {
    use crate::migration_content::image_provider::ImageGenerationConfig;
    use ai_platform_gateway_foundation::provider_startup_seeder::{
        ChatEndpointResolver, EndpointDomain,
    };
    let endpoints = ChatEndpointResolver::new(p.environment);
    let google_endpoint = endpoints.resolve_google(EndpointDomain::Image);
    let dash_endpoint = endpoints.resolve_dashscope(EndpointDomain::Image);
    let backend = google_endpoint.backend;
    let google = google_endpoint.api_key;
    let dash = dash_endpoint.api_key;
    let ark = first(p, &["ARK_API_KEY"]);
    let mut provider = p.env("IMAGE_GENERATION_PROVIDER", "").trim().to_lowercase();
    if provider.is_empty() {
        provider = if !google.is_empty() {
            "google"
        } else if !dash.is_empty() {
            "dashscope"
        } else if !ark.is_empty() {
            "doubao"
        } else {
            "none"
        }
        .into()
    }
    let mut result = ImageGenerationConfig::default();
    let suffixes: Vec<String> = p
        .env("IMAGE_GENERATION_RESULT_HOST_SUFFIXES", "")
        .split(',')
        .map(|v| v.trim().to_lowercase())
        .filter(|v| !v.is_empty())
        .collect();
    if !suffixes.is_empty() {
        result.result_host_suffixes = suffixes
    }
    match provider.as_str() {
        "google" | "gemini" | "ai_studio" | "vertex" => {
            result.provider = "google".into();
            result.api_key = google;
            result.base_url = if backend == "vertex" {
                "https://aiplatform.googleapis.com"
            } else {
                "https://generativelanguage.googleapis.com"
            }
            .into();
            result.google_backend = backend;
            result.model = p.env("GOOGLE_IMAGE_MODEL", "gemini-3.1-flash-image-preview");
            result.supports_reference_images = matches!(
                p.env("IMAGE_GENERATION_SUPPORTS_REFERENCE_IMAGES", "true")
                    .to_lowercase()
                    .as_str(),
                "1" | "true" | "yes"
            );
        }
        "dashscope" | "qwen" => {
            result.provider = "dashscope".into();
            result.api_key = dash;
            result.base_url = dash_endpoint.base_url;
            result.model = p.env("DASHSCOPE_IMAGE_MODEL", "wan2.6-t2i");
            result.dashscope_submit_path = p.env(
                "DASHSCOPE_IMAGE_SUBMIT_PATH",
                "/services/aigc/image-generation/generation",
            );
            result.dashscope_protocol = p
                .env("DASHSCOPE_IMAGE_PROTOCOL", "wan26")
                .trim()
                .to_lowercase();
        }
        "doubao" | "volcengine" | "ark" => {
            result.provider = "doubao".into();
            result.api_key = ark;
            result.base_url = p
                .env("ARK_BASE_URL", "https://ark.cn-beijing.volces.com/api/v3")
                .trim()
                .into();
            result.model = p.env("DOUBAO_IMAGE_MODEL", "doubao-seedream-5-0-260128");
        }
        _ => result.provider = provider,
    }
    result
}
pub fn project_rates(
    p: &SettingsProjection<'_>,
) -> (
    crate::rate_limits::MultiDimensionConfig,
    crate::migration_host::rate::RateConfig,
) {
    let mut md = crate::rate_limits::MultiDimensionConfig::default();
    let integer = |name: &str, default: i64| {
        p.environment
            .get(name)
            .and_then(|v| v.trim().parse::<i64>().ok())
            .unwrap_or(default)
    };
    if matches!(
        p.env("RATE_LIMIT_IP_ENABLED", "").to_lowercase().as_str(),
        "false" | "0" | "no"
    ) {
        md.ip = None
    } else if let Some(ip) = &mut md.ip {
        ip.requests = integer("RATE_LIMIT_IP_LIMIT", 30)
    }
    for (tier, limit) in &mut md.user_tiers {
        limit.requests = integer(
            &format!("RATE_LIMIT_{}_LIMIT", tier.to_uppercase()),
            limit.requests,
        )
    }
    for (operation, limit) in &mut md.operations {
        if !["run_create", "thread_create", "store_write"].contains(&operation.as_str()) {
            limit.requests = integer(
                &format!("RATE_LIMIT_{}_LIMIT", operation.to_uppercase()),
                limit.requests,
            )
        }
    }
    let tiers: BTreeMap<String, i64> = md
        .user_tiers
        .iter()
        .map(|(k, v)| (k.clone(), v.requests.saturating_add(v.burst).max(1)))
        .collect();
    let http = crate::migration_host::rate::RateConfig {
        enabled: true,
        global_limit: 5000,
        global_window: 60,
        user_limit: *tiers.get("normal").unwrap_or(&60),
        user_window: 60,
        guest_limit: 200,
        guest_window: 60,
        ip_limit: integer("RATE_LIMIT_IP_LIMIT", 30),
        ip_window: 60,
        tenant_limit: Some(5000),
        tenant_window: 60,
        user_tier_limits: tiers,
        whitelist_paths: [
            "/health",
            "/health/live",
            "/health/ready",
            "/docs",
            "/openapi.json",
            "/version",
            "/api/v1/services",
            "/api/v1/assistant/config",
            "/api/v1/assistant/models",
            "/api/v1/assistant/datasets",
            "/internal/v1/agent-model-plane/responses",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect(),
        trusted_proxy_cidrs: p
            .env("GATEWAY_TRUSTED_PROXY_CIDRS", "127.0.0.0/8,::1/128")
            .split(',')
            .map(str::to_owned)
            .collect(),
    };
    (md, http)
}

#[cfg(test)]
mod tests {
    use super::provider_environment;
    use std::collections::BTreeMap;

    #[test]
    fn local_node_private_configuration_is_not_forwarded_to_provider_or_mcp_secrets() {
        let environment = BTreeMap::from([
            (
                "DASHSCOPE_API_KEY".to_owned(),
                "provider-placeholder".to_owned(),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_PLATFORM_SIGNING_SEED_B64".to_owned(),
                "local-private-placeholder".to_owned(),
            ),
            (
                "AI_PLATFORM_LOCAL_NODE_TLS_KEY_PATH".to_owned(),
                "/private/local-node-key.pem".to_owned(),
            ),
        ]);

        let projected = provider_environment(&environment);
        assert_eq!(
            projected.get("DASHSCOPE_API_KEY").map(String::as_str),
            Some("provider-placeholder")
        );
        assert!(
            !projected
                .keys()
                .any(|name| name.starts_with("AI_PLATFORM_LOCAL_NODE_"))
        );
    }
}
