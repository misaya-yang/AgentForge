//! Actual HA process construction. Opt-in never changes singleton release gates.
//! Every shared adapter uses the admitted coordination handle, never cache or a
//! disconnected legacy Redis fallback. Lease loss is terminal for this process.
mod supervisor;
#[cfg(test)]
mod tests;
use super::BootstrapError;
use crate::migration_host::{context_metrics::ContextMetricsCollector, middleware::DrainState};
use ai_platform_gateway_foundation::multi_instance::{
    affinity::{Directory, InstanceIdentity},
    dashboard::SocketLeases,
};
use ai_platform_service_primitives::{
    valkey::CoordConnection,
    workload_identity::{
        ServiceIdentity, http::WorkloadHttp, secrets::CredentialFiles, tls::ClientCredentials,
    },
};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
pub use supervisor::LeaseHealth;
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub enabled: bool,
    #[serde(default)]
    pub instance_id: String,
    #[serde(default)]
    pub origin: String,
    #[serde(default = "lease_default")]
    pub lease_seconds: u64,
    #[serde(default = "global_default")]
    pub dashboard_global: u64,
    #[serde(default = "tenant_default")]
    pub dashboard_tenant: u64,
}
fn lease_default() -> u64 {
    60
}
fn global_default() -> u64 {
    512
}
fn tenant_default() -> u64 {
    128
}
impl Configuration {
    pub fn from_environment(e: &BTreeMap<String, String>) -> Result<Option<Self>, BootstrapError> {
        let Some(raw) = e.get("AI_PLATFORM_GATEWAY_HA") else {
            return Ok(None);
        };
        if raw.len() > 32768 {
            return Err(BootstrapError::Configuration("ha_configuration"));
        }
        let c: Self = serde_json::from_str(raw)
            .map_err(|_| BootstrapError::Configuration("ha_configuration"))?;
        if !c.enabled {
            return Ok(None);
        }
        c.validate()?;
        if e.get("AI_PLATFORM_WORKLOAD_MTLS_MODE").map(String::as_str) != Some("required") {
            return Err(BootstrapError::Configuration(
                "ha_requires_workload_required",
            ));
        }
        Ok(Some(c))
    }
    fn validate(&self) -> Result<(), BootstrapError> {
        if !self.enabled {
            return Err(BootstrapError::Configuration("ha_requires_explicit_enable"));
        }
        InstanceIdentity::new(&self.instance_id)
            .map_err(|_| BootstrapError::Configuration("ha_instance_id"))?;
        let u = reqwest::Url::parse(&self.origin)
            .map_err(|_| BootstrapError::Configuration("ha_origin"))?;
        if u.scheme() != "https"
            || u.host_str().is_none()
            || !u.username().is_empty()
            || u.password().is_some()
            || u.path() != "/"
            || u.query().is_some()
            || u.fragment().is_some()
            || self.origin.ends_with('/')
            || !(15..=300).contains(&self.lease_seconds)
            || self.dashboard_tenant == 0
            || self.dashboard_tenant > self.dashboard_global
            || self.dashboard_global > 1_000_000
        {
            return Err(BootstrapError::Configuration("ha_configuration"));
        }
        Ok(())
    }
}
pub struct Runtime {
    directory: Directory,
    coordination: redis::aio::ConnectionManager,
    pub context: Arc<ContextMetricsCollector>,
    sockets: SocketLeases,
    routing: crate::ha::ProvenanceRouting,
    supervisor: Arc<supervisor::Supervisor>,
}
impl Runtime {
    pub async fn new(
        c: Configuration,
        coordination: Option<&CoordConnection>,
        e: &BTreeMap<String, String>,
        workload: Option<&crate::workload_tls::Configuration>,
        drain: Arc<DrainState>,
    ) -> Result<Arc<Self>, BootstrapError> {
        c.validate()?;
        if e.get("AI_PLATFORM_WORKLOAD_MTLS_MODE").map(String::as_str) != Some("required") {
            return Err(BootstrapError::Configuration(
                "ha_requires_workload_required",
            ));
        }
        let redis = coordination
            .ok_or(BootstrapError::Configuration("ha_requires_coordination"))?
            .coordination_manager();
        let workload = workload.ok_or(BootstrapError::Configuration(
            "ha_requires_workload_listener",
        ))?;
        let own = ServiceIdentity::parse(
            e.get("AI_PLATFORM_WORKLOAD_SPIFFE_ID")
                .ok_or(BootstrapError::Configuration("ha_workload_identity"))?,
        )
        .map_err(|_| BootstrapError::Configuration("ha_workload_identity"))?;
        let allowed = e
            .get("AI_PLATFORM_WORKLOAD_ALLOWED_CLIENT_IDS")
            .ok_or(BootstrapError::Configuration("ha_gateway_peer_allowlist"))?;
        if !allowed.split(',').any(|v| v == own.as_str()) {
            return Err(BootstrapError::Configuration("ha_gateway_peer_allowlist"));
        }
        let peers: BTreeMap<String, String> = serde_json::from_str(
            e.get("AI_PLATFORM_WORKLOAD_PEERS_JSON")
                .ok_or(BootstrapError::Configuration("ha_peer_origins"))?,
        )
        .map_err(|_| BootstrapError::Configuration("ha_peer_origins"))?;
        if peers.get(&c.origin).map(String::as_str) != Some(own.as_str()) {
            return Err(BootstrapError::Configuration("ha_self_origin_not_admitted"));
        }
        let url = reqwest::Url::parse(&c.origin)
            .map_err(|_| BootstrapError::Configuration("ha_origin"))?;
        let addresses = tokio::net::lookup_host((
            url.host_str().unwrap(),
            url.port_or_known_default().unwrap(),
        ))
        .await
        .map_err(|_| BootstrapError::Configuration("ha_origin_dns"))?
        .collect::<std::collections::BTreeSet<_>>();
        validate_binding(&addresses, workload.address)?;
        let identity = InstanceIdentity::new(&c.instance_id)
            .map_err(|_| BootstrapError::Configuration("ha_instance_id"))?;
        let directory = Directory::new(
            redis.clone(),
            identity,
            &c.origin,
            Duration::from_secs(c.lease_seconds),
        )
        .map_err(|_| BootstrapError::Configuration("ha_directory"))?;
        let credentials = ClientCredentials::new(
            CredentialFiles::new(
                e.get("AI_PLATFORM_WORKLOAD_CREDENTIALS_DIR")
                    .ok_or(BootstrapError::Configuration("ha_workload_files"))?
                    .as_str()
                    .into(),
            )
            .map_err(|_| BootstrapError::Configuration("ha_workload_files"))?,
            own.clone(),
        );
        // A poisoned directory cannot redirect a grant to any Runtime/provider
        // origin which happens to be admitted by another private HTTP client.
        let peers = peers
            .into_iter()
            .filter(|(_, id)| id == own.as_str())
            .map(|(origin, _)| (origin, own.clone()));
        let http = WorkloadHttp::new(
            credentials,
            peers,
            Duration::from_secs(2),
            Duration::from_secs(300),
        )
        .map_err(|_| BootstrapError::Configuration("ha_peer_transport"))?;
        let routing = crate::ha::ProvenanceRouting::new(
            directory.clone(),
            http,
            callback_paths().map(str::to_owned),
            16 * 1024 * 1024,
        )
        .map_err(BootstrapError::Configuration)?;
        let supervisor = supervisor::Supervisor::new(
            Arc::new(supervisor::DirectoryAuthority(directory.clone())),
            drain,
            Duration::from_secs(c.lease_seconds / 3),
        );
        Ok(Arc::new(Self {
            context: Arc::new(ContextMetricsCollector::shared(redis.clone())),
            sockets: SocketLeases::new(
                redis.clone(),
                c.dashboard_global,
                c.dashboard_tenant,
                Duration::from_secs(c.lease_seconds),
            )
            .map_err(|_| BootstrapError::Configuration("ha_dashboard"))?,
            directory,
            coordination: redis,
            routing,
            supervisor,
        }))
    }
    pub fn coordination(&self) -> redis::aio::ConnectionManager {
        self.coordination.clone()
    }
    pub fn directory(&self) -> &Directory {
        &self.directory
    }
    pub fn health(&self) -> Arc<LeaseHealth> {
        self.supervisor.health.clone()
    }
    pub fn routing(&self) -> crate::ha::ProvenanceRouting {
        self.routing.clone()
    }
    pub fn sockets(&self) -> SocketLeases {
        self.sockets.clone()
    }
    pub fn configure_grants(
        &self,
        registry: &crate::service_registry::ServiceRegistry,
    ) -> Result<(), BootstrapError> {
        registry
            .sdk_read_grants()
            .configure_affinity(self.directory.identity().clone())
            .map_err(|_| BootstrapError::Construction("ha_grant_affinity"))
    }
    pub fn supervise_workers(
        &self,
        stops: &[crate::migration_host::lifecycle::LifecycleStep],
    ) -> Result<(), BootstrapError> {
        let mut operations = stops
            .iter()
            .filter(|step| {
                step.name == "task_worker"
                    || ai_platform_gateway_foundation::worker_lease::valid_job(step.name)
            })
            .map(|step| (step.name, step.operation.clone()))
            .collect::<Vec<_>>();
        operations.sort_by_key(|(name, _)| *name != "task_worker");
        self.supervisor
            .on_loss
            .set(Arc::new(move || {
                let operations = operations.clone();
                Box::pin(async move {
                    let mut failed = false;
                    // Stop the real singleton supervisors too, so they cannot regain
                    // a PG lease and issue new process-owned grants after HA loss.
                    for (_, operation) in operations {
                        failed |= operation().await.is_err();
                    }
                    if failed {
                        Err(crate::migration_host::lifecycle::HostError::Shutdown(
                            "ha_workers",
                        ))
                    } else {
                        Ok(())
                    }
                })
            }))
            .map_err(|_| BootstrapError::Construction("ha_supervisor_frozen"))
    }
    pub fn guard_start(
        &self,
        step: crate::migration_host::lifecycle::LifecycleStep,
    ) -> crate::migration_host::lifecycle::LifecycleStep {
        self.supervisor.guard_start(step)
    }
    pub fn lifecycle_steps(
        &self,
    ) -> (
        crate::migration_host::lifecycle::LifecycleStep,
        crate::migration_host::lifecycle::LifecycleStep,
    ) {
        self.supervisor.lifecycle_steps()
    }
    pub fn verify(&self, graph: &super::graph::GatewayGraph) -> Result<(), BootstrapError> {
        ai_platform_gateway_foundation::multi_instance::readiness::require_shared(
            &graph.native_foundation.quota.rpm,
            &graph.content.guests,
            &graph.core.registry.sdk_read_grants(),
            &graph.core.tasks,
            &graph.foundation.apis.dashboard,
            graph.context_metrics.shared_authority_configured(),
            &self.directory,
        )
        .map_err(|_| BootstrapError::Construction("ha_all_shared_authorities"))
    }
}
fn validate_binding(
    addresses: &std::collections::BTreeSet<std::net::SocketAddr>,
    bound: std::net::SocketAddr,
) -> Result<(), BootstrapError> {
    if addresses.len() != 1 || !addresses.contains(&bound) {
        Err(BootstrapError::Configuration(
            "ha_origin_must_be_instance_listener",
        ))
    } else {
        Ok(())
    }
}
pub fn callback_paths() -> impl Iterator<Item = &'static str> {
    [
        "/internal/v1/sdk-tools/capabilities/admit",
        "/internal/v1/sdk-tools/capabilities/execute",
        "/internal/v1/sdk-tools/knowledge/retrieve",
        "/internal/v1/sdk-tools/knowledge/admit",
        "/internal/v1/sdk-tools/models/lease",
        "/internal/v1/sdk-tools/models/execute",
        "/internal/v1/sdk-tools/models/revocations",
    ]
    .into_iter()
}
pub fn worker_routes() -> Result<crate::workload_tls::Routes, BootstrapError> {
    crate::workload_tls::Routes::new(callback_paths().map(|p| ("POST".into(), p.into())))
        .map_err(BootstrapError::Configuration)
}
