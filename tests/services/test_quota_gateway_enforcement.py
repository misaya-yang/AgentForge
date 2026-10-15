from __future__ import annotations

import json
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

import src.api.v1.proxy as proxy_module
import src.proxy.langgraph_governance as governance_module
from src.api.deps import AuthContext
from src.core.auth.user_resolver import UserContext
from src.services.billing.quota_service import (
    OverageStrategy,
    QuotaCheckResult,
    QuotaService,
    QuotaStatus,
    UserQuota,
)


def _request(*, failure_mode: str = "fail_open") -> SimpleNamespace:
    return SimpleNamespace(
        method="POST",
        state=SimpleNamespace(request_id="req-p2-quota"),
        app=SimpleNamespace(
            state=SimpleNamespace(
                settings=SimpleNamespace(
                    proxy=SimpleNamespace(
                        quota_check_failure_mode=failure_mode,
                        quota_alert_dedupe_ttl_seconds=60,
                    )
                )
            )
        ),
    )


def _user() -> UserContext:
    return UserContext(
        user_id="user-a",
        tenant_id="tenant-a",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
        ip="127.0.0.1",
    )


def _auth() -> AuthContext:
    return AuthContext(
        user_id="user-a",
        tenant_id="tenant-a",
        roles=["user"],
        permissions=["conversation:playground:access"],
        is_authenticated=True,
    )


async def _apply(
    *,
    request: SimpleNamespace,
    body: dict | None = None,
    model_hint: str | None = "primary-model",
) -> tuple[bytes | None, str | None]:
    raw_body = json.dumps(body or {"input": {"message": "hello"}}).encode("utf-8")
    body_bytes, model, _ = await proxy_module._apply_quota_policy(
        request=request,
        user=_user(),
        auth=_auth(),
        service_name="agent",
        operation="run_wait",
        path="threads/t1/runs/wait",
        body=raw_body,
        model_hint=model_hint,
    )
    return body_bytes, model


class _FakeQuotaService:
    def __init__(self, result: QuotaCheckResult | None = None, error: Exception | None = None):
        self.database = object()
        self.result = result
        self.error = error
        self.check_calls: list[dict] = []
        self.alerts: list[dict] = []

    async def check_quota(self, **kwargs):
        self.check_calls.append(kwargs)
        if self.error:
            raise self.error
        return self.result

    async def create_alert(self, **kwargs):
        self.alerts.append(kwargs)


async def _append_event(events: list[dict], **kwargs) -> None:
    events.append(kwargs)


async def _ignore_event(**kwargs) -> None:
    return None


@pytest.mark.asyncio
async def test_hard_block_returns_403_before_upstream(monkeypatch) -> None:
    service = _FakeQuotaService(
        QuotaCheckResult(
            status=QuotaStatus.BLOCKED,
            message="blocked",
            overage_strategy=OverageStrategy.HARD_BLOCK,
        )
    )
    events = []
    monkeypatch.setattr(governance_module, "get_quota_service", lambda: service)
    monkeypatch.setattr(
        governance_module,
        "record_security_event",
        lambda **kwargs: _append_event(events, **kwargs),
    )

    with pytest.raises(HTTPException) as exc:
        await _apply(request=_request())

    assert exc.value.status_code == 403
    assert service.check_calls[0]["tenant_id"] == "tenant-a"
    assert events == [
        {
            "event_type": "quota_exceeded",
            "tenant_id": "tenant-a",
            "user_id": "user-a",
            "service_id": "agent",
            "metadata": {
                "policy": "hard_block",
                "status": "blocked",
                "request_id": "req-p2-quota",
                "message": "blocked",
            },
        }
    ]


@pytest.mark.asyncio
async def test_rate_limit_returns_429_with_retry_after(monkeypatch) -> None:
    service = _FakeQuotaService(
        QuotaCheckResult(
            status=QuotaStatus.EXCEEDED,
            message="Minute request limit exceeded",
            overage_strategy=OverageStrategy.RATE_LIMIT,
            retry_after_seconds=17,
        )
    )
    monkeypatch.setattr(governance_module, "get_quota_service", lambda: service)
    monkeypatch.setattr(governance_module, "record_security_event", _ignore_event)

    with pytest.raises(HTTPException) as exc:
        await _apply(request=_request())

    assert exc.value.status_code == 429
    assert exc.value.headers["Retry-After"] == "17"
    assert exc.value.detail["policy"] == "rate_limit"


@pytest.mark.asyncio
async def test_allow_but_alert_proceeds_and_alerts_once_per_window(monkeypatch) -> None:
    service = _FakeQuotaService(
        QuotaCheckResult(
            status=QuotaStatus.EXCEEDED,
            message="Daily token limit exceeded",
            daily_tokens_used=10,
            daily_tokens_limit=10,
            overage_strategy=OverageStrategy.ALLOW_BUT_ALERT,
        )
    )
    monkeypatch.setattr(governance_module, "get_quota_service", lambda: service)
    monkeypatch.setattr(governance_module, "record_security_event", _ignore_event)
    request = _request()

    await _apply(request=request)
    await _apply(request=request)

    assert len(service.alerts) == 1
    assert service.alerts[0]["tenant_id"] == "tenant-a"
    assert service.alerts[0]["alert_type"] == "quota_allow_but_alert"


