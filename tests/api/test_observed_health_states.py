"""Health and empty metrics describe observed evidence rather than configuration."""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock

import pytest
from fastapi import HTTPException

from src.api.v1 import dashboard, health, providers, usage
from src.core.auth.user_resolver import UserContext
from src.services.llm.provider_health import (
    MAX_PROBE_RECEIPTS,
    PROBE_TTL_SECONDS,
    begin_provider_probe,
    invalidate_provider_probe,
    provider_probe_status,
    record_provider_probe,
)
from src.services.metrics.metrics_recorder import MetricsRecorder
from src.services.metrics.realtime_metrics import RealtimeSnapshot

NOW = datetime(2026, 9, 7, tzinfo=timezone.utc)
ADMIN = UserContext(user_id="admin", tenant_id="tenant-a", roles=["admin"], is_authenticated=True)


def _request(**state):
    return SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(**state)))


def _status(request, *, tenant="tenant-a", configured=True, now=NOW):
    return provider_probe_status(request, tenant, "custom-provider", configured=configured, now=now)


@pytest.mark.parametrize("success,expected", [(True, "healthy"), (False, "unhealthy")])
def test_probe_requires_a_real_result_and_expires_without_refreshing_its_timestamp(
    success, expected
):
    request = _request()
    assert _status(request) == {"status": "unverified", "last_check": None, "probe_source": None}
    probe = begin_provider_probe(request, "tenant-a", "custom-provider")
    assert _status(request)["status"] == "unverified"
    record_provider_probe(
        request,
        "tenant-a",
        "custom-provider",
        {"success": success, "message": "private response"},
        probe_id=probe,
        now=NOW,
    )
    assert _status(request)["status"] == expected
    stale = _status(request, now=NOW + timedelta(seconds=PROBE_TTL_SECONDS + 1))
    assert stale == {
        "status": "stale",
        "last_check": NOW.isoformat(),
        "probe_source": "connection_test",
    }
    assert _status(request, tenant="tenant-b")["status"] == "unverified"
    assert _status(request, configured=False)["status"] == "not_configured"
    assert "private response" not in str(request.app.state.provider_probe_receipts)


def test_config_change_and_newer_probe_prevent_late_old_result_becoming_healthy():
    request = _request()
    old = begin_provider_probe(request, "tenant-a", "custom-provider")
    invalidate_provider_probe(request, "tenant-a", "custom-provider")
    record_provider_probe(
        request, "tenant-a", "custom-provider", {"success": True}, probe_id=old, now=NOW
    )
    assert _status(request)["status"] == "unverified"

    newer = begin_provider_probe(request, "tenant-a", "custom-provider")
    record_provider_probe(
        request, "tenant-a", "custom-provider", {"success": False}, probe_id=newer, now=NOW
    )
    record_provider_probe(
        request, "tenant-a", "custom-provider", {"success": True}, probe_id=old, now=NOW
    )
    assert _status(request)["status"] == "unhealthy"


def test_probe_receipts_are_bounded_and_eviction_means_unverified():
    request = _request()
    for i in range(MAX_PROBE_RECEIPTS + 1):
        begin_provider_probe(request, "tenant-a", str(i))
    assert len(request.app.state.provider_probe_receipts) == MAX_PROBE_RECEIPTS
    assert (
        provider_probe_status(request, "tenant-a", "0", configured=True)["status"] == "unverified"
    )


async def test_provider_test_route_records_only_completed_test_and_config_mutation_invalidates_it():
    request = _request()

    async def changed_during_test(**_kwargs):
        invalidate_provider_probe(request, "tenant-a", "custom-provider")
        return {"success": True}

    service = SimpleNamespace(test_connection=AsyncMock(side_effect=changed_during_test))
    await providers.test_provider_connection(
        "custom-provider", request=request, provider_service=service, user=ADMIN
    )
    assert _status(request)["status"] == "unverified"
    service.test_connection = AsyncMock(return_value={"success": False})
    await providers.test_provider_connection(
        "custom-provider", request=request, provider_service=service, user=ADMIN
    )
    assert _status(request, now=datetime.now(timezone.utc))["status"] == "unhealthy"


async def test_provider_health_exposes_custom_provider_without_inventing_a_probe():
    metadata = SimpleNamespace(
        list_enabled_providers=AsyncMock(return_value=["custom-provider"]),
        is_provider_configured=AsyncMock(return_value=True),
        count_enabled_models_by_provider=AsyncMock(return_value={"custom-provider": 2}),
    )
    result = await health.all_providers_health(request=_request(model_meta=metadata), user=ADMIN)
    assert result["custom-provider"] == {
        "name": "custom-provider",
        "status": "unverified",
        "last_check": None,
        "probe_source": None,
        "configured": True,
        "model_count": 2,
    }
    assert result["openai"]["status"] == "not_configured"
    metadata.list_enabled_providers.assert_awaited_once_with("tenant-a")


