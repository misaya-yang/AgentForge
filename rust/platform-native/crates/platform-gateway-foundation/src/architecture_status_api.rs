//! Opt-in architecture projection from src/api/v1/architecture_status.py.
//! Embedded canonical manifest is parsed once. Host supplies environment values,
//! clock, monitor and the existing gateway dependency state; construction is inert.
//! Order is gate → manifest/mode → clock → awaited snapshot → monitor → rows →
//! ordered dependency mutation → groups. Later rows see earlier degraded statuses,
//! whereas earlier dependencies keep their captured first-pass status.
//! No extra authentication predicate, health probes, DB or credential access.
//! Timestamp strings use native ProtocolTimestamp's ISO subset (no Python week
//! dates/basic ISO/arbitrary separator or fractional-second timezone offsets).
//! Typed monitor datetimes are UTC; arbitrary Python objects/nonfinite JSON,
//! ambient local timezone, encoded static paths/slash redirects remain host seams.
use crate::{
    health_api::{
        HealthApiError, HealthDependencyState, HealthHttpCancellation, HealthStatusSource,
        gateway_dependency_snapshot,
    },
    protocol::ProtocolTimestamp,
    setup_api::{
        SetupIdentity, SetupIdentityResolver, require_setup_capability, setup_error_response,
        setup_method_error, setup_request_trace,
    },
    usage_parser::{python_value_to_string, python_whitespace},
};
use ai_platform_auth::rbac::Rbac;
use axum::{
    Json, Router,
    extract::{Request, State},
    http::Method,
    response::{IntoResponse, Response},
    routing::any,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, OnceLock},
};

