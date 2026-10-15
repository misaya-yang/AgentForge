from __future__ import annotations

import uuid
from types import SimpleNamespace

import pytest
from ai_gateway_contracts.capability_proof import canonical_body_hash, sign_capability_proof
from fastapi import HTTPException

from src.api.internal_mcp_broker import broker_mcp_read


class _Pool:
    def __init__(self, row: dict) -> None:
        self.row = row

    async def fetchrow(self, *_args, **_kwargs):
        return self.row


class _Broker:
    def __init__(self) -> None:
        self.calls: list[dict] = []

    async def invoke_read_only(self, **values):
        self.calls.append(values)
        return {"tool_name": values["runtime_name"], "content": [], "is_error": False}


def _body() -> dict:
    arguments = {"q": "hello"}
    return {
        "connection_id": "connection-a",
        "principal_type": "user_delegated",
        "channel": "assistant",
        "runtime_name": "mcp_search",
        "schema_hash": "sha256:" + "a" * 64,
        "risk_level": "low",
        "arguments": arguments,
        "arguments_hash": "sha256:" + canonical_body_hash(arguments),
    }


def _request(row: dict) -> tuple[SimpleNamespace, _Broker]:
    broker = _Broker()
    state = SimpleNamespace(
        database=SimpleNamespace(_pool=_Pool(row)),
        mcp_gateway_broker=broker,
    )
    return SimpleNamespace(app=SimpleNamespace(state=state)), broker


def _headers(body: dict, monkeypatch) -> dict:
    execution_id, run_id = str(uuid.uuid4()), str(uuid.uuid4())
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "internal-token")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "p" * 32)
    proof = sign_capability_proof(
        "p" * 32,
        method="POST",
        path="/internal/v2/agent-capabilities/mcp/read",
        body=body,
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        execution_id=execution_id,
        run_id=run_id,
        nonce="nonce-a",
    )
    return {
        "x_ai_platform_internal_token": "internal-token",
        "x_ai_tenant_id": "tenant-a",
        "x_ai_user_id": "user-a",
        "x_ai_session_id": "session-a",
        "x_ai_execution_id": execution_id,
        "x_ai_run_id": run_id,
        "x_ai_tool_call_id": "call-a",
        "x_ai_capability_proof": proof,
    }


@pytest.mark.asyncio
async def test_read_broker_requires_active_scope_bound_execution(monkeypatch) -> None:
    body = _body()
    row = {
        "capability_id": "mcp_search",
        "effect": "read",
        "approval_status": "not_required",
        "status": "dispatched",
        "tool_call_id": "call-a",
        "arguments_sha256": body["arguments_hash"][7:],
    }
    request, broker = _request(row)

    result = await broker_mcp_read(request, body, **_headers(body, monkeypatch))

    assert result["tool_name"] == "mcp_search"
    assert broker.calls[0]["tenant_id"] == "tenant-a"
    assert broker.calls[0]["arguments"] == {"q": "hello"}


@pytest.mark.asyncio
async def test_read_broker_rejects_write_execution_before_network(monkeypatch) -> None:
    body = _body()
    row = {
        "capability_id": "mcp_search",
        "effect": "write",
        "approval_status": "consumed",
        "status": "dispatched",
        "tool_call_id": "call-a",
        "arguments_sha256": body["arguments_hash"][7:],
    }
    request, broker = _request(row)

    with pytest.raises(HTTPException) as error:
        await broker_mcp_read(request, body, **_headers(body, monkeypatch))

    assert error.value.status_code == 403
    assert broker.calls == []


@pytest.mark.asyncio
async def test_read_broker_rejects_tampered_arguments(monkeypatch) -> None:
    body = _body()
    headers = _headers(body, monkeypatch)
    body["arguments"] = {"q": "tampered"}
    request, broker = _request({})

    with pytest.raises(HTTPException) as error:
        await broker_mcp_read(request, body, **headers)

    assert error.value.status_code == 422
    assert broker.calls == []
