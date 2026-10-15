from __future__ import annotations

import json
import uuid
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner

from src.services.agent_runtime.control.tool_policy import runtime_tool_policy
from src.services.agent_runtime.control_plane import AgentRuntimeControlPlane
from src.services.agent_runtime.model.authorization import native_web_search_authorized


def _descriptor(name: str) -> dict:
    return {"id": name, "name": name, "version": "v1", "schema_hash": "sha256:" + "a" * 64,
            "description": name, "schema": {"type": "object"}, "read_only": True,
            "kind": "tool", "source": "platform", "tenant_id": "tenant-a", "capability_revision": 7}


def _identities(policy: dict) -> set[tuple[str | None, str]]:
    return {(item["namespace"], item["name"]) for item in policy["allowedTools"]}


def test_generic_ceiling_preserves_later_search_and_attachment_without_granting_shell() -> None:
    readonly = {"tools": [_descriptor("lookup")], "responses_tool_names": None}
    ceiling = _identities(runtime_tool_policy(readonly, startup=True, generic_product=True, native_search_allowed=True))
    assert {(None, "web_search"), (None, "read_attachment"), (None, "lookup"), ("multi_agent_v1", "wait_agent")} <= ceiling
    assert not {(None, "exec_command"), (None, "write_stdin"), (None, "apply_patch"), (None, "exec")} & ceiling
    assert (None, "web_search") not in _identities(runtime_tool_policy(readonly))
    assert not native_web_search_authorized({"readonly_capabilities": readonly, "capabilities": {"native_search": {"enabled": True}}})
    with_attachment = {**readonly, "attachment_tools": [_descriptor("read_attachment")]}
    assert _identities(runtime_tool_policy(with_attachment, native_search_allowed=True)) <= ceiling
    assert runtime_tool_policy({**readonly, "responses_tool_choice": "none"}) == {"allowedTools": []}
    assert _identities(runtime_tool_policy(readonly)) <= ceiling
    function_only = {**readonly, "responses_tool_names": ["lookup"]}
    assert _identities(runtime_tool_policy(function_only, startup=True, native_search_allowed=True)) == {(None, "lookup")}


async def test_thread_fingerprint_uses_catalog_ceiling_while_turn_selects_subset() -> None:
    thread_id = uuid.uuid4()
    requests = []
    bound = {}
    async def handler(request: httpx.Request) -> httpx.Response:
        requests.append(json.loads(request.content))
        return httpx.Response(200, request=request, json={"thread": {"id": str(thread_id)}})
    async def fetchrow(_query, *_args):
        return bound or None
    async def execute(_query, *args):
        bound.update(runtime_thread_id=thread_id, last_sequence=0, dynamic_tool_fingerprint=args[0])
        return "UPDATE 1"
    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(fetchrow=fetchrow, execute=execute),
        model_service=SimpleNamespace(get_model=AsyncMock(return_value={"is_enabled": True, "capability_revision": 7})),
        provider_service=SimpleNamespace(), assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32), runtime_url="http://runtime.test",
        runtime_internal_token="token", model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="target", http_client=client,
    )
    plane.capability_catalog_client = SimpleNamespace(fetch_catalog=AsyncMock(return_value={
        "schema_version": "agent-capability-catalog/v1", "capability_revision": 7,
        "tools": [_descriptor("lookup"), _descriptor("read")], "mcp": [], "deferred": [],
    }))
    scope = {"tenant_id": "tenant-a", "user_id": "user-a", "session_id": "session-a", "model_id": "model"}
    try:
        await plane.ensure_thread(**scope)
        assert {(None, "web_search"), (None, "read_attachment")} <= _identities(requests[0]["toolPolicy"])
        for choice in ("auto", "none", "auto"):
            readonly = plane._readonly_capability_payload(
                {"responses_tool_names": ["lookup"], "responses_tool_choice": choice},
                tenant_id="tenant-a", capability_revision=7,
            )
            await plane._fetch_capability_catalog(readonly, **scope, capability_revision=7)
            await plane.ensure_thread(**scope, readonly_capabilities=readonly)
            assert [tool["name"] for tool in readonly["tools"]] == ["lookup"]
            assert "_thread_capabilities" not in readonly
            selected = _identities(runtime_tool_policy(readonly))
            assert selected == (set() if choice == "none" else {(None, "lookup")})
        assert len(requests) == 1
        await plane.verify_thread(runtime_thread_id=str(thread_id), **scope)
        verification = requests[-1]
        assert verification["model"] == "model"
        assert verification["modelPlaneBaseUrl"] == plane.model_plane_base_url
        assert "toolPolicy" not in verification
        assert "nativeWebSearchEnabled" not in verification
        assert verification["baseInstructions"] is None
        assert verification["developerInstructions"] is None
    finally:
        await client.aclose()


async def test_cold_verification_pins_gateway_route_without_authority_overrides() -> None:
    captured = []
    stored = {"provider": "ai-platform-gateway", "instructions": "stored instructions",
              "native_search": True, "tool_policy": {"allowedTools": []}}
    live = None

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal live
        payload = json.loads(request.content)
        captured.append(payload)
        # Model the HTTP contract's cold branch: no routing override means
        # upstream default provider; later loaded resumes may ignore overrides.
        if live is None:
            live = {**stored, "provider": "ai-platform-gateway" if payload.get("modelPlaneBaseUrl") else "openai"}
        if payload.get("baseInstructions") is not None:
            live["instructions"] = payload["baseInstructions"]
        if "nativeWebSearchEnabled" in payload:
            live["native_search"] = payload["nativeWebSearchEnabled"]
        if "toolPolicy" in payload:
            live["tool_policy"] = payload["toolPolicy"]
        return httpx.Response(200, request=request, json={"thread": {"id": "thread"}})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    model_service = SimpleNamespace(get_model=AsyncMock())
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(), model_service=model_service,
        provider_service=SimpleNamespace(), assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32), runtime_url="http://runtime.test",
        runtime_internal_token="token", model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="target", http_client=client,
    )
    try:
        await plane.verify_thread(runtime_thread_id=str(uuid.uuid4()), tenant_id="tenant-a", user_id="user-a",
                                  session_id="session-a", model_id="selected-model")
        assert live == stored
        assert captured[0]["model"] == "selected-model"
        assert captured[0]["modelPlaneBaseUrl"] == plane.model_plane_base_url
        assert "toolPolicy" not in captured[0]
        assert "nativeWebSearchEnabled" not in captured[0]
        model_service.get_model.assert_not_awaited()
    finally:
        await client.aclose()
