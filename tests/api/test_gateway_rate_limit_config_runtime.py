from __future__ import annotations

from types import SimpleNamespace

import pytest

from src.api.deps import AuthContext
from src.api.v1.config import (
    RateLimitRule,
    _runtime_config,
    create_rate_limit,
    delete_rate_limit,
)
from src.config.settings import Settings


def _request() -> SimpleNamespace:
    return SimpleNamespace(
        state=SimpleNamespace(),
        app=SimpleNamespace(state=SimpleNamespace(settings=Settings(), database=None)),
    )


def _auth() -> AuthContext:
    return AuthContext(
        user_id="admin",
        tenant_id="default",
        roles=["developer"],
        permissions=["console:rate_limits:edit"],
        is_authenticated=True,
    )


@pytest.mark.asyncio
async def test_create_and_delete_rate_limit_updates_runtime_resolver() -> None:
    _runtime_config["rate_limits"] = []
    request = _request()

    await create_rate_limit(
        body=RateLimitRule(
            scope="tenant",
            scope_id="tenant-a",
            requests=3,
            window=60,
            burst=0,
            enabled=True,
        ),
        request=request,
        auth=_auth(),
    )

    assert request.app.state.rate_limit_rules == [
        {
            "scope": "tenant",
            "scope_id": "tenant-a",
            "requests": 3,
            "window": 60,
            "burst": 0,
            "strategy": "sliding_window",
            "enabled": True,
        }
    ]
    first_resolver = request.app.state.rate_policy_resolver
    assert first_resolver._epoch == 1

    await delete_rate_limit(
        scope="tenant",
        scope_id="tenant-a",
        request=request,
        auth=_auth(),
    )

    assert request.app.state.rate_limit_rules == []
    assert request.app.state.rate_policy_resolver is first_resolver
    assert first_resolver._epoch == 2
