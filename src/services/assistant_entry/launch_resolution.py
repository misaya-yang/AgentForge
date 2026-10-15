"""Gateway-owned policy resolution into one immutable Agent launch contract."""

from __future__ import annotations

import copy
import inspect
from collections.abc import Mapping
from typing import Any

from ai_gateway_contracts.agent_launch import (
    RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION,
    ResolvedAgentLaunchError,
    ResolvedAgentLaunchV1,
)
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_core.agents.system_prompt import GENERIC_AGENT_INSTRUCTIONS

from ..llm.model_service import ModelProviderAmbiguous


class AgentLaunchResolutionError(RuntimeError):
    def __init__(self, code: str, *, status_code: int = 503) -> None:
        self.code = code
        self.status_code = status_code
        super().__init__(code)


async def _model_binding(
    *,
    tenant_id: str,
    model_id: str,
    model_service: Any | None,
    provider_id: str | None,
    model_profile: Mapping[str, Any] | None,
    expected_model_ref: Mapping[str, Any] | None = None,
) -> tuple[str, dict[str, Any]]:
    """Resolve the exact enabled model/profile without knowing an HTTP request."""

    provider = str(provider_id or "").strip()
    profile = dict(model_profile or {})
    if model_service is not None and callable(getattr(model_service, "get_model", None)):
        try:
            result = (
                model_service.get_model(tenant_id, model_id, provider_id=provider)
                if provider else model_service.get_model(tenant_id, model_id)
            )
            row = await result if inspect.isawaitable(result) else result
        except ModelProviderAmbiguous as exc:
            raise AgentLaunchResolutionError("MODEL_PROVIDER_REQUIRED", status_code=409) from exc
        except Exception as exc:  # noqa: BLE001 - policy uncertainty is deny
            raise AgentLaunchResolutionError("AGENT_RUNTIME_MODEL_UNAVAILABLE") from exc
        if not isinstance(row, dict) or not bool(row.get("is_enabled", True)):
            raise AgentLaunchResolutionError("AGENT_RUNTIME_MODEL_UNAVAILABLE")
        resolved_id = str(row.get("model_id") or row.get("id") or model_id)
        resolved_provider = str(row.get("provider_id") or row.get("provider") or "")
        resolved_profile = row.get("effective_capabilities")
        if (
            resolved_id != model_id
            or not resolved_provider
            or not isinstance(resolved_profile, dict)
            or (provider and provider != resolved_provider)
            or (profile and profile != resolved_profile)
        ):
            raise AgentLaunchResolutionError(
                "AGENT_RUNTIME_MODEL_MISMATCH", status_code=409
            )
        if expected_model_ref is not None:
            receipt = row.get("pricing_snapshot") or {}
            if any(expected_model_ref.get(key) != value for key, value in (
                ("tenant_id", tenant_id), ("model_id", resolved_id), ("provider_id", resolved_provider),
                ("capability_revision", int(row.get("capability_revision") or 1)),
                ("price_version", receipt.get("version")),
            )):
                raise AgentLaunchResolutionError("EVAL_MODEL_REF_CHANGED", status_code=409)
        provider = resolved_provider
        profile = dict(resolved_profile)
    if not provider:
        # Lightweight route fixtures may replace ControlPlane with a recorder.
        # Production always supplies its model service and never reaches this
        # compatibility identity.
        provider = "gateway-resolved"
    return provider, profile


def _builtin_knowledge(readonly: Mapping[str, Any]) -> dict[str, Any]:
    raw = readonly.get("knowledge")
    raw = raw if isinstance(raw, Mapping) else {}
    datasets = sorted(
        {
            str(item)
            for item in raw.get("dataset_ids", [])
            if isinstance(item, str) and item
        }
    )
    return {
        "datasets": datasets,
        "retrieval": {
            "mode": str(raw.get("mode") or "off"),
            "top_k": int(raw.get("top_k") or 5),
            "threshold": float(raw.get("score_threshold") or 0.4),
        },
    }


def _turn_policy(
    *,
    reasoning_option: str | None,
    legacy_thinking_level: str | None,
    max_tokens: int | None,
    temperature: float | None,
    style_guidance: str | None,
    memory_mode: str,
    memory_profile: str | None,
    enable_dynamic_tools: bool,
) -> dict[str, Any]:
    return {
        "reasoning_option": reasoning_option,
        "legacy_thinking_level": legacy_thinking_level,
        "max_tokens": max_tokens,
        "temperature": temperature,
        "style_guidance": style_guidance,
        "memory_mode": str(memory_mode or "auto").strip().lower(),
        "memory_profile": str(memory_profile or "basic").strip().lower(),
        "enable_dynamic_tools": enable_dynamic_tools,
    }


