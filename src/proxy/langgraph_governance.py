"""Shared LangGraph run governance: model allowlist, quota, and billing hints."""

from __future__ import annotations

import asyncio
import contextlib
import json
import time
from typing import Any

from ai_gateway_core.logging import get_logger
from fastapi import HTTPException, Request

from ..api.deps import AuthContext
from ..core.auth.user_resolver import UserContext
from ..core.utils import estimate_tokens
from ..services.billing import get_quota_service
from ..services.billing.quota_service import OverageStrategy, QuotaStatus
from ..services.metrics.usage_parser import extract_model, extract_provider
from .config_loader import ProxyServiceConfig
from .langgraph_run_body import (
    apply_quota_model_downgrade,
    inject_resolved_model_override,
    resolve_langgraph_model_override,
)

logger = get_logger(__name__)


def resolve_auth_context(request: Request, user: UserContext) -> AuthContext:
    """Resolve AuthContext from request cache or user fields."""
    cached = getattr(request.state, "auth", None)
    if isinstance(cached, AuthContext):
        return cached
    return AuthContext(
        user_id=user.user_id or "",
        tenant_id=user.tenant_id or "",
        roles=list(user.roles or []),
        permissions=[],
        is_authenticated=user.is_authenticated,
    )


def normalize_allowed_models(value: Any) -> list[str]:
    if not value:
        return []
    if isinstance(value, str):
        return [value.strip()] if value.strip() else []
    if isinstance(value, list):
        return [str(v).strip() for v in value if str(v).strip()]
    return []


def is_model_allowed(allowed_models: list[str], model: str) -> bool:
    if not allowed_models:
        return True
    normalized_model = (model or "").strip().lower()
    if not normalized_model:
        return False
    for allowed in allowed_models:
        normalized_allowed = allowed.strip().lower()
        if not normalized_allowed:
            continue
        if normalized_allowed == "*":
            return True
        if normalized_allowed.endswith("*"):
            if normalized_model.startswith(normalized_allowed[:-1]):
                return True
            continue
        if normalized_model == normalized_allowed:
            return True
    return False


