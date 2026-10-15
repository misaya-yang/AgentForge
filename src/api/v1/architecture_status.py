"""Administrator-only, sanitized platform architecture status."""

from __future__ import annotations

import json
import os
import re
from datetime import datetime, timezone
from functools import lru_cache
from pathlib import Path
from typing import Any, Literal

from fastapi import APIRouter, Depends, Request
from pydantic import BaseModel, Field

from ...core.auth.permissions import Capability
from ..deps import AuthContext, get_auth_context, get_health_monitor, require_platform_admin
from .health import _gateway_dependency_snapshot

router = APIRouter(prefix="/admin/architecture-status", tags=["administration"])
TOPOLOGY_PATH = Path(__file__).resolve().parents[2] / "core/data/service_topology.json"
HEALTHY_STATUS = {"healthy", "ready", "ok"}
DEGRADED_STATUS = {
    "disabled",
    "draining",
    "error",
    "misconfigured",
    "not_configured",
    "not_ready",
    "schema_mismatch",
    "unavailable",
    "unhealthy",
}


class ArchitectureDependency(BaseModel):
    service_id: str
    required: bool
    status: str


class ArchitectureServiceStatus(BaseModel):
    service_id: str
    display_name: str
    bounded_context: str
    responsibility: str
    lifecycle: str
    exposure: str
    status: str
    version: str
    state_owner: str
    scale_support: str
    replicas: int = Field(ge=0)
    active_in_mode: bool
    dependencies: list[ArchitectureDependency]
    degraded_reasons: list[str]
    last_check: str | None


class ArchitectureGroupStatus(BaseModel):
    group_id: str
    display_name: str
    services: list[ArchitectureServiceStatus]


class ArchitectureStatusResponse(BaseModel):
    schema_version: Literal["ai-gateway/architecture-status/v1"]
    topology_revision: str
    mode: Literal["compact", "full", "scale"]
    mode_configuration_valid: bool
    last_check: str
    groups: list[ArchitectureGroupStatus]


@lru_cache(maxsize=1)
def _topology() -> dict[str, Any]:
    payload = json.loads(TOPOLOGY_PATH.read_text(encoding="utf-8"))
    if payload.get("schema") != "ai-gateway/service-topology/v1":
        raise RuntimeError("invalid service topology manifest")
    return payload


def _deployment_mode(topology: dict[str, Any]) -> tuple[str, bool]:
    mode = os.environ.get("AI_PLATFORM_TOPOLOGY_MODE", topology["default_mode"])
    if mode in {"compact", "full", "scale"}:
        return mode, True
    return topology["default_mode"], False


def _version(service: dict[str, Any]) -> str:
    artifact = service["image_artifact"]
    raw = os.environ.get(str(artifact.get("version_env") or ""), "")
    candidate = raw.rsplit("@", 1)[-1].rsplit(":", 1)[-1] if raw else ""
    if re.fullmatch(r"[A-Za-z0-9._+-]{1,64}", candidate):
        return candidate
    return str(artifact.get("default_version") or "unknown")[:64]


def _normalized_status(value: Any) -> str:
    status = str(value or "unknown").strip().lower()
    if status in HEALTHY_STATUS:
        return "healthy"
    if status == "degraded":
        return "degraded"
    if status in DEGRADED_STATUS or status.startswith(("error:", "status_")):
        return "unavailable"
    return "unknown"


def _timestamp(value: Any) -> str | None:
    if isinstance(value, datetime):
        return value.astimezone(timezone.utc).isoformat()
    if isinstance(value, str) and len(value) <= 64:
        try:
            parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError:
            return None
        if parsed.tzinfo is None:
            parsed = parsed.replace(tzinfo=timezone.utc)
        return parsed.astimezone(timezone.utc).isoformat()
    return None


