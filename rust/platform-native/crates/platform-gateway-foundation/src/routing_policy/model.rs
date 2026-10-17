use crate::canonical_json_v2::CanonicalDocument;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const SCHEMA: &str = "agentforge.routing_policy/v1";
pub const MAX_EXACT: u64 = 9_007_199_254_740_991;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingError {
    InvalidPolicy,
    InvalidSnapshot,
    Forbidden,
    PinMismatch,
    NoCandidate,
    Storage,
    Conflict,
    NotFound,
    UncertainCommit,
}
impl std::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "routing_policy_{self:?}")
    }
}
impl std::error::Error for RoutingError {}
pub type Result<T> = std::result::Result<T, RoutingError>;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub provider: String,
    pub model: String,
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Priority,
    Weighted,
    LowestLatency,
    LowestCost,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateRule {
    pub target: Identity,
    pub priority: u32,
    pub weight: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: String,
    pub tenant: String,
    pub policy_id: String,
    pub version: String,
    /// Cost rules and observations use USD nanos; no implicit FX conversion.
    pub cost_currency: String,
    pub strategy: Strategy,
    pub candidates: Vec<CandidateRule>,
    pub required_capabilities: BTreeSet<String>,
    pub max_latency_ms: Option<u64>,
    pub max_cost_nanos: Option<u64>,
    pub metric_max_age_ms: u32,
    pub max_attempts: u8,
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        if self.cost_currency != "USD"
            || self.schema != SCHEMA
            || !text(&self.tenant)
            || !text(&self.policy_id)
            || !text(&self.version)
            || self.policy_id.len() > 128
            || self.version.len() > 128
            || self.candidates.is_empty()
            || self.candidates.len() > 128
            || !(1..=8).contains(&self.max_attempts)
            || !(1..=3_600_000).contains(&self.metric_max_age_ms)
            || self.max_latency_ms.is_some_and(|v| v > MAX_EXACT)
            || self.max_cost_nanos.is_some_and(|v| v > MAX_EXACT)
            || self.required_capabilities.len() > 64
            || self.required_capabilities.iter().any(|s| !text(s))
        {
            return Err(RoutingError::InvalidPolicy);
        }
        let mut ids = BTreeSet::new();
        for c in &self.candidates {
            if !valid_identity(&c.target)
                || c.weight == 0
                || c.weight > 1_000_000
                || !ids.insert(&c.target)
            {
                return Err(RoutingError::InvalidPolicy);
            }
        }
        Ok(())
    }
    pub fn pin(&self) -> Result<String> {
        self.validate()?;
        hash("policy", self)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub value: u64,
    pub measured_at_ms: i64,
    pub reference: String,
}
/// Evidence is assembled by the server from existing registry and authorization
/// authorities. Policies cannot grant access or invent a model/catalog record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub tenant: String,
    pub target: Identity,
    pub catalog_available: bool,
    pub authorized: bool,
    pub invocation_compatible: bool,
    pub capabilities: BTreeSet<String>,
    pub latency: Option<Observation>,
    pub cost: Option<Observation>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub tenant: String,
    pub actor: String,
    pub invocation_id: String,
    pub policy_pin: String,
    pub seed: String,
    pub at_ms: i64,
    pub required_capabilities: BTreeSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Exclusion {
    InvocationUnsupported,
    NotAuthorized,
    CatalogUnavailable,
    CapabilityMismatch,
    LatencyUnknown,
    LatencyLimit,
    CostUnknown,
    CostLimit,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateTrace {
    pub target: Identity,
    pub exclusions: Vec<Exclusion>,
    pub latency: Option<Observation>,
    pub cost: Option<Observation>,
    pub priority: u32,
    pub weight: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub schema: String,
    pub id: String,
    pub policy_id: String,
    pub policy_version: String,
    pub context: Evaluation,
    pub snapshot_pin: String,
    pub selected: Identity,
    pub eligible: Vec<Identity>,
    pub trace: Vec<CandidateTrace>,
    pub max_attempts: u8,
}
impl Decision {
    pub fn verify(&self) -> Result<()> {
        let mut unsigned = self.clone();
        unsigned.id.clear();
        if hash("decision", &unsigned)? != self.id {
            return Err(RoutingError::PinMismatch);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderOutcome {
    DefinitelyNotSent,
    ExplicitRateLimitBeforeOutput,
    RejectedBeforeExecution,
    ExecutionMayHaveStarted,
    OutputStarted,
    Completed,
    Unknown,
}
pub(super) fn text(s: &str) -> bool {
    !s.is_empty() && s.len() <= 512 && s == s.trim() && !s.chars().any(char::is_control)
}
pub(super) fn valid_identity(i: &Identity) -> bool {
    [&i.provider, &i.model, &i.version].iter().all(|s| text(s))
}
pub(super) fn hash(kind: &str, value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(&("agentforge.routing_policy/jcs-v2", kind, value))
        .map_err(|_| RoutingError::InvalidSnapshot)?;
    CanonicalDocument::from_json(&bytes)
        .and_then(|d| d.sha256_hex())
        .map_err(|_| RoutingError::InvalidSnapshot)
}
