//! Schema-validated Cedar PDP. Shadow is observational; every enforcement flip
//! recomputes the differential matrix against the actual configured role map.
//! No policy decision is inferred from the legacy boolean input.
use crate::rbac::{Capability, Rbac};
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityUid, PolicySet, Request, Schema, ValidationMode,
    Validator,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, sync::Arc};
pub mod resources;
const POLICY: &str = include_str!("cedar/policy.cedar");
pub const CONFIG_KEY: &str = "GATEWAY_CEDAR_CONFIGURATION";
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Off,
    Shadow,
    Enforce,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub resources: resources::ResourceConfiguration,
    /// Explicit per-capability flips; never a wildcard enable switch.
    #[serde(default)]
    pub enforce: BTreeSet<String>,
    #[serde(default)]
    pub expected_policy_sha256: Option<String>,
    #[serde(default)]
    pub expected_matrix_sha256: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicyError {
    Configuration,
    Evaluation,
    MatrixMismatch,
    AuditUnavailable,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DecisionLog {
    pub decision_id: String,
    pub capability: String,
    pub policy_sha256: String,
    pub matrix_sha256: String,
    pub principal_fingerprint: String,
    pub mode: Mode,
    pub legacy_allowed: bool,
    pub cedar_allowed: bool,
    pub enforced_allowed: bool,
    pub evaluation_error: bool,
    #[serde(default)]
    pub resource: Option<resources::ResourceAudit>,
}
// Initialization diagnostics only describe the shipped schema/policy, never
// request attributes, configured credentials, or a subject's authorization facts.
fn configuration_error(stage: &str, detail: impl std::fmt::Debug) -> PolicyError {
    eprintln!("Cedar initialization rejected at {stage}: {detail:?}");
    PolicyError::Configuration
}
fn validated_policies(
    schema: &Schema,
    source: &str,
    stage: &str,
) -> Result<PolicySet, PolicyError> {
    let policies = source
        .parse::<PolicySet>()
        .map_err(|error| configuration_error(stage, error))?;
    let validation = Validator::new(schema.clone()).validate(&policies, ValidationMode::Strict);
    if !validation.validation_passed() {
        return Err(configuration_error(
            stage,
            validation.validation_errors().collect::<Vec<_>>(),
        ));
    }
    Ok(policies)
}
pub trait DecisionAudit: Send + Sync {
    fn ready(&self) -> bool;
    fn record(&self, decision: DecisionLog) -> Result<(), PolicyError>;
}
pub struct DecisionPoint {
    config: Configuration,
    objects: resources::ObjectPolicy,
    schema: Schema,
    policies: PolicySet,
    policy_sha256: String,
    matrix_sha256: String,
    audit: Arc<dyn DecisionAudit>,
}
impl std::fmt::Debug for DecisionPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CedarDecisionPoint")
            .field("mode", &self.config.mode)
            .field("policy_sha256", &self.policy_sha256)
            .finish_non_exhaustive()
    }
}
pub fn capabilities() -> &'static [Capability] {
    &[
        Capability::AgentInvoke,
        Capability::ServiceListRead,
        Capability::ServiceConfigWrite,
        Capability::GatewayMetricsRead,
        Capability::GatewayUsageRead,
        Capability::GatewayQuotaRead,
        Capability::GatewayQuotaWrite,
        Capability::GatewayRateLimitWrite,
        Capability::GatewayProviderConfigRead,
        Capability::GatewayProviderConfigWrite,
        Capability::GatewaySkillRead,
        Capability::GatewaySkillWrite,
        Capability::GatewayMcpRead,
        Capability::GatewayMcpWrite,
        Capability::GatewayModelConfigWrite,
        Capability::GatewayServiceConfigWrite,
        Capability::GatewayEvalTraceRead,
        Capability::GatewayEvalRun,
        Capability::GatewayConnectorConfigRead,
        Capability::GatewayConnectorConfigWrite,
    ]
}
fn schema() -> Result<Schema, PolicyError> {
    let actions=capabilities().iter().map(|c|(c.name().to_owned(),json!({"appliesTo":{"principalTypes":["Subject"],"resourceTypes":["Resource"],"context":{"type":"Record","attributes":{"accepted":{"type":"Set","element":{"type":"String"}}}}}}))).collect::<serde_json::Map<_,_>>();
    Schema::from_json_value(json!({"AgentForge":{"entityTypes":{
        "Subject":{"memberOfTypes":[],"shape":{"type":"Record","attributes":{"tenant":{"type":"String"},"permissions":{"type":"Set","element":{"type":"String"}}}}},
        "Resource":{"memberOfTypes":[],"shape":{"type":"Record","attributes":{"tenant":{"type":"String"}}}}},"actions":actions}})).map_err(|error|configuration_error("capability schema",error))
}
fn ancestors(permission: &str) -> Vec<String> {
    let mut out = vec![permission.to_owned(), "admin:*".into()];
    let mut end = permission.len();
    while let Some(index) = permission[..end].rfind(':') {
        out.push(format!("{}:*", &permission[..index]));
        end = index;
    }
    out
}
fn subjects(rbac: &Rbac, roles: &[String], permissions: &[String]) -> BTreeSet<String> {
    let base = roles
        .iter()
        .chain(permissions)
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<BTreeSet<_>>();
    let mut out = base
        .iter()
        .map(|s| (*s).to_owned())
        .collect::<BTreeSet<_>>();
    for role in base {
        if let Some(permissions) = rbac.role_permissions.get(role) {
            out.extend(permissions.iter().cloned());
        }
    }
    out
}
impl DecisionPoint {
    pub fn new(
        config: Configuration,
        rbac: &Rbac,
        audit: Arc<dyn DecisionAudit>,
    ) -> Result<Arc<Self>, PolicyError> {
        if config.mode == Mode::Off
            || config
                .enforce
                .iter()
                .any(|name| !capabilities().iter().any(|c| c.name() == name))
            || config.mode != Mode::Enforce
                && (!config.enforce.is_empty() || !config.resources.enforce.is_empty())
        {
            return Err(PolicyError::Configuration);
        }
        let schema = schema()?;
        let policies = validated_policies(&schema, POLICY, "capability policy")?;
        let policy_sha256 = format!("{:x}", Sha256::digest(POLICY.as_bytes()));
        let objects = resources::ObjectPolicy::new(rbac.role_permissions.clone())?;
        let mut point = Self {
            objects,
            config,
            schema,
            policies,
            policy_sha256,
            matrix_sha256: String::new(),
            audit,
        };
        let matrix = point.differential_matrix(rbac)?;
        point.matrix_sha256 = matrix;
        if point.config.mode == Mode::Enforce {
            if point.config.enforce.is_empty() && point.config.resources.enforce.is_empty()
                || !point.config.enforce.is_empty()
                    && (point.config.expected_policy_sha256.as_deref()
                        != Some(point.policy_sha256.as_str())
                        || point.config.expected_matrix_sha256.as_deref()
                            != Some(point.matrix_sha256.as_str()))
                || !point.config.resources.enforce.is_empty()
                    && (point.config.resources.expected_policy_sha256.as_deref()
                        != Some(point.objects.policy_sha256.as_str())
                        || point.config.resources.expected_matrix_sha256.as_deref()
                            != Some(point.objects.matrix_sha256.as_str()))
            {
                return Err(PolicyError::MatrixMismatch);
            }
        }
        Ok(Arc::new(point))
    }
    pub fn policy_sha256(&self) -> &str {
        &self.policy_sha256
    }
    pub fn matrix_sha256(&self) -> &str {
        &self.matrix_sha256
    }
    pub fn mode(&self) -> Mode {
        self.config.mode
    }
    fn evaluate(
        &self,
        rbac: &Rbac,
        roles: &[String],
        permissions: &[String],
        capability: Capability,
        principal_tenant: &str,
        resource_tenant: &str,
    ) -> Result<bool, PolicyError> {
        let principal: EntityUid = "AgentForge::Subject::\"current\""
            .parse()
            .map_err(|_| PolicyError::Evaluation)?;
        let action: EntityUid = format!("AgentForge::Action::\"{}\"", capability.name())
            .parse()
            .map_err(|_| PolicyError::Evaluation)?;
        let resource: EntityUid = "AgentForge::Resource::\"current\""
            .parse()
            .map_err(|_| PolicyError::Evaluation)?;
        let entities=Entities::from_json_value(json!([
            {"uid":{"type":"AgentForge::Subject","id":"current"},"attrs":{"tenant":principal_tenant,"permissions":subjects(rbac,roles,permissions)},"parents":[]},
            {"uid":{"type":"AgentForge::Resource","id":"current"},"attrs":{"tenant":resource_tenant},"parents":[]}
        ]),Some(&self.schema)).map_err(|_|PolicyError::Evaluation)?;
        let accepted = capability
            .accepted_permissions()
            .iter()
            .flat_map(|p| ancestors(p))
            .collect::<BTreeSet<_>>();
        let context =
            Context::from_json_value(json!({"accepted":accepted}), Some((&self.schema, &action)))
                .map_err(|_| PolicyError::Evaluation)?;
        let request = Request::new(principal, action, resource, context, Some(&self.schema))
            .map_err(|_| PolicyError::Evaluation)?;
        let response = Authorizer::new().is_authorized(&request, &self.policies, &entities);
        if response.diagnostics().errors().next().is_some() {
            return Err(PolicyError::Evaluation);
        }
        Ok(response.decision() == Decision::Allow)
    }
    pub fn decide(
        &self,
        rbac: &Rbac,
        roles: &[String],
        permissions: &[String],
        capability: Capability,
        legacy_allowed: bool,
    ) -> bool {
        let evaluated = self.evaluate(rbac, roles, permissions, capability, "current", "current");
        let cedar_allowed = matches!(evaluated, Ok(true));
        let enforce =
            self.config.mode == Mode::Enforce && self.config.enforce.contains(capability.name());
        let mut allowed = if enforce {
            cedar_allowed && self.audit.ready()
        } else {
            legacy_allowed
        };
        let principal = subjects(rbac, roles, permissions)
            .into_iter()
            .collect::<Vec<_>>();
        let fingerprint = serde_json::to_vec(&principal)
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .unwrap_or_default();
        let log = DecisionLog {
            decision_id: uuid::Uuid::new_v4().to_string(),
            capability: capability.name().into(),
            policy_sha256: self.policy_sha256.clone(),
            matrix_sha256: self.matrix_sha256.clone(),
            principal_fingerprint: fingerprint,
            mode: self.config.mode,
            legacy_allowed,
            cedar_allowed,
            enforced_allowed: allowed,
            evaluation_error: evaluated.is_err(),
            resource: None,
        };
        if self.audit.record(log).is_err() && enforce {
            allowed = false;
        }
        allowed
    }
    fn differential_matrix(&self, rbac: &Rbac) -> Result<String, PolicyError> {
        let mut cases = vec![vec![], vec!["unknown-role".into()], vec!["admin:*".into()]];
        cases.extend(rbac.role_permissions.keys().map(|role| vec![role.clone()]));
        for capability in capabilities() {
            for permission in capability.accepted_permissions() {
                for permission in ancestors(permission) {
                    cases.push(vec![permission]);
                }
            }
        }
        cases.sort();
        cases.dedup();
        let mut matrix = Vec::new();
        for case in cases {
            for capability in capabilities() {
                // Independent legacy implementation, not a supplied decision bit.
                let normalized = case
                    .iter()
                    .map(|value| value.trim())
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let legacy = capability
                    .accepted_permissions()
                    .iter()
                    .any(|p| rbac.has_permission(&normalized, p));
                let cedar = self.evaluate(rbac, &case, &[], *capability, "current", "current")?;
                if cedar != legacy {
                    return Err(PolicyError::MatrixMismatch);
                }
                if self.evaluate(rbac, &case, &[], *capability, "tenant-a", "tenant-b")? {
                    return Err(PolicyError::MatrixMismatch);
                }
                matrix.push(json!([case, capability.name(), legacy, cedar]));
            }
        }
        serde_json::to_vec(&matrix)
            .map(|bytes| format!("{:x}", Sha256::digest(bytes)))
            .map_err(|_| PolicyError::Configuration)
    }
}
#[cfg(test)]
mod tests;

