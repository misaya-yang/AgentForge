"""Model/capability/knowledge resolution for Agent Runtime snapshots.

ARC-01B split of ``src/api/v1/agent_runtime.py``.  Moved verbatim; the facade
keeps time-limited re-exports for pre-split import paths.
"""

from __future__ import annotations

import inspect
import os
import re
from datetime import datetime, timezone
from typing import Any

from fastapi import Request

from ....core.auth.user_resolver import UserContext
from .core import _is_tenant_admin, _raise_runtime_error


def _runtime_knowledge_config(
    request: Request,
    raw: Any,
    *,
    channel: str,
) -> dict[str, Any]:
    """Normalize the closed, secret-free per-Dataset runtime contract."""

    config = raw if isinstance(raw, dict) else {}
    allowed = {"mode", "top_k", "threshold", "score_threshold", "include_images"}
    if set(config) - allowed:
        _raise_runtime_error(
            request,
            422 if channel == "preview" else 409,
            "AGENT_KNOWLEDGE_CONFIG_INVALID",
            "A bound Knowledge retrieval configuration is unsupported",
        )
    mode = config.get("mode", "auto")
    top_k = config.get("top_k", 5)
    threshold = config.get("threshold", config.get("score_threshold", 0.4))
    include_images = config.get("include_images", False)
    if (
        mode not in {"auto", "tool", "off"}
        or isinstance(top_k, bool)
        or not isinstance(top_k, int)
        or not 1 <= top_k <= 20
        or isinstance(threshold, bool)
        or not isinstance(threshold, (int, float))
        or not 0 <= float(threshold) <= 1
        or not isinstance(include_images, bool)
    ):
        _raise_runtime_error(
            request,
            422 if channel == "preview" else 409,
            "AGENT_KNOWLEDGE_CONFIG_INVALID",
            "A bound Knowledge retrieval configuration is invalid",
        )
    if (
        "threshold" in config
        and "score_threshold" in config
        and config["threshold"] != config["score_threshold"]
    ):
        _raise_runtime_error(
            request,
            422 if channel == "preview" else 409,
            "AGENT_KNOWLEDGE_CONFIG_INVALID",
            "A bound Knowledge retrieval threshold is ambiguous",
        )
    return {
        "mode": str(mode),
        "top_k": top_k,
        "threshold": float(threshold),
        "include_images": include_images,
    }


