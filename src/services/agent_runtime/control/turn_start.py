"""Turn-start orchestration extracted from the thread lifecycle facade."""

from __future__ import annotations

import logging
import secrets
import uuid
from collections.abc import Callable
from datetime import datetime, timedelta, timezone
from hashlib import sha256
from typing import TYPE_CHECKING, Any

from ai_gateway_contracts.agent_launch import (
    ResolvedAgentLaunchError,
    ResolvedAgentLaunchV1,
)
from ai_gateway_contracts.agent_runtime import canonical_runtime_json
from ai_gateway_contracts.agent_runtime_lease import (
    RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
    RuntimeModelLeaseClaims,
)
from ai_gateway_core.models import resolve_reasoning_option

from ..runtime_configuration import (
    RuntimePlatformConfigError,
    build_runtime_platform_config,
    runtime_platform_config_hash,
)
from .capacity import note_dispatch
from .http_headers import runtime_headers
from .tool_policy import runtime_tool_policy
from .types import (
    BASE_AGENT_INSTRUCTIONS_V1,
    GENERIC_AGENT_INSTRUCTIONS_V1,
    AgentRuntimeControlError,
    AgentTurn,
    _provider_revision,
)

if TYPE_CHECKING:
    from ..control_plane import AgentRuntimeControlPlane

logger = logging.getLogger(__name__)

_DEFAULT_TURN_OUTPUT_TOKENS = 32_768


def _resolve_output_limit(effective_max_tokens: int | None, model_max_output_tokens: Any) -> int:
    """Resolve one model call's output budget without reserving the model maximum.

    A model's maximum is a capability ceiling, not a sensible default for each
    step in a multi-call Web turn. Reserving that ceiling repeatedly exhausts
    the lease before the model can synthesize its final answer.
    """

    model_limit = max(1, int(model_max_output_tokens or 4_096))
    requested = (
        effective_max_tokens
        if effective_max_tokens is not None
        else min(_DEFAULT_TURN_OUTPUT_TOKENS, model_limit)
    )
    return max(1, min(int(requested), model_limit))


