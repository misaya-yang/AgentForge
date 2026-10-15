"""Capability catalog fetch and validation for Runtime threads.

ARC-02 split of ``control_plane.py``.  This module owns the control-plane
side of the capability preflight: fetching the tenant-scoped read-only
catalog, applying the worker write gate, and pinning the exact live
descriptor allowlist into the turn's readonly payload.
"""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, Any

from ..capability_catalog import (
    RUNTIME_CATALOG_SCHEMA_VERSION,
    CapabilityCatalogError,
    CapabilityCatalogQuery,
)
from .types import DISCOVERY_BRIDGE_NAMES, AgentRuntimeControlError

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane


def worker_ready_for_writes() -> bool:
    return (
        os.getenv("AI_PLATFORM_CAPABILITY_WORKER_ENABLED", "").lower() == "true"
        and os.getenv("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED", "").lower() == "true"
        and bool(os.getenv("AI_PLATFORM_CAPABILITY_WORKER_URL", "").strip())
        and bool(os.getenv("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET", "").strip())
    )


async def fetch_capability_catalog(
    plane: AgentRuntimeControlPlane,
    readonly: dict[str, Any],
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    model_id: str,
    capability_revision: int,
    capability_allowlist: list[dict[str, Any]] | None = None,
) -> None:
    """Fetch stable read-only schemas before the first Thread is created."""

    if capability_allowlist == [] and not readonly.get("attachment_tools"):
        # A signed, explicitly empty Agent binding has no catalog dependency.
        # In particular an anonymous model-only publication must not inherit
        # discovery bridges or request an authenticated user's tool catalog.
        readonly.update(tools=[], mcp=[], deferred=[], capability_allowlist=[], attachment_tools=[])
        readonly["_thread_capabilities"] = {
            **{key: value for key, value in readonly.items() if key != "_thread_capabilities"},
            "responses_tool_names": None,
        }
        return

    try:
        query = CapabilityCatalogQuery.create(
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            capability_revision=capability_revision,
            capability_allowlist=capability_allowlist,
        )
        payload = await plane.capability_catalog_client.fetch_catalog(query)
    except CapabilityCatalogError as exc:
        code = (
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_DEGRADED"
            if exc.code == "CAPABILITY_CATALOG_DEGRADED"
            else "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_UNAVAILABLE"
        )
        raise AgentRuntimeControlError(code, status_code=503) from exc
    if not isinstance(payload, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_INVALID", status_code=503
        )
    tools = payload.get("tools")
    mcp = payload.get("mcp", [])
    deferred = payload.get("deferred", [])
    if (
        (
            payload.get("schema_version") is not None
            and payload.get("schema_version") != RUNTIME_CATALOG_SCHEMA_VERSION
        )
        or (
            payload.get("capability_revision") is not None
            and payload.get("capability_revision") != capability_revision
        )
        or not isinstance(tools, list)
        or not isinstance(mcp, list)
        or not isinstance(deferred, list)
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_INVALID", status_code=503
        )
    attachment_tools = readonly.get("attachment_tools") or []
    if not isinstance(attachment_tools, list):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_INVALID", status_code=503
        )
    if deferred and not plane._worker_ready_for_writes():
        if capability_allowlist is not None:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_WRITE_CAPABILITY_NOT_MIGRATED",
                status_code=409,
            )
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_WRITE_CAPABILITY_DEGRADED",
            status_code=503,
        )
    readonly["tools"] = [
        plane._validate_catalog_descriptor(
            descriptor,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
        )
        for descriptor in tools
    ]
    readonly["mcp"] = [
        plane._validate_catalog_descriptor(
            descriptor,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
        )
        for descriptor in mcp
    ]
    deferred = [
        plane._validate_catalog_descriptor(
            descriptor,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
            allow_deferred=True,
        )
        for descriptor in deferred
    ]
    # Thread identity binds the authenticated catalog maximum, while each
    # turn can choose a subset. Keep this transient and out of prompt/snapshot.
    readonly["_thread_capabilities"] = {
        **readonly,
        "tools": plane._allowlisted_catalog_descriptors(readonly["tools"], capability_allowlist),
        "mcp": plane._allowlisted_catalog_descriptors(readonly["mcp"], capability_allowlist),
        "deferred": plane._allowlisted_catalog_descriptors(deferred, capability_allowlist)
        if capability_allowlist is not None else deferred,
        "responses_tool_names": None,
    }
    # Keep the catalog maximum pinned at Thread creation, but expose the
    # read_attachment tool only on turns that explicitly select an artifact.
    has_attachment = any(
        isinstance(item, dict) and item.get("kind") == "attachment"
        for item in readonly.get("items", [])
    )
    if not has_attachment:
        tools = [item for item in tools if item.get("name") != "read_attachment"]
    requested_tool_names = readonly.get("responses_tool_names")
    if requested_tool_names is not None:
        catalog_by_name: dict[str, list[tuple[str, dict[str, Any]]]] = {}
        for kind, descriptors in (
            ("tools", tools),
            ("mcp", mcp),
            ("deferred", deferred),
        ):
            for descriptor in descriptors:
                catalog_by_name.setdefault(str(descriptor["name"]), []).append(
                    (kind, descriptor)
                )
        selected_tools: list[dict[str, Any]] = []
        selected_mcp: list[dict[str, Any]] = []
        for name in requested_tool_names:
            matches = catalog_by_name.get(name, [])
            if len(matches) != 1:
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_RESPONSE_TOOL_NOT_FOUND", status_code=400
                )
            kind, descriptor = matches[0]
            if descriptor.get("read_only") is not True:
                # Public Responses cannot mint a write-capable descriptor;
                # those require a signed AgentSpec and approval contract.
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_RESPONSE_TOOL_NOT_READONLY", status_code=409
                )
            if kind == "tools":
                selected_tools.append(descriptor)
            else:
                selected_mcp.append(descriptor)
        tools = selected_tools
        mcp = selected_mcp
        deferred = []
        bridges: list[dict[str, Any]] = []
    else:
        bridges = [item for item in readonly["tools"] if item.get("name") in DISCOVERY_BRIDGE_NAMES]
    bound_tools = [
        item for item in tools if item.get("name") not in DISCOVERY_BRIDGE_NAMES
    ]
    allowed_tools = plane._allowlisted_catalog_descriptors(
        bound_tools, capability_allowlist
    )
    allowed_mcp = plane._allowlisted_catalog_descriptors(mcp, capability_allowlist)
    allowed_deferred = (
        plane._allowlisted_catalog_descriptors(deferred, capability_allowlist)
        if capability_allowlist is not None
        else deferred
    )
    readonly["tools"] = allowed_tools + bridges
    readonly["mcp"] = allowed_mcp
    readonly["deferred"] = allowed_deferred
    if capability_allowlist is not None and (
        len(allowed_tools) + len(allowed_mcp) + len(allowed_deferred)
        != len(capability_allowlist)
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_SCOPE_MISMATCH",
            status_code=409,
        )
    # Every turn, including the generic Assistant without an AgentSpec,
    # carries an exact live descriptor allowlist. Preserve any signed
    # connector binding while replacing only version/schema with the
    # values from the live catalog (a connector may publish without a
    # schema hash and receive it here).
    live_descriptors = [
        *allowed_tools,
        *allowed_mcp,
        *allowed_deferred,
        *bridges,
        *attachment_tools,
    ]
    thread_capabilities = readonly["_thread_capabilities"]
    thread_capabilities["capability_allowlist"] = [
        {"id": item["id"], "name": item["name"], "version": item.get("version"), "schema_hash": item.get("schema_hash")}
        for kind in ("tools", "mcp", "deferred") for item in thread_capabilities[kind]
    ]
    final_allowlist: list[dict[str, Any]] = []
    for item in live_descriptors:
        matches = [
            entry
            for entry in (capability_allowlist or [])
            if str(entry.get("id") or "") == str(item.get("id") or "")
        ]
        if len(matches) > 1:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_SCOPE_MISMATCH",
                status_code=409,
            )
        entry = dict(matches[0]) if matches else {}
        entry.update(
            {
                # Capability descriptors bind to their runtime kind.  A
                # read_attachment descriptor is an internal tool even
                # though it is implicit in the signed attachment refs.
                "type": str(item["kind"]),
                "name": str(item["name"]),
                "id": str(item["id"]),
                "version": item.get("version"),
                "schema_hash": item.get("schema_hash"),
            }
        )
        final_allowlist.append(entry)
    readonly["capability_allowlist"] = final_allowlist
    readonly["attachment_tools"] = attachment_tools


__all__ = ["fetch_capability_catalog", "worker_ready_for_writes"]
