"""Turn snapshot material: readonly payloads, dynamic tools, allowlists.

ARC-02 split of ``control_plane.py``.  Pure functions that build and validate
the immutable material pinned into a Runtime turn snapshot: the normalized
read-only capability payload, the Codex ``dynamicTools`` projection, the
thread tool fingerprint, signed-AgentSpec capability allowlists, and the
attachment read descriptor.  The facade binds these onto
``AgentRuntimeControlPlane`` preserving their original static/class-method
calling conventions.
"""

from __future__ import annotations

import logging
import re
import uuid
from collections.abc import Callable, Mapping
from hashlib import sha256
from typing import Any

from ai_gateway_contracts.agent_runtime import canonical_runtime_json

from .types import (
    KERNEL_OWNED_AGENT_TOOL_ALIASES,
    AgentRuntimeControlError,
)

logger = logging.getLogger(__name__)
_ARTIFACT_REF = re.compile(r"art_[0-9a-f]{16}\Z")


def runtime_model_config(
    model_plane_base_url: str, model_id: str, *, native_web_search_enabled: bool = False
) -> dict[str, Any]:
    return {
        "model_provider": "ai-platform-gateway",
        "model": model_id,
        # Hosted search is exposed only when the immutable model profile
        # declares its native wire. Provider adapters still own the final
        # request serialization, so unsupported models never receive it.
        "web_search": "live" if native_web_search_enabled else "disabled",
        "features": {
            # The platform Gateway provider exposes Qwen's hosted
            # Responses search directly. Do not replace it with the
            # upstream standalone extension, which has a different auth
            # plane and would bypass the tenant model snapshot.
            "standalone_web_search": False,
            "multi_agent_v2": {
                "enabled": True,
                # The root thread occupies one slot; five child workers
                # remain available without overloading local deployments.
                "max_concurrent_threads_per_session": 6,
            }
        },
        "model_providers": {
            "ai-platform-gateway": {
                "name": "AI Platform Gateway Model Plane",
                "base_url": model_plane_base_url,
                "env_key": "AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN",
                "wire_api": "responses",
                "requires_openai_auth": False,
                "supports_websockets": False,
                "request_max_retries": 0,
                "stream_max_retries": 0,
            }
        },
    }


def dynamic_tools(readonly: dict[str, Any]) -> list[dict[str, Any]]:
    tools = [
        *(readonly.get("tools") or []),
        *(readonly.get("mcp") or []),
        *(readonly.get("deferred") or []),
        *(readonly.get("attachment_tools") or []),
    ]
    if not isinstance(tools, list):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    result: list[dict[str, Any]] = []
    allowlist = readonly.get("capability_allowlist")
    for descriptor in tools:
        if not isinstance(descriptor, dict):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
        if descriptor.get("read_only") is not True and (
            not isinstance(allowlist, list)
            or sum(
                1
                for entry in allowlist
                if isinstance(entry, dict)
                and entry.get("id") == descriptor.get("id")
                and entry.get("name") == descriptor.get("name")
                and entry.get("version") == descriptor.get("version")
                and entry.get("schema_hash") == descriptor.get("schema_hash")
            )
            != 1
        ):
            continue
        name = descriptor.get("name")
        description = descriptor.get("description")
        schema = descriptor.get("schema")
        if (
            not isinstance(name, str)
            or not isinstance(description, str)
            or not isinstance(schema, dict)
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
        if name in KERNEL_OWNED_AGENT_TOOL_ALIASES:
            continue
        result.append(
            {
                "type": "function",
                "name": name,
                "description": description,
                "inputSchema": schema,
            }
        )
    return result


def validate_catalog_descriptor(
    descriptor: Any,
    *,
    tenant_id: str,
    capability_revision: int,
    allow_deferred: bool = False,
) -> dict[str, Any]:
    if (
        not isinstance(descriptor, dict)
        or (
            descriptor.get("read_only") is not True
            and not allow_deferred
        )
        or descriptor.get("tenant_id") != tenant_id
        or descriptor.get("capability_revision") != capability_revision
        or not isinstance(descriptor.get("name"), str)
        or not isinstance(descriptor.get("description"), str)
        or not isinstance(descriptor.get("schema"), dict)
        or not isinstance(descriptor.get("id"), str)
        or not descriptor.get("id")
        or not isinstance(descriptor.get("schema_hash"), str)
        or len(descriptor.get("schema_hash")) != 71
        or not descriptor.get("schema_hash").startswith("sha256:")
        or any(
            character not in "0123456789abcdef"
            for character in descriptor.get("schema_hash")[7:]
        )
        or descriptor.get("kind")
        not in {
            "tool",
            "knowledge",
            "mcp",
            "connector",
            "office_read",
            "platform_tool_discovery",
        }
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_INVALID", status_code=503
        )
    return descriptor


def allowlisted_catalog_descriptors(
    descriptors: list[dict[str, Any]],
    allowlist: list[dict[str, Any]] | None,
    *,
    _logger: logging.Logger = logger,
) -> list[dict[str, Any]]:
    if allowlist is None:
        return descriptors
    allowed: list[dict[str, Any]] = []
    for descriptor in descriptors:
        matches = [
            entry
            for entry in allowlist
            if str(entry.get("id") or "") == str(descriptor.get("id") or "")
        ]
        if len(matches) != 1:
            # The worker publishes its whole registry and does not apply the
            # requested allowlist, so a descriptor the Agent never configured
            # is expected here. Withhold it instead of failing the turn:
            # dropping is at least as closed as rejecting for capability
            # exposure, and rejecting made every Agent preview whose
            # allowlist did not happen to name each worker builtin 409.
            _logger.debug(
                "Withholding capability %r: not named exactly once by the "
                "agent allowlist (%d entries)",
                str(descriptor.get("id") or ""),
                len(allowlist),
            )
            continue
        expected = matches[0]
        for field in ("version", "schema_hash"):
            expected_value = expected.get(field)
            actual_value = descriptor.get(field)
            allow_platform_schema_resolution = (
                field == "schema_hash"
                and expected.get("type") in {"platform", "connector"}
                and expected_value is None
            )
            if actual_value != expected_value and not allow_platform_schema_resolution:
                # An allowlisted capability whose pinned version/schema does
                # not match is tampering or drift, not scoping. Keep failing
                # closed for that.
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_CATALOG_SCOPE_MISMATCH",
                    status_code=409,
                )
        allowed.append(descriptor)
    return allowed


