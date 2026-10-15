"""Single-pass LangGraph run body preparation for transparent proxy requests."""

from __future__ import annotations

import asyncio
import copy
import hashlib
import json
import re
import time
from dataclasses import replace
from types import SimpleNamespace
from typing import Any

from ai_gateway_core.logging import get_logger
from fastapi import HTTPException, Request

from ..services.llm.model_failover import (
    ModelOverrideRuntimeError,
    build_runtime_model_override_config,
)
from .config_loader import ProxyServiceConfig

logger = get_logger(__name__)

_LANGGRAPH_RUN_RE = re.compile(
    r"/(runs|threads/[^/]+/runs)(/stream|/wait)?$",
    re.IGNORECASE,
)

LANGGRAPH_CALLER_CONFIGURABLE_BLOCKLIST = {
    "user_id",
    "tenant_id",
    "checkpoint_ns",
    "gateway_model",
    "_api_key",
    "api_key",
    "apikey",
    "api-key",
    "provider_api_key",
    "provider_credentials",
    "credential",
    "credentials",
    "password",
    "secret",
    "token",
    "auth_token",
    "access_token",
    "authorization",
    "service_id",
    "assistant_id",
    "thread_id",
}

_RuntimeOverrideCacheKey = tuple[str, str, str, str]
_RUNTIME_OVERRIDE_CACHE: dict[_RuntimeOverrideCacheKey, tuple[float, dict[str, Any]]] = {}
_RUNTIME_OVERRIDE_INFLIGHT: dict[_RuntimeOverrideCacheKey, asyncio.Task[dict[str, Any]]] = {}
_RUNTIME_OVERRIDE_CACHE_LOCK = asyncio.Lock()
_RUNTIME_OVERRIDE_CACHE_TTL_SECONDS = 60.0
_RUNTIME_OVERRIDE_CACHE_MAX_SIZE = 2048


def decode_json_body(body: bytes | None) -> Any | None:
    if not body:
        return None
    try:
        return json.loads(body.decode("utf-8"))
    except Exception:
        return None


def encode_json_body(payload: Any) -> bytes | None:
    if payload is None:
        return None
    return json.dumps(payload, ensure_ascii=False).encode("utf-8")


def normalize_domain_policy(value: Any) -> str:
    policy = str(value or "").strip().lower()
    if not policy or policy == "none":
        return "none"
    if len(policy) <= 64 and all(ch.isalnum() or ch in "._-" for ch in policy):
        return policy
    return "none"


def is_langgraph_proxy_service(service_config: ProxyServiceConfig | None) -> bool:
    if not service_config:
        return False
    service_meta = service_config.metadata if isinstance(service_config.metadata, dict) else {}
    adapter_type = str(service_meta.get("adapter_type") or "").strip().lower()
    return (
        adapter_type == "langgraph"
        or bool((service_config.assistant_id or "").strip())
        or bool((service_config.graph_id or "").strip())
    )


def is_langgraph_run_path(method: str, path: str) -> bool:
    if method.upper() not in {"POST", "PUT", "PATCH"}:
        return False
    return bool(_LANGGRAPH_RUN_RE.search(_normalize_path(path).lower()))


def should_prepare_langgraph_run_body(
    method: str,
    path: str,
    service_config: ProxyServiceConfig | None,
) -> bool:
    return bool(
        service_config
        and is_langgraph_proxy_service(service_config)
        and is_langgraph_run_path(method, path)
    )


def _normalize_path(path: str) -> str:
    normalized = (path or "").strip()
    if not normalized:
        return "/"
    if not normalized.startswith("/"):
        normalized = "/" + normalized
    return normalized


def _normalize_configurable_key(key: Any) -> str:
    return str(key or "").strip().lower().replace("-", "_")


