"""Read-only quota snapshots retain configuration and share admission arithmetic."""
from contextlib import asynccontextmanager
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace

import pytest

from src.api.v1 import quota as api
from src.services.billing import quota_service as billing

NOW = datetime(2026, 10, 12, 12, tzinfo=timezone.utc)


@pytest.mark.parametrize("base", [None, 0, 100, 9_007_199_254_740_993])
@pytest.mark.parametrize("expires", [None, NOW - timedelta(seconds=1), NOW, NOW + timedelta(seconds=1)])
def test_effective_limit_observation_preserves_existing_arithmetic(base, expires):
    value = billing.resolve_effective_quota_limits(
        daily_token_limit=base,
        monthly_token_limit=base,
        monthly_cost_limit_cents=base,
        temporary_extra_tokens=100,
        temporary_extra_cost_cents=7,
        temporary_expires_at=expires,
        now=NOW,
    )
    active = expires is not None and expires > NOW
    for key, extra in [("daily_tokens", 100), ("monthly_tokens", 100), ("monthly_cost_cents", 7)]:
        expected = base + (extra if active else 0) if base is not None else None
        assert value["effective"][key] == expected
        expected_source = (
            "unlimited" if expected is None or expected == 0
            else "configured_plus_temporary_boost" if active else "configured"
        )
        assert value["sources"][key] == expected_source
    assert value["temporary_boost"]["active"] is active
    assert value["temporary_boost"]["extra_tokens"] == 100
    assert value["observed_at"] == NOW.isoformat()


def make_row(**overrides):
    return {
        "tenant_id": "fixture-tenant", "user_id": "fixture-user",
        "daily_token_limit": 100, "monthly_token_limit": None,
        "monthly_cost_limit_cents": None, "current_daily_tokens": 110,
        "current_monthly_tokens": 0, "current_monthly_cost_cents": 0,
        "is_blocked": False, "blocked_reason": None,
        "overage_strategy": "hard_block", "downgraded_model": None,
        "warning_threshold": 80, "temporary_extra_tokens": 100,
        "temporary_extra_cost_cents": 0,
        "temporary_expires_at": NOW + timedelta(seconds=1), **overrides,
    }


class Pool:
    def __init__(self, rows):
        self.rows = rows

    @asynccontextmanager
    async def acquire(self):
        yield self

    async def fetch(self, sql, *args):
        assert "temporary_extra_tokens" in sql and "temporary_expires_at" in sql
        assert args == ("fixture-tenant", 20)
        return self.rows

    async def fetchrow(self, sql, *args):
        assert "SELECT *" in sql
        assert args == ("fixture-tenant", "fixture-user")
        return self.rows[0]

    async def execute(self, *_args):
        raise AssertionError("Overview/admission observation must not write")


def wire(monkeypatch, rows):
    service = billing.QuotaService(SimpleNamespace(_pool=Pool(rows)))
    monkeypatch.setattr(api, "get_quota_service", lambda: service)
    monkeypatch.setattr(api, "_require_quota_read", lambda *_args: None)
    monkeypatch.setattr(api, "_require_quota_subject", lambda *_args: None)
    monkeypatch.setattr(api, "_require_quota_tenant_scope", lambda *_args: None)

    class Clock(datetime):
        calls = 0

        @classmethod
        def now(cls, tz=None):
            cls.calls += 1
            assert tz is timezone.utc
            return NOW

    monkeypatch.setattr(api, "datetime", Clock)
    monkeypatch.setattr(billing, "datetime", Clock)
    return service, Clock


@pytest.mark.asyncio
@pytest.mark.parametrize("field,usage,extra", [
    ("daily_token_limit", "current_daily_tokens", "temporary_extra_tokens"),
    ("monthly_token_limit", "current_monthly_tokens", "temporary_extra_tokens"),
    ("monthly_cost_limit_cents", "current_monthly_cost_cents", "temporary_extra_cost_cents"),
])
async def test_serialized_overview_uses_effective_snapshot_and_keeps_configuration(monkeypatch, field, usage, extra):
    row = make_row(daily_token_limit=None, current_daily_tokens=0)
    row.update({field: 100, usage: 110, extra: 100})
    service, _ = wire(monkeypatch, [row])
    auth = SimpleNamespace(tenant_id="fixture-tenant", user_id="fixture-user")
    response = await api.get_quota_users_overview(None, 20, "daily_tokens", auth)
    item = response.model_dump(mode="json")["users"][0]
    assert item["status"] == "ok"
    assert response.summary["ok"] == 1
    assert item["limit_observation"]["observed_at"] == NOW.isoformat()
    configured = (await api.get_user_quota("fixture-user", None, auth)).model_dump(mode="json")
    key = {"daily_token_limit": "daily_tokens", "monthly_token_limit": "monthly_tokens", "monthly_cost_limit_cents": "monthly_cost_cents"}[field]
    assert configured["limits"][key] == 100
    assert item["limit_observation"]["effective"][key] == 200
    old_key = {"daily_token_limit": "daily_tokens_limit", "monthly_token_limit": "monthly_tokens_limit", "monthly_cost_limit_cents": "monthly_cost_limit_cents"}[field]
    assert item[old_key] == 100
    admission = await service.check_quota("fixture-tenant", "fixture-user", estimated_tokens=1, record_security_event=False)
    assert admission.can_proceed


@pytest.mark.asyncio
async def test_overview_one_clock_for_all_rows(monkeypatch):
    rows = [make_row(user_id=f"fixture-{i}") for i in range(3)]
    _, clock = wire(monkeypatch, rows)
    response = await api.get_quota_users_overview(None, 20, "daily_tokens", SimpleNamespace(tenant_id="fixture-tenant"))
    assert clock.calls == 1
    assert len(response.users) == 3
    assert {x.limit_observation.observed_at for x in response.users} == {NOW.isoformat()}
    assert all(x.limit_observation.temporary_boost.active for x in response.users)


@pytest.mark.asyncio
async def test_at_effective_limit_is_usage_snapshot_not_admission_guarantee(monkeypatch):
    service, _ = wire(monkeypatch, [make_row(current_daily_tokens=200)])
    response = await api.get_quota_users_overview(None, 20, "daily_tokens", SimpleNamespace(tenant_id="fixture-tenant"))
    assert response.users[0].status == "exceeded"
    zero = await service.check_quota("fixture-tenant", "fixture-user", estimated_tokens=0, record_security_event=False)
    positive = await service.check_quota("fixture-tenant", "fixture-user", estimated_tokens=1, record_security_event=False)
    assert zero.can_proceed and not positive.can_proceed
