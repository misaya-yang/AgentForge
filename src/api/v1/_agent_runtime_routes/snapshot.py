"""Immutable runtime snapshot assembly for Agent Studio Runtime.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; the facade
keeps time-limited re-exports for pre-split import paths.
"""

from __future__ import annotations

from typing import Any

from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.agents.spec import render_agent_outcome_contract
from ai_gateway_core.logging import get_logger
from fastapi import Request

from ....core.auth.user_resolver import UserContext
from .core import _prefixed_hash, _raise_runtime_error
from .resolution import (
    _channel_policy,
    _confirmation_stamp,
    _effective_capabilities,
    _effective_knowledge,
    _resolved_model,
    _runtime_knowledge_config,
)

logger = get_logger(__name__)

# High-risk platform capabilities whose snapshot binding carries no
# confirmation pin.  This map only de-duplicates a one-shot warning per
# capability id; it never feeds an authorization decision.  It is bounded
# (FIFO eviction past ``_UNPINNED_WARNING_CAP``) because capability ids
# arrive via publication bindings and must not grow this process-local map
# without limit; eviction can at most repeat a warning.
_UNPINNED_WARNING_CAP = 256
_unpinned_high_risk_platform: dict[str, None] = {}


def _assert_attachments_allowed(
    request: Request,
    snapshot: dict[str, Any],
    attachments: list[Any],
) -> None:
    if attachments and not snapshot["channel_policy"]["attachments"]:
        _raise_runtime_error(
            request,
            422,
            "AGENT_RUNTIME_ATTACHMENTS_FORBIDDEN",
            "Attachments are disabled for this Agent channel",
        )