def sanitize_langgraph_caller_configurable(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    sanitized: dict[str, Any] = {}
    for key, child in value.items():
        normalized = _normalize_configurable_key(key)
        if (
            normalized in LANGGRAPH_CALLER_CONFIGURABLE_BLOCKLIST
            or normalized.endswith("_api_key")
            or normalized.endswith("_token")
            or normalized.endswith("_secret")
            or normalized.endswith("_credentials")
        ):
            continue
        sanitized[key] = child
    return sanitized


def extract_langgraph_thread_id(path: str) -> str | None:
    segments = [segment for segment in str(path or "").strip("/").split("/") if segment]
    for index, segment in enumerate(segments):
        if segment.lower() != "threads":
            continue
        if index + 2 < len(segments) and segments[index + 2].lower() == "runs":
            return segments[index + 1]
    return None


def _current_trace_id(request: Request) -> str:
    from ..api.v1._route_trace import current_trace_id

    return current_trace_id(request)


def _stream_mode_wants_messages(stream_mode: Any) -> bool:
    if stream_mode is None:
        return False
    if isinstance(stream_mode, str):
        modes = [mode.strip() for mode in stream_mode.split(",") if mode.strip()]
    elif isinstance(stream_mode, list):
        modes = [str(mode).strip() for mode in stream_mode if str(mode).strip()]
    else:
        return False
    return any(mode in ("messages", "messages-tuple") for mode in modes)


def _is_streaming_run_path(path: str) -> bool:
    normalized_path = _normalize_path(path).lower()
    if "/runs/stream" not in normalized_path:
        return False
    return any(
        normalized_path.endswith(suffix) for suffix in ("/stream", "/runs/stream", "/sse")
    )


def _needs_assistant_id_injection(path: str) -> bool:
    return bool(_LANGGRAPH_RUN_RE.search(_normalize_path(path).lower()))


def resolve_domain_policy(
    *,
    service_config: ProxyServiceConfig | None = None,
    assistant_payload: dict[str, Any] | None = None,
) -> str:
    """Service registration metadata wins; assistant metadata is the fallback."""
    if service_config:
        service_meta = service_config.metadata if isinstance(service_config.metadata, dict) else {}
        policy = normalize_domain_policy(service_meta.get("domain_policy"))
        if policy != "none":
            return policy
    if isinstance(assistant_payload, dict):
        assistant_meta = assistant_payload.get("metadata")
        if isinstance(assistant_meta, dict):
            policy = normalize_domain_policy(assistant_meta.get("domain_policy"))
            if policy != "none":
                return policy
    return "none"


def apply_domain_policy_metadata(
    payload: dict[str, Any],
    *,
    service_config: ProxyServiceConfig | None,
    assistant_payload: dict[str, Any] | None = None,
) -> bool:
    domain_policy = resolve_domain_policy(
        service_config=service_config,
        assistant_payload=assistant_payload,
    )
    if domain_policy == "none":
        return False

    run_metadata = payload.get("metadata")
    metadata_dict = dict(run_metadata) if isinstance(run_metadata, dict) else {}
    gateway_meta = metadata_dict.get("gateway")
    gateway_dict = dict(gateway_meta) if isinstance(gateway_meta, dict) else {}
    if gateway_dict.get("domain_policy"):
        return False
    gateway_dict["domain_policy"] = domain_policy
    metadata_dict["gateway"] = gateway_dict
    payload["metadata"] = metadata_dict
    return True


def merge_gateway_domain_policy_metadata(
    *,
    metadata: dict[str, Any] | None,
    service_config: ProxyServiceConfig | None = None,
    assistant_payload: dict[str, Any] | None = None,
) -> dict[str, Any] | None:
    """Merge domain policy into run metadata without mutating caller-owned dicts."""
    domain_policy = resolve_domain_policy(
        service_config=service_config,
        assistant_payload=assistant_payload,
    )
    if domain_policy == "none" and not metadata:
        return None

    payload: dict[str, Any] = {"metadata": dict(metadata) if isinstance(metadata, dict) else {}}
    if apply_domain_policy_metadata(
        payload,
        service_config=service_config,
        assistant_payload=assistant_payload,
    ):
        merged = payload.get("metadata")
        return dict(merged) if isinstance(merged, dict) else None
    return dict(metadata) if isinstance(metadata, dict) else None


def scrub_caller_gateway_model(payload: dict[str, Any]) -> bool:
    """Remove caller-supplied gateway_model from a parsed run payload."""
    return _scrub_gateway_model(payload)


def _scrub_gateway_model(payload: dict[str, Any]) -> bool:
    run_config = payload.get("config")
    if not isinstance(run_config, dict):
        return False
    updated_config = dict(run_config)
    configurable = updated_config.get("configurable")
    if not isinstance(configurable, dict) or "gateway_model" not in configurable:
        return False
    updated_config["configurable"] = {
        key: value for key, value in configurable.items() if key != "gateway_model"
    }
    payload["config"] = updated_config
    return True


def _apply_gateway_configurable(
    payload: dict[str, Any],
    *,
    request: Request,
    path: str,
    user: Any,
    auth: Any,
) -> bool:
    run_config = payload.get("config")
    original_config = dict(run_config) if isinstance(run_config, dict) else {}
    updated_config = dict(original_config)
    previous_configurable = sanitize_langgraph_caller_configurable(
        updated_config.get("configurable")
    )
    previous_metadata = (
        dict(updated_config.get("metadata"))
        if isinstance(updated_config.get("metadata"), dict)
        else {}
    )

    configurable = dict(previous_configurable)
    gateway_user_id = str(
        getattr(auth, "user_id", "") or getattr(user, "user_id", "") or "anonymous"
    )
    gateway_tenant_id = str(
        getattr(auth, "tenant_id", "") or getattr(user, "tenant_id", "") or "default"
    )
    configurable["user_id"] = gateway_user_id
    configurable["tenant_id"] = gateway_tenant_id
    configurable["checkpoint_ns"] = gateway_tenant_id
    user_tier = str(getattr(user, "tier", "") or "").strip()
    if user_tier:
        configurable["user_tier"] = user_tier

    thread_id = extract_langgraph_thread_id(path)
    if thread_id:
        configurable["thread_id"] = thread_id
    else:
        fallback_thread_id = str(
            payload.get("thread_id")
            or getattr(request.state, "thread_id", "")
            or getattr(request.state, "request_id", "")
            or getattr(request.state, "trace_id", "")
        ).strip()
        if fallback_thread_id:
            configurable["thread_id"] = fallback_thread_id

    metadata = dict(previous_metadata)
    request_id = str(getattr(request.state, "request_id", "") or _current_trace_id(request))
    trace_id = str(getattr(request.state, "trace_id", "") or _current_trace_id(request))
    if request_id:
        metadata["gateway_request_id"] = request_id
    if trace_id:
        metadata["gateway_trace_id"] = trace_id

    if configurable == previous_configurable and metadata == previous_metadata:
        return False

    updated_config["configurable"] = configurable
    updated_config["metadata"] = metadata
    if updated_config == original_config:
        return False
    payload["config"] = updated_config
    return True


def _apply_assistant_id(
    payload: dict[str, Any],
    *,
    path: str,
    assistant_id: str,
) -> bool:
    if not assistant_id or not _needs_assistant_id_injection(path):
        return False
    if payload.get("assistant_id") == assistant_id:
        return False
    payload["assistant_id"] = assistant_id
    return True


def _apply_stream_defaults(payload: dict[str, Any], *, path: str) -> bool:
    if not _is_streaming_run_path(path):
        return False

    changed = False
    if not payload.get("stream_mode"):
        payload["stream_mode"] = ["messages", "updates", "custom"]
        changed = True
    if "stream_subgraphs" not in payload and _stream_mode_wants_messages(payload.get("stream_mode")):
        payload["stream_subgraphs"] = True
        changed = True
    return changed


def prepare_langgraph_run_payload(
    payload: dict[str, Any],
    *,
    method: str,
    path: str,
    request: Request,
    user: Any,
    auth: Any | None = None,
    service_config: ProxyServiceConfig | None,
    assistant_payload: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], bool]:
    """Apply synchronous LangGraph run mutations to an already-parsed payload."""
    if not should_prepare_langgraph_run_body(method, path, service_config):
        return payload, False

    assert service_config is not None
    auth = auth if auth is not None else user
    changed = False
    changed = (
        apply_domain_policy_metadata(
            payload,
            service_config=service_config,
            assistant_payload=assistant_payload,
        )
        or changed
    )
    changed = _scrub_gateway_model(payload) or changed
    changed = _apply_gateway_configurable(
        payload,
        request=request,
        path=path,
        user=user,
        auth=auth,
    ) or changed

    assistant_id = str(service_config.assistant_id or "").strip()
    if assistant_id:
        changed = _apply_assistant_id(payload, path=path, assistant_id=assistant_id) or changed
    changed = _apply_stream_defaults(payload, path=path) or changed
    return payload, changed


