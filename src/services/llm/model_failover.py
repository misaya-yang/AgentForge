"""Gateway-owned LangGraph model failover helpers.

This module builds the server-only ``configurable.gateway_model`` payload consumed
by LangChain middleware. Browser payloads are never trusted for provider,
model, or credential selection.
"""

from __future__ import annotations

import hashlib
from typing import Any

MODEL_OVERRIDE_SECRET_FIELD_NAMES = {
    "_api_key",
    "api_key",
    "apikey",
    "api-key",
    "credential",
    "credentials",
    "password",
    "private_key",
    "secret",
    "service_account",
    "service_account_json",
    "token",
}

class ModelOverrideRuntimeError(ValueError):
    """Safe error raised while resolving Gateway-owned model runtime config."""

    def __init__(self, code: str) -> None:
        self.code = code
        super().__init__(code)


def has_secret_field(value: Any) -> bool:
    """Return true when a nested object includes credential-like keys."""
    if isinstance(value, dict):
        for key, child in value.items():
            normalized = str(key or "").strip().lower()
            if normalized in MODEL_OVERRIDE_SECRET_FIELD_NAMES:
                return True
            if has_secret_field(child):
                return True
        return False
    if isinstance(value, list):
        return any(has_secret_field(item) for item in value)
    return False


def safe_api_key_fingerprint(api_key: object) -> str | None:
    if not api_key:
        return None
    return hashlib.sha256(str(api_key).encode("utf-8")).hexdigest()[:16]


def normalize_max_attempts(value: Any, *, default: int = 3) -> int:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        parsed = default
    return max(1, min(parsed, 10))


def normalize_failover_attempts(value: Any, *, candidate_count: int, default: int = 3) -> int:
    """Normalize attempts so an enabled fallback chain can actually reach a fallback."""
    max_attempts = normalize_max_attempts(value, default=default)
    if candidate_count > 1:
        max_attempts = max(max_attempts, 2)
    return min(max_attempts, max(candidate_count, 1))


def _allows_environment_credentials(provider: dict[str, Any]) -> bool:
    return bool(
        provider.get("allow_environment_credentials")
        or provider.get("uses_environment_credentials")
    )


def _provider_runtime_metadata(provider: dict[str, Any]) -> dict[str, Any]:
    metadata = provider.get("metadata")
    if not isinstance(metadata, dict):
        return {}
    runtime: dict[str, Any] = {}
    for source, target in (
        ("project", "project"),
        ("project_id", "project"),
        ("google_cloud_project", "project"),
        ("location", "location"),
        ("region", "location"),
        ("google_cloud_location", "location"),
        ("credentials_path", "credentials_path"),
        ("google_application_credentials", "credentials_path"),
    ):
        value = metadata.get(source)
        if value is not None and str(value).strip():
            runtime[target] = str(value).strip()
    return runtime


async def _load_model(
    *,
    tenant_id: str,
    provider_id: str,
    model_id: str,
    model_service: Any,
) -> dict[str, Any] | None:
    get_provider_model = getattr(model_service, "get_provider_model", None)
    if callable(get_provider_model):
        return await get_provider_model(tenant_id, provider_id, model_id)
    return await model_service.get_model(
        tenant_id,
        model_id,
        provider_id=provider_id,
    )