async def start_turn(
    plane: AgentRuntimeControlPlane,
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    message: str,
    image_inputs: list[str] | None = None,
    model_id: str,
    reasoning_option: str | None,
    legacy_thinking_level: str | None,
    max_tokens: int | None,
    temperature: float | None = None,
    readonly_capabilities: dict[str, Any] | None = None,
    resolved_agent_snapshot: dict[str, Any] | None = None,
    resolved_agent_launch: ResolvedAgentLaunchV1 | None = None,
    developer_instructions: str | None = None,
    style_guidance: str | None = None,
    memory_mode: str = "auto",
    memory_profile: str | None = None,
    enable_dynamic_tools: bool = True,
    _logger: logging.Logger = logger,
    _provider_revision_func: Callable[[Any], str] = _provider_revision,
) -> AgentTurn:
    if resolved_agent_launch is not None:
        try:
            launch = (
                resolved_agent_launch
                if isinstance(resolved_agent_launch, ResolvedAgentLaunchV1)
                else ResolvedAgentLaunchV1.parse(resolved_agent_launch)
            )
        except (ResolvedAgentLaunchError, TypeError, ValueError) as exc:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_LAUNCH_INVALID", status_code=409
            ) from exc
        identity = launch.identity
        if (
            identity["tenant_id"] != tenant_id
            or identity["user_id"] != user_id
            or identity["session_id"] != session_id
            or launch.model["id"] != model_id
            or resolved_agent_snapshot is not None
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_LAUNCH_SCOPE_MISMATCH", status_code=403
            )
        resolved_agent_snapshot = launch.to_control_snapshot()
        launch_policy = launch.turn_policy
        readonly_capabilities = launch.runtime_inputs["readonly_capabilities"]
        reasoning_option = launch_policy["reasoning_option"]
        legacy_thinking_level = launch_policy["legacy_thinking_level"]
        max_tokens = launch_policy["max_tokens"]
        temperature = launch_policy["temperature"]
        developer_instructions = resolved_agent_snapshot["agent_spec"]["developerInstructions"]
        style_guidance = None
        memory_mode = launch_policy["memory_mode"]
        memory_profile = launch_policy["memory_profile"]
        enable_dynamic_tools = launch_policy["enable_dynamic_tools"]
    elif resolved_agent_snapshot is None:
        raise AgentRuntimeControlError("AI_PLATFORM_AGENT_RUNTIME_LAUNCH_REQUIRED", status_code=409)

    assignment_row = await plane._assignment(tenant_id, user_id, session_id)
    model = await plane.model_service.get_model(tenant_id, model_id, provider_id=str(resolved_agent_snapshot["model"]["provider"]))
    if not model or not bool(model.get("is_enabled", True)):
        raise AgentRuntimeControlError("AI_PLATFORM_AGENT_RUNTIME_MODEL_NOT_FOUND", status_code=400)
    provider_id = str(model.get("provider_id") or "")
    provider = await plane.provider_service.get_provider(tenant_id, provider_id)
    if not provider or not bool(provider.get("is_enabled")):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_PROVIDER_UNAVAILABLE", status_code=503
        )
    profile = model.get("effective_capabilities")
    if not isinstance(profile, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_PROFILE_INVALID", status_code=503
        )
    if resolved_agent_launch is not None:
        launch_profile = launch.model_profile
        if launch_profile and launch_profile != profile:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_LAUNCH_MODEL_PROFILE_MISMATCH",
                status_code=409,
            )
        if launch_profile:
            profile = launch_profile
    else:
        publication = (
            resolved_agent_snapshot.get("publication")
            if isinstance(resolved_agent_snapshot, dict)
            and isinstance(resolved_agent_snapshot.get("publication"), dict)
            else {}
        )
        channel = str(publication.get("channel") or "")
        entrypoint = (
            "assistant"
            if channel == "builtin"
            else "studio_preview"
            if channel == "preview"
            else "published_agent"
        )
        try:
            launch = ResolvedAgentLaunchV1.from_legacy_snapshot(
                resolved_agent_snapshot,
                user_id=user_id,
                session_id=session_id,
                entrypoint=entrypoint,
                model_profile=profile,
                readonly_capabilities=readonly_capabilities or {},
                turn_policy={
                    "reasoning_option": reasoning_option,
                    "legacy_thinking_level": legacy_thinking_level,
                    "max_tokens": max_tokens,
                    "temperature": temperature,
                    "style_guidance": style_guidance,
                    "memory_mode": memory_mode,
                    "memory_profile": memory_profile or "basic",
                    "enable_dynamic_tools": enable_dynamic_tools,
                    "draft_revision": resolved_agent_snapshot.get("draft_revision"),
                },
            )
        except (ResolvedAgentLaunchError, TypeError, ValueError) as exc:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_LEGACY_LAUNCH_INVALID", status_code=409
            ) from exc
        resolved_agent_snapshot = launch.to_control_snapshot()
    signed_model: dict[str, Any] = {}
    signed_agent_spec: dict[str, Any] | None = None
    if resolved_agent_snapshot is not None:
        if not isinstance(resolved_agent_snapshot, dict):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        if (
            str(resolved_agent_snapshot.get("tenant_id") or "") != tenant_id
            or str(resolved_agent_snapshot.get("user_id") or "") not in {"", user_id}
            or str(resolved_agent_snapshot.get("session_id") or "") not in {"", session_id}
            or not str(resolved_agent_snapshot.get("agent_id") or "")
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_SCOPE_MISMATCH", status_code=403
            )
        signed_model = resolved_agent_snapshot.get("model")
        signed_agent_spec = resolved_agent_snapshot.get("agent_spec")
        if not isinstance(signed_model, dict) or not isinstance(signed_agent_spec, dict):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        if (
            str(signed_model.get("id") or "") != model_id
            or str(signed_model.get("provider") or "") != provider_id
            or not isinstance(signed_agent_spec.get("developerInstructions"), str)
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_MODEL_MISMATCH", status_code=409
            )
        if not signed_agent_spec["developerInstructions"].strip():
            signed_agent_spec = {
                **signed_agent_spec,
                "developerInstructions": GENERIC_AGENT_INSTRUCTIONS_V1,
            }
    signed_parameters = signed_model.get("parameters") if signed_model else {}
    if not isinstance(signed_parameters, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
        )
    effective_max_tokens = max_tokens
    signed_max_tokens = signed_parameters.get("max_tokens")
    if effective_max_tokens is None and signed_max_tokens is not None:
        if isinstance(signed_max_tokens, bool) or not isinstance(signed_max_tokens, int):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        effective_max_tokens = signed_max_tokens
    effective_temperature = temperature
    signed_temperature = signed_parameters.get("temperature")
    if effective_temperature is None and signed_temperature is not None:
        if (
            isinstance(signed_temperature, bool)
            or not isinstance(signed_temperature, int | float)
            or not 0 <= float(signed_temperature) <= 2
        ):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        effective_temperature = float(signed_temperature)
    effective_reasoning_option = reasoning_option
    effective_legacy_thinking = legacy_thinking_level
    signed_thinking_mode = signed_parameters.get("thinking_mode")
    if not effective_reasoning_option and not effective_legacy_thinking and signed_thinking_mode:
        if not isinstance(signed_thinking_mode, str) or len(signed_thinking_mode) > 100:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_SNAPSHOT_INVALID", status_code=409
            )
        effective_legacy_thinking = signed_thinking_mode
    requested = effective_reasoning_option or effective_legacy_thinking or "auto"
    if developer_instructions is not None and (
        not isinstance(developer_instructions, str)
        or not developer_instructions.strip()
        or len(developer_instructions) > 256 * 1024
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_INVALID", status_code=400
        )
    if (
        signed_agent_spec is not None
        and developer_instructions is not None
        and developer_instructions != signed_agent_spec["developerInstructions"]
    ):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_MISMATCH", status_code=409
        )
    agent_spec = signed_agent_spec or {
        "developerInstructions": GENERIC_AGENT_INSTRUCTIONS_V1,
        "model": {
            "id": model_id,
            "provider": provider_id,
            "parameters": {},
        },
        "knowledge": {"datasets": [], "retrieval": {}},
        "capabilities": [],
        "memory": {"mode": "session"},
    }
    if developer_instructions is not None:
        agent_spec = {**agent_spec, "developerInstructions": developer_instructions}
    if signed_agent_spec is None and isinstance(style_guidance, str) and style_guidance.strip():
        combined = (
            f"{str(agent_spec['developerInstructions']).rstrip()}\n\n{style_guidance.strip()}"
        )
        if len(combined) > 256 * 1024:
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_AGENT_INSTRUCTIONS_INVALID", status_code=400
            )
        agent_spec = {**agent_spec, "developerInstructions": combined}
    signed_memory = agent_spec.get("memory")
    signed_memory_mode = (
        str(signed_memory.get("mode") or "session")
        if isinstance(signed_memory, dict)
        else "session"
    )
    builtin_launch = resolved_agent_launch is not None and launch.identity["entrypoint"] in {
        "assistant",
        "responses",
    }
    selected_memory_mode = (
        str(memory_mode or "auto").strip().lower()
        if builtin_launch or signed_agent_spec is None
        else signed_memory_mode
    )
    if selected_memory_mode not in {"off", "session", "auto", "strict", "user"}:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_MEMORY_MODE_INVALID", status_code=400
        )
    signed_memory_profile = (
        signed_memory.get("profile") if isinstance(signed_memory, dict) else None
    )
    selected_memory_profile_raw = (
        signed_memory_profile
        if signed_agent_spec is not None and signed_memory_profile is not None
        else memory_profile or "basic"
    )
    if not isinstance(selected_memory_profile_raw, str):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_MEMORY_PROFILE_INVALID", status_code=400
        )
    selected_memory_profile = selected_memory_profile_raw.strip().lower()
    if selected_memory_profile not in {"off", "basic", "hybrid"}:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_MEMORY_PROFILE_INVALID", status_code=400
        )
    memory_write_allowed = (
        selected_memory_mode != "off"
        if builtin_launch or signed_agent_spec is None
        else selected_memory_mode == "user"
    )
    memory_policy = (
        {
            "authoritative_profile": selected_memory_profile,
            "agent_memory_mode": "user",
            "memory_principal": user_id,
        }
        if memory_write_allowed
        else None
    )
    memory_context = await plane._load_memory_context(
        tenant_id=tenant_id,
        user_id=user_id,
        mode=selected_memory_mode,
    )
    capability_allowlist = plane._snapshot_capability_allowlist(resolved_agent_snapshot)
    readonly_input = dict(readonly_capabilities or {})
    if capability_allowlist is not None:
        readonly_input["capability_allowlist"] = capability_allowlist
    if memory_context and memory_context.get("status") == "available":
        readonly_input["memory_context"] = memory_context["context"]
    resolved = resolve_reasoning_option(profile, requested)
    capability_revision = int(model.get("capability_revision") or 1)
    provider_revision = _provider_revision_func(provider.get("updated_at"))
    wire_protocols = profile.get("wire_protocols")
    if not isinstance(wire_protocols, dict):
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_WIRE_CAPABILITY_INVALID",
            status_code=503,
        )
    wire_protocol = str(wire_protocols.get("preferred") or "")
    supported_wires = wire_protocols.get("supported")
    if not isinstance(supported_wires, list) or wire_protocol not in supported_wires:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_WIRE_CAPABILITY_INVALID",
            status_code=503,
        )
    output_limit = _resolve_output_limit(
        effective_max_tokens,
        model.get("max_output_tokens"),
    )
    readonly = plane._readonly_capability_payload(
        readonly_input,
        tenant_id=tenant_id,
        capability_revision=capability_revision,
    )
    try:
        platform_config = build_runtime_platform_config(
            {
                **(resolved_agent_snapshot or {}),
                "agent_spec": agent_spec,
                "capabilities": (
                    resolved_agent_snapshot.get("capabilities", [])
                    if isinstance(resolved_agent_snapshot, dict)
                    else []
                ),
            },
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            attachment_refs=[
                str(item.get("payload", {}).get("content_ref"))
                for item in readonly.get("items", [])
                if isinstance(item, dict)
                and item.get("kind") == "attachment"
                and isinstance(item.get("payload"), dict)
                and item["payload"].get("content_ref")
            ],
        )
    except RuntimePlatformConfigError as exc:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_PLATFORM_CONFIG_INVALID", status_code=409
        ) from exc
    platform_config["config_hash"] = runtime_platform_config_hash(platform_config)
    readonly["platform_config"] = platform_config
    plane._attach_read_attachment_descriptors(
        readonly,
        tenant_id=tenant_id,
        capability_revision=capability_revision,
    )
    if enable_dynamic_tools:
        await plane._fetch_capability_catalog(
            readonly,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            capability_revision=capability_revision,
            capability_allowlist=capability_allowlist,
        )
    native_web_search_enabled = (
            readonly.get("responses_tool_choice") != "none"
            and readonly.get("responses_tool_names") is None
            and any(item.get("source") == "web-search" for item in readonly.get("items", []) if isinstance(item, dict))
            and isinstance(profile.get("native_search"), dict)
            and profile["native_search"].get("enabled") is True
            and isinstance(profile.get("tools"), dict)
            and profile["tools"].get("web_search_wire") == "native"
        )
    thread = await plane.ensure_thread(
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        model_id=model_id,
        readonly_capabilities=readonly,
        capability_allowlist=capability_allowlist,
        native_web_search_enabled=native_web_search_enabled,
        developer_instructions=agent_spec["developerInstructions"],
    )
    runtime_thread_id = uuid.UUID(str(thread["runtime_thread_id"]))
    tool_policy = runtime_tool_policy(readonly, native_search_allowed=native_web_search_enabled)
    await plane._resume_thread(
        runtime_thread_id=runtime_thread_id,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        model_id=model_id,
        developer_instructions=agent_spec["developerInstructions"],
        tool_policy=tool_policy,
        model_context_window=int(model.get("context_window") or 128000),
        auto_compact_token_limit=(
            int(model["auto_compact_token_limit"])
            if isinstance(model.get("auto_compact_token_limit"), int)
            and not isinstance(model.get("auto_compact_token_limit"), bool)
            else (
                int(profile["auto_compact_token_limit"])
                if isinstance(profile.get("auto_compact_token_limit"), int)
                and not isinstance(profile.get("auto_compact_token_limit"), bool)
                else None
            )
        ),
        native_web_search_enabled=native_web_search_enabled,
    )
    run_id = uuid.uuid4()
    snapshot_id = uuid.uuid4()
    lease_id = uuid.uuid4()
    nonce_sha256 = sha256(secrets.token_bytes(32)).hexdigest()
    now = datetime.now(timezone.utc)
    expires_at = now + timedelta(seconds=plane.lease_ttl_seconds)
    snapshot = {
        "schema_version": "agent-runtime-snapshot/v1",
        "tenant_id": tenant_id,
        "user_id": user_id,
        "session_id": session_id,
        "runtime_thread_id": str(runtime_thread_id),
        "run_id": str(run_id),
        "kernel_revision": assignment_row.kernel_revision,
        "model": {
            "id": model_id,
            "provider_id": provider_id,
            "api_type": str(provider.get("api_type") or ""),
            "wire_protocol": wire_protocol,
            "provider_revision": provider_revision,
        },
        "capability_revision": capability_revision,
        "capabilities": profile,
        "instructions": {
            "baseInstructions": BASE_AGENT_INSTRUCTIONS_V1,
            "developerInstructions": agent_spec["developerInstructions"],
        },
        "runtime_resume_config": {
            "model": model_id,
            "modelPlaneBaseUrl": plane.model_plane_base_url,
            "baseInstructions": BASE_AGENT_INSTRUCTIONS_V1,
            "developerInstructions": agent_spec["developerInstructions"],
            "modelContextWindow": int(model.get("context_window") or 128000),
            "nativeWebSearchEnabled": native_web_search_enabled,
        },
        "agent_spec": agent_spec,
        "memory": {
            "mode": selected_memory_mode,
            "context_status": (memory_context or {}).get("status", "not_loaded"),
            "policy": memory_policy,
        },
        "memory_context": memory_context,
        "reasoning": {
            "requested_option": resolved.requested,
            "effective_option": resolved.effective,
            "adapter_id": resolved.adapter_id,
            "canonical_effort": resolved.canonical_effort,
            "settings": resolved.settings,
            "fallback_reason": resolved.fallback_reason,
        },
        "input": {"message": message},
        "limits": {
            "context_window": int(model.get("context_window") or 128000),
            "max_output_tokens": output_limit,
            "max_model_calls": plane.max_model_calls,
        },
        "parameters": (
            {"temperature": effective_temperature} if effective_temperature is not None else {}
        ),
        "pricing": {
            "snapshot": model.get("pricing_snapshot"),
            "input_price_per_1k": float(model.get("input_price_per_1k") or 0),
            "output_price_per_1k": float(model.get("output_price_per_1k") or 0),
        },
        "tools": {
            "enabled": bool(readonly.get("tools") or readonly.get("mcp")),
            "phase": "readonly"
            if readonly.get("items") or readonly.get("tools") or readonly.get("mcp")
            else "pure_text",
        },
        "readonly_capabilities": readonly,
        "tool_policy": tool_policy,
        "platform_config": platform_config,
        "platform_config_hash": runtime_platform_config_hash(platform_config),
    }
    # Keep compaction/model limits data-driven. Providers may omit an
    # explicit threshold; in that case the kernel uses its own bounded
    # default rather than a model-name branch.
    raw_compact_limit = model.get("auto_compact_token_limit")
    if raw_compact_limit is None:
        raw_compact_limit = profile.get("auto_compact_token_limit")
    if raw_compact_limit is not None:
        if isinstance(raw_compact_limit, bool) or not isinstance(raw_compact_limit, int):
            raise AgentRuntimeControlError(
                "AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_PROFILE_INVALID", status_code=503
            )
        snapshot["limits"]["auto_compact_token_limit"] = max(1, raw_compact_limit)
    snapshot["eval_fingerprint"] = {
        "system_prompt_hash": sha256(canonical_runtime_json(snapshot["instructions"]).encode()).hexdigest(),
        "tool_schema_hash": sha256(canonical_runtime_json(plane._dynamic_tools(readonly)).encode()).hexdigest(),
    }
    snapshot_json = canonical_runtime_json(snapshot)
    snapshot_hash = sha256(snapshot_json.encode()).hexdigest()
    max_input_tokens = min(
        int(model.get("context_window") or 128000) * plane.max_model_calls,
        10_000_000,
    )
    max_output_tokens = min(output_limit * plane.max_model_calls, 1_000_000)
    await plane.database.fetchrow(
        """
        SELECT issue_assistant_runtime_turn(
            $1, $2, $3, $4, $5, $6, $7, $8,
            'agent-runtime-snapshot/v1', $9::jsonb, $10, $11, $12,
            $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23
        )
        """,
        snapshot_id,
        lease_id,
        run_id,
        runtime_thread_id,
        tenant_id,
        user_id,
        session_id,
        assignment_row.kernel_revision,
        snapshot_json,
        snapshot_hash,
        capability_revision,
        resolved.effective,
        RUNTIME_MODEL_LEASE_SCHEMA_VERSION,
        provider_id,
        model_id,
        provider_revision,
        nonce_sha256,
        plane.max_model_calls,
        max_input_tokens,
        max_output_tokens,
        plane.max_cost_microusd,
        expires_at,
        message,
    )
    lease_row = await plane.database.fetchrow(
        "SELECT * FROM assistant_runtime_model_leases WHERE lease_id = $1",
        lease_id,
    )
    if lease_row is None:
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_LEASE_ISSUE_FAILED", status_code=503
        )
    lease_data = dict(lease_row)
    claims = RuntimeModelLeaseClaims(
        schema_version=str(lease_data["schema_version"]),
        lease_id=str(lease_id),
        snapshot_id=str(snapshot_id),
        run_id=str(run_id),
        runtime_thread_id=str(runtime_thread_id),
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        provider_id=provider_id,
        model_id=model_id,
        capability_revision=capability_revision,
        issued_at_ms=int(lease_data["issued_at"].timestamp() * 1000),
        expires_at_ms=int(lease_data["expires_at"].timestamp() * 1000),
        nonce_sha256=nonce_sha256,
    )
    signature = plane.lease_signer.sign(claims)
    effort = resolved.canonical_effort
    if effort not in {"minimal", "low", "medium", "high", "xhigh", "max", "ultra"}:
        effort = None
    note_dispatch(runtime_thread_id=str(runtime_thread_id), run_id=str(run_id),
                  after_sequence=int(thread.get("last_sequence") or 0))
    response = await plane.http_client.post(
        f"{plane.runtime_url}/internal/v1/threads/{runtime_thread_id}/turns",
        headers=runtime_headers(
            plane,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            run_id=run_id,
            turn_id=run_id,
        ),
        json={
            "runId": str(run_id),
            "snapshotId": str(snapshot_id),
            "leaseId": str(lease_id),
            "leaseSignature": signature,
            "message": message,
            "images": image_inputs or [],
            "model": model_id,
            "effort": effort,
            "capabilityRevision": capability_revision,
            "readonly": plane._turn_prompt_readonly(readonly),
            "platformConfig": platform_config,
        },
    )
    if response.status_code >= 400:
        try:
            runtime_error = response.json().get("error")
        except (ValueError, AttributeError):
            runtime_error = None
        _logger.warning(
            "Agent Runtime rejected turn start status=%s error=%s",
            response.status_code,
            str(runtime_error or "unknown")[:160],
        )
        await plane._fail_run(run_id, snapshot_id, "agent_turn_start_rejected")
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_TURN_START_FAILED", status_code=503
        )
    payload = response.json()
    returned_turn_id = str(((payload.get("turn") or {}).get("id")) or "")
    if returned_turn_id != str(run_id):
        await plane._fail_run(run_id, snapshot_id, "agent_turn_identity_mismatch")
        raise AgentRuntimeControlError(
            "AI_PLATFORM_AGENT_RUNTIME_TURN_IDENTITY_MISMATCH", status_code=503
        )
    return AgentTurn(
        runtime_thread_id=str(runtime_thread_id),
        run_id=str(run_id),
        snapshot_id=str(snapshot_id),
        lease_id=str(lease_id),
        after_sequence=int(thread.get("last_sequence") or 0),
        requested_reasoning_option=resolved.requested,
        effective_reasoning_option=resolved.effective,
        reasoning_adapter_id=resolved.adapter_id,
        capability_revision=capability_revision,
        fallback_reason=resolved.fallback_reason,
    )
