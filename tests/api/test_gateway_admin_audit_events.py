from __future__ import annotations

import json
from decimal import Decimal
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.deps import AuthContext, require_gateway_capability
from src.core.auth.permissions import Capability
from src.core.auth.rbac import RBAC
from src.services.metrics.audit_event_writer import record_config_change
from src.services.metrics.redaction import redact_sensitive_data


class _FakeDatabase:
    def __init__(self):
        self.events: list[dict] = []

    async def record_audit_event(self, **event):
        self.events.append(event)


def _request(db: _FakeDatabase):
    return SimpleNamespace(
        app=SimpleNamespace(
            state=SimpleNamespace(
                database=db,
                dispatcher=SimpleNamespace(rbac=RBAC(role_permissions={"admin": ["admin:*"]})),
            )
        ),
        state=SimpleNamespace(request_id="req-admin-audit", trace_id="trace-admin-audit"),
        client=SimpleNamespace(host="127.0.0.1"),
        headers={"user-agent": "pytest", "authorization": "Bearer should-not-leak"},
    )


@pytest.mark.asyncio
async def test_config_change_audit_event_redacts_nested_secrets():
    db = _FakeDatabase()
    request = _request(db)
    auth = AuthContext(
        user_id="admin-a",
        tenant_id="tenant-a",
        roles=["admin"],
        permissions=[],
        is_authenticated=True,
    )

    await record_config_change(
        request=request,
        auth=auth,
        resource_type="provider",
        resource_id="google",
        action="update",
        before={"api_key": "old-secret", "api_key_fingerprint": "fp-old"},
        after={
            "api_key": "new-secret",
            "headers": {"Authorization": "Bearer upstream-secret"},
            "api_key_fingerprint": "fp-new",
        },
    )

    assert len(db.events) == 1
    event = db.events[0]
    assert event["event_type"] == "config_changed"
    assert event["tenant_id"] == "tenant-a"
    assert event["request_summary"]["request_id"] == "req-admin-audit"
    rendered = str(event)
    assert "old-secret" not in rendered
    assert "new-secret" not in rendered
    assert "upstream-secret" not in rendered
    assert "fp-new" in rendered


@pytest.mark.asyncio
async def test_config_change_audit_event_normalizes_decimal_payloads():
    db = _FakeDatabase()
    request = _request(db)
    auth = AuthContext(
        user_id="admin-a",
        tenant_id="tenant-a",
        roles=["admin"],
        permissions=[],
        is_authenticated=True,
    )

    await record_config_change(
        request=request,
        auth=auth,
        resource_type="model",
        resource_id="qwen3.8-flash",
        action="create",
        before=None,
        after={
            "model_id": "qwen3.8-flash",
            "input_price_per_1k": Decimal("0.001167"),
            "output_price_per_1k": Decimal("0.003427"),
        },
    )

    summary = db.events[0]["request_summary"]
    assert summary["after"]["input_price_per_1k"] == 0.001167
    assert summary["after"]["output_price_per_1k"] == 0.003427
    json.dumps(summary)  # the real pool fallback must be able to serialize it


def test_redaction_preserves_api_key_fingerprint():
    payload = {
        "api_key": "raw-key",
        "_api_key": "runtime-key",
        "api_key_fingerprint": "abc123",
        "nested": {"Cookie": "session=secret", "password": "secret-password"},
    }

    redacted = redact_sensitive_data(payload)

    assert redacted["api_key"] == "***"
    assert redacted["_api_key"] == "***"
    assert redacted["api_key_fingerprint"] == "abc123"
    assert redacted["nested"]["Cookie"] == "***"
    assert redacted["nested"]["password"] == "***"


@pytest.mark.asyncio
async def test_non_admin_denial_records_auth_failed_security_event(monkeypatch):
    calls: list[dict] = []

    class _Recorder:
        async def record_event(self, **kwargs):
            calls.append(kwargs)

    import src.services.metrics.security_event_recorder as recorder_module

    monkeypatch.setattr(recorder_module, "_security_event_recorder", _Recorder())
    request = _request(_FakeDatabase())
    auth = AuthContext(
        user_id="user-a",
        tenant_id="tenant-a",
        roles=["user"],
        permissions=[],
        is_authenticated=True,
    )

    with pytest.raises(HTTPException):
        require_gateway_capability(request, auth, Capability.GATEWAY_RATE_LIMIT_WRITE)

    await __import__("asyncio").sleep(0)
    assert calls
    assert calls[0]["event_type"] == "auth_failed"
    assert calls[0]["tenant_id"] == "tenant-a"
