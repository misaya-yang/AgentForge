use super::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: String,
    pub gateway_origin: String,
    pub trace_origin: String,
    pub actor_token_file: PathBuf,
    pub trace_token_file: Option<PathBuf>,
    pub tenant_id: String,
    pub user_id: String,
    pub services: Vec<String>,
    pub environment: Environment,
    pub budgets: Budgets,
    pub fixture_checks: Vec<Probe>,
    pub cases: Vec<Case>,
    pub mock: Option<MockConfig>,
    pub accounting: Option<Accounting>,
    #[serde(default)]
    pub transports: BTreeMap<String, Transport>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transport {
    pub ca_file: Option<TransportFile>,
    pub identity_file: Option<TransportFile>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportFile {
    pub path: PathBuf,
    pub sha256: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Accounting {
    pub config_file: PathBuf,
    pub config_sha256: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    pub topology: String,
    pub cpu_limit_millicores: u64,
    pub memory_limit_bytes: u64,
    pub database_pool_limit: u32,
    pub corpus_sha256: String,
    pub index_sha256: String,
    pub build_flags_sha256: String,
    pub cache_condition: String,
    pub background_load: String,
    pub controls: BTreeMap<String, bool>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Budgets {
    pub request_ms: u64,
    pub run_seconds: u64,
    pub sample_ms: u64,
    pub sample_limit: usize,
    pub max_body_bytes: usize,
    pub trace_wait_ms: u64,
    pub poll_ms: u64,
    pub case_gap_ms: u64,
    pub rss_stop_bytes: BTreeMap<String, u64>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub path: String,
    pub checks: Vec<Check>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub pointer: String,
    pub equals: Value,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub kind: Kind,
    pub provider_scope: ProviderScope,
    pub samples: usize,
    pub warmup: usize,
    pub gap_ms: u64,
    pub path: String,
    pub body: Value,
    pub checks: Vec<Check>,
    pub boundaries: BTreeMap<String, Boundary>,
    pub stages: BTreeMap<String, Stage>,
    pub retrieval: Option<Retrieval>,
    pub ingestion: Option<Ingestion>,
    pub direct_control_body: Option<Value>,
    pub upstream_identity: Option<UpstreamIdentity>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamIdentity {
    pub service: String,
    pub span: String,
    pub attributes: BTreeMap<String, String>,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    GatewayMock,
    GatewayReal,
    Retrieval,
    Ingestion,
}
#[derive(Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderScope {
    SyntheticOrLocal,
    AuthorizedReal,
}
impl ProviderScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::SyntheticOrLocal => "synthetic_or_local",
            Self::AuthorizedReal => "authorized_real",
        }
    }
}
impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::GatewayMock => "deterministic_synthetic_upstream",
            Self::GatewayReal => "real_provider",
            Self::Retrieval => "resource_authorized_retrieval",
            Self::Ingestion => "resource_authorized_ingestion",
        }
    }
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Boundary {
    pub service: String,
    pub span: String,
    pub event: Option<String>,
    pub edge: Edge,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Edge {
    Start,
    End,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub service: String,
    pub span: String,
    pub response_pointer: Option<String>,
    pub active_when: Option<Check>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retrieval {
    pub results_pointer: String,
    pub id_pointer: String,
    pub relevant_document_ids: Vec<String>,
    pub top_k: usize,
    pub min_recall_at_k: f64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ingestion {
    pub dataset_id: String,
    pub document_id_pointer: String,
    pub state_pointer: String,
    pub chunks_pointer: String,
    pub completed_state: String,
    pub failed_states: Vec<String>,
    pub content_pointer: String,
    pub timeout_ms: u64,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MockConfig {
    pub origin: String,
    pub token_file: PathBuf,
    pub model: String,
    pub response_text: String,
    pub embedding_dimensions: usize,
    pub delay_ms: u64,
    pub max_requests: usize,
}
pub fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "." | "..")
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
pub fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn origin(raw: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(raw).map_err(|_| "invalid_loopback_origin")?;
    ensure(
        matches!(u.scheme(), "http" | "https")
            && u.host_str() == Some("127.0.0.1")
            && u.port().is_some_and(|p| p >= 1024)
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/"
            && u.query().is_none()
            && u.fragment().is_none()
            && raw == u.origin().ascii_serialization(),
        "numeric_loopback_origin_required",
    )?;
    Ok(u)
}
pub fn path(value: &str) -> bool {
    value.starts_with('/')
        && !value.starts_with("//")
        && !value.contains(['?', '#', '%', '\\'])
        && value.split('/').skip(1).all(|p| safe_id(p))
}
pub fn validate_profile(p: &Profile) -> Result<()> {
    ensure(
        p.schema == "agentforge-performance-profile/v1",
        "profile_schema",
    )?;
    origin(&p.gateway_origin)?;
    origin(&p.trace_origin)?;
    ensure(
        safe_id(&p.tenant_id) && safe_id(&p.user_id) && p.actor_token_file.is_absolute(),
        "existing_actor_required",
    )?;
    ensure(
        (3..=24).contains(&p.services.len())
            && p.services.iter().all(|s| safe_id(s))
            && p.services.iter().collect::<BTreeSet<_>>().len() == p.services.len(),
        "service_inventory",
    )?;
    for required in ["gateway", "knowledge-service", "knowledge-worker"] {
        ensure(
            p.services.iter().any(|s| s == required),
            "required_service_missing",
        )?;
    }
    ensure(
        p.transports
            .keys()
            .all(|k| matches!(k.as_str(), "gateway" | "trace")),
        "unknown_transport_target",
    )?;
    for transport in p.transports.values() {
        for file in [&transport.ca_file, &transport.identity_file]
            .into_iter()
            .flatten()
        {
            ensure(
                file.path.is_absolute() && digest(&file.sha256),
                "transport_material_reference",
            )?;
        }
    }
    let e = &p.environment;
    ensure(
        safe_id(&e.topology)
            && e.cpu_limit_millicores > 0
            && e.memory_limit_bytes > 0
            && e.database_pool_limit > 0
            && digest(&e.corpus_sha256)
            && digest(&e.index_sha256)
            && digest(&e.build_flags_sha256)
            && ["warm", "cold", "mixed_declared"].contains(&e.cache_condition.as_str())
            && safe_id(&e.background_load),
        "environment_profile_incomplete",
    )?;
    for control in [
        "authentication",
        "limiter",
        "tenant_acl",
        "billing",
        "csrf_when_applicable",
    ] {
        ensure(
            e.controls.get(control) == Some(&true),
            "correctness_control_must_remain_enabled",
        )?;
    }
    let b = &p.budgets;
    ensure(
        (100..=30000).contains(&b.request_ms)
            && (1..=3600).contains(&b.run_seconds)
            && (50..=1000).contains(&b.sample_ms)
            && (2..=72002).contains(&b.sample_limit)
            && (4096..=4 * 1024 * 1024).contains(&b.max_body_bytes)
            && (100..=30000).contains(&b.trace_wait_ms)
            && (100..=10000).contains(&b.poll_ms)
            && b.case_gap_ms <= 600000,
        "finite_safety_budget",
    )?;
    ensure(
        p.services
            .iter()
            .all(|s| b.rss_stop_bytes.get(s).is_some_and(|v| *v > 0)),
        "rss_safety_budget_per_service_required",
    )?;
    ensure(
        (1..=20).contains(&p.fixture_checks.len())
            && p.fixture_checks
                .iter()
                .all(|c| path(&c.path) && !c.checks.is_empty()),
        "read_only_fixture_checks_required",
    )?;
    ensure(
        (3..=12).contains(&p.cases.len()),
        "bounded_complete_workload_required",
    )?;
    let mut ids = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    for c in &p.cases {
        ensure(
            safe_id(&c.id)
                && ids.insert(c.id.clone())
                && (1..=50).contains(&c.samples)
                && c.warmup <= 2
                && c.gap_ms <= 600000
                && path(&c.path)
                && c.body.is_object()
                && !c.checks.is_empty(),
            "case_shape",
        )?;
        kinds.insert(c.kind.label());
        ensure(
            c.body.get("stream") != Some(&Value::Bool(true)),
            "streaming_requires_separate_workload",
        )?;
        match c.kind {
            Kind::GatewayMock | Kind::GatewayReal => {
                ensure(
                    c.boundaries.values().all(|b| b.service == "gateway"),
                    "gateway_boundaries_must_share_actual_gateway_clock",
                )?;
                ensure(
                    (c.kind == Kind::GatewayMock
                        && c.provider_scope == ProviderScope::SyntheticOrLocal)
                        || (c.kind == Kind::GatewayReal
                            && c.provider_scope == ProviderScope::AuthorizedReal),
                    "gateway_provider_population_mismatch",
                )?;
                ensure(
                    c.path == "/v1/chat/completions"
                        && c.body.get("model").is_some_and(Value::is_string),
                    "admitted_public_model_route_required",
                )?;
                for k in [
                    "receive",
                    "admit",
                    "upstream_dispatch",
                    "upstream_first_byte",
                    "upstream_complete",
                    "response_complete",
                ] {
                    ensure(
                        c.boundaries.contains_key(k),
                        "gateway_boundary_contract_required",
                    )?;
                }
                if c.kind == Kind::GatewayMock {
                    ensure(
                        p.mock.is_some()
                            && serde_json::to_string(&c.body)
                                .unwrap_or_default()
                                .contains("{{correlation}}"),
                        "synthetic_correlation_marker_required",
                    )?;
                } else {
                    let identity = c
                        .upstream_identity
                        .as_ref()
                        .ok_or("real_provider_trace_identity_required")?;
                    ensure(
                        identity.service == "gateway"
                            && !identity.span.is_empty()
                            && [
                                "server.address",
                                "gen_ai.provider.name",
                                "gen_ai.request.model",
                            ]
                            .iter()
                            .all(|k| identity.attributes.get(*k).is_some_and(|s| !s.is_empty())),
                        "real_provider_trace_identity_required",
                    )?;
                    ensure(
                        c.direct_control_body.is_none(),
                        "real_provider_direct_control_not_authorized",
                    )?;
                }
            }
            Kind::Retrieval => {
                ensure(
                    c.path.starts_with("/api/v1/knowledge/")
                        && c.path.ends_with("/retrieve")
                        && c.retrieval.is_some(),
                    "gateway_authorized_retrieval_route_required",
                )?;
                for stage in [
                    "embedding",
                    "cache_lookup",
                    "lexical",
                    "vector",
                    "fusion",
                    "rerank",
                    "serialization",
                ] {
                    ensure(
                        c.stages.contains_key(stage),
                        "all_retrieval_stage_contracts_required",
                    )?;
                }
                let r = c.retrieval.as_ref().unwrap();
                ensure(
                    !r.relevant_document_ids.is_empty()
                        && r.top_k > 0
                        && r.top_k <= 100
                        && r.min_recall_at_k.is_finite()
                        && r.min_recall_at_k > 0.0
                        && r.min_recall_at_k <= 1.0,
                    "relevance_fixture_required",
                )?;
            }
            Kind::Ingestion => {
                let i = c.ingestion.as_ref().ok_or("ingestion_fixture_required")?;
                ensure(
                    safe_id(&i.dataset_id)
                        && c.path == format!("/api/v1/knowledge/{}/documents/text", i.dataset_id)
                        && c.samples <= 8
                        && c.warmup == 0
                        && (1000..=900000).contains(&i.timeout_ms)
                        && c.body
                            .pointer(&i.content_pointer)
                            .is_some_and(Value::is_string)
                        && !i.failed_states.is_empty(),
                    "ingestion_fixture_shape",
                )?;
            }
        }
        for boundary in c.boundaries.values() {
            ensure(
                p.services.contains(&boundary.service)
                    && !boundary.span.is_empty()
                    && boundary.span.len() <= 200,
                "boundary_service",
            )?;
        }
        for (name, stage) in &c.stages {
            ensure(
                safe_id(name) && p.services.contains(&stage.service) && !stage.span.is_empty(),
                "stage_service",
            )?;
        }
    }
    for kind in [
        "deterministic_synthetic_upstream",
        "resource_authorized_retrieval",
        "resource_authorized_ingestion",
    ] {
        ensure(kinds.contains(kind), "mandatory_baseline_workload_missing")?;
    }
    if let Some(m) = &p.mock {
        let calls = p
            .cases
            .iter()
            .filter(|c| c.kind == Kind::GatewayMock)
            .map(|c| (c.samples + c.warmup) * (1 + usize::from(c.direct_control_body.is_some())))
            .sum::<usize>();
        ensure(
            m.max_requests >= calls,
            "mock_budget_cannot_cover_planned_calls",
        )?;
        mock_origin(&m.origin)?;
        ensure(
            m.origin != p.gateway_origin
                && m.origin != p.trace_origin
                && m.token_file.is_absolute()
                && safe_id(&m.model)
                && !m.response_text.is_empty()
                && m.response_text.len() <= 8192
                && (1..=4096).contains(&m.embedding_dimensions)
                && m.delay_ms <= 1000
                && (1..=1000).contains(&m.max_requests),
            "bounded_mock_profile",
        )?;
    }
    Ok(())
}

/// The upstream may bind an explicitly accepted local bridge interface so an
/// unchanged bridged Gateway can reach it. No wildcard/public/DNS binding.
pub fn mock_origin(raw: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(raw).map_err(|_| "mock_origin")?;
    let ip = u
        .host_str()
        .and_then(|s| s.parse::<std::net::Ipv4Addr>().ok())
        .ok_or("mock_numeric_local_address_required")?;
    ensure(
        (ip.is_loopback() || ip.is_private())
            && u.scheme() == "http"
            && u.port().is_some_and(|p| p >= 1024)
            && u.username().is_empty()
            && u.password().is_none()
            && u.path() == "/"
            && u.query().is_none()
            && u.fragment().is_none()
            && raw == u.origin().ascii_serialization(),
        "mock_local_interface_origin_required",
    )?;
    Ok(u)
}