async def _resolved_model(
    request: Request,
    resolution: dict[str, Any],
    user: UserContext,
) -> dict[str, Any]:
    """Resolve model readiness/permission from server-owned metadata only."""

    spec = resolution["spec"]
    requested = spec.get("model") if isinstance(spec.get("model"), dict) else {}
    requested_model_id = str(requested.get("model_id") or "").strip()
    uses_default_model = not requested_model_id
    model_id = requested_model_id
    if uses_default_model:
        settings = getattr(request.app.state, "settings", None)
        model_id = str(getattr(settings, "default_model", "") or "").strip()
    if not model_id:
        _raise_runtime_error(
            request,
            503,
            "AGENT_RUNTIME_MODEL_UNAVAILABLE",
            "Agent model is unavailable",
        )
    effective_requested = dict(requested)
    effective_requested["model_id"] = model_id
    if uses_default_model:
        # An empty model_id delegates both model and provider selection to the
        # server. A UI placeholder provider must not constrain that lookup.
        effective_requested.pop("provider_id", None)

    e2e_stub_enabled = os.getenv("ASSISTANT_E2E_STUB_LLM", "").strip().lower() in {
        "1",
        "true",
        "yes",
        "on",
    }
    resolver = getattr(request.app.state, "agent_runtime_model_resolver", None)
    if e2e_stub_enabled:
        provider = str(effective_requested.get("provider_id") or "dashscope")
    elif resolver is not None:
        try:
            result = resolver.resolve(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                model=effective_requested,
            )
            if inspect.isawaitable(result):
                result = await result
        except Exception:  # noqa: BLE001 - readiness uncertainty is deny
            result = None
        if not isinstance(result, dict) or not result.get("provider"):
            _raise_runtime_error(
                request,
                503,
                "AGENT_RUNTIME_MODEL_UNAVAILABLE",
                "Agent model is unavailable",
            )
        if str(result.get("id") or model_id) != model_id:
            _raise_runtime_error(
                request,
                422,
                "AGENT_RUNTIME_MODEL_MISMATCH",
                "Agent model configuration is invalid",
            )
        provider = str(result["provider"])
    else:
        model_meta = getattr(request.app.state, "model_meta", None)
        model_service = getattr(model_meta, "model_service", None)
        if model_meta is None or model_service is None:
            _raise_runtime_error(
                request,
                503,
                "AGENT_RUNTIME_MODEL_UNAVAILABLE",
                "Agent model is unavailable",
            )
        try:
            get_model_parameters = inspect.signature(model_service.get_model).parameters
            if "provider_id" in get_model_parameters:
                row = await model_service.get_model(
                    user.tenant_id,
                    model_id,
                    provider_id=(
                        str(effective_requested.get("provider_id") or "") or None
                    ),
                )
            else:
                row = await model_service.get_model(user.tenant_id, model_id)
            provider = str((row or {}).get("provider_id") or "")
            configured = bool(provider) and await model_meta.is_provider_configured(
                user.tenant_id,
                provider,
            )
        except Exception:  # noqa: BLE001 - metadata/readiness uncertainty is deny
            row = None
            configured = False
        if not row or not bool(row.get("is_enabled", True)) or not configured:
            _raise_runtime_error(
                request,
                503,
                "AGENT_RUNTIME_MODEL_UNAVAILABLE",
                "Agent model is unavailable",
            )
        requested_provider = str(effective_requested.get("provider_id") or "")
        if requested_provider and requested_provider != provider:
            _raise_runtime_error(
                request,
                422,
                "AGENT_RUNTIME_MODEL_MISMATCH",
                "Agent model configuration is invalid",
            )
        from ....services.assistant_entry.model_access import user_can_access_model

        if not user_can_access_model(user, str(row.get("access_level") or "public")):
            _raise_runtime_error(
                request,
                403,
                "AGENT_RUNTIME_MODEL_FORBIDDEN",
                "Agent model is unavailable",
            )

    parameters = {
        key: requested[key]
        for key in ("temperature", "max_tokens", "thinking_mode")
        if requested.get(key) is not None
    }
    return {"id": model_id, "provider": provider, "parameters": parameters}


async def _effective_capabilities(
    request: Request,
    resolution: dict[str, Any],
    user: UserContext,
    *,
    channel: str,
    channel_policy: dict[str, Any],
) -> list[dict[str, Any]]:
    """Return a server-authorized subset; absence/uncertainty means empty."""

    bindings = [dict(item) for item in resolution.get("capabilities") or []]
    resolver = getattr(request.app.state, "agent_runtime_capability_resolver", None)
    if resolver is None:
        return []
    try:
        result = resolver.resolve(
            tenant_id=resolution["agent"]["tenant_id"],
            agent_id=resolution["agent"]["agent_id"],
            bindings=bindings,
            channel=channel,
            channel_policy=channel_policy,
            user_id=user.user_id,
            authenticated=user.is_authenticated,
            is_tenant_admin=_is_tenant_admin(user),
        )
        if inspect.isawaitable(result):
            result = await result
    except Exception:  # noqa: BLE001 - policy uncertainty is deny, not a 500 leak
        return []
    if not isinstance(result, list):
        return []
    bound_by_key = {
        (
            str(item.get("capability_type") or item.get("type") or ""),
            str(item.get("resource_id") or item.get("id") or ""),
        ): item
        for item in bindings
        if str(item.get("resource_id") or item.get("id") or "")
    }
    effective: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for raw in result:
        if not isinstance(raw, dict):
            continue
        capability_type = str(raw.get("capability_type") or raw.get("type") or "")
        resource_id = str(raw.get("resource_id") or raw.get("id") or "")
        key = (capability_type, resource_id)
        if not resource_id or key not in bound_by_key or key in seen:
            continue
        seen.add(key)
        # The resolver authorizes a subset; it is not an alternate source for
        # immutable Version metadata. Keeping the original binding prevents a
        # same-ID response from lowering risk or replacing version/schema/config.
        effective.append(dict(bound_by_key[key]))
    return effective