def build_control_plane_request(
    *,
    provider_service: Any,
    model_service: Any,
    request_id: str = "",
    trace_id: str = "",
) -> SimpleNamespace:
    """Minimal request-like object for model override resolution outside FastAPI routes."""
    state = SimpleNamespace(
        provider_service=provider_service,
        model_service=model_service,
        request_id=request_id,
        trace_id=trace_id,
    )
    return SimpleNamespace(app=SimpleNamespace(state=state), state=state)


def proxy_service_config_from_connector(
    connector_config: dict[str, Any],
    *,
    service_id: str,
    service_name: str,
    assistant_id: str | None = None,
    metadata: dict[str, Any] | None = None,
) -> ProxyServiceConfig:
    meta = dict(metadata or {})
    meta.setdefault("adapter_type", "langgraph")
    return ProxyServiceConfig(
        service_id=service_id,
        service_name=service_name,
        upstream_url=str(
            connector_config.get("base_url") or connector_config.get("upstream_url") or ""
        ),
        assistant_id=assistant_id or connector_config.get("assistant_id"),
        graph_id=connector_config.get("graph_id"),
        model_override=connector_config.get("model_override"),
        metadata=meta,
    )


async def resolve_langgraph_service_config(
    request: Request,
    *,
    assistant_id: str | None = None,
) -> ProxyServiceConfig:
    """Resolve proxy service config for typed LangGraph routes and passthrough."""
    loader = getattr(request.app.state, "proxy_config_loader", None)
    candidates: list[str] = []
    if assistant_id:
        candidates.append(str(assistant_id).strip())
    candidates.extend(("langgraph", "langgraph-agent"))

    base: ProxyServiceConfig | None = None
    if loader is not None:
        for name in candidates:
            if not name:
                continue
            loaded = await loader.get_config(name)
            if loaded is not None:
                base = loaded
                break

    if base is None:
        base = ProxyServiceConfig(
            service_id="langgraph",
            service_name="langgraph",
            upstream_url="",
            metadata={"adapter_type": "langgraph"},
        )

    effective_assistant_id = str(assistant_id or base.assistant_id or "").strip() or None
    if effective_assistant_id and base.assistant_id != effective_assistant_id:
        return replace(base, assistant_id=effective_assistant_id)
    return base