def estimate_tokens_from_payload(payload: Any) -> int:
    """Best-effort request token estimate for pre-quota checks."""
    if payload is None:
        return 0
    if isinstance(payload, str):
        return estimate_tokens(payload)
    if isinstance(payload, (list, dict)):
        text_like_keys = {
            "prompt",
            "query",
            "question",
            "instruction",
            "text",
            "content",
            "message",
            "messages",
            "input",
        }

        def _walk(node: Any, parent_key: str = "") -> int:
            if isinstance(node, str):
                return estimate_tokens(node) if parent_key in text_like_keys else 0
            if isinstance(node, list):
                return sum(_walk(item, parent_key) for item in node)
            if isinstance(node, dict):
                subtotal = 0
                for key, value in node.items():
                    key_name = str(key).strip().lower()
                    if isinstance(value, str):
                        if key_name in text_like_keys:
                            subtotal += estimate_tokens(value)
                    else:
                        subtotal += _walk(value, key_name)
                return subtotal
            return 0

        total = _walk(payload)
    else:
        total = 0

    if total <= 0:
        raw_size = len(json.dumps(payload, ensure_ascii=False))
        total = max(raw_size // 4, 1)
    return total


def override_model_in_request_payload(payload: Any, model: str) -> Any:
    if not isinstance(payload, dict):
        return payload
    updated = dict(payload)
    updated["model"] = model
    if isinstance(updated.get("input"), dict):
        input_payload = dict(updated["input"])
        input_payload["model"] = model
        updated["input"] = input_payload
    if isinstance(updated.get("config"), dict):
        config_payload = dict(updated["config"])
        configurable = config_payload.get("configurable")
        if isinstance(configurable, dict):
            new_configurable = dict(configurable)
            new_configurable["model"] = model
            config_payload["configurable"] = new_configurable
        updated["config"] = config_payload
    return updated


def should_apply_quota_policy(method: str, operation: str, path: str) -> bool:
    if method.upper() not in {"POST", "PUT", "PATCH"}:
        return False
    if operation.startswith("run_"):
        return True
    normalized_path = path.lower()
    return "/runs" in normalized_path


def quota_check_failure_mode(request: Request) -> str:
    settings = getattr(request.app.state, "settings", None)
    configured = getattr(
        getattr(settings, "proxy", None),
        "quota_check_failure_mode",
        "fail_open",
    )
    mode = str(configured or "fail_open").strip().lower()
    return mode if mode in {"fail_open", "fail_closed"} else "fail_open"


def quota_retry_after_seconds(check: Any) -> int:
    try:
        retry_after = int(getattr(check, "retry_after_seconds", 0) or 0)
    except Exception:
        retry_after = 0
    return retry_after if retry_after > 0 else 60


def _current_trace_id(request: Request) -> str:
    from ..api.v1._route_trace import current_trace_id

    return current_trace_id(request)


async def record_security_event(
    event_type: str,
    tenant_id: str,
    user_id: str,
    service_id: str | None,
    metadata: dict[str, Any] | None = None,
) -> None:
    try:
        from ..services.metrics import get_security_event_recorder

        recorder = get_security_event_recorder()
        await recorder.record_event(
            tenant_id=tenant_id or "public",
            user_id=user_id or None,
            service_id=service_id,
            event_type=event_type,
            metadata=metadata,
        )
    except Exception:
        pass


def quota_security_metadata(request: Request, check: Any) -> dict[str, Any]:
    return {
        "policy": str(getattr(getattr(check, "overage_strategy", None), "value", "") or ""),
        "status": str(getattr(getattr(check, "status", None), "value", "") or ""),
        "request_id": _current_trace_id(request),
        "message": str(getattr(check, "message", "") or ""),
    }


async def record_quota_exceeded_decision(
    *,
    request: Request,
    auth: AuthContext,
    user: UserContext,
    service_name: str,
    check: Any,
) -> None:
    await record_security_event(
        event_type="quota_exceeded",
        tenant_id=auth.tenant_id or user.tenant_id,
        user_id=user.user_id or auth.user_id,
        service_id=service_name,
        metadata=quota_security_metadata(request, check),
    )


def should_create_quota_alert(
    *,
    request: Request,
    tenant_id: str,
    user_id: str,
    check: Any,
) -> bool:
    settings = getattr(request.app.state, "settings", None)
    ttl = getattr(
        getattr(settings, "proxy", None),
        "quota_alert_dedupe_ttl_seconds",
        60,
    )
    try:
        ttl_seconds = max(float(ttl or 0), 0.0)
    except Exception:
        ttl_seconds = 60.0
    if ttl_seconds <= 0:
        return True

    cache = getattr(request.app.state, "_quota_alert_cache", None)
    if not isinstance(cache, dict):
        cache = {}
        request.app.state._quota_alert_cache = cache

    now = time.monotonic()
    expired = [key for key, expires_at in cache.items() if float(expires_at or 0) <= now]
    for key in expired:
        cache.pop(key, None)

    key = "|".join(
        (
            tenant_id,
            user_id,
            str(getattr(getattr(check, "overage_strategy", None), "value", "") or ""),
            str(getattr(check, "message", "") or ""),
            str(getattr(check, "daily_tokens_limit", "") or ""),
            str(getattr(check, "monthly_cost_limit", "") or ""),
        )
    )
    if key in cache:
        return False
    cache[key] = now + ttl_seconds
    return True


async def enforce_model_allowlist(
    request: Request,
    service_name: str,
    user: UserContext,
    auth: AuthContext,
    model: str | None,
) -> None:
    api_key_info = getattr(request.state, "api_key_info", None)
    if not api_key_info:
        return
    allowed_models = normalize_allowed_models(api_key_info.get("allowed_models"))
    if not allowed_models:
        return
    resolved_model = (model or "").strip()
    if is_model_allowed(allowed_models, resolved_model):
        return

    await record_security_event(
        event_type="auth_failed",
        tenant_id=auth.tenant_id or user.tenant_id or "public",
        user_id=user.user_id or auth.user_id or "anonymous",
        service_id=service_name,
    )
    raise HTTPException(
        status_code=403,
        detail=f"Permission denied: model '{resolved_model or '<empty>'}' not allowed for this API key",
    )


async def apply_quota_policy(
    *,
    request: Request,
    user: UserContext,
    auth: AuthContext,
    service_name: str,
    operation: str,
    path: str,
    body: bytes | None,
    model_hint: str | None,
    payload: Any | None = None,
    defer_encode: bool = False,
) -> tuple[bytes | None, str | None, bool]:
    """
    Enforce quota governance policy before proxying.

    Returns:
        (possibly mutated body, final model to use, payload mutated flag)
    """
    method = str(getattr(request, "method", "POST") or "POST")
    if not should_apply_quota_policy(method, operation, path):
        return (
            (None, model_hint, False)
            if defer_encode
            else (body, model_hint, False)
        )

    quota_service = get_quota_service()
    if not quota_service or not quota_service.database:
        return (
            (None, model_hint, False)
            if defer_encode
            else (body, model_hint, False)
        )

    if payload is None:
        if not body:
            payload = None
        else:
            try:
                payload = json.loads(body.decode("utf-8"))
            except Exception:
                payload = None
    estimated_tokens = estimate_tokens_from_payload(payload)

    try:
        check = await quota_service.check_quota(
            tenant_id=auth.tenant_id or user.tenant_id or "default",
            user_id=user.user_id or auth.user_id or "anonymous",
            estimated_tokens=estimated_tokens,
            record_security_event=False,
        )
    except Exception as exc:
        failure_mode = quota_check_failure_mode(request)
        logger.warning(
            "[ProxyQuota] quota check failed, mode=%s: %s",
            failure_mode,
            exc,
        )
        if failure_mode == "fail_closed":
            await record_security_event(
                event_type="quota_check_failed",
                tenant_id=auth.tenant_id or user.tenant_id,
                user_id=user.user_id or auth.user_id,
                service_id=service_name,
                metadata={
                    "request_id": _current_trace_id(request),
                    "mode": failure_mode,
                    "error": str(exc)[:256],
                },
            )
            raise HTTPException(
                status_code=503,
                detail={
                    "code": "QUOTA_CHECK_UNAVAILABLE",
                    "message": "Quota check failed and fail-closed mode is enabled",
                    "policy": failure_mode,
                },
            ) from exc
        return (
            (None, model_hint, False)
            if defer_encode
            else (body, model_hint, False)
        )

    payload_mutated = False
    if check.status == QuotaStatus.BLOCKED:
        await record_quota_exceeded_decision(
            request=request,
            auth=auth,
            user=user,
            service_name=service_name,
            check=check,
        )
        raise HTTPException(
            status_code=403,
            detail={
                "message": check.message,
                "status": check.status.value,
                "policy": check.overage_strategy.value,
            },
        )

    if not check.can_proceed:
        status_code = 429 if check.overage_strategy == OverageStrategy.RATE_LIMIT else 403
        await record_quota_exceeded_decision(
            request=request,
            auth=auth,
            user=user,
            service_name=service_name,
            check=check,
        )
        raise HTTPException(
            status_code=status_code,
            detail={
                "message": check.message,
                "status": check.status.value,
                "policy": check.overage_strategy.value,
            },
            headers={"Retry-After": str(quota_retry_after_seconds(check))}
            if status_code == 429
            else None,
        )

    final_model = (model_hint or "").strip() or None
    if check.status == QuotaStatus.EXCEEDED:
        await record_quota_exceeded_decision(
            request=request,
            auth=auth,
            user=user,
            service_name=service_name,
            check=check,
        )
        if check.overage_strategy == OverageStrategy.DOWNGRADE_MODEL and check.downgraded_model:
            downgraded_model = check.downgraded_model.strip()
            if downgraded_model and payload is not None:
                downgraded_payload = override_model_in_request_payload(payload, downgraded_model)
                if defer_encode and isinstance(payload, dict) and isinstance(downgraded_payload, dict):
                    payload.clear()
                    payload.update(downgraded_payload)
                else:
                    payload = downgraded_payload
                payload_mutated = True
                if not defer_encode:
                    body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
            final_model = downgraded_model
        elif check.overage_strategy == OverageStrategy.ALLOW_BUT_ALERT:
            with contextlib.suppress(Exception):
                tenant_id = auth.tenant_id or user.tenant_id or "default"
                user_id = user.user_id or auth.user_id or "anonymous"
                if should_create_quota_alert(
                    request=request,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    check=check,
                ):
                    await quota_service.create_alert(
                        tenant_id=tenant_id,
                        user_id=user_id,
                        alert_type="quota_allow_but_alert",
                        threshold_value=check.warning_threshold,
                        current_value=check.daily_tokens_used,
                        limit_value=check.daily_tokens_limit or 0,
                        message=check.message,
                    )

    if defer_encode:
        return None, final_model, payload_mutated
    return body, final_model, payload_mutated


def resolve_effective_provider(
    parsed_body: dict[str, Any] | None,
    service_config: ProxyServiceConfig | None,
) -> str | None:
    if isinstance(parsed_body, dict):
        provider = extract_provider(parsed_body)
        if provider:
            return provider
    if service_config:
        model_override = service_config.model_override
        if isinstance(model_override, dict):
            provider_id = str(model_override.get("provider_id") or "").strip()
            if provider_id:
                return provider_id
        default_provider = str(service_config.default_provider or "").strip()
        if default_provider:
            return default_provider
    return None


def store_effective_billing_hints(
    request: Request,
    *,
    effective_model: str | None,
    effective_provider: str | None,
) -> None:
    if effective_model:
        request.state.effective_model = effective_model
    if effective_provider:
        request.state.effective_provider = effective_provider


async def apply_langgraph_run_governance(
    *,
    request: Request,
    user: UserContext,
    payload: dict[str, Any],
    path: str,
    service_config: ProxyServiceConfig | None,
    auth: AuthContext | None = None,
    service_name: str | None = None,
    operation: str = "run_create",
    tenant_id: str | None = None,
) -> tuple[str | None, str | None, bool]:
    """
    Apply model allowlist, quota policy, and server-side model override to a run payload.

    Returns:
        (effective_model, effective_provider, payload_mutated)
    """
    resolved_auth = auth or resolve_auth_context(request, user)
    resolved_service = service_name or (
        str(service_config.service_name or service_config.service_id or "").strip()
        if service_config
        else "langgraph"
    ) or "langgraph"
    effective_tenant = tenant_id or resolved_auth.tenant_id or user.tenant_id or "default"

    requested_model = extract_model(payload)
    if not requested_model and service_config:
        requested_model = service_config.default_model

    await enforce_model_allowlist(
        request,
        resolved_service,
        user,
        resolved_auth,
        requested_model,
    )

    runtime_config, (_, effective_model, quota_mutated) = await asyncio.gather(
        resolve_langgraph_model_override(
            request=request,
            service_config=service_config,
            tenant_id=effective_tenant,
        ),
        apply_quota_policy(
            request=request,
            user=user,
            auth=resolved_auth,
            service_name=resolved_service,
            operation=operation,
            path=path,
            body=None,
            model_hint=requested_model,
            payload=payload,
            defer_encode=True,
        ),
    )

    post_mutated = quota_mutated
    if runtime_config is not None:
        inject_resolved_model_override(payload, runtime_config)
        post_mutated = True
        if apply_quota_model_downgrade(
            payload,
            downgraded_model=effective_model,
            requested_model=requested_model,
        ):
            post_mutated = True

    effective_provider = resolve_effective_provider(payload, service_config)
    store_effective_billing_hints(
        request,
        effective_model=effective_model,
        effective_provider=effective_provider,
    )
    return effective_model, effective_provider, post_mutated