def snapshot_capability_allowlist(
    snapshot: dict[str, Any] | None,
) -> list[dict[str, Any]] | None:
    """Project signed AgentSpec capabilities into a catalog allowlist.

    Connector credentials are deliberately not part of this projection.
    The resolver has already authorized the binding while building the
    Agent snapshot, so only the non-secret principal identity is retained
    for the worker's later, request-time revocation check.  In particular,
    the channel comes from the signed AgentSpec and cannot be overridden by
    connector config.
    """

    if snapshot is None:
        return None
    raw_capabilities = snapshot.get("capabilities")
    if not isinstance(raw_capabilities, list):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
        )
    if not raw_capabilities:
        agent_spec = snapshot.get("agent_spec")
        channel = agent_spec.get("channel") if isinstance(agent_spec, dict) else None
        # The built-in Assistant/Responses surface intentionally inherits the
        # platform catalog.  Treating its empty signed binding list as an
        # explicit deny-list made Thread creation pin the full catalog while
        # the first Turn projected an empty catalog, forcing every live chat
        # to fail with CAPABILITY_THREAD_RECREATE_REQUIRED.  Published Agents
        # keep the closed empty-list meaning.
        if channel == "builtin":
            return None
    allowlist: list[dict[str, Any]] = []
    seen: set[tuple[str, str, str, str]] = set()
    for raw in raw_capabilities:
        if not isinstance(raw, dict):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        capability_type = str(raw.get("type") or "")
        capability_id = str(raw.get("id") or "")
        config = raw.get("config") if isinstance(raw.get("config"), dict) else {}
        name = str(config.get("tool_name") or config.get("name") or capability_id)
        version = str(raw.get("version") or "")
        schema_hash = str(raw.get("schema_hash") or "")
        if not capability_type or not capability_id or not name:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        if not schema_hash and capability_type not in {"platform", "connector"}:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
            )
        key = (capability_type, capability_id, version, schema_hash)
        if key in seen:
            continue
        seen.add(key)
        entry = {
            "type": capability_type,
            "name": name,
            "id": capability_id,
            "version": version or None,
            "schema_hash": schema_hash or None,
        }
        if capability_type == "mcp":
            if not isinstance(raw.get("config"), dict) or set(config) - {
                "connection_id",
                "principal_type",
                "risk",
            }:
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            connection_id = str(config.get("connection_id") or "")
            principal_type = str(config.get("principal_type") or "")
            risk_level = str(config.get("risk") or raw.get("risk") or "")
            agent_spec = snapshot.get("agent_spec")
            channel = agent_spec.get("channel") if isinstance(agent_spec, dict) else None
            try:
                uuid.UUID(connection_id)
            except (ValueError, AttributeError, TypeError):
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                ) from None
            if (
                principal_type not in {"service_account", "user_delegated"}
                or risk_level not in {"low", "medium", "high", "critical"}
                or channel
                not in {"preview", "hosted_private", "hosted_public", "embed", "api"}
                or len(schema_hash) != 71
                or not schema_hash.startswith("sha256:")
            ):
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            entry["connector_binding"] = {
                "binding_type": "grant",
                "provider": "mcp",
                "tool_name": name,
                "principal_type": principal_type,
                "grant_id": None,
                "connection_id": connection_id,
                "schema_hash": schema_hash,
                "risk_level": risk_level,
                "channel": channel,
            }
        elif capability_type == "connector":
            if not isinstance(raw.get("config"), dict):
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            connector_config = raw["config"]
            if set(connector_config) - {
                "provider",
                "tool_name",
                "principal_type",
                "grant_id",
            }:
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            provider = connector_config.get("provider")
            tool_name = connector_config.get("tool_name", name)
            principal_type = connector_config.get("principal_type")
            grant_id = connector_config.get("grant_id")
            agent_spec = snapshot.get("agent_spec")
            channel = agent_spec.get("channel") if isinstance(agent_spec, dict) else None
            if (
                not isinstance(provider, str)
                or not provider
                or len(provider) > 128
                or not isinstance(tool_name, str)
                or tool_name != name
                or not isinstance(channel, str)
                or channel
                not in {
                    "preview",
                    "hosted",
                    "hosted_private",
                    "hosted_public",
                    "embed",
                    "api",
                    "builtin",
                }
            ):
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            if (principal_type is None) != (grant_id is None):
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            if principal_type is not None and (
                principal_type not in {"service_account", "user_delegated"}
                or not isinstance(grant_id, str)
                or not grant_id
            ):
                raise AgentRuntimeControlError(
                    "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                )
            if grant_id is not None:
                try:
                    uuid.UUID(grant_id)
                except (ValueError, AttributeError, TypeError):
                    raise AgentRuntimeControlError(
                        "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_BINDING_INVALID", status_code=409
                    ) from None
            entry["connector_binding"] = {
                "binding_type": "grant" if grant_id is not None else "catalog",
                "provider": provider,
                "tool_name": tool_name,
                "principal_type": principal_type,
                "grant_id": grant_id,
                "channel": channel,
            }
        allowlist.append(entry)
    # Knowledge bindings are authorized separately from generic tool bindings.
    # Project their read capability into both the Thread and Turn catalogs.
    knowledge = snapshot.get("knowledge") or {}
    retrieval = knowledge.get("retrieval") or {}
    if (
        knowledge.get("datasets")
        and retrieval.get("mode", "auto") in {"auto", "tool"}
        and not any(entry["id"] == "search_knowledge_base" for entry in allowlist)
    ):
        allowlist.append({
            "type": "platform",
            "id": "search_knowledge_base",
            "name": "search_knowledge_base",
            "version": None,
            "schema_hash": None,
        })
    return allowlist


def dynamic_tool_fingerprint(readonly: dict[str, Any]) -> str:
    # Attachment refs are turn-scoped data inputs. Their read_attachment
    # descriptor is rebuilt from the normalized refs for each turn and is
    # authorized by the immutable snapshot/Worker lease; pinning that
    # ephemeral descriptor to the Thread would make equivalent attachment
    # normalization spuriously require Thread recreation.
    tools = [
        *(readonly.get("tools") or []),
        *(readonly.get("mcp") or []),
        *(readonly.get("deferred") or []),
    ]
    return sha256(canonical_runtime_json(tools).encode()).hexdigest()


def attachment_tool_descriptor(
    *, tenant_id: str, capability_revision: int, references: list[str]
) -> dict[str, Any]:
    schema = {
        "type": "object",
        "properties": {
            "attachment_id": {"type": "string", "enum": references},
            "operation": {"type": "string", "enum": ["metadata", "content"]},
            "max_chars": {"type": "integer", "minimum": 1, "maximum": 500_000},
        },
        "required": ["attachment_id", "operation", "max_chars"],
        "additionalProperties": False,
    }
    schema_hash = "sha256:" + sha256(canonical_runtime_json(schema).encode()).hexdigest()
    return {
        "name": "read_attachment",
        "description": "Read metadata or bounded content from an explicitly attached artifact ID.",
        "schema": schema,
        "tenant_id": tenant_id,
        "capability_revision": capability_revision,
        "source": "attachments",
        "kind": "tool",
        "category": "retrieval",
        "protocol": "internal",
        "id": "read_attachment",
        "version": "v1",
        "schema_hash": schema_hash,
        "read_only": True,
        "metadata": {"effect": "read", "attachment_refs": references},
    }


def attach_read_attachment_descriptors(
    readonly: dict[str, Any],
    *,
    tenant_id: str,
    capability_revision: int,
    descriptor_factory: Callable[..., dict[str, Any]] = attachment_tool_descriptor,
) -> None:
    refs = [
        str(item["payload"].get("content_ref"))
        for item in readonly.get("items", [])
        if isinstance(item, dict)
        and item.get("kind") == "attachment"
        and isinstance(item.get("payload"), dict)
        and item["payload"].get("content_ref")
    ]
    references = sorted(set(refs))
    # The Worker owns one fixed read_attachment schema/hash. A second dynamic
    # descriptor with a per-turn enum collides with that catalog entry and
    # cannot pass the Worker's descriptor equality check. Bound IDs remain in
    # immutable readonly.items; Runtime checks each tool argument against it.
    if any(not _ARTIFACT_REF.fullmatch(reference) for reference in references):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_ATTACHMENT_INVALID", status_code=422
        )
    readonly["attachment_tools"] = []


