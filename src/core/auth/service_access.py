from __future__ import annotations

from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from enum import Enum
from typing import Any


class ServiceAccessMode(str, Enum):
    ALL = "all"
    ALLOWLIST = "allowlist"


def _normalize_token(value: Any) -> str:
    return str(value or "").strip().lower()


def normalize_service_scope(value: Any) -> list[str]:
    """Normalize allow/deny service scope definitions.

    Supports:
    - comma separated string: "agent,flash"
    - list/tuple/set values
    """
    if not value:
        return []

    raw_items: list[Any]
    if isinstance(value, str):
        raw_items = [part.strip() for part in value.split(",")]
    elif isinstance(value, (list, tuple, set)):
        raw_items = list(value)
    else:
        return []

    deduped: list[str] = []
    for item in raw_items:
        token = _normalize_token(item)
        if token and token not in deduped:
            deduped.append(token)
    return deduped


def service_scope_matches(scope: Sequence[str], candidates: Sequence[str]) -> bool:
    """Return True when any candidate is matched by the scope rules.

    Scope rules support:
    - exact match: "agent"
    - prefix wildcard: "agent-*"
    - global wildcard: "*"
    """
    normalized_scope = normalize_service_scope(scope)
    if not normalized_scope:
        return True

    normalized_candidates = {_normalize_token(c) for c in candidates if _normalize_token(c)}
    if not normalized_candidates:
        return False

    if "*" in normalized_scope:
        return True

    for rule in normalized_scope:
        if rule.endswith("*"):
            prefix = rule[:-1]
            if any(candidate.startswith(prefix) for candidate in normalized_candidates):
                return True
            continue
        if rule in normalized_candidates:
            return True
    return False


def normalize_service_candidates(values: Iterable[Any]) -> tuple[str, ...]:
    deduped: list[str] = []
    for value in values:
        token = _normalize_token(value)
        if token and token not in deduped:
            deduped.append(token)
    return tuple(deduped)


@dataclass(frozen=True)
class ServiceAccessPolicy:
    mode: ServiceAccessMode = ServiceAccessMode.ALL
    allowed_services: tuple[str, ...] = ()
    denied_services: tuple[str, ...] = ()


def service_access_policy_from_metadata(metadata: Any) -> ServiceAccessPolicy:
    """Build a user-level service access policy from users.metadata."""
    if not isinstance(metadata, dict):
        return ServiceAccessPolicy()

    payload = metadata.get("service_access")
    payload = payload if isinstance(payload, dict) else {}

    allowed_services = normalize_service_scope(payload.get("allowed_services"))
    denied_services = normalize_service_scope(payload.get("denied_services"))

    mode_raw = _normalize_token(payload.get("mode"))
    if mode_raw not in (ServiceAccessMode.ALL.value, ServiceAccessMode.ALLOWLIST.value):
        mode = ServiceAccessMode.ALLOWLIST if allowed_services else ServiceAccessMode.ALL
    else:
        mode = ServiceAccessMode(mode_raw)

    return ServiceAccessPolicy(
        mode=mode,
        allowed_services=tuple(allowed_services),
        denied_services=tuple(denied_services),
    )


def evaluate_service_access(
    policy: ServiceAccessPolicy,
    candidates: Sequence[str],
) -> tuple[bool, str]:
    """Evaluate whether service candidates are allowed by policy."""
    normalized_candidates = normalize_service_candidates(candidates)
    if policy.denied_services and service_scope_matches(
        policy.denied_services, normalized_candidates
    ):
        return False, "denied_by_user_policy"

    if policy.mode == ServiceAccessMode.ALLOWLIST:
        if not policy.allowed_services:
            return False, "user_allowlist_empty"
        if not service_scope_matches(policy.allowed_services, normalized_candidates):
            return False, "not_in_user_allowlist"

    return True, "allowed"