async def finalize_langgraph_run_payload(
    *,
    request: Request,
    payload: dict[str, Any],
    service_config: ProxyServiceConfig | None,
    tenant_id: str,
) -> bool:
    """Resolve and inject server-side gateway_model into a prepared run payload."""
    if service_config is None:
        return False
    runtime_config = await resolve_langgraph_model_override(
        request=request,
        service_config=service_config,
        tenant_id=tenant_id or "default",
    )
    if runtime_config is None:
        return False
    inject_resolved_model_override(payload, runtime_config)
    return True


async def prepare_and_finalize_langgraph_run_payload(
    payload: dict[str, Any],
    *,
    method: str,
    path: str,
    request: Request,
    user: Any,
    auth: Any | None = None,
    service_config: ProxyServiceConfig | None = None,
    assistant_id: str | None = None,
    assistant_payload: dict[str, Any] | None = None,
    tenant_id: str | None = None,
) -> dict[str, Any]:
    """Single canonical LangGraph run preparation for all gateway entry points."""
    from .langgraph_governance import apply_langgraph_run_governance

    resolved_assistant_id = str(
        assistant_id or payload.get("assistant_id") or ""
    ).strip() or None
    if service_config is None:
        service_config = await resolve_langgraph_service_config(
            request,
            assistant_id=resolved_assistant_id,
        )
    elif resolved_assistant_id and service_config.assistant_id != resolved_assistant_id:
        service_config = replace(service_config, assistant_id=resolved_assistant_id)

    effective_tenant = tenant_id or str(getattr(user, "tenant_id", "") or "default")
    prepare_langgraph_run_payload(
        payload,
        method=method,
        path=path,
        request=request,
        user=user,
        auth=auth,
        service_config=service_config,
        assistant_payload=assistant_payload,
    )
    await apply_langgraph_run_governance(
        request=request,
        user=user,
        payload=payload,
        path=path,
        service_config=service_config,
        auth=auth,
        tenant_id=effective_tenant,
    )
    return payload


