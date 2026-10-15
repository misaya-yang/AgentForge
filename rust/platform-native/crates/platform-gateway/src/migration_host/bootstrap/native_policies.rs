//! Explicit host policies for captured architecture configuration and transport
//! cancellation. No constructor probes services, reads environment, or creates
//! credentials. Absent source device/registrar authorities cannot become success.
use super::{BootstrapError, foundation_backends::FoundationHostPolicies};
use crate::{
    architecture_status_api::ArchitectureRuntime,
    connector_user_activation::{
        ConnectorConfluenceRegistrar, UnavailableCanonicalConfluenceRegistrar,
    },
    health_api::{HealthDependencyState, HealthHttpCancellation},
    mcp_gateway_broker::McpSecretResolver,
    migration_host::foundation::FoundationCancellation,
};
use axum::response::Response;
use chrono::{DateTime, Utc};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

#[derive(Clone)]
pub struct ArchitectureConfiguration {
    /// Captured AI_PLATFORM_TOPOLOGY_MODE; None preserves manifest default.
    /// Invalid values are retained so the mature projection reports invalid mode.
    pub deployment_mode: Option<String>,
    /// Captured image-version variables. Only variables named in the canonical
    /// topology are queried; the API owns version parsing and sanitization.
    pub image_versions: BTreeMap<String, String>,
}
pub struct CapturedArchitectureRuntime {
    config: ArchitectureConfiguration,
}
impl CapturedArchitectureRuntime {
    pub fn new(config: ArchitectureConfiguration) -> Self {
        Self { config }
    }
}
impl ArchitectureRuntime for CapturedArchitectureRuntime {
    fn deployment_mode(&self) -> Option<String> {
        self.config.deployment_mode.clone()
    }
    fn checked_at(&self) -> DateTime<Utc> {
        Utc::now()
    }
    fn image_version(&self, variable: &str) -> Option<String> {
        self.config.image_versions.get(variable).cloned()
    }
}

/// `Required` is rejected until a genuine device-channel implementation is
/// supplied through the explicit host composition path. Headers/body cannot
/// impersonate a device credential. Source main.py initializes all three device
/// adapters to None, while keeping durable owner-scoped public reads available.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalChannelMode {
    Disabled,
    Required,
}
/// This mode controls legacy dynamic tool registration only. It does not disable
/// the existing connector OAuth endpoints or the private Confluence HTTP broker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfluenceRegistrarMode {
    Disabled,
    Required,
}
/// Explicit publication policy. The executable host supplies the real shared
/// Redis transport for Required; Absent deliberately disables publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublisherMode {
    Absent,
    Required,
}
#[derive(Clone)]
pub struct NativePolicyConfiguration {
    pub architecture: ArchitectureConfiguration,
    pub local_channel: LocalChannelMode,
    pub confluence_registration: ConfluenceRegistrarMode,
    pub model_publication: PublisherMode,
    pub provider_publication: PublisherMode,
}

/// Concrete request-owned cancellation policy. An explicit collaborator
/// cancellation does not fabricate an HTTP response. It suspends until the
/// transport drops the request future. The host MUST impose a server-drain
/// deadline and abort outstanding connection tasks when that deadline expires.
/// Merely invoking this hook is not cancellation completion. Native database
/// supervisors must still be drained after callers have been aborted and joined.
#[derive(Default)]
pub struct NativeRequestCancellation {
    observed: AtomicU64,
    pending: Arc<AtomicUsize>,
}
struct PendingCancellation(Arc<AtomicUsize>);
impl Drop for PendingCancellation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
impl NativeRequestCancellation {
    pub fn observed_count(&self) -> u64 {
        self.observed.load(Ordering::Acquire)
    }
    pub fn pending_count(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }
    fn response(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
        Box::pin(async move {
            self.observed.fetch_add(1, Ordering::AcqRel);
            self.pending.fetch_add(1, Ordering::AcqRel);
            let _pending = PendingCancellation(self.pending.clone());
            std::future::pending::<Response>().await
        })
    }
    pub fn foundation_hooks(self: &Arc<Self>) -> FoundationCancellation {
        FoundationCancellation {
            service: self.clone(),
            balancer: self.clone(),
            rate: self.clone(),
            model: self.clone(),
            usage: self.clone(),
            provider: self.clone(),
            quota: self.clone(),
            metrics: self.clone(),
        }
    }
}
macro_rules! cancellation {
    ($trait:path) => {
        impl $trait for NativeRequestCancellation {
            fn cancelled(&self) -> Pin<Box<dyn Future<Output = Response> + Send + '_>> {
                self.response()
            }
        }
    };
}
cancellation!(HealthHttpCancellation);
cancellation!(crate::service_config_api::ServiceConfigCancellation);
cancellation!(crate::balancer_config_api::BalancerConfigCancellation);
cancellation!(crate::rate_config_api::RateConfigCancellation);
cancellation!(crate::model_admin_api::ModelAdminCancellation);
cancellation!(crate::usage_admin_api::UsageAdminCancellation);
cancellation!(crate::provider_admin_api::ProviderAdminCancellation);
cancellation!(crate::quota_admin_api::QuotaAdminCancellation);
cancellation!(crate::metrics_admin_api::MetricsAdminCancellation);

#[derive(Clone)]
pub struct NativePolicyServices {
    pub architecture: Arc<CapturedArchitectureRuntime>,
    pub cancellation: Arc<NativeRequestCancellation>,
    pub confluence_registrar: Arc<dyn ConnectorConfluenceRegistrar>,
    pub model_publication: PublisherMode,
    pub provider_publication: PublisherMode,
}
impl NativePolicyServices {
    pub fn new(config: NativePolicyConfiguration) -> Result<Self, BootstrapError> {
        if config.local_channel == LocalChannelMode::Required {
            return Err(BootstrapError::Configuration(
                "local_node_device_channel_unavailable",
            ));
        }
        if config.confluence_registration == ConfluenceRegistrarMode::Required {
            return Err(BootstrapError::Configuration(
                "canonical_confluence_registrar_module_absent",
            ));
        }
        Ok(Self {
            model_publication: config.model_publication,
            provider_publication: config.provider_publication,
            architecture: Arc::new(CapturedArchitectureRuntime::new(config.architecture)),
            cancellation: Arc::new(NativeRequestCancellation::default()),
            // The source imports tools.confluence_tool and fails before setting
            // its active flag. That module is absent from the canonical tree.
            confluence_registrar: Arc::new(UnavailableCanonicalConfluenceRegistrar),
        })
    }
    pub fn foundation_policies(
        &self,
        health_dependencies: Arc<dyn HealthDependencyState>,
        mcp_secrets: Arc<dyn McpSecretResolver>,
    ) -> FoundationHostPolicies {
        FoundationHostPolicies {
            role_cache: None,
            tool_invalidation: None,
            mcp_invalidation: None,
            confluence_registrar: self.confluence_registrar.clone(),
            mcp_secrets,
            model_publisher: None,
            provider_publisher: None,
            health_dependencies,
            health_cancellation: self.cancellation.clone(),
            architecture_runtime: self.architecture.clone(),
            local_channel: None,
        }
    }
    pub fn cancellation_hooks(&self) -> FoundationCancellation {
        self.cancellation.foundation_hooks()
    }
}