async def _effective_knowledge(
    request: Request,
    resolution: dict[str, Any],
    user: UserContext,
    *,
    channel: str,
) -> list[dict[str, Any]]:
    """Return the caller-authorized subset; missing resolver means no datasets."""

    bindings = [dict(item) for item in resolution.get("knowledge") or []]
    resolver = getattr(request.app.state, "agent_runtime_knowledge_resolver", None)
    if resolver is None:
        return []
    try:
        result = resolver.resolve(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            agent_id=resolution["agent"]["agent_id"],
            bindings=bindings,
            channel=channel,
            authenticated=user.is_authenticated,
            roles=list(user.roles or []),
            is_tenant_admin=_is_tenant_admin(user),
        )
        if inspect.isawaitable(result):
            result = await result
    except Exception:  # noqa: BLE001 - authorization/readiness uncertainty is deny
        return []
    if not isinstance(result, list):
        return []
    allowed = {str(item.get("dataset_id") if isinstance(item, dict) else item) for item in result}
    return [binding for binding in bindings if str(binding.get("dataset_id") or "") in allowed]


def _channel_policy(resolution: dict[str, Any], *, channel: str) -> dict[str, Any]:
    publication = resolution.get("publication") or {}
    raw = publication.get("policy") if isinstance(publication, dict) else {}
    raw = raw if isinstance(raw, dict) else {}
    allowed_origins = raw.get("allowed_origins")
    policy = {
        "attachments": bool(raw.get("attachments", channel == "preview")),
        "high_risk_tools": bool(raw.get("high_risk_tools", channel == "preview")),
        "allowed_origins": [
            str(origin) for origin in (allowed_origins or []) if isinstance(origin, str)
        ],
    }
    if isinstance(raw.get("expires_at"), str) and raw["expires_at"]:
        expiry = datetime.fromisoformat(raw["expires_at"].replace("Z", "+00:00"))
        if expiry.tzinfo is None or expiry.utcoffset() is None:
            raise ValueError("Publication expiry must include a timezone")
        policy["expires_at"] = expiry.astimezone(timezone.utc).isoformat()
    return policy


def _public_effective_native_capabilities(snapshot: dict[str, Any]) -> list[dict[str, Any]]:
    projected: list[dict[str, Any]] = []
    for raw in snapshot.get("capabilities") or []:
        if not isinstance(raw, dict) or raw.get("type") != "platform":
            continue
        name = str(raw.get("id") or "")
        schema_hash = str(raw.get("schema_hash") or "")
        risk = str(raw.get("risk") or "")
        config = raw.get("config") if isinstance(raw.get("config"), dict) else {}
        requires_confirmation = config.get("requires_confirmation")
        # The snapshot no longer fabricates a confirmation pin, but this
        # client-visible projection must still list high/critical native
        # capabilities as needing confirmation (conservative default matches
        # the runtime's definition-based fail-closed enforcement).
        if requires_confirmation is None and risk in {"high", "critical"}:
            requires_confirmation = True
        if (
            re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,199}", name) is None
            or re.fullmatch(r"sha256:[a-f0-9]{64}", schema_hash) is None
            or risk not in {"low", "medium", "high", "critical"}
            or not isinstance(requires_confirmation, bool)
        ):
            continue
        projected.append(
            {
                "name": name,
                "schema_hash": schema_hash,
                "risk": risk,
                "requires_confirmation": requires_confirmation,
            }
        )
    return sorted(projected, key=lambda item: item["name"])


def _confirmation_stamp(
    binding_config: dict[str, Any],
    *,
    risk: str,
    runtime_type: str,
    definition: Any | None,
) -> dict[str, Any]:
    """Decide the platform high/critical confirmation pin.

    The gateway cannot resolve the assistant's live tool definitions, so
    callers normally pass ``definition=None``.  Stamping True
    unconditionally was inert for enforcement — the runtime validates
    against the live definition and fails closed when a high/critical
    tool cannot confirm — while writing a misleading pin into the
    snapshot.  Stamp only when the live definition actually supports
    confirmation; otherwise leave the binding unpinned.
    """
    if runtime_type != "platform" or risk not in {"high", "critical"}:
        return binding_config
    if "requires_confirmation" in binding_config:
        return binding_config
    if definition is not None and bool(getattr(definition, "requires_confirmation", False)):
        binding_config["requires_confirmation"] = True
    return binding_config