def readonly_capability_payload(
    value: dict[str, Any] | None,
    *,
    tenant_id: str,
    capability_revision: int,
) -> dict[str, Any]:
    """Normalize explicit read-only references into the runtime contract.

    This adapter accepts platform-selected references only. It does not
    inspect user text and it never carries a write-capable tool schema.
    """

    raw = value if isinstance(value, dict) else {}
    allowed = {
        "knowledge",
        "attachments",
        "web_search",
        "tools",
        "mcp",
        "deferred",
        "memory_context",
        "capability_allowlist",
        "platform_config",
        "attachment_tools",
        "responses_tool_names",
        "responses_tool_choice",
        "responses_parallel_tool_calls",
    }
    if set(raw) - allowed:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    items: list[dict[str, Any]] = []
    knowledge = raw.get("knowledge")
    if knowledge is not None and not isinstance(knowledge, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    if isinstance(knowledge, dict):
        dataset_ids = knowledge.get("dataset_ids") or []
        if not isinstance(dataset_ids, list) or any(
            not isinstance(item, str) for item in dataset_ids
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
        for dataset_id in dataset_ids:
            items.append(
                {
                    "item_id": f"knowledge:{dataset_id}",
                    "kind": "knowledge",
                    "source": "knowledge",
                    "payload": {"dataset_id": dataset_id},
                    "tenant_id": tenant_id,
                    "capability_revision": capability_revision,
                }
            )
    attachments = raw.get("attachments")
    if attachments is not None and not isinstance(attachments, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    if isinstance(attachments, dict):
        refs = attachments.get("refs") or []
        if not isinstance(refs, list) or any(not isinstance(item, str) for item in refs):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
        for reference in refs:
            items.append(
                {
                    "item_id": f"attachment:{reference}",
                    "kind": "attachment",
                    "source": "attachments",
                    "payload": {"content_ref": reference},
                    "tenant_id": tenant_id,
                    "capability_revision": capability_revision,
                }
            )
    web_search = raw.get("web_search")
    if web_search is not None and not isinstance(web_search, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    if isinstance(web_search, dict) and web_search.get("enabled"):
        max_results = web_search.get("max_results") or 5
        if isinstance(max_results, bool):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
        try:
            max_results = int(max_results)
        except (TypeError, ValueError) as exc:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            ) from exc
        items.append(
            {
                "item_id": "context:web-search",
                "kind": "context",
                "source": "web-search",
                "payload": {"max_results": max_results},
                "tenant_id": tenant_id,
                "capability_revision": capability_revision,
            }
        )
    memory_context = raw.get("memory_context")
    if memory_context is not None:
        if not isinstance(memory_context, dict):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_MEMORY_CONTEXT_INVALID", status_code=503
            )
        items.append(
            {
                "item_id": "context:long-term-memory",
                "kind": "context",
                "source": "long-term-memory",
                "payload": memory_context,
                "tenant_id": tenant_id,
                "capability_revision": capability_revision,
            }
        )
    tools = raw.get("tools") or []
    mcp = raw.get("mcp") or []
    if not isinstance(tools, list) or not isinstance(mcp, list):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    deferred = raw.get("deferred") or []
    if not isinstance(deferred, list):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    for descriptor in [*tools, *mcp]:
        if (
            not isinstance(descriptor, dict)
            or descriptor.get("read_only") is not True
            or descriptor.get("tenant_id") != tenant_id
            or int(descriptor.get("capability_revision") or 0) != capability_revision
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
    attachment_tools = raw.get("attachment_tools") or []
    if not isinstance(attachment_tools, list):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    for descriptor in attachment_tools:
        if (
            not isinstance(descriptor, dict)
            or descriptor.get("read_only") is not True
            or descriptor.get("tenant_id") != tenant_id
            or int(descriptor.get("capability_revision") or 0) != capability_revision
            or descriptor.get("name") != "read_attachment"
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
    for descriptor in deferred:
        if (
            not isinstance(descriptor, dict)
            or descriptor.get("read_only") is True
            or descriptor.get("tenant_id") != tenant_id
            or int(descriptor.get("capability_revision") or 0) != capability_revision
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
            )
    responses_tool_names = raw.get("responses_tool_names")
    if responses_tool_names is not None and (
        not isinstance(responses_tool_names, list)
        or len(responses_tool_names) > 128
        or any(not isinstance(name, str) or not name.strip() for name in responses_tool_names)
        or len(set(responses_tool_names)) != len(responses_tool_names)
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    responses_tool_choice = raw.get("responses_tool_choice", "auto")
    if not (
        isinstance(responses_tool_choice, str)
        and responses_tool_choice in {"auto", "none", "required"}
    ) and not (
        isinstance(responses_tool_choice, dict)
        and responses_tool_choice.get("type") == "function"
        and isinstance(responses_tool_choice.get("name"), str)
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    responses_parallel_tool_calls = raw.get("responses_parallel_tool_calls", True)
    if not isinstance(responses_parallel_tool_calls, bool):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    capability_allowlist = raw.get("capability_allowlist")
    if capability_allowlist is not None and (
        not isinstance(capability_allowlist, list)
        or any(not isinstance(item, dict) for item in capability_allowlist)
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_READONLY_PAYLOAD_INVALID", status_code=400
        )
    normalized = {
        "schema_version": "agent-readonly-capability/v1",
        "tenant_id": tenant_id,
        "capability_revision": capability_revision,
        "items": items,
        "tools": tools,
        "mcp": mcp,
        "deferred": deferred,
        "attachment_tools": attachment_tools,
        "responses_tool_names": responses_tool_names,
        "responses_tool_choice": responses_tool_choice,
        "responses_parallel_tool_calls": responses_parallel_tool_calls,
    }
    platform_config = raw.get("platform_config")
    if platform_config is not None:
        if not isinstance(platform_config, dict):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_PLATFORM_CONFIG_INVALID", status_code=409
            )
        normalized["platform_config"] = platform_config
    if capability_allowlist is not None:
        normalized["capability_allowlist"] = capability_allowlist
    return normalized


def turn_prompt_readonly(readonly: Mapping[str, Any]) -> dict[str, Any]:
    """Turn HTTP payload is Codex additional_context material.

    Knowledge, memory, attachments, and web-search items belong here.
    Tool catalogs stay on the snapshot and on thread ``dynamicTools``;
    they are never turn input.
    """
    payload = {
        "schema_version": readonly["schema_version"],
        "tenant_id": readonly["tenant_id"],
        "capability_revision": readonly["capability_revision"],
        "items": list(readonly.get("items") or []),
        "tools": [],
        "mcp": [],
        "deferred": [],
        "attachment_tools": [],
    }
    platform_config = readonly.get("platform_config")
    if platform_config is not None:
        payload["platform_config"] = platform_config
    return payload


__all__ = [
    "allowlisted_catalog_descriptors",
    "attach_read_attachment_descriptors",
    "attachment_tool_descriptor",
    "dynamic_tool_fingerprint",
    "dynamic_tools",
    "readonly_capability_payload",
    "runtime_model_config",
    "snapshot_capability_allowlist",
    "turn_prompt_readonly",
    "validate_catalog_descriptor",
]