@pytest.mark.asyncio
async def test_downgrade_model_only_mutates_model_fields(monkeypatch) -> None:
    service = _FakeQuotaService(
        QuotaCheckResult(
            status=QuotaStatus.EXCEEDED,
            message="downgrade",
            overage_strategy=OverageStrategy.DOWNGRADE_MODEL,
            downgraded_model="safe-model",
        )
    )
    monkeypatch.setattr(governance_module, "get_quota_service", lambda: service)
    monkeypatch.setattr(governance_module, "record_security_event", _ignore_event)

    body, model = await _apply(
        request=_request(),
        body={
            "model": "expensive-model",
            "provider_api_key": "secret-provider-key",
            "input": {"model": "expensive-model", "text": "hello"},
            "config": {
                "configurable": {
                    "model": "expensive-model",
                    "provider_api_key": "nested-secret",
                }
            },
        },
    )

    payload = json.loads((body or b"{}").decode("utf-8"))
    assert model == "safe-model"
    assert payload["model"] == "safe-model"
    assert payload["input"]["model"] == "safe-model"
    assert payload["config"]["configurable"]["model"] == "safe-model"
    assert payload["provider_api_key"] == "secret-provider-key"
    assert payload["config"]["configurable"]["provider_api_key"] == "nested-secret"


@pytest.mark.asyncio
async def test_downgrade_model_mutates_deferred_parsed_payload(monkeypatch) -> None:
    service = _FakeQuotaService(
        QuotaCheckResult(
            status=QuotaStatus.EXCEEDED,
            message="downgrade",
            overage_strategy=OverageStrategy.DOWNGRADE_MODEL,
            downgraded_model="safe-model",
        )
    )
    monkeypatch.setattr(governance_module, "get_quota_service", lambda: service)
    monkeypatch.setattr(governance_module, "record_security_event", _ignore_event)
    payload = {
        "model": "expensive-model",
        "input": {"model": "expensive-model", "text": "hello"},
        "config": {"configurable": {"model": "expensive-model"}},
    }

    body, model, mutated = await proxy_module._apply_quota_policy(
        request=_request(),
        user=_user(),
        auth=_auth(),
        service_name="agent",
        operation="run_wait",
        path="threads/t1/runs/wait",
        body=json.dumps(payload).encode("utf-8"),
        model_hint="expensive-model",
        payload=payload,
        defer_encode=True,
    )

    assert body is None
    assert model == "safe-model"
    assert mutated is True
    assert payload["model"] == "safe-model"
    assert payload["input"]["model"] == "safe-model"
    assert payload["config"]["configurable"]["model"] == "safe-model"


@pytest.mark.asyncio
async def test_quota_check_failure_policy_is_explicit(monkeypatch) -> None:
    service = _FakeQuotaService(error=RuntimeError("quota store down"))
    events = []
    monkeypatch.setattr(governance_module, "get_quota_service", lambda: service)
    monkeypatch.setattr(
        governance_module,
        "record_security_event",
        lambda **kwargs: _append_event(events, **kwargs),
    )

    body, model = await _apply(request=_request(failure_mode="fail_open"))
    assert body is not None
    assert model == "primary-model"

    with pytest.raises(HTTPException) as exc:
        await _apply(request=_request(failure_mode="fail_closed"))

    assert exc.value.status_code == 503
    assert exc.value.detail["code"] == "QUOTA_CHECK_UNAVAILABLE"
    assert events[-1]["event_type"] == "quota_check_failed"
    assert events[-1]["metadata"]["request_id"] == "req-p2-quota"


@pytest.mark.asyncio
async def test_requests_per_day_and_minute_are_enforced(monkeypatch) -> None:
    service = QuotaService(database=None)
    quota = UserQuota(
        tenant_id="tenant-a",
        user_id="user-a",
        requests_per_day=1,
        requests_per_minute=1,
        current_daily_requests=0,
        overage_strategy=OverageStrategy.RATE_LIMIT,
    )

    async def _quota(tenant_id: str, user_id: str) -> UserQuota:
        return quota

    async def _ignore_quota_event(*args, **kwargs) -> None:
        return None

    monkeypatch.setattr(service, "_get_or_create_quota", _quota)
    monkeypatch.setattr(service, "_record_quota_exceeded_event", _ignore_quota_event)

    first = await service.check_quota("tenant-a", "user-a")
    second = await service.check_quota("tenant-a", "user-a")

    assert first.status == QuotaStatus.OK
    assert second.status == QuotaStatus.EXCEEDED
    assert second.overage_strategy == OverageStrategy.RATE_LIMIT
    assert second.requests_per_minute_limit == 1

    quota.current_daily_requests = 1
    day_result = await service.check_quota("tenant-a", "user-a")
    assert day_result.status == QuotaStatus.EXCEEDED
    assert "Daily request limit exceeded" in day_result.message
