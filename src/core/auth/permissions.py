from __future__ import annotations

from collections.abc import Sequence
from dataclasses import dataclass
from enum import Enum

from ai_gateway_core.exceptions import PermissionDeniedError

from .rbac import RBAC


class Capability(str, Enum):
    """Gateway capability abstraction over concrete permission codes."""

    AGENT_INVOKE = "AgentInvoke"
    SERVICE_LIST_READ = "ServiceListRead"
    SERVICE_CONFIG_WRITE = "ServiceConfigWrite"
    GATEWAY_METRICS_READ = "GatewayMetricsRead"
    GATEWAY_USAGE_READ = "GatewayUsageRead"
    GATEWAY_QUOTA_READ = "GatewayQuotaRead"
    GATEWAY_QUOTA_WRITE = "GatewayQuotaWrite"
    GATEWAY_RATE_LIMIT_WRITE = "GatewayRateLimitWrite"
    GATEWAY_PROVIDER_CONFIG_READ = "GatewayProviderConfigRead"
    GATEWAY_PROVIDER_CONFIG_WRITE = "GatewayProviderConfigWrite"
    GATEWAY_SKILL_READ = "GatewaySkillRead"
    GATEWAY_SKILL_WRITE = "GatewaySkillWrite"
    GATEWAY_MCP_READ = "GatewayMcpRead"
    GATEWAY_MCP_WRITE = "GatewayMcpWrite"
    GATEWAY_MODEL_CONFIG_WRITE = "GatewayModelConfigWrite"
    GATEWAY_SERVICE_CONFIG_WRITE = "GatewayServiceConfigWrite"
    GATEWAY_EVAL_TRACE_READ = "GatewayEvalTraceRead"
    GATEWAY_EVAL_RUN = "GatewayEvalRun"
    GATEWAY_CONNECTOR_CONFIG_READ = "GatewayConnectorConfigRead"
    GATEWAY_CONNECTOR_CONFIG_WRITE = "GatewayConnectorConfigWrite"


_CAPABILITY_PERMISSIONS: dict[Capability, str] = {
    Capability.AGENT_INVOKE: "conversation:playground:access",
    Capability.SERVICE_LIST_READ: "console:services:view",
    Capability.SERVICE_CONFIG_WRITE: "console:services:edit",
    Capability.GATEWAY_METRICS_READ: "console:metrics:view",
    Capability.GATEWAY_USAGE_READ: "console:usage:view",
    Capability.GATEWAY_QUOTA_READ: "console:quota:view",
    Capability.GATEWAY_QUOTA_WRITE: "console:quota:edit",
    Capability.GATEWAY_RATE_LIMIT_WRITE: "console:rate_limits:edit",
    Capability.GATEWAY_PROVIDER_CONFIG_READ: "console:providers:view",
    Capability.GATEWAY_PROVIDER_CONFIG_WRITE: "console:providers:edit",
    Capability.GATEWAY_SKILL_READ: "console:skills:view",
    Capability.GATEWAY_SKILL_WRITE: "console:skills:edit",
    Capability.GATEWAY_MCP_READ: "console:mcp:view",
    Capability.GATEWAY_MCP_WRITE: "console:mcp:edit",
    Capability.GATEWAY_MODEL_CONFIG_WRITE: "console:models:edit",
    Capability.GATEWAY_SERVICE_CONFIG_WRITE: "console:services:edit",
    Capability.GATEWAY_EVAL_TRACE_READ: "console:eval:view",
    Capability.GATEWAY_EVAL_RUN: "console:eval:run",
    Capability.GATEWAY_CONNECTOR_CONFIG_READ: "console:settings:view",
    Capability.GATEWAY_CONNECTOR_CONFIG_WRITE: "console:settings:edit",
}