#[derive(Deserialize)]
pub struct ArchitectureTopology {
    schema: String,
    revision: String,
    default_mode: String,
    groups: Vec<TopologyGroup>,
    services: Vec<TopologyService>,
}
#[derive(Deserialize)]
struct TopologyGroup {
    id: String,
    display_name: String,
}
#[derive(Deserialize)]
struct TopologyService {
    service_id: String,
    display_name: String,
    bounded_context: String,
    process_role: String,
    lifecycle: String,
    exposure: String,
    required_deps: Vec<String>,
    optional_deps: Vec<String>,
    state_owner: String,
    scale_support: String,
    health_contract: HealthContract,
    image_artifact: ImageArtifact,
    modes: HashMap<String, ModeResolution>,
}
#[derive(Deserialize)]
struct HealthContract {
    source_key: String,
}
#[derive(Deserialize)]
struct ImageArtifact {
    version_env: Option<String>,
    default_version: Option<String>,
}
#[derive(Deserialize)]
struct ModeResolution {
    present: bool,
    replicas: u64,
}
impl ArchitectureTopology {
    pub fn parse(source: &str) -> Result<Self, HealthApiError> {
        let result: Self = serde_json::from_str(source).map_err(|_| HealthApiError::Internal)?;
        if result.schema != "ai-gateway/service-topology/v1" {
            return Err(HealthApiError::Internal);
        }
        Ok(result)
    }
    pub fn canonical() -> Result<&'static Self, HealthApiError> {
        static TOPOLOGY: OnceLock<Result<ArchitectureTopology, HealthApiError>> = OnceLock::new();
        TOPOLOGY
            .get_or_init(|| Self::parse(include_str!("../data/service_topology.json")))
            .as_ref()
            .map_err(Clone::clone)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ArchitectureDependency {
    pub service_id: String,
    pub required: bool,
    pub status: String,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ArchitectureServiceStatus {
    pub service_id: String,
    pub display_name: String,
    pub bounded_context: String,
    pub responsibility: String,
    pub lifecycle: String,
    pub exposure: String,
    pub status: String,
    pub version: String,
    pub state_owner: String,
    pub scale_support: String,
    pub replicas: u64,
    pub active_in_mode: bool,
    pub dependencies: Vec<ArchitectureDependency>,
    pub degraded_reasons: Vec<String>,
    pub last_check: Option<String>,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ArchitectureGroupStatus {
    pub group_id: String,
    pub display_name: String,
    pub services: Vec<ArchitectureServiceStatus>,
}
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ArchitectureStatusResponse {
    pub schema_version: &'static str,
    pub topology_revision: String,
    pub mode: String,
    pub mode_configuration_valid: bool,
    pub last_check: String,
    pub groups: Vec<ArchitectureGroupStatus>,
}
/// Reads are deliberately at source call sites: mode before await, image values
/// after await. A host may use a stable config snapshot or its live environment.
pub trait ArchitectureRuntime: Send + Sync {
    fn deployment_mode(&self) -> Option<String>;
    fn checked_at(&self) -> DateTime<Utc>;
    fn image_version(&self, variable: &str) -> Option<String>;
}
#[derive(Clone)]
pub struct ArchitectureObservation {
    pub service_id: String,
    pub status: Value,
    pub last_check: Value,
}
pub trait ArchitectureMonitor: Send + Sync {
    fn all_status(&self) -> Result<Vec<ArchitectureObservation>, HealthApiError>;
}
/// Read-only bridge to the existing monitor; never refreshes or probes it.
pub struct NativeArchitectureMonitor(pub Arc<dyn HealthStatusSource>);
impl ArchitectureMonitor for NativeArchitectureMonitor {
    fn all_status(&self) -> Result<Vec<ArchitectureObservation>, HealthApiError> {
        Ok(self
            .0
            .all_status()?
            .into_iter()
            .map(|(id, row)| ArchitectureObservation {
                service_id: id,
                status: Value::String(row.status.clone()),
                last_check: Value::String(utc_protocol_timestamp(row.last_check)),
            })
            .collect())
    }
}
fn utc_protocol_timestamp(value: ProtocolTimestamp) -> String {
    utc_text(match value {
        ProtocolTimestamp::Naive(value) => value.and_utc(),
        ProtocolTimestamp::Offset(value) => value.with_timezone(&Utc),
    })
}
fn utc_text(value: DateTime<Utc>) -> String {
    let format = if value.timestamp_subsec_micros() == 0 {
        "%Y-%m-%dT%H:%M:%S+00:00"
    } else {
        "%Y-%m-%dT%H:%M:%S%.6f+00:00"
    };
    value.format(format).to_string()
}
pub fn architecture_timestamp(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    if text.chars().count() > 64 {
        return None;
    }
    ProtocolTimestamp::parse_isoformat(&text.replace('Z', "+00:00"))
        .ok()
        .map(utc_protocol_timestamp)
}
pub fn architecture_normalized_status(value: &Value) -> &'static str {
    let raw = if crate::service_registry::python_truthy(value) {
        python_value_to_string(value)
    } else {
        "unknown".into()
    };
    let status = raw.trim_matches(python_whitespace).to_lowercase();
    match status.as_str() {
        "healthy" | "ready" | "ok" => "healthy",
        "degraded" => "degraded",
        "disabled" | "draining" | "error" | "misconfigured" | "not_configured" | "not_ready"
        | "schema_mismatch" | "unavailable" | "unhealthy" => "unavailable",
        s if s.starts_with("error:") || s.starts_with("status_") => "unavailable",
        _ => "unknown",
    }
}
fn version(service: &TopologyService, runtime: &dyn ArchitectureRuntime) -> String {
    let raw = runtime
        .image_version(service.image_artifact.version_env.as_deref().unwrap_or(""))
        .unwrap_or_default();
    let candidate = raw
        .rsplit('@')
        .next()
        .unwrap_or("")
        .rsplit(':')
        .next()
        .unwrap_or("");
    if (1..=64).contains(&candidate.len())
        && candidate
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
    {
        candidate.into()
    } else {
        service
            .image_artifact
            .default_version
            .as_deref()
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown")
            .chars()
            .take(64)
            .collect()
    }
}
pub struct ArchitectureStatusApi {
    pub identity: Arc<dyn SetupIdentityResolver>,
    pub rbac: Rbac,
    pub runtime: Arc<dyn ArchitectureRuntime>,
    pub dependencies: Arc<dyn HealthDependencyState>,
    pub monitor: Arc<dyn ArchitectureMonitor>,
    pub cancellation: Arc<dyn HealthHttpCancellation>,
}
impl ArchitectureStatusApi {
    pub async fn status(
        &self,
        auth: &SetupIdentity,
        trace: &str,
    ) -> Result<ArchitectureStatusResponse, HealthApiError> {
        require_setup_capability(auth, &self.rbac, self.identity.as_ref(), trace, true)?;
        let topology = ArchitectureTopology::canonical()?;
        let raw_mode = self
            .runtime
            .deployment_mode()
            .unwrap_or_else(|| topology.default_mode.clone());
        let valid = matches!(raw_mode.as_str(), "compact" | "full" | "scale");
        let mode = if valid {
            raw_mode
        } else {
            topology.default_mode.clone()
        };
        let checked = utc_text(self.runtime.checked_at());
        let snapshot = gateway_dependency_snapshot(self.dependencies.as_ref()).await?;
        let observations = self.monitor.all_status()?;
        project_architecture(
            topology,
            &mode,
            valid,
            &checked,
            &snapshot,
            observations,
            self.runtime.as_ref(),
        )
    }
}
pub fn project_architecture(
    topology: &ArchitectureTopology,
    mode: &str,
    valid_mode: bool,
    checked_at: &str,
    snapshot: &Map<String, Value>,
    observations: Vec<ArchitectureObservation>,
    runtime: &dyn ArchitectureRuntime,
) -> Result<ArchitectureStatusResponse, HealthApiError> {
    let mut health: HashMap<String, (String, Option<String>)> = HashMap::new();
    for observed in observations {
        health.insert(
            observed.service_id,
            (
                architecture_normalized_status(&observed.status).into(),
                architecture_timestamp(&observed.last_check),
            ),
        );
    }
    for key in ["core", "capabilities"] {
        if let Some(source) = snapshot.get(key).and_then(Value::as_object) {
            for (key, value) in source {
                health.insert(
                    key.clone(),
                    (
                        architecture_normalized_status(value).into(),
                        Some(checked_at.into()),
                    ),
                );
            }
        }
    }
    health.insert(
        "gateway_core".into(),
        (
            if snapshot.get("core_ready") == Some(&Value::Bool(true)) {
                "healthy"
            } else {
                "unavailable"
            }
            .into(),
            Some(checked_at.into()),
        ),
    );
    let mut rows = HashMap::<String, ArchitectureServiceStatus>::new();
    for service in &topology.services {
        let resolution = service.modes.get(mode).ok_or(HealthApiError::Internal)?;
        let active = resolution.present;
        let observed = health.get(&service.health_contract.source_key);
        let status = if service.lifecycle == "one-shot" {
            "one-shot"
        } else if matches!(service.lifecycle.as_str(), "external" | "optional") && !active {
            "not-applicable"
        } else if !active {
            if service.service_id == "knowledge-worker" {
                "integrated"
            } else {
                "inactive"
            }
        } else {
            observed
                .map(|v| architecture_normalized_status(&Value::String(v.0.clone())))
                .unwrap_or("unknown")
        };
        rows.insert(
            service.service_id.clone(),
            ArchitectureServiceStatus {
                service_id: service.service_id.clone(),
                display_name: service.display_name.clone(),
                bounded_context: service.bounded_context.clone(),
                responsibility: service.process_role.clone(),
                lifecycle: service.lifecycle.clone(),
                exposure: service.exposure.clone(),
                status: status.into(),
                version: version(service, runtime),
                state_owner: service.state_owner.clone(),
                scale_support: service.scale_support.clone(),
                replicas: resolution.replicas,
                active_in_mode: active,
                dependencies: vec![],
                degraded_reasons: vec![],
                last_check: if active {
                    observed.and_then(|v| v.1.clone())
                } else {
                    None
                },
            },
        );
    }
    for service in &topology.services {
        let mut row = rows
            .get(&service.service_id)
            .ok_or(HealthApiError::Internal)?
            .clone();
        let mut reasons = BTreeSet::<String>::new();
        for (required, ids) in [
            (true, &service.required_deps),
            (false, &service.optional_deps),
        ] {
            for id in ids {
                let status = rows.get(id).map(|r| r.status.as_str()).unwrap_or("unknown");
                row.dependencies.push(ArchitectureDependency {
                    service_id: id.clone(),
                    required,
                    status: status.into(),
                });
                if row.active_in_mode
                    && matches!(status, "degraded" | "unavailable" | "not_ready" | "unknown")
                {
                    reasons.insert(
                        if required {
                            "required_dependency_unavailable"
                        } else {
                            "optional_dependency_degraded"
                        }
                        .into(),
                    );
                }
            }
        }
        if row.active_in_mode && row.lifecycle == "long-running" && row.status != "healthy" {
            reasons.insert("health_contract_degraded".into());
        }
        if row.status == "healthy" && reasons.contains("optional_dependency_degraded") {
            row.status = "degraded".into();
        }
        row.degraded_reasons = reasons.into_iter().collect();
        rows.insert(service.service_id.clone(), row);
    }
    let groups = topology
        .groups
        .iter()
        .map(|group| ArchitectureGroupStatus {
            group_id: group.id.clone(),
            display_name: group.display_name.clone(),
            services: topology
                .services
                .iter()
                .filter(|s| s.bounded_context == group.id)
                .filter_map(|s| rows.get(&s.service_id).cloned())
                .collect(),
        })
        .collect();
    Ok(ArchitectureStatusResponse {
        schema_version: "ai-gateway/architecture-status/v1",
        topology_revision: topology.revision.clone(),
        mode: mode.into(),
        mode_configuration_valid: valid_mode,
        last_check: checked_at.into(),
        groups,
    })
}
pub fn architecture_status_router(api: Arc<ArchitectureStatusApi>) -> Router {
    Router::new()
        .route("/admin/architecture-status", any(handler))
        .with_state(api)
}
async fn handler(State(api): State<Arc<ArchitectureStatusApi>>, request: Request) -> Response {
    if request.method() != Method::GET {
        return setup_method_error();
    }
    let trace = setup_request_trace(&request);
    // The source ignores GET bodies. Drop it before awaiting the trusted resolver;
    // borrowing Request<Body> across await would require Body to be Sync.
    let (parts, body) = request.into_parts();
    drop(body);
    let outcome = async {
        let auth = api
            .identity
            .resolve(&parts.headers, parts.uri.path())
            .await?;
        api.status(&auth, &trace).await
    }
    .await;
    match outcome {
        Ok(value) => Json(value).into_response(),
        Err(HealthApiError::Cancelled) => api.cancellation.cancelled().await,
        Err(error) => setup_error_response(error, &trace),
    }
}