async def prepare_langgraph_run_body_for_passthrough(
    body: bytes | None,
    *,
    method: str,
    path: str,
    request: Request,
    user: Any,
    auth: Any | None = None,
    assistant_payload: dict[str, Any] | None = None,
) -> bytes | None:
    """Decode, prepare, finalize, and re-encode LangGraph run bodies for passthrough."""
    if not is_langgraph_run_path(method, path):
        return body

    payload = decode_json_body(body)
    if not isinstance(payload, dict):
        return body

    service_config = await resolve_langgraph_service_config(
        request,
        assistant_id=str(payload.get("assistant_id") or "").strip() or None,
    )
    from .langgraph_governance import apply_langgraph_run_governance

    _, sync_changed = prepare_langgraph_run_payload(
        payload,
        method=method,
        path=path,
        request=request,
        user=user,
        auth=auth,
        service_config=service_config,
        assistant_payload=assistant_payload,
    )
    _, _, governance_changed = await apply_langgraph_run_governance(
        request=request,
        user=user,
        payload=payload,
        path=path,
        service_config=service_config,
        auth=auth,
        tenant_id=str(getattr(user, "tenant_id", "") or "default"),
    )
    if sync_changed or governance_changed:
        return encode_json_body(payload)
    return body


def prepare_langgraph_run_body(
    *,
    body: bytes | None,
    method: str,
    path: str,
    request: Request,
    user: Any,
    auth: Any,
    service_config: ProxyServiceConfig | None,
) -> tuple[bytes | None, dict[str, Any] | None, bool]:
    """Decode once, apply synchronous LangGraph run mutations; encode only when changed."""
    if not should_prepare_langgraph_run_body(method, path, service_config):
        parsed = decode_json_body(body)
        return body, parsed if isinstance(parsed, dict) else None, False

    payload = decode_json_body(body)
    if not isinstance(payload, dict):
        return body, None, False

    assert service_config is not None
    payload, changed = prepare_langgraph_run_payload(
        payload,
        method=method,
        path=path,
        request=request,
        user=user,
        auth=auth,
        service_config=service_config,
    )
    return body if not changed else encode_json_body(payload), payload, changed