async def _build_snapshot(
    request: Request,
    resolution: dict[str, Any],
    user: UserContext,
    *,
    channel: str,
) -> dict[str, Any]:
    spec = resolution["spec"]
    model = await _resolved_model(request, resolution, user)
    instructions = str(spec.get("instructions") or "")
    outcome_contract = render_agent_outcome_contract(spec)
    if outcome_contract:
        instructions = f"{outcome_contract}\n\n[Detailed instructions]\n{instructions}"
    publication = resolution.get("publication") or {}
    version = resolution.get("version") or {}
    draft = resolution.get("draft") or {}
    policy = _channel_policy(resolution, channel=channel)
    effective = await _effective_capabilities(
        request,
        resolution,
        user,
        channel=channel,
        channel_policy=policy,
    )
    bound_skill_keys = {
        (
            str(item.get("resource_id") or item.get("id") or ""),
            str(item.get("resource_version") or item.get("version") or ""),
        )
        for item in resolution.get("capabilities") or []
        if str(item.get("capability_type") or item.get("type") or "") == "skill"
    }
    effective_skill_keys = {
        (
            str(item.get("resource_id") or item.get("id") or ""),
            str(item.get("resource_version") or item.get("version") or ""),
        )
        for item in effective
        if str(item.get("capability_type") or item.get("type") or "") == "skill"
    }
    if bound_skill_keys != effective_skill_keys:
        _raise_runtime_error(
            request,
            422 if channel == "preview" else 409,
            "AGENT_SKILL_UNAVAILABLE",
            "A bound Skill version is unavailable",
        )

    capabilities: list[dict[str, Any]] = []
    for binding in effective:
        raw_type = str(binding.get("capability_type") or binding.get("type") or "")
        if raw_type == "knowledge":
            continue
        runtime_type = "platform" if raw_type in {"native", "model_native"} else raw_type
        if runtime_type not in {"platform", "mcp", "skill", "connector"}:
            continue
        schema_hash = binding.get("schema_hash")
        binding_config = binding.get("config")
        binding_config = dict(binding_config) if isinstance(binding_config, dict) else {}
        risk = str(
            binding.get("risk")
            or binding_config.get("risk")
            or ("low" if runtime_type == "platform" else "high")
        )
        if risk not in {"low", "medium", "high", "critical"}:
            continue
        if risk in {"high", "critical"} and not policy["high_risk_tools"]:
            continue
        binding_config = _confirmation_stamp(
            binding_config,
            risk=risk,
            runtime_type=runtime_type,
            definition=None,  # gateway cannot resolve assistant tool definitions
        )
        if (
            runtime_type == "platform"
            and risk in {"high", "critical"}
            and "requires_confirmation" not in binding_config
        ):
            capability_id = str(binding.get("resource_id") or binding.get("id") or "")
            if capability_id and capability_id not in _unpinned_high_risk_platform:
                if len(_unpinned_high_risk_platform) >= _UNPINNED_WARNING_CAP:
                    _unpinned_high_risk_platform.pop(next(iter(_unpinned_high_risk_platform)))
                _unpinned_high_risk_platform[capability_id] = None
                logger.warning(
                    "High-risk platform capability bound without a confirmation pin "
                    "(capability_id=%s); enforcement stays fail-closed at runtime",
                    capability_id,
                )
        if not user.is_authenticated and channel in {"hosted", "embed"}:
            mutating = bool(
                binding_config.get("write")
                or binding_config.get("mutating")
                or binding_config.get("side_effects") in {"write", "external", "destructive"}
            )
            if mutating or risk in {"high", "critical"}:
                continue
        capabilities.append(
            {
                "type": runtime_type,
                "id": str(binding.get("resource_id") or binding.get("id") or ""),
                "version": binding.get("resource_version") or binding.get("version"),
                "schema_hash": _prefixed_hash(schema_hash) if schema_hash else None,
                "risk": risk,
                "config": dict(binding_config),
            }
        )
    capabilities.sort(key=lambda item: (item["type"], item["id"]))

    knowledge_rows = await _effective_knowledge(
        request,
        resolution,
        user,
        channel=channel,
    )
    bound_dataset_ids = {
        str(item.get("dataset_id") or "")
        for item in resolution.get("knowledge") or []
        if isinstance(item, dict) and item.get("dataset_id")
    }
    effective_dataset_ids = {
        str(item.get("dataset_id") or "")
        for item in knowledge_rows
        if isinstance(item, dict) and item.get("dataset_id")
    }
    if bound_dataset_ids != effective_dataset_ids:
        _raise_runtime_error(
            request,
            422 if channel == "preview" else 409,
            "AGENT_KNOWLEDGE_UNAVAILABLE",
            "A bound Knowledge Dataset is unavailable",
        )
    dataset_ids = sorted(
        {
            str(item.get("dataset_id"))
            for item in knowledge_rows
            if isinstance(item, dict) and item.get("dataset_id")
        }
    )
    configs_by_dataset = {
        str(item["dataset_id"]): _runtime_knowledge_config(
            request,
            item.get("retrieval_config"),
            channel=channel,
        )
        for item in knowledge_rows
        if isinstance(item, dict) and item.get("dataset_id")
    }
    modes = {config["mode"] for config in configs_by_dataset.values()}
    aggregate_mode = (
        "auto" if "auto" in modes else "tool" if "tool" in modes else "off" if modes else "auto"
    )
    retrieval: dict[str, Any] = {
        "mode": aggregate_mode,
        "top_k": max(
            (config["top_k"] for config in configs_by_dataset.values()),
            default=5,
        ),
        "threshold": min(
            (config["threshold"] for config in configs_by_dataset.values()),
            default=0.4,
        ),
        "include_images": any(config["include_images"] for config in configs_by_dataset.values()),
        "config_scope": "per_dataset",
        "by_dataset": {
            dataset_id: configs_by_dataset[dataset_id] for dataset_id in sorted(configs_by_dataset)
        },
    }
    retrieval["provenance"] = [
        {
            "dataset_id": dataset_id,
            "content_mode": "live_latest",
            "historical_replayable": False,
            "revision_source": "assistant_run_catalog",
        }
        for dataset_id in dataset_ids
    ]
    retrieval["replayability"] = "live_content_provenance_only"

    spec_hash = _prefixed_hash(version.get("spec_hash") or draft.get("spec_hash"))
    agent_version_id = version.get("agent_version_id") or None
    publication_id = publication.get("publication_id") or None
    memory_raw = spec.get("memory") if isinstance(spec.get("memory"), dict) else {}
    memory_mode = str(memory_raw.get("mode") or "session")
    if memory_mode not in {"off", "session", "user"}:
        memory_mode = "session"
    if not user.is_authenticated and channel in {"hosted", "embed"}:
        memory_mode = "session"
    agent_spec = {
        "agentId": str(resolution["agent"]["agent_id"]),
        "agentVersionId": str(agent_version_id) if agent_version_id else None,
        "channel": channel,
        "developerInstructions": instructions,
        "model": {
            "id": str(model["id"]),
            "provider": str(model["provider"]),
            "parameters": dict(model["parameters"]),
        },
        "knowledge": {"datasets": dataset_ids, "retrieval": retrieval},
        "capabilities": capabilities,
        "memory": {"mode": memory_mode},
    }
    return {
        "schema_version": "agent-runtime/v1",
        "tenant_id": str(resolution["agent"]["tenant_id"]),
        "agent_id": str(resolution["agent"]["agent_id"]),
        "agent_version_id": str(agent_version_id) if agent_version_id else None,
        "publication": {
            "id": str(publication_id) if publication_id else None,
            "channel": channel,
            "auth_mode": str(publication.get("auth_mode") or "private"),
        },
        "model": {
            "id": str(model["id"]),
            "provider": str(model["provider"]),
            "parameters": dict(model["parameters"]),
        },
        "instructions": {
            "agent": instructions,
            "developerInstructions": instructions,
            "prompt_hash": runtime_sha256(instructions),
        },
        "agent_spec": agent_spec,
        "capabilities": capabilities,
        "knowledge": {"datasets": dataset_ids, "retrieval": retrieval},
        "memory": {"mode": memory_mode},
        "channel_policy": policy,
        "fingerprints": {
            "spec": spec_hash,
            "tool_schema": runtime_sha256(capabilities),
            "skills": runtime_sha256([item for item in capabilities if item["type"] == "skill"]),
            "knowledge_revision": runtime_sha256({"datasets": dataset_ids, "retrieval": retrieval}),
        },
    }