async def resolve_agent_launch(
    *,
    entrypoint: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    model_id: str,
    model_service: Any | None,
    provider_id: str | None = None,
    expected_model_ref: Mapping[str, Any] | None = None,
    readonly_capabilities: Mapping[str, Any] | None = None,
    reasoning_option: str | None = None,
    legacy_thinking_level: str | None = None,
    max_tokens: int | None = None,
    temperature: float | None = None,
    developer_instructions: str | None = None,
    style_guidance: str | None = None,
    memory_mode: str = "auto",
    memory_profile: str | None = None,
    enable_dynamic_tools: bool = True,
    legacy_snapshot: Mapping[str, Any] | None = None,
    draft_revision: int | None = None,
) -> ResolvedAgentLaunchV1:
    """Resolve every Gateway entry through the same closed policy boundary."""

    readonly = dict(readonly_capabilities or {})
    policy = _turn_policy(
        reasoning_option=reasoning_option,
        legacy_thinking_level=legacy_thinking_level,
        max_tokens=max_tokens,
        temperature=temperature,
        style_guidance=style_guidance,
        memory_mode=memory_mode,
        memory_profile=memory_profile,
        enable_dynamic_tools=enable_dynamic_tools,
    )
    if legacy_snapshot is not None:
        snapshot = copy.deepcopy(dict(legacy_snapshot))
        snapshot_model = snapshot.get("model")
        if not isinstance(snapshot_model, dict) or str(snapshot_model.get("id") or "") != model_id:
            raise AgentLaunchResolutionError(
                "RESOLVED_AGENT_LAUNCH_MODEL_MISMATCH", status_code=409
            )
        provider, profile = await _model_binding(
            tenant_id=tenant_id,
            model_id=model_id,
            model_service=model_service,
            provider_id=str(snapshot_model.get("provider") or ""),
            model_profile=(
                snapshot.get("model_profile")
                if isinstance(snapshot.get("model_profile"), dict)
                else None
            ),
        )
        snapshot = {**snapshot, "model_profile": profile}
        agent_spec = snapshot.get("agent_spec")
        if isinstance(agent_spec, dict) and not str(
            agent_spec.get("developerInstructions") or ""
        ).strip():
            agent_spec["developerInstructions"] = GENERIC_AGENT_INSTRUCTIONS
        if provider != str(snapshot_model.get("provider") or ""):
            raise AgentLaunchResolutionError(
                "RESOLVED_AGENT_LAUNCH_MODEL_MISMATCH", status_code=409
            )
        legacy_memory = snapshot.get("memory")
        legacy_memory = legacy_memory if isinstance(legacy_memory, dict) else {}
        policy["memory_mode"] = str(legacy_memory.get("mode") or "session")
        policy["memory_profile"] = "basic"
        policy["draft_revision"] = draft_revision
        try:
            return ResolvedAgentLaunchV1.from_legacy_snapshot(
                snapshot,
                user_id=user_id,
                session_id=session_id,
                entrypoint=entrypoint,
                model_profile=profile,
                readonly_capabilities=readonly,
                turn_policy=policy,
            )
        except ResolvedAgentLaunchError as exc:
            raise AgentLaunchResolutionError(exc.code, status_code=409) from exc

    provider, profile = await _model_binding(
        tenant_id=tenant_id,
        model_id=model_id,
        model_service=model_service,
        provider_id=provider_id,
        model_profile=None,
        expected_model_ref=expected_model_ref,
    )
    instructions = str(developer_instructions or GENERIC_AGENT_INSTRUCTIONS).strip()
    if style_guidance:
        instructions = f"{instructions.rstrip()}\n\n{style_guidance.strip()}"
    parameters = {
        **({"temperature": temperature} if temperature is not None else {}),
        **({"max_tokens": max_tokens} if max_tokens is not None else {}),
        **(
            {"thinking_mode": legacy_thinking_level}
            if legacy_thinking_level is not None
            else {}
        ),
    }
    model = {"id": model_id, "provider": provider, "parameters": parameters}
    knowledge = _builtin_knowledge(readonly)
    agent_memory_mode = (
        "off"
        if policy["memory_mode"] == "off"
        else "user"
        if policy["memory_mode"] == "user"
        else "session"
    )
    capabilities: list[dict[str, Any]] = []
    agent_spec = {
        "agentId": "__builtin_assistant__",
        "agentVersionId": None,
        "channel": "builtin",
        "developerInstructions": instructions,
        "model": model,
        "knowledge": knowledge,
        "capabilities": capabilities,
        "memory": {"mode": agent_memory_mode},
    }
    fingerprints = {
        "spec": runtime_sha256(agent_spec),
        "tool_schema": runtime_sha256(capabilities),
        "skills": runtime_sha256([]),
        "knowledge_revision": runtime_sha256(knowledge),
    }
    payload = {
        "schema_version": RESOLVED_AGENT_LAUNCH_SCHEMA_VERSION,
        "identity": {
            "tenant_id": tenant_id,
            "user_id": user_id,
            "session_id": session_id,
            "agent_id": "__builtin_assistant__",
            "agent_version_id": None,
            "draft_revision": None,
            "publication_id": None,
            "channel": "builtin",
            "entrypoint": entrypoint,
            "auth_mode": "private",
        },
        "agent_spec": agent_spec,
        "model": model,
        "model_profile": profile,
        "capability_bindings": capabilities,
        "knowledge_bindings": knowledge,
        "memory_policy": {
            "mode": policy["memory_mode"],
            "profile": policy["memory_profile"],
        },
        "channel_policy": {
            "attachments": True,
            "high_risk_tools": True,
            "allowed_origins": [],
        },
        "runtime_inputs": {"readonly_capabilities": readonly},
        "turn_policy": policy,
        "fingerprints": fingerprints,
    }
    try:
        return ResolvedAgentLaunchV1.parse(payload)
    except ResolvedAgentLaunchError as exc:
        raise AgentLaunchResolutionError(exc.code, status_code=409) from exc


__all__ = [
    "AgentLaunchResolutionError",
    "ResolvedAgentLaunchV1",
    "resolve_agent_launch",
]
