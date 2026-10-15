//! Exact permission matching and capability aliases from src/core/auth/{rbac,permissions}.py.
//! Inputs must come from verified current authority; this does not authenticate a caller.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    AgentInvoke,
    ServiceListRead,
    ServiceConfigWrite,
    GatewayMetricsRead,
    GatewayUsageRead,
    GatewayQuotaRead,
    GatewayQuotaWrite,
    GatewayRateLimitWrite,
    GatewayProviderConfigRead,
    GatewayProviderConfigWrite,
    GatewaySkillRead,
    GatewaySkillWrite,
    GatewayMcpRead,
    GatewayMcpWrite,
    GatewayModelConfigWrite,
    GatewayServiceConfigWrite,
    GatewayEvalTraceRead,
    GatewayEvalRun,
    GatewayConnectorConfigRead,
    GatewayConnectorConfigWrite,
}
impl Capability {
    pub fn name(self) -> &'static str {
        match self {
            Self::AgentInvoke => "AgentInvoke",
            Self::ServiceListRead => "ServiceListRead",
            Self::ServiceConfigWrite => "ServiceConfigWrite",
            Self::GatewayMetricsRead => "GatewayMetricsRead",
            Self::GatewayUsageRead => "GatewayUsageRead",
            Self::GatewayQuotaRead => "GatewayQuotaRead",
            Self::GatewayQuotaWrite => "GatewayQuotaWrite",
            Self::GatewayRateLimitWrite => "GatewayRateLimitWrite",
            Self::GatewayProviderConfigRead => "GatewayProviderConfigRead",
            Self::GatewayProviderConfigWrite => "GatewayProviderConfigWrite",
            Self::GatewaySkillRead => "GatewaySkillRead",
            Self::GatewaySkillWrite => "GatewaySkillWrite",
            Self::GatewayMcpRead => "GatewayMcpRead",
            Self::GatewayMcpWrite => "GatewayMcpWrite",
            Self::GatewayModelConfigWrite => "GatewayModelConfigWrite",
            Self::GatewayServiceConfigWrite => "GatewayServiceConfigWrite",
            Self::GatewayEvalTraceRead => "GatewayEvalTraceRead",
            Self::GatewayEvalRun => "GatewayEvalRun",
            Self::GatewayConnectorConfigRead => "GatewayConnectorConfigRead",
            Self::GatewayConnectorConfigWrite => "GatewayConnectorConfigWrite",
        }
    }
    pub fn canonical_permission(self) -> &'static str {
        match self {
            Self::AgentInvoke => "conversation:playground:access",
            Self::ServiceListRead => "console:services:view",
            Self::ServiceConfigWrite => "console:services:edit",
            Self::GatewayMetricsRead => "console:metrics:view",
            Self::GatewayUsageRead => "console:usage:view",
            Self::GatewayQuotaRead => "console:quota:view",
            Self::GatewayQuotaWrite => "console:quota:edit",
            Self::GatewayRateLimitWrite => "console:rate_limits:edit",
            Self::GatewayProviderConfigRead => "console:providers:view",
            Self::GatewayProviderConfigWrite => "console:providers:edit",
            Self::GatewaySkillRead => "console:skills:view",
            Self::GatewaySkillWrite => "console:skills:edit",
            Self::GatewayMcpRead => "console:mcp:view",
            Self::GatewayMcpWrite => "console:mcp:edit",
            Self::GatewayModelConfigWrite => "console:models:edit",
            Self::GatewayServiceConfigWrite => "console:services:edit",
            Self::GatewayEvalTraceRead => "console:eval:view",
            Self::GatewayEvalRun => "console:eval:run",
            Self::GatewayConnectorConfigRead => "console:settings:view",
            Self::GatewayConnectorConfigWrite => "console:settings:edit",
        }
    }
    pub fn accepted_permissions(self) -> &'static [&'static str] {
        match self {
            Self::AgentInvoke => &["conversation:playground:access", "service:invoke"],
            Self::ServiceListRead => &["console:services:view", "service:view"],
            Self::ServiceConfigWrite => &["console:services:edit", "service:manage"],
            Self::GatewayMetricsRead => &["console:metrics:view", "metrics:view"],
            Self::GatewayUsageRead => &["console:usage:view", "usage:view"],
            Self::GatewayQuotaRead => &["console:quota:view", "quota:view"],
            Self::GatewayQuotaWrite => &["console:quota:edit", "quota:manage"],
            Self::GatewayRateLimitWrite => &["console:rate_limits:edit", "rate_limits:manage"],
            Self::GatewayProviderConfigRead => &[
                "console:providers:view",
                "providers:view",
                "console:providers:edit",
                "providers:manage",
            ],
            Self::GatewayProviderConfigWrite => &["console:providers:edit", "providers:manage"],
            Self::GatewaySkillRead => &[
                "console:skills:view",
                "skills:view",
                "console:skills:edit",
                "skills:manage",
            ],
            Self::GatewaySkillWrite => &["console:skills:edit", "skills:manage"],
            Self::GatewayMcpRead => &[
                "console:mcp:view",
                "mcp:view",
                "console:mcp:edit",
                "mcp:manage",
            ],
            Self::GatewayMcpWrite => &["console:mcp:edit", "mcp:manage"],
            Self::GatewayModelConfigWrite => &["console:models:edit", "models:manage"],
            Self::GatewayServiceConfigWrite => &["console:services:edit", "service:manage"],
            Self::GatewayEvalTraceRead => &["console:eval:view"],
            Self::GatewayEvalRun => &["console:eval:run"],
            Self::GatewayConnectorConfigRead => &["console:settings:view", "console:settings:edit"],
            Self::GatewayConnectorConfigWrite => &["console:settings:edit"],
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Rbac {
    pub role_permissions: HashMap<String, Vec<String>>,
}
impl Rbac {
    pub fn new(role_permissions: HashMap<String, Vec<String>>) -> Self {
        Self { role_permissions }
    }
    pub fn matches_permission(permissions: &HashSet<&str>, permission: &str) -> bool {
        if permissions.contains("admin:*") || permissions.contains(permission) {
            return true;
        }
        let mut end = permission.len();
        while let Some(index) = permission[..end].rfind(':') {
            if permissions.contains(format!("{}:*", &permission[..index]).as_str()) {
                return true;
            }
            end = index;
        }
        false
    }
    pub fn permissions_for_roles<'a>(&'a self, roles: &[String]) -> HashSet<&'a str> {
        roles
            .iter()
            .filter_map(|role| self.role_permissions.get(role))
            .flatten()
            .map(String::as_str)
            .collect()
    }
    pub fn has_permission(&self, roles: &[String], permission: &str) -> bool {
        Self::matches_permission(&roles.iter().map(String::as_str).collect(), permission)
            || Self::matches_permission(&self.permissions_for_roles(roles), permission)
    }
    pub fn require(&self, roles: &[String], permission: &str) -> Result<(), String> {
        if self.has_permission(roles, permission) {
            Ok(())
        } else {
            Err(format!("Missing permission: {permission}"))
        }
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CapabilityDecision {
    pub capability: Capability,
    pub required_permission: &'static str,
    pub accepted_permissions: &'static [&'static str],
    pub matched_permission: Option<&'static str>,
}
impl CapabilityDecision {
    pub fn allowed(&self) -> bool {
        self.matched_permission.is_some()
    }
}
pub fn check_capability(
    rbac: &Rbac,
    roles: &[String],
    permissions: &[String],
    capability: Capability,
) -> CapabilityDecision {
    let mut subjects = Vec::new();
    for raw in roles.iter().chain(permissions) {
        let v = raw.trim();
        if !v.is_empty() && !subjects.iter().any(|s| s == v) {
            subjects.push(v.to_owned());
        }
    }
    let accepted = capability.accepted_permissions();
    let matched = accepted
        .iter()
        .copied()
        .find(|p| rbac.has_permission(&subjects, p));
    CapabilityDecision {
        capability,
        required_permission: capability.canonical_permission(),
        accepted_permissions: accepted,
        matched_permission: matched,
    }
}
pub fn require_capability(
    rbac: &Rbac,
    roles: &[String],
    permissions: &[String],
    capability: Capability,
) -> Result<CapabilityDecision, String> {
    let decision = check_capability(rbac, roles, permissions, capability);
    if decision.allowed() {
        Ok(decision)
    } else {
        Err(format!(
            "Missing capability {}: {}",
            capability.name(),
            decision.required_permission
        ))
    }
}
pub fn permission_denied_detail(
    capability: Capability,
    trace_id: &str,
    message: Option<&str>,
) -> Value {
    json!({"message":message.filter(|s|!s.is_empty()).map(str::to_owned).unwrap_or_else(||format!("Permission denied: {} required",capability.canonical_permission())),"required_capability":capability.name(),"required_permission":capability.canonical_permission(),"accepted_permissions":capability.accepted_permissions(),"trace_id":trace_id})
}