_CAPABILITY_LEGACY_ALIASES: dict[Capability, tuple[str, ...]] = {
    Capability.AGENT_INVOKE: ("service:invoke",),
    Capability.SERVICE_LIST_READ: ("service:view",),
    Capability.SERVICE_CONFIG_WRITE: ("service:manage",),
    Capability.GATEWAY_METRICS_READ: ("metrics:view",),
    Capability.GATEWAY_USAGE_READ: ("usage:view",),
    Capability.GATEWAY_QUOTA_READ: ("quota:view",),
    Capability.GATEWAY_QUOTA_WRITE: ("quota:manage",),
    Capability.GATEWAY_RATE_LIMIT_WRITE: ("rate_limits:manage",),
    Capability.GATEWAY_PROVIDER_CONFIG_READ: (
        "providers:view",
        "console:providers:edit",
        "providers:manage",
    ),
    Capability.GATEWAY_PROVIDER_CONFIG_WRITE: ("providers:manage",),
    Capability.GATEWAY_SKILL_READ: (
        "skills:view",
        "console:skills:edit",
        "skills:manage",
    ),
    Capability.GATEWAY_SKILL_WRITE: ("skills:manage",),
    Capability.GATEWAY_MCP_READ: (
        "mcp:view",
        "console:mcp:edit",
        "mcp:manage",
    ),
    Capability.GATEWAY_MCP_WRITE: ("mcp:manage",),
    Capability.GATEWAY_CONNECTOR_CONFIG_READ: ("console:settings:edit",),
    Capability.GATEWAY_MODEL_CONFIG_WRITE: ("models:manage",),
    Capability.GATEWAY_SERVICE_CONFIG_WRITE: ("service:manage",),
}


@dataclass(frozen=True)
class CapabilityDecision:
    capability: Capability
    required_permission: str
    accepted_permissions: tuple[str, ...]
    matched_permission: str | None = None

    @property
    def allowed(self) -> bool:
        return bool(self.matched_permission)


def canonical_permission(capability: Capability) -> str:
    return _CAPABILITY_PERMISSIONS[capability]


def accepted_permissions(capability: Capability) -> tuple[str, ...]:
    canonical = canonical_permission(capability)
    aliases = _CAPABILITY_LEGACY_ALIASES.get(capability, ())
    return (canonical, *aliases)


def _merge_subjects(roles: Sequence[str], permissions: Sequence[str] | None) -> list[str]:
    subjects: list[str] = []
    for raw in [*(roles or []), *(permissions or [])]:
        value = str(raw or "").strip()
        if value and value not in subjects:
            subjects.append(value)
    return subjects


def check_capability(
    *,
    rbac: RBAC,
    roles: Sequence[str],
    permissions: Sequence[str] | None,
    capability: Capability,
) -> CapabilityDecision:
    required = canonical_permission(capability)
    accepted = accepted_permissions(capability)
    subjects = _merge_subjects(roles, permissions)

    for permission in accepted:
        if rbac.has_permission(subjects, permission):
            return CapabilityDecision(
                capability=capability,
                required_permission=required,
                accepted_permissions=accepted,
                matched_permission=permission,
            )

    return CapabilityDecision(
        capability=capability,
        required_permission=required,
        accepted_permissions=accepted,
        matched_permission=None,
    )


def require_capability(
    *,
    rbac: RBAC,
    roles: Sequence[str],
    permissions: Sequence[str] | None,
    capability: Capability,
) -> CapabilityDecision:
    decision = check_capability(
        rbac=rbac,
        roles=roles,
        permissions=permissions,
        capability=capability,
    )
    if decision.allowed:
        return decision
    raise PermissionDeniedError(
        f"Missing capability {capability.value}: {decision.required_permission}"
    )


def build_permission_denied_detail(
    *,
    capability: Capability,
    trace_id: str = "",
    message: str | None = None,
) -> dict[str, object]:
    required = canonical_permission(capability)
    return {
        "message": message or f"Permission denied: {required} required",
        "required_capability": capability.value,
        "required_permission": required,
        "accepted_permissions": list(accepted_permissions(capability)),
        "trace_id": trace_id,
    }