/// Config-only validation runs the same schema and zero-diff matrix as startup,
/// without starting an audit writer or evaluating a user request.
pub fn validate_configuration(
    configuration: Configuration,
    rbac: &Rbac,
) -> Result<Option<(String, String)>, PolicyError> {
    struct ValidationAudit;
    impl DecisionAudit for ValidationAudit {
        fn ready(&self) -> bool {
            true
        }
        fn record(&self, _: DecisionLog) -> Result<(), PolicyError> {
            Ok(())
        }
    }
    if configuration.mode == Mode::Off {
        if !configuration.resources.enforce.is_empty()
            || configuration.resources.expected_policy_sha256.is_some()
            || configuration.resources.expected_matrix_sha256.is_some()
            || !configuration.enforce.is_empty()
            || configuration.expected_policy_sha256.is_some()
            || configuration.expected_matrix_sha256.is_some()
        {
            return Err(PolicyError::Configuration);
        }
        return Ok(None);
    }
    let point = DecisionPoint::new(configuration, rbac, Arc::new(ValidationAudit))?;
    Ok(Some((
        point.policy_sha256.clone(),
        point.matrix_sha256.clone(),
    )))
}

/// Public, secret-free evidence for an operator's reviewed per-resource flip.
/// This executes the same strict policies and differential basis as startup.
#[derive(Clone, Debug, Serialize)]
pub struct AuthorizationEvidence {
    pub capability_policy_sha256: String,
    pub capability_matrix_sha256: String,
    pub resource_policy_sha256: String,
    pub resource_matrix_sha256: String,
}
pub fn authorization_evidence(rbac: &Rbac) -> Result<AuthorizationEvidence, PolicyError> {
    struct EvidenceAudit;
    impl DecisionAudit for EvidenceAudit {
        fn ready(&self) -> bool {
            true
        }
        fn record(&self, _: DecisionLog) -> Result<(), PolicyError> {
            Ok(())
        }
    }
    let point = DecisionPoint::new(
        Configuration {
            mode: Mode::Shadow,
            ..Default::default()
        },
        rbac,
        Arc::new(EvidenceAudit),
    )?;
    Ok(AuthorizationEvidence {
        capability_policy_sha256: point.policy_sha256().into(),
        capability_matrix_sha256: point.matrix_sha256().into(),
        resource_policy_sha256: point.resource_policy_sha256().into(),
        resource_matrix_sha256: point.resource_matrix_sha256().into(),
    })
}