@pytest.mark.parametrize("missing", [False, True])
async def test_provider_health_collection_failure_is_not_an_empty_or_healthy_catalog(missing):
    metadata = (
        None
        if missing
        else SimpleNamespace(
            list_enabled_providers=AsyncMock(side_effect=RuntimeError("private database failure"))
        )
    )
    with pytest.raises(HTTPException) as error:
        await health.all_providers_health(request=_request(model_meta=metadata), user=ADMIN)
    assert error.value.status_code == 503
    assert error.value.detail == {"code": "PROVIDER_HEALTH_COLLECTION_UNAVAILABLE"}


def _usage_summary(total=0, rate=100.0, data_status=None):
    result = {
        "total_requests": total,
        "success_rate": rate,
        "total_input_tokens": 0,
        "total_output_tokens": 0,
        "total_tokens": 0,
        "total_cost_usd": 0,
        "avg_latency_ms": 0,
        "start_date": NOW.date().isoformat(),
        "end_date": NOW.date().isoformat(),
    }
    if data_status:
        result["data_status"] = data_status
    return result


@pytest.mark.parametrize("state", ["empty", "collection_error", "unavailable"])
async def test_zero_request_usage_has_no_success_percentage_and_preserves_source_failure(
    monkeypatch, state
):
    recorder = SimpleNamespace(
        get_usage_summary=AsyncMock(return_value=_usage_summary(data_status=state)),
        get_last_ingested_at=AsyncMock(return_value=None),
    )
    monkeypatch.setattr(usage, "get_usage_recorder", lambda: recorder)
    monkeypatch.setattr(usage, "_require_usage_read", lambda *_args: None)
    response = await usage.get_usage_summary(
        request=_request(),
        start_date=NOW.date(),
        end_date=NOW.date(),
        user_id=None,
        model=None,
        service_id=None,
        assistant_id=None,
        provider=None,
        auth=ADMIN,
    )
    assert response.success_rate is None
    assert response.data_status == state


async def test_measured_all_failed_usage_remains_zero_success(monkeypatch):
    recorder = SimpleNamespace(
        get_usage_summary=AsyncMock(return_value=_usage_summary(total=3, rate=0.0)),
        get_last_ingested_at=AsyncMock(return_value=datetime.now(timezone.utc)),
    )
    monkeypatch.setattr(usage, "get_usage_recorder", lambda: recorder)
    monkeypatch.setattr(usage, "_require_usage_read", lambda *_args: None)
    response = await usage.get_usage_summary(
        request=_request(),
        start_date=NOW.date(),
        end_date=NOW.date(),
        user_id=None,
        model=None,
        service_id=None,
        assistant_id=None,
        provider=None,
        auth=ADMIN,
    )
    assert response.success_rate == 0.0
    assert response.data_status == "ok"


@pytest.mark.parametrize("source", ["empty", "collection_error", "unavailable"])
async def test_redis_metrics_distinguish_empty_collection_error_and_missing_collector(source):
    if source == "unavailable":
        recorder = MetricsRecorder(redis=None)
    else:
        pipeline = MagicMock()
        pipeline.execute = AsyncMock(return_value=[None] * 37)
        if source == "collection_error":
            pipeline.execute.side_effect = RuntimeError("unavailable")
        client = SimpleNamespace(pipeline=lambda: pipeline)
        recorder = MetricsRecorder(redis=SimpleNamespace(_client=client))
        recorder.get_latency_percentiles = AsyncMock(return_value={"p50": 0, "p95": 0, "p99": 0})
    result = await recorder.get_today_summary()
    assert result["data_status"] == source
    assert result["success_rate"] is None
    assert result["run_success_rate"] is None


async def test_dashboard_summary_keeps_collection_failure_and_no_run_has_no_success_rate(
    monkeypatch,
):
    snapshot = RealtimeSnapshot()
    recorder = SimpleNamespace(
        get_today_summary=AsyncMock(return_value=MetricsRecorder()._empty_summary())
    )
    usage_recorder = SimpleNamespace(
        database=True,
        get_usage_summary=AsyncMock(return_value=_usage_summary(data_status="collection_error")),
        get_last_ingested_at=AsyncMock(return_value=None),
    )
    monkeypatch.setattr(dashboard, "_require_platform_metrics", lambda *_args: None)
    monkeypatch.setattr(dashboard, "get_metrics_recorder", lambda: recorder)
    monkeypatch.setattr(dashboard, "get_usage_recorder", lambda: usage_recorder)
    monkeypatch.setattr(
        dashboard,
        "get_realtime_metrics",
        lambda: SimpleNamespace(get_realtime_snapshot=AsyncMock(return_value=snapshot)),
    )
    result = await dashboard.get_dashboard_summary(request=_request(), period="today", auth=ADMIN)
    assert result["overview"]["success_rate"] is None
    assert result["data_status"] == "collection_error"
    realtime = await dashboard.get_realtime_dashboard(request=_request(), auth=ADMIN)
    assert realtime.runs.success_rate is None