def _model_override_signature(model_override: dict[str, Any]) -> str:
    canonical = {
        "provider_id": model_override.get("provider_id"),
        "model_id": model_override.get("model_id"),
        "temperature": model_override.get("temperature"),
        "failover": model_override.get("failover"),
    }
    digest = hashlib.sha256(
        json.dumps(canonical, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    return digest[:16]


def _runtime_override_cache_key(
    *,
    tenant_id: str,
    service_config: ProxyServiceConfig,
    model_override: dict[str, Any],
) -> tuple[str, str, str, str]:
    service_id = str(service_config.service_id or service_config.service_name or "").strip()
    cache_epoch = str(model_override.get("cache_epoch") or "0")
    return (
        tenant_id or "default",
        service_id,
        cache_epoch,
        _model_override_signature(model_override),
    )


async def _build_runtime_model_override_config(
    *,
    request: Request,
    tenant_id: str,
    service_config: ProxyServiceConfig,
    model_override: dict[str, Any],
) -> dict[str, Any]:
    provider_service = getattr(request.app.state, "provider_service", None)
    model_service = getattr(request.app.state, "model_service", None)
    if provider_service is None or model_service is None:
        raise HTTPException(
            status_code=503,
            detail="MODEL_OVERRIDE_CONTROL_PLANE_UNAVAILABLE",
        )
    try:
        return await build_runtime_model_override_config(
            tenant_id=tenant_id or "default",
            model_override=model_override,
            provider_service=provider_service,
            model_service=model_service,
        )
    except ModelOverrideRuntimeError as exc:
        raise HTTPException(status_code=422, detail=exc.code) from exc


async def _get_runtime_model_override_config(
    *,
    request: Request,
    tenant_id: str,
    service_config: ProxyServiceConfig,
    model_override: dict[str, Any],
) -> dict[str, Any]:
    cache_key = _runtime_override_cache_key(
        tenant_id=tenant_id,
        service_config=service_config,
        model_override=model_override,
    )
    now = time.monotonic()
    async with _RUNTIME_OVERRIDE_CACHE_LOCK:
        cached = _RUNTIME_OVERRIDE_CACHE.get(cache_key)
        if cached and (now - cached[0]) <= _RUNTIME_OVERRIDE_CACHE_TTL_SECONDS:
            return copy.deepcopy(cached[1])
        inflight = _RUNTIME_OVERRIDE_INFLIGHT.get(cache_key)
        if inflight is None:
            inflight = asyncio.create_task(
                _build_runtime_model_override_config(
                    request=request,
                    tenant_id=tenant_id,
                    service_config=service_config,
                    model_override=model_override,
                )
            )
            _RUNTIME_OVERRIDE_INFLIGHT[cache_key] = inflight

    try:
        runtime_config = await inflight
    finally:
        async with _RUNTIME_OVERRIDE_CACHE_LOCK:
            current = _RUNTIME_OVERRIDE_INFLIGHT.get(cache_key)
            if current is inflight:
                _RUNTIME_OVERRIDE_INFLIGHT.pop(cache_key, None)

    async with _RUNTIME_OVERRIDE_CACHE_LOCK:
        if cache_key not in _RUNTIME_OVERRIDE_CACHE:
            while len(_RUNTIME_OVERRIDE_CACHE) >= _RUNTIME_OVERRIDE_CACHE_MAX_SIZE:
                _RUNTIME_OVERRIDE_CACHE.pop(next(iter(_RUNTIME_OVERRIDE_CACHE)))
            _RUNTIME_OVERRIDE_CACHE[cache_key] = (now, copy.deepcopy(runtime_config))

    return copy.deepcopy(runtime_config)


async def resolve_langgraph_model_override(
    *,
    request: Request,
    service_config: ProxyServiceConfig | None,
    tenant_id: str,
) -> dict[str, Any] | None:
    """Resolve server-side runtime model config without mutating the run payload."""
    if not service_config:
        return None

    model_override = service_config.model_override
    if not isinstance(model_override, dict) or not model_override.get("enabled"):
        return None

    return await _get_runtime_model_override_config(
        request=request,
        tenant_id=tenant_id,
        service_config=service_config,
        model_override=model_override,
    )


def inject_resolved_model_override(
    payload: dict[str, Any],
    runtime_config: dict[str, Any],
) -> None:
    run_config = payload.get("config")
    updated_config = dict(run_config) if isinstance(run_config, dict) else {}
    configurable = updated_config.get("configurable")
    updated_config["configurable"] = dict(configurable) if isinstance(configurable, dict) else {}
    updated_config["configurable"]["gateway_model"] = runtime_config
    payload["config"] = updated_config

    logger.info(
        "Injected LangGraph model override provider_id=%s model_id=%s cache_epoch=%s "
        "api_key_fingerprint=%s failover_candidates=%s failover_warnings=%s",
        runtime_config.get("provider_id"),
        runtime_config.get("model_id"),
        runtime_config["cache_epoch"],
        runtime_config.get("api_key_fingerprint"),
        len((runtime_config.get("failover") or {}).get("candidates") or []),
        len((runtime_config.get("failover") or {}).get("warnings") or []),
    )


async def apply_langgraph_model_override(
    *,
    request: Request,
    payload: dict[str, Any] | None,
    service_config: ProxyServiceConfig | None,
    tenant_id: str,
) -> None:
    if payload is None:
        return

    runtime_config = await resolve_langgraph_model_override(
        request=request,
        service_config=service_config,
        tenant_id=tenant_id,
    )
    if runtime_config is None:
        return

    inject_resolved_model_override(payload, runtime_config)


def apply_quota_model_downgrade(
    payload: dict[str, Any],
    *,
    downgraded_model: str | None,
    requested_model: str | None,
) -> bool:
    """Apply quota downgrade hints to gateway_model when server override is injected."""
    model = str(downgraded_model or "").strip()
    if not model or model == str(requested_model or "").strip():
        return False

    run_config = payload.get("config")
    if not isinstance(run_config, dict):
        return False
    configurable = run_config.get("configurable")
    if not isinstance(configurable, dict):
        return False
    gateway_model = configurable.get("gateway_model")
    if not isinstance(gateway_model, dict):
        return False

    if gateway_model.get("model_id") == model and gateway_model.get("model") == model:
        return False

    gateway_model["model_id"] = model
    gateway_model["model"] = model
    return True


_GATEWAY_MODEL_BILLING_HINT_KEYS = (
    "model_id",
    "model",
    "provider_id",
    "provider",
    "cache_epoch",
    "api_key_fingerprint",
)


def redact_gateway_model_for_billing(gateway_model: Any) -> dict[str, Any] | None:
    """Keep attribution hints while stripping runtime secrets from gateway_model."""
    if not isinstance(gateway_model, dict):
        return None
    hints = {
        key: gateway_model[key]
        for key in _GATEWAY_MODEL_BILLING_HINT_KEYS
        if key in gateway_model and gateway_model[key] is not None
    }
    return hints or None


def billing_request_snapshot(payload: dict[str, Any] | None) -> dict[str, Any] | None:
    """Redacted request snapshot for capacity/billing hints (no gateway_model secrets)."""
    if payload is None:
        return None
    snapshot = dict(payload)
    run_config = payload.get("config")
    if isinstance(run_config, dict):
        updated_config = dict(run_config)
        configurable = run_config.get("configurable")
        if isinstance(configurable, dict):
            updated_configurable = dict(configurable)
            redacted_hints = redact_gateway_model_for_billing(configurable.get("gateway_model"))
            updated_configurable.pop("gateway_model", None)
            if redacted_hints:
                updated_configurable["gateway_model"] = redacted_hints
            updated_config["configurable"] = updated_configurable
        snapshot["config"] = updated_config
    return snapshot


def build_minimal_langgraph_prep_request() -> SimpleNamespace:
    """Request-like object for sync LangGraph prep when no FastAPI request is available."""
    return build_control_plane_request(
        provider_service=None,
        model_service=None,
    )


def synthetic_langgraph_service_config(
    *,
    assistant_id: str | None = None,
) -> ProxyServiceConfig:
    effective_assistant_id = str(assistant_id or "").strip() or None
    return ProxyServiceConfig(
        service_id="langgraph",
        service_name="langgraph",
        upstream_url="",
        assistant_id=effective_assistant_id,
        metadata={"adapter_type": "langgraph"},
    )


def inject_domain_policy_metadata_bytes(
    *,
    body: bytes | None,
    method: str,
    path: str,
    service_config: ProxyServiceConfig | None,
) -> bytes | None:
    """Inject service-level domain policy into a LangGraph run body."""
    if not should_prepare_langgraph_run_body(method, path, service_config):
        return body
    payload = decode_json_body(body)
    if not isinstance(payload, dict) or service_config is None:
        return body
    if not apply_domain_policy_metadata(payload, service_config=service_config):
        return body
    return encode_json_body(payload)


def inject_langgraph_gateway_configurable_bytes(
    *,
    body: bytes | None,
    method: str,
    path: str,
    request: Request,
    user: Any,
    auth: Any,
    service_config: ProxyServiceConfig | None = None,
) -> bytes | None:
    """Apply synchronous LangGraph gateway configurable injection to a run body."""
    if not body or not is_langgraph_run_path(method, path):
        return body

    payload = decode_json_body(body)
    if not isinstance(payload, dict):
        return body

    resolved_config = service_config or synthetic_langgraph_service_config()
    _, changed = prepare_langgraph_run_payload(
        payload,
        method=method,
        path=path,
        request=request,
        user=user,
        auth=auth,
        service_config=resolved_config,
    )
    return encode_json_body(payload) if changed else body


async def inject_langgraph_model_override_bytes(
    *,
    body: bytes | None,
    method: str,
    path: str,
    request: Request,
    service_config: ProxyServiceConfig | None,
    tenant_id: str,
) -> bytes | None:
    """Scrub caller gateway_model and inject server-side runtime override into a run body."""
    if not body or not is_langgraph_run_path(method, path):
        return body

    payload = decode_json_body(body)
    if not isinstance(payload, dict):
        return body

    scrub_changed = scrub_caller_gateway_model(payload)
    if (
        not service_config
        or not should_prepare_langgraph_run_body(method, path, service_config)
    ):
        return encode_json_body(payload) if scrub_changed else body

    before = json.dumps(payload, sort_keys=True)
    await apply_langgraph_model_override(
        request=request,
        payload=payload,
        service_config=service_config,
        tenant_id=tenant_id,
    )
    after = json.dumps(payload, sort_keys=True)
    if scrub_changed or before != after:
        return encode_json_body(payload)
    return body


def clear_runtime_model_override_cache() -> None:
    """Test helper to reset the in-process runtime override cache."""
    _RUNTIME_OVERRIDE_CACHE.clear()
    _RUNTIME_OVERRIDE_INFLIGHT.clear()
