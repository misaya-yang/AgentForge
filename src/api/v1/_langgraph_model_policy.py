"""LangGraph model override and failover persistence policy."""

from __future__ import annotations

from typing import Any

from fastapi import HTTPException, Request

from ...services.llm.model_failover import (
    has_secret_field,
    normalize_failover_attempts,
    normalize_max_attempts,
)
from ._langgraph_connector_config import _is_langgraph_definition

_MODEL_OVERRIDE_EPOCH_IGNORED_FIELDS = {"cache_epoch"}
_DEFAULT_LANGGRAPH_FAILOVER_PROVIDER_PRIORITY = (
    "google",
    "dashscope",
    "dashscope-intl",
    "dashscope-cn",
)
_DEFAULT_LANGGRAPH_FAILOVER_MAX_CANDIDATES = 2


def _as_bool(value: Any, *, default: bool = False) -> bool:
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in {"1", "true", "yes", "on"}
    return bool(value)


def _model_override_error(code: str, status_code: int = 422) -> HTTPException:
    return HTTPException(status_code=status_code, detail=code)


def _coerce_cache_epoch(value: Any) -> int:
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return 0


def _meaningful_model_override(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    return {
        key: value.get(key)
        for key in sorted(value)
        if key not in _MODEL_OVERRIDE_EPOCH_IGNORED_FIELDS
    }


def _override_allows_environment_credentials(provider_service: Any, provider: dict) -> bool:
    checker = getattr(provider_service, "allows_environment_credentials", None)
    if callable(checker):
        return bool(checker(provider))
    return bool(
        provider.get("allow_environment_credentials")
        or provider.get("uses_environment_credentials")
    )


async def _validate_model_override_provider_model(
    *,
    tenant_id: str,
    provider_id: str,
    model_id: str,
    provider_service: Any,
    model_service: Any,
    error_prefix: str,
) -> None:
    provider = await provider_service.get_provider(tenant_id, provider_id)
    if not provider:
        raise _model_override_error(f"{error_prefix}_PROVIDER_NOT_FOUND")
    if not bool(provider.get("is_enabled")):
        raise _model_override_error(f"{error_prefix}_PROVIDER_DISABLED")

    has_key = bool(provider.get("has_api_key"))
    if not has_key and not _override_allows_environment_credentials(provider_service, provider):
        raise _model_override_error(f"{error_prefix}_API_KEY_MISSING")

    get_provider_model = getattr(model_service, "get_provider_model", None)
    if callable(get_provider_model):
        model = await get_provider_model(tenant_id, provider_id, model_id)
    else:
        model = await model_service.get_model(
            tenant_id,
            model_id,
            provider_id=provider_id,
        )
    if not model:
        raise _model_override_error(f"{error_prefix}_MODEL_NOT_FOUND")
    if not bool(model.get("is_enabled")):
        raise _model_override_error(f"{error_prefix}_MODEL_DISABLED")


async def _normalize_langgraph_failover(
    raw_failover: Any,
    *,
    tenant_id: str,
    primary_provider_id: str,
    primary_model_id: str,
    provider_service: Any,
    model_service: Any,
) -> dict[str, Any] | None:
    if raw_failover is None:
        return None
    if not isinstance(raw_failover, dict):
        raise _model_override_error("MODEL_OVERRIDE_FAILOVER_INVALID")
    if has_secret_field(raw_failover):
        raise _model_override_error("MODEL_OVERRIDE_API_KEY_FORBIDDEN")

    enabled = _as_bool(raw_failover.get("enabled"), default=False)
    normalized = {
        "enabled": enabled,
        "max_attempts": normalize_max_attempts(raw_failover.get("max_attempts"), default=3),
        "candidates": [],
    }
    if not enabled:
        return normalized

    raw_candidates = raw_failover.get("candidates")
    if raw_candidates is None:
        raw_candidates = []
    if not isinstance(raw_candidates, list):
        raise _model_override_error("MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID")

    seen = {(primary_provider_id, primary_model_id)}
    candidates: list[dict[str, str]] = []
    for raw_candidate in raw_candidates:
        if not isinstance(raw_candidate, dict):
            raise _model_override_error("MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID")
        if has_secret_field(raw_candidate):
            raise _model_override_error("MODEL_OVERRIDE_API_KEY_FORBIDDEN")

        provider_id = str(raw_candidate.get("provider_id") or "").strip()
        model_id = str(raw_candidate.get("model_id") or "").strip()
        if not provider_id:
            raise _model_override_error("MODEL_OVERRIDE_FAILOVER_PROVIDER_REQUIRED")
        if not model_id:
            raise _model_override_error("MODEL_OVERRIDE_FAILOVER_MODEL_REQUIRED")
        if (provider_id, model_id) in seen:
            raise _model_override_error("MODEL_OVERRIDE_FAILOVER_DUPLICATE")

        await _validate_model_override_provider_model(
            tenant_id=tenant_id,
            provider_id=provider_id,
            model_id=model_id,
            provider_service=provider_service,
            model_service=model_service,
            error_prefix="MODEL_OVERRIDE_FAILOVER",
        )
        candidates.append({"provider_id": provider_id, "model_id": model_id})
        seen.add((provider_id, model_id))

    normalized["candidates"] = candidates
    normalized["max_attempts"] = normalize_failover_attempts(
        normalized["max_attempts"],
        candidate_count=len(candidates) + 1,
        default=3,
    )
    return normalized


def _raw_failover_has_candidates(raw_failover: Any) -> bool:
    if not isinstance(raw_failover, dict):
        return False
    raw_candidates = raw_failover.get("candidates")
    return isinstance(raw_candidates, list) and len(raw_candidates) > 0


def _model_sort_key(model: dict[str, Any]) -> tuple[int, str]:
    try:
        sort_order = int(model.get("sort_order") or 0)
    except (TypeError, ValueError):
        sort_order = 0
    return (sort_order, str(model.get("display_name") or model.get("model_id") or ""))


async def _list_enabled_provider_models(
    *,
    tenant_id: str,
    provider_id: str,
    model_service: Any,
) -> list[dict[str, Any]]:
    list_models = getattr(model_service, "list_models", None)
    if callable(list_models):
        models = await list_models(
            tenant_id=tenant_id,
            provider_id=provider_id,
            include_disabled=False,
        )
    else:
        models = [
            model
            for (candidate_provider_id, _), model in getattr(model_service, "models", {}).items()
            if candidate_provider_id == provider_id
        ]
    if not isinstance(models, list):
        return []
    return [model for model in models if isinstance(model, dict) and bool(model.get("is_enabled"))]


async def _build_default_langgraph_failover_candidates(
    *,
    tenant_id: str,
    primary_provider_id: str,
    primary_model_id: str,
    provider_service: Any,
    model_service: Any,
) -> list[dict[str, str]]:
    candidates: list[dict[str, str]] = []
    seen = {(primary_provider_id, primary_model_id)}
    seen_providers = {primary_provider_id}

    for provider_id in _DEFAULT_LANGGRAPH_FAILOVER_PROVIDER_PRIORITY:
        if provider_id in seen_providers:
            continue
        provider = await provider_service.get_provider(tenant_id, provider_id)
        if not provider or not bool(provider.get("is_enabled")):
            continue
        if not bool(provider.get("has_api_key")) and not _override_allows_environment_credentials(
            provider_service,
            provider,
        ):
            continue

        models = await _list_enabled_provider_models(
            tenant_id=tenant_id,
            provider_id=provider_id,
            model_service=model_service,
        )
        if not models:
            continue

        model = max(models, key=_model_sort_key)
        model_id = str(model.get("model_id") or "").strip()
        if not model_id or (provider_id, model_id) in seen:
            continue

        candidates.append({"provider_id": provider_id, "model_id": model_id})
        seen.add((provider_id, model_id))
        seen_providers.add(provider_id)
        if len(candidates) >= _DEFAULT_LANGGRAPH_FAILOVER_MAX_CANDIDATES:
            break

    return candidates


async def _seed_default_langgraph_failover(
    *,
    tenant_id: str,
    model_override: dict[str, Any],
    primary_provider_id: str,
    primary_model_id: str,
    provider_service: Any,
    model_service: Any,
) -> None:
    raw_failover = model_override.get("failover")
    if _raw_failover_has_candidates(raw_failover):
        return

    candidates = await _build_default_langgraph_failover_candidates(
        tenant_id=tenant_id,
        primary_provider_id=primary_provider_id,
        primary_model_id=primary_model_id,
        provider_service=provider_service,
        model_service=model_service,
    )
    if not candidates:
        return

    failover = raw_failover if isinstance(raw_failover, dict) else {}
    model_override["failover"] = {
        **failover,
        "enabled": True,
        "max_attempts": max(normalize_max_attempts(failover.get("max_attempts"), default=3), 2),
        "candidates": candidates,
    }


async def _validate_langgraph_model_override(
    request: Request,
    *,
    tenant_id: str,
    definition: dict,
    previous_connector_config: dict | None = None,
) -> None:
    """Validate and normalize connector_config.model_override for LangGraph services."""
    if not _is_langgraph_definition(definition):
        return

    connector_config = definition.get("connector_config")
    if not isinstance(connector_config, dict):
        return

    raw_override = connector_config.get("model_override")
    if raw_override is None:
        return
    if not isinstance(raw_override, dict):
        raise _model_override_error("MODEL_OVERRIDE_INVALID")

    if has_secret_field(raw_override):
        raise _model_override_error("MODEL_OVERRIDE_API_KEY_FORBIDDEN")

    model_override = dict(raw_override)
    enabled = _as_bool(model_override.get("enabled"), default=False)
    model_override["enabled"] = enabled

    if "temperature" in model_override and model_override["temperature"] is not None:
        try:
            temperature = float(model_override["temperature"])
        except (TypeError, ValueError) as exc:
            raise _model_override_error("MODEL_OVERRIDE_TEMPERATURE_INVALID") from exc
        if temperature < 0 or temperature > 2:
            raise _model_override_error("MODEL_OVERRIDE_TEMPERATURE_INVALID")
        model_override["temperature"] = temperature

    provider_id = str(model_override.get("provider_id") or "").strip()
    model_id = str(model_override.get("model_id") or "").strip()

    provider_service = getattr(request.app.state, "provider_service", None)
    model_service = getattr(request.app.state, "model_service", None)
    if enabled and (provider_service is None or model_service is None):
        raise _model_override_error("MODEL_OVERRIDE_CONTROL_PLANE_UNAVAILABLE", status_code=503)

    normalized_failover = None
    if not enabled:
        raw_failover = model_override.get("failover")
        if raw_failover is not None:
            if not isinstance(raw_failover, dict):
                raise _model_override_error("MODEL_OVERRIDE_FAILOVER_INVALID")
            if has_secret_field(raw_failover):
                raise _model_override_error("MODEL_OVERRIDE_API_KEY_FORBIDDEN")
            normalized_failover = {
                "enabled": False,
                "max_attempts": normalize_max_attempts(raw_failover.get("max_attempts"), default=3),
                "candidates": [],
            }
    else:
        if not provider_id:
            raise _model_override_error("MODEL_OVERRIDE_PROVIDER_REQUIRED")
        if not model_id:
            raise _model_override_error("MODEL_OVERRIDE_MODEL_REQUIRED")

        await _validate_model_override_provider_model(
            tenant_id=tenant_id,
            provider_id=provider_id,
            model_id=model_id,
            provider_service=provider_service,
            model_service=model_service,
            error_prefix="MODEL_OVERRIDE",
        )
        await _seed_default_langgraph_failover(
            tenant_id=tenant_id,
            model_override=model_override,
            primary_provider_id=provider_id,
            primary_model_id=model_id,
            provider_service=provider_service,
            model_service=model_service,
        )
        normalized_failover = await _normalize_langgraph_failover(
            model_override.get("failover"),
            tenant_id=tenant_id,
            primary_provider_id=provider_id,
            primary_model_id=model_id,
            provider_service=provider_service,
            model_service=model_service,
        )

    if provider_id:
        model_override["provider_id"] = provider_id
    if model_id:
        model_override["model_id"] = model_id
    if normalized_failover is not None:
        model_override["failover"] = normalized_failover

    previous_override = {}
    if isinstance(previous_connector_config, dict):
        previous_raw = previous_connector_config.get("model_override")
        if isinstance(previous_raw, dict):
            previous_override = previous_raw

    previous_epoch = _coerce_cache_epoch(previous_override.get("cache_epoch"))
    if _meaningful_model_override(model_override) != _meaningful_model_override(previous_override):
        model_override["cache_epoch"] = previous_epoch + 1
    else:
        model_override["cache_epoch"] = previous_epoch

    connector_config["model_override"] = model_override