async def build_runtime_candidate(
    *,
    tenant_id: str,
    provider_id: str,
    model_id: str,
    temperature: Any,
    cache_epoch: Any,
    provider_service: Any,
    model_service: Any,
    error_prefix: str = "MODEL_OVERRIDE",
) -> dict[str, Any]:
    provider_id = str(provider_id or "").strip()
    model_id = str(model_id or "").strip()
    if not provider_id:
        raise ModelOverrideRuntimeError(f"{error_prefix}_PROVIDER_REQUIRED")
    if not model_id:
        raise ModelOverrideRuntimeError(f"{error_prefix}_MODEL_REQUIRED")

    try:
        provider = await provider_service.get_runtime_provider_config(tenant_id, provider_id)
    except ValueError as exc:
        raise ModelOverrideRuntimeError(f"{error_prefix}_PROVIDER_NOT_FOUND") from exc

    if not bool(provider.get("is_enabled")):
        raise ModelOverrideRuntimeError(f"{error_prefix}_PROVIDER_DISABLED")

    model = await _load_model(
        tenant_id=tenant_id,
        provider_id=provider_id,
        model_id=model_id,
        model_service=model_service,
    )
    if not model:
        raise ModelOverrideRuntimeError(f"{error_prefix}_MODEL_NOT_FOUND")
    if not bool(model.get("is_enabled")):
        raise ModelOverrideRuntimeError(f"{error_prefix}_MODEL_DISABLED")

    api_key = provider.get("api_key")
    if not api_key and not _allows_environment_credentials(provider):
        raise ModelOverrideRuntimeError(f"{error_prefix}_API_KEY_MISSING")

    runtime_provider = str(provider.get("runtime_provider") or "").strip()
    if not runtime_provider:
        raise ModelOverrideRuntimeError(f"{error_prefix}_PROVIDER_UNSUPPORTED")

    candidate = {
        "enabled": True,
        "tenant_id": tenant_id,
        "provider_id": provider_id,
        "provider": runtime_provider,
        "model_id": model_id,
        "model": model_id,
        "temperature": temperature,
        "base_url": provider.get("runtime_base_url"),
        "api_key_fingerprint": safe_api_key_fingerprint(api_key),
        "cache_epoch": str(cache_epoch or "0"),
    }
    candidate.update(_provider_runtime_metadata(provider))
    if api_key:
        candidate["_api_key"] = api_key
    return candidate


async def build_runtime_model_override_config(
    *,
    tenant_id: str,
    model_override: dict[str, Any],
    provider_service: Any,
    model_service: Any,
) -> dict[str, Any]:
    """Resolve stored model override config into a server-only runtime payload."""
    if not isinstance(model_override, dict) or not model_override.get("enabled"):
        raise ModelOverrideRuntimeError("MODEL_OVERRIDE_INVALID")

    cache_epoch = model_override.get("cache_epoch") or "0"
    temperature = model_override.get("temperature")
    primary = await build_runtime_candidate(
        tenant_id=tenant_id,
        provider_id=model_override.get("provider_id"),
        model_id=model_override.get("model_id"),
        temperature=temperature,
        cache_epoch=cache_epoch,
        provider_service=provider_service,
        model_service=model_service,
    )

    runtime_config = dict(primary)
    failover = model_override.get("failover")
    if not isinstance(failover, dict) or not failover.get("enabled"):
        return runtime_config

    max_attempts = normalize_max_attempts(failover.get("max_attempts"), default=3)
    candidates = [primary]
    warnings: list[dict[str, Any]] = []
    seen = {(primary["provider_id"], primary["model_id"])}

    raw_candidates = failover.get("candidates")
    if isinstance(raw_candidates, list):
        for raw_candidate in raw_candidates:
            if not isinstance(raw_candidate, dict):
                warnings.append({"code": "MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID"})
                continue
            provider_id = str(raw_candidate.get("provider_id") or "").strip()
            model_id = str(raw_candidate.get("model_id") or "").strip()
            warning_base = {"provider_id": provider_id or None, "model_id": model_id or None}
            if not provider_id or not model_id:
                warnings.append({**warning_base, "code": "MODEL_OVERRIDE_FAILOVER_CANDIDATE_INVALID"})
                continue
            key = (provider_id, model_id)
            if key in seen:
                warnings.append({**warning_base, "code": "MODEL_OVERRIDE_FAILOVER_DUPLICATE"})
                continue
            try:
                candidate = await build_runtime_candidate(
                    tenant_id=tenant_id,
                    provider_id=provider_id,
                    model_id=model_id,
                    temperature=temperature,
                    cache_epoch=cache_epoch,
                    provider_service=provider_service,
                    model_service=model_service,
                    error_prefix="MODEL_OVERRIDE_FAILOVER",
                )
            except ModelOverrideRuntimeError as exc:
                warnings.append({**warning_base, "code": exc.code})
                continue
            candidates.append(candidate)
            seen.add(key)

    runtime_config["failover"] = {
        "enabled": True,
        "max_attempts": normalize_failover_attempts(
            max_attempts,
            candidate_count=len(candidates),
            default=3,
        ),
        "candidates": candidates,
    }
    if warnings:
        runtime_config["failover"]["warnings"] = warnings

    return runtime_config