@router.get("", response_model=ArchitectureStatusResponse)
async def architecture_status(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    monitor: Any = Depends(get_health_monitor),
) -> ArchitectureStatusResponse:
    require_platform_admin(request, auth, Capability.SERVICE_LIST_READ)
    topology = _topology()
    mode, valid_mode = _deployment_mode(topology)
    checked_at = datetime.now(timezone.utc).isoformat()
    gateway_snapshot = await _gateway_dependency_snapshot(request)

    health: dict[str, dict[str, Any]] = {}
    for service_id, status in monitor.all_status().items():
        health[str(service_id)] = {
            "status": _normalized_status(getattr(status, "status", None)),
            "last_check": _timestamp(getattr(status, "last_check", None)),
        }
    core = gateway_snapshot.get("core") if isinstance(gateway_snapshot.get("core"), dict) else {}
    capabilities = (
        gateway_snapshot.get("capabilities")
        if isinstance(gateway_snapshot.get("capabilities"), dict)
        else {}
    )
    for source in (core, capabilities):
        for key, value in source.items():
            health[str(key)] = {"status": _normalized_status(value), "last_check": checked_at}
    health["gateway_core"] = {
        "status": "healthy" if gateway_snapshot.get("core_ready") is True else "unavailable",
        "last_check": checked_at,
    }

    service_rows: dict[str, ArchitectureServiceStatus] = {}
    for service in topology["services"]:
        resolution = service["modes"][mode]
        active = bool(resolution["present"])
        source_key = str(service["health_contract"]["source_key"])
        observed = health.get(source_key, {})
        if service["lifecycle"] == "one-shot":
            status = "one-shot"
        elif service["lifecycle"] in {"external", "optional"} and not active:
            status = "not-applicable"
        elif not active:
            status = "integrated" if service["service_id"] == "knowledge-worker" else "inactive"
        else:
            status = _normalized_status(observed.get("status"))
        service_rows[service["service_id"]] = ArchitectureServiceStatus(
            service_id=service["service_id"],
            display_name=service["display_name"],
            bounded_context=service["bounded_context"],
            responsibility=service["process_role"],
            lifecycle=service["lifecycle"],
            exposure=service["exposure"],
            status=status,
            version=_version(service),
            state_owner=service["state_owner"],
            scale_support=service["scale_support"],
            replicas=int(resolution["replicas"]),
            active_in_mode=active,
            dependencies=[],
            degraded_reasons=[],
            last_check=_timestamp(observed.get("last_check")) if active else None,
        )

    for service in topology["services"]:
        row = service_rows[service["service_id"]]
        dependencies: list[ArchitectureDependency] = []
        reasons: set[str] = set()
        for required, dependency_ids in (
            (True, service["required_deps"]),
            (False, service["optional_deps"]),
        ):
            for dependency_id in dependency_ids:
                dependency = service_rows.get(dependency_id)
                dependency_status = dependency.status if dependency else "unknown"
                dependencies.append(
                    ArchitectureDependency(
                        service_id=dependency_id,
                        required=required,
                        status=dependency_status,
                    )
                )
                if row.active_in_mode and dependency_status in {
                    "degraded",
                    "unavailable",
                    "not_ready",
                    "unknown",
                }:
                    reasons.add(
                        "required_dependency_unavailable"
                        if required
                        else "optional_dependency_degraded"
                    )
        if row.active_in_mode and row.lifecycle == "long-running" and row.status != "healthy":
            reasons.add("health_contract_degraded")
        if row.status == "healthy" and "optional_dependency_degraded" in reasons:
            row.status = "degraded"
        row.dependencies = dependencies
        row.degraded_reasons = sorted(reasons)

    groups = []
    for group in topology["groups"]:
        groups.append(
            ArchitectureGroupStatus(
                group_id=group["id"],
                display_name=group["display_name"],
                services=[
                    service_rows[service["service_id"]]
                    for service in topology["services"]
                    if service["bounded_context"] == group["id"]
                ],
            )
        )
    return ArchitectureStatusResponse(
        schema_version="ai-gateway/architecture-status/v1",
        topology_revision=topology["revision"],
        mode=mode,
        mode_configuration_valid=valid_mode,
        last_check=checked_at,
        groups=groups,
    )
