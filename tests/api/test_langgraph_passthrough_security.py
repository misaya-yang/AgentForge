from __future__ import annotations

from types import SimpleNamespace

from src.api.v1.langgraph import _build_langgraph_passthrough_headers
from src.core.auth.user_resolver import UserContext


class _Proxy:
    auth_token = "service-token"


def _request(headers: dict[str, str]) -> SimpleNamespace:
    return SimpleNamespace(
        headers=headers,
        state=SimpleNamespace(request_id="req-1", trace_id="trace-1"),
        client=SimpleNamespace(host="203.0.113.9"),
    )


def test_langgraph_passthrough_strips_client_identity_and_auth_headers():
    user = UserContext(
        user_id="user-auth",
        tenant_id="tenant-auth",
        tier="premium",
        is_authenticated=True,
        roles=["user"],
    )

    headers = _build_langgraph_passthrough_headers(
        _request(
            {
                "Authorization": "Bearer attacker-token",
                "Cookie": "session=attacker",
                "X-Api-Key": "attacker-key",
                "X-User-Id": "admin",
                "X-Tenant-Id": "root",
                "X-GW-Authenticated": "false",
                "Accept": "text/event-stream",
                "X-Custom-Trace": "keep-me",
            }
        ),
        user,
        _Proxy(),
    )

    assert headers["X-User-Id"] == "user-auth"
    assert headers["X-GW-User-ID"] == "user-auth"
    assert headers["X-Tenant-Id"] == "tenant-auth"
    assert headers["X-GW-Tenant-ID"] == "tenant-auth"
    assert headers["X-GW-Authenticated"] == "true"
    assert headers["X-Api-Key"] == "service-token"
    assert headers["Authorization"] == "Bearer service-token"
    assert headers["Accept"] == "text/event-stream"
    assert headers["X-Custom-Trace"] == "keep-me"
    assert "attacker" not in " ".join(headers.values())
    assert "admin" not in headers.values()
    assert "root" not in headers.values()
