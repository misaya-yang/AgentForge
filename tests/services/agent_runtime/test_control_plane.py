from __future__ import annotations

import json
import uuid
from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from typing import Any

import httpx
import pytest
from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1
from ai_gateway_contracts.agent_runtime import runtime_sha256
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner
from ai_gateway_core.models import get_builtin_model_capabilities

from src.services.agent_runtime.control_plane import (
    BASE_AGENT_INSTRUCTIONS_V1,
    GENERIC_AGENT_INSTRUCTIONS_V1,
    AgentRuntimeControlError,
    AgentRuntimeControlPlane,
    AgentTurn,
    _project_child_runtime_event,
)
from src.services.assistant_entry.launch_resolution import resolve_agent_launch


class _StaticCatalogClient:
    async def fetch_catalog(self, _query):
        return {
            "schema_version": "agent-capability-catalog/v1",
            "capability_revision": 7,
            "tools": [],
            "mcp": [],
            "deferred": [],
        }


def test_child_runtime_events_project_to_subagent_lifecycle() -> None:
    parent_turn_id = "parent-turn"
    started = _project_child_runtime_event(
        {
            "event_type": "run_started",
            "data": {
                "run_id": "child-turn",
                "thread_id": "child-thread",
                "session_id": "session-a",
                "status": "running",
            },
        },
        parent_turn_id,
    )
    assert started is not None
    assert started["event_type"] == "subagent_started"
    assert started["data"]["agent_id"] == "child-thread"
    assert started["data"]["parent_task_id"] == parent_turn_id

    finished = _project_child_runtime_event(
        {
            "event_type": "run_finished",
            "data": {
                "run_id": "child-turn",
                "thread_id": "child-thread",
                "session_id": "session-a",
                "status": "succeeded",
                "exit": "succeeded",
            },
        },
        parent_turn_id,
    )
    assert finished is not None
    assert finished["event_type"] == "subagent_finished"
    assert finished["data"]["status"] == "completed"

    parent = {"event_type": "run_finished", "data": {"run_id": parent_turn_id}}
    assert _project_child_runtime_event(parent, parent_turn_id) is parent
    assert _project_child_runtime_event(
        {"event_type": "thinking_delta", "data": {"run_id": "child-turn"}},
        parent_turn_id,
    ) is None


class _Database:
    def __init__(self, runtime_thread_id: uuid.UUID) -> None:
        self.runtime_thread_id = runtime_thread_id
        self.issued_snapshot: dict[str, Any] | None = None
        self.lease_id: uuid.UUID | None = None
        self.snapshot_id: uuid.UUID | None = None
        self.run_id: uuid.UUID | None = None

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        if "FROM assistant_runtime_threads" in query:
            return {
                "runtime_thread_id": self.runtime_thread_id,
                "last_sequence": 9,
                "dynamic_tool_fingerprint": (
                    AgentRuntimeControlPlane._dynamic_tool_fingerprint({})
                ),
            }
        if "issue_assistant_runtime_turn" in query:
            self.snapshot_id = args[0]
            self.lease_id = args[1]
            self.run_id = args[2]
            self.issued_snapshot = json.loads(args[8])
            return {"issued": True}
        if "FROM assistant_runtime_model_leases" in query:
            now = datetime.now(timezone.utc)
            return {
                "schema_version": "agent-runtime-model-lease/v1",
                "issued_at": now,
                "expires_at": now + timedelta(minutes=15),
            }
        raise AssertionError(f"unexpected query: {query}")

    async def execute(self, query: str, *args: Any) -> str:
        del query, args
        return "UPDATE 1"


class _ModelService:
    async def get_model(self, tenant_id: str, model_id: str, *, provider_id: str | None = None) -> dict[str, Any]:
        del provider_id
        assert tenant_id == "tenant-a"
        assert model_id == "qwen3.7-plus"
        profile = get_builtin_model_capabilities("dashscope", model_id)
        assert profile is not None
        return {
            "model_id": model_id,
            "provider_id": "dashscope",
            "is_enabled": True,
            "effective_capabilities": profile,
            "capability_revision": 7,
            "context_window": 1_000_000,
            "max_output_tokens": 65_536,
            "input_price_per_1k": 0.001,
            "output_price_per_1k": 0.002,
        }


class _ProviderService:
    async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        assert provider_id == "dashscope"
        return {
            "provider_id": provider_id,
            "api_type": "openai",
            "is_enabled": True,
            # A stale provider-level declaration must not override the model profile.
            "metadata": {"wire_protocol": "chat_completions"},
            "updated_at": "provider-revision-1",
        }


def _connector_snapshot(config: dict[str, Any]) -> dict[str, Any]:
    return {
        "agent_spec": {"channel": "api"},
        "capabilities": [
            {
                "type": "connector",
                "id": "confluence_read",
                "version": "v1",
                "schema_hash": "sha256:" + "a" * 64,
                "config": config,
            }
        ],
    }


def test_connector_allowlist_keeps_only_signed_non_secret_binding() -> None:
    with pytest.raises(AgentRuntimeControlError):
        AgentRuntimeControlPlane._snapshot_capability_allowlist(
            _connector_snapshot(
                {
                    "provider": "confluence",
                    "tool_name": "confluence_read",
                    "principal_type": "service_account",
                    "grant_id": "00000000-0000-0000-0000-000000000001",
                    "api_token": "must-not-cross-boundary",
                }
            )
        )


def test_builtin_empty_capabilities_inherit_platform_catalog() -> None:
    assert (
        AgentRuntimeControlPlane._snapshot_capability_allowlist(
            {"agent_spec": {"channel": "builtin"}, "capabilities": []}
        )
        is None
    )


def test_published_empty_capabilities_remain_closed() -> None:
    assert (
        AgentRuntimeControlPlane._snapshot_capability_allowlist(
            {"agent_spec": {"channel": "api"}, "capabilities": []}
        )
        == []
    )


@pytest.mark.parametrize("channel", ["preview", "api", "hosted_private"])
@pytest.mark.parametrize("mode", ["auto", "tool", "off"])
async def test_agent_knowledge_binding_exposes_only_its_read_tool(channel, mode) -> None:
    snapshot = {
        "agent_spec": {"channel": channel},
        "capabilities": [],
        "knowledge": {"datasets": ["kb-authorized"], "retrieval": {"mode": mode}},
    }
    allowlist = AgentRuntimeControlPlane._snapshot_capability_allowlist(snapshot)
    descriptor = {
        "id": "search_knowledge_base", "name": "search_knowledge_base",
        "version": None, "schema_hash": "sha256:" + "a" * 64,
    }
    unrelated = {**descriptor, "id": "write_file", "name": "write_file"}
    allowed = AgentRuntimeControlPlane._allowlisted_catalog_descriptors(
        [descriptor, unrelated], allowlist,
    )
    assert allowed == ([descriptor] if mode != "off" else [])
    snapshot["knowledge"]["datasets"] = []
    assert AgentRuntimeControlPlane._snapshot_capability_allowlist(snapshot) == []


def test_responses_tool_controls_are_preserved_in_runtime_readonly_snapshot() -> None:
    payload = AgentRuntimeControlPlane._readonly_capability_payload(
        {
            "responses_tool_names": ["search_knowledge_base"],
            "responses_tool_choice": {"type": "function", "name": "search_knowledge_base"},
            "responses_parallel_tool_calls": False,
        },
        tenant_id="tenant-a",
        capability_revision=3,
    )

    assert payload["responses_tool_names"] == ["search_knowledge_base"]
    assert payload["responses_tool_choice"] == {
        "type": "function",
        "name": "search_knowledge_base",
    }
    assert payload["responses_parallel_tool_calls"] is False


def test_connector_allowlist_catalog_binding_uses_signed_channel() -> None:
    allowlist = AgentRuntimeControlPlane._snapshot_capability_allowlist(
        _connector_snapshot({"provider": "confluence"})
    )
    assert allowlist == [
        {
            "type": "connector",
            "name": "confluence_read",
            "id": "confluence_read",
            "version": "v1",
            "schema_hash": "sha256:" + "a" * 64,
            "connector_binding": {
                "binding_type": "catalog",
                "provider": "confluence",
                "tool_name": "confluence_read",
                "principal_type": None,
                "grant_id": None,
                "channel": "api",
            },
        }
    ]


class _AssignmentStore:
    async def resolve(self, **scope: str) -> SimpleNamespace:
        assert scope == {
            "tenant_id": "tenant-a",
            "user_id": "user-a",
            "session_id": "session-a",
        }
        return SimpleNamespace(
            runtime_owner="agent_runtime",
            kernel_revision="kernel-1",
        )


def test_signed_capability_catalog_allowlist_withholds_unbound_and_rejects_unsealed() -> None:
    """The allowlist scopes the catalog; a version/hash mismatch still fails closed.

    The worker publishes its whole registry and does not apply the requested
    allowlist, so descriptors the Agent never configured are expected in the
    response and are withheld. A descriptor that *is* allowlisted but whose
    pinned version or schema hash differs is tampering and must still raise.
    """

    allowlist = [
        {
            "type": "mcp",
            "name": "tool-a",
            "id": "server-a",
            "version": "v1",
            "schema_hash": "sha256:" + "a" * 64,
        }
    ]
    selected = AgentRuntimeControlPlane._allowlisted_catalog_descriptors(
        [
            {
                "name": "tool-a",
                "id": "server-a",
                "version": "v1",
                "schema_hash": "sha256:" + "a" * 64,
            }
        ],
        allowlist,
    )
    assert [item["name"] for item in selected] == ["tool-a"]
    # Not named by the allowlist: withheld from the Agent, not fatal to the turn.
    assert (
        AgentRuntimeControlPlane._allowlisted_catalog_descriptors(
            [
                {
                    "name": "tool-b",
                    "id": "server-b",
                    "version": "v1",
                    "schema_hash": "sha256:" + "b" * 64,
                }
            ],
            allowlist,
        )
        == []
    )

    platform_without_legacy_hash = [
        {
            "type": "platform",
            "name": "safe-read",
            "id": "safe-read",
            "version": None,
            "schema_hash": None,
        }
    ]
    selected = AgentRuntimeControlPlane._allowlisted_catalog_descriptors(
        [
            {
                "name": "safe-read",
                "id": "safe-read",
                "version": None,
                "schema_hash": "sha256:" + "c" * 64,
            }
        ],
        platform_without_legacy_hash,
    )
    assert selected[0]["schema_hash"] == "sha256:" + "c" * 64
    with pytest.raises(AgentRuntimeControlError, match="SCOPE_MISMATCH"):
        AgentRuntimeControlPlane._allowlisted_catalog_descriptors(
            [
                {
                    "name": "tool-a",
                    "id": "server-a",
                    "version": "v2",
                    "schema_hash": "sha256:" + "a" * 64,
                }
            ],
            allowlist,
        )


def test_mcp_snapshot_preserves_exact_runtime_principal_binding() -> None:
    connection_id = str(uuid.uuid4())
    schema_hash = "sha256:" + "a" * 64
    snapshot = {
        "agent_spec": {"channel": "api"},
        "capabilities": [
            {
                "type": "mcp",
                "id": "tenant_search",
                "version": "v1",
                "schema_hash": schema_hash,
                "risk": "low",
                "config": {
                    "connection_id": connection_id,
                    "principal_type": "user_delegated",
                    "risk": "low",
                },
            }
        ],
    }

    allowlist = AgentRuntimeControlPlane._snapshot_capability_allowlist(snapshot)

    assert allowlist is not None
    assert allowlist[0]["connector_binding"] == {
        "binding_type": "grant",
        "provider": "mcp",
        "tool_name": "tenant_search",
        "principal_type": "user_delegated",
        "grant_id": None,
        "connection_id": connection_id,
        "schema_hash": schema_hash,
        "risk_level": "low",
        "channel": "api",
    }


def test_runtime_uses_native_multi_agent_and_hides_legacy_loop_alias() -> None:
    config = AgentRuntimeControlPlane._runtime_model_config(
        SimpleNamespace(model_plane_base_url="http://gateway.test/model-plane"),
        "qwen3.7-plus",
    )
    assert config["features"]["multi_agent_v2"] == {
        "enabled": True,
        "max_concurrent_threads_per_session": 6,
    }
    assert config["features"]["standalone_web_search"] is False
    assert config["web_search"] == "disabled"
    native_config = AgentRuntimeControlPlane._runtime_model_config(
        SimpleNamespace(model_plane_base_url="http://gateway.test/model-plane"),
        "qwen3.7-plus",
        native_web_search_enabled=True,
    )
    assert native_config["web_search"] == "live"
    dynamic = AgentRuntimeControlPlane._dynamic_tools(
        {
            "tools": [
                {
                    "name": "spawn_subagent",
                    "description": "Legacy Python subagent loop",
                    "schema": {"type": "object"},
                    "read_only": True,
                },
                {
                    "name": "tool_search",
                    "description": "Compatibility tool search",
                    "schema": {"type": "object"},
                    "read_only": True,
                },
                {
                    "name": "context_compact",
                    "description": "Compatibility compaction control",
                    "schema": {"type": "object"},
                    "read_only": False,
                },
                {
                    "name": "search_knowledge_base",
                    "description": "Search knowledge",
                    "schema": {"type": "object"},
                    "read_only": True,
                },
            ]
        }
    )
    assert [tool["name"] for tool in dynamic] == ["search_knowledge_base"]


def test_dynamic_tools_exposes_deferred_only_for_exact_snapshot_allowlist() -> None:
    deferred = {
        "name": "document_write",
        "description": "Write a document",
        "schema": {"type": "object"},
        "read_only": False,
        "id": "docs:document_write",
        "version": "v1",
        "schema_hash": "sha256:" + "a" * 64,
    }
    readonly = {
        "tools": [],
        "mcp": [],
        "deferred": [deferred],
        "capability_allowlist": [
            {
                "type": "tool",
                "name": deferred["name"],
                "id": deferred["id"],
                "version": deferred["version"],
                "schema_hash": deferred["schema_hash"],
            }
        ],
    }
    assert [tool["name"] for tool in AgentRuntimeControlPlane._dynamic_tools(readonly)] == [
        "document_write"
    ]
    readonly["capability_allowlist"][0]["schema_hash"] = "sha256:" + "b" * 64
    assert AgentRuntimeControlPlane._dynamic_tools(readonly) == []


def test_attachment_refs_do_not_duplicate_worker_catalog_descriptor() -> None:
    readonly = AgentRuntimeControlPlane._readonly_capability_payload(
        {
            "attachments": {"refs": ["art_1111111111111111", "art_2222222222222222"]},
        },
        tenant_id="tenant-a",
        capability_revision=7,
    )
    AgentRuntimeControlPlane._attach_read_attachment_descriptors(
        readonly,
        tenant_id="tenant-a",
        capability_revision=7,
    )
    assert readonly["attachment_tools"] == []
    assert [item["payload"]["content_ref"] for item in readonly["items"]] == [
        "art_1111111111111111", "art_2222222222222222",
    ]
    assert AgentRuntimeControlPlane._dynamic_tools(readonly) == []


def test_attachment_descriptor_is_turn_scoped_not_thread_fingerprint_state() -> None:
    descriptor = AgentRuntimeControlPlane._attachment_tool_descriptor(
        tenant_id="tenant-a",
        capability_revision=7,
        references=["art_1111111111111111"],
    )
    assert AgentRuntimeControlPlane._dynamic_tool_fingerprint({}) == (
        AgentRuntimeControlPlane._dynamic_tool_fingerprint({"attachment_tools": [descriptor]})
    )


def test_deferred_worker_gate_requires_explicit_runtime_configuration(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_ENABLED", "true")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED", "true")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_URL", "http://worker")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET", "configured")
    assert AgentRuntimeControlPlane._worker_ready_for_writes()
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED", "false")
    assert not AgentRuntimeControlPlane._worker_ready_for_writes()
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED", "true")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_ENABLED", "false")
    assert not AgentRuntimeControlPlane._worker_ready_for_writes()


@pytest.mark.asyncio
async def test_memory_context_is_tenant_scoped_and_mode_gated() -> None:
    class Memory:
        def __init__(self) -> None:
            self.calls: list[tuple[str, str, int]] = []

        async def get_long_term_context(self, *, tenant_id: str, user_id: str, limit: int):
            self.calls.append((tenant_id, user_id, limit))
            return {
                "preferences": {"language": "zh-CN"},
                "frequent_memories": [{"key": "project", "value": "alpha"}],
            }

    memory = Memory()
    plane = object.__new__(AgentRuntimeControlPlane)
    plane.memory_service = memory
    assert (
        await plane._load_memory_context(tenant_id="tenant-a", user_id="user-a", mode="off") is None
    )
    assert (
        await plane._load_memory_context(tenant_id="tenant-a", user_id="user-a", mode="session")
        is None
    )
    loaded = await plane._load_memory_context(tenant_id="tenant-a", user_id="user-a", mode="user")
    assert loaded == {
        "status": "available",
        "context": {
            "preferences": {"language": "zh-CN"},
            "frequent_memories": [{"key": "project", "value": "alpha", "access_count": 0}],
        },
    }
    assert memory.calls == [("tenant-a", "user-a", 20)]

    class Broken:
        async def get_long_term_context(self, **_kwargs: Any):
            raise RuntimeError("memory unavailable")

    plane.memory_service = Broken()
    with pytest.raises(AgentRuntimeControlError, match="MEMORY_UNAVAILABLE"):
        await plane._load_memory_context(tenant_id="tenant-a", user_id="user-a", mode="strict")
    degraded = await plane._load_memory_context(tenant_id="tenant-a", user_id="user-a", mode="auto")
    assert degraded == {"status": "unavailable", "reason": "memory_lookup_failed"}


@pytest.mark.asyncio
async def test_empty_developer_instructions_use_stable_generic_default() -> None:
    captured: dict[str, Any] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        captured.update(json.loads(request.content))
        return httpx.Response(200, request=request, json={"thread": {"id": "thread-a"}})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=_Database(uuid.uuid4()),
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    try:
        await plane._resume_thread(
            runtime_thread_id=uuid.uuid4(),
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            developer_instructions="",
        )
    finally:
        await client.aclose()
    assert captured["developerInstructions"] == GENERIC_AGENT_INSTRUCTIONS_V1


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("entrypoint", "channel", "publication_id"),
    [("studio_preview", "preview", None), ("published_agent", "api", "publication-a")],
)
async def test_version_first_turn_binds_signed_instructions_at_thread_creation(
    entrypoint: str, channel: str, publication_id: str | None,
) -> None:
    runtime_thread_id = uuid.uuid4()
    requests: list[tuple[str, dict[str, Any]]] = []

    class FreshDatabase(_Database):
        async def fetchrow(self, query: str, *args: Any):
            if "FROM assistant_runtime_threads" in query:
                return None
            return await super().fetchrow(query, *args)

    async def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        requests.append((request.url.path, body))
        if request.url.path.endswith("/threads") or request.url.path.endswith("/resume"):
            return httpx.Response(200, request=request, json={"thread": {"id": str(runtime_thread_id)}})
        if request.url.path.endswith("/turns"):
            return httpx.Response(200, request=request, json={"turn": {
                "id": body["runId"], "items": [], "itemsView": "notLoaded", "status": "inProgress",
            }})
        raise AssertionError(request.url.path)

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=FreshDatabase(runtime_thread_id),
        model_service=_ModelService(), provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test", runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1", http_client=client,
    )
    try:
        builtin = await resolve_agent_launch(
            entrypoint="assistant", tenant_id="tenant-a", user_id="user-a",
            session_id="session-a", model_id="qwen3.7-plus",
            model_service=plane.model_service,
        )
        version = builtin.to_dict()
        version["identity"].update(
            agent_id="agent-a", agent_version_id="version-a",
            publication_id=publication_id, channel=channel, entrypoint=entrypoint,
            auth_mode="private" if channel == "preview" else "token",
        )
        version["agent_spec"].update(
            agentId="agent-a", agentVersionId="version-a", channel=channel,
            developerInstructions="Version one instructions: R3_OLD",
        )
        version["fingerprints"]["spec"] = runtime_sha256(version["agent_spec"])
        snapshot = ResolvedAgentLaunchV1.parse(version).to_control_snapshot()
        await plane.start_turn(
            tenant_id="tenant-a", user_id="user-a", session_id="session-a",
            message="hello", model_id="qwen3.7-plus", reasoning_option="auto",
            legacy_thinking_level=None, max_tokens=64,
            resolved_agent_snapshot=snapshot, enable_dynamic_tools=False,
        )
    finally:
        await client.aclose()

    first = next(body for path, body in requests if path.endswith("/threads"))
    assert first["start"]["developerInstructions"] == "Version one instructions: R3_OLD"
    assert first["start"]["baseInstructions"] == BASE_AGENT_INSTRUCTIONS_V1
    assert len([path for path, _ in requests if path.endswith("/turns")]) == 1


@pytest.mark.asyncio
async def test_control_plane_pins_qwen_responses_profile_into_turn_snapshot() -> None:
    runtime_thread_id = uuid.uuid4()
    database = _Database(runtime_thread_id)
    captured: dict[str, Any] = {"requests": []}

    async def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        captured["requests"].append((request.url.path, body))
        if request.url.path.endswith("/resume"):
            return httpx.Response(
                200,
                request=request,
                json={"thread": {"id": str(runtime_thread_id)}},
            )
        captured["url"] = str(request.url)
        captured["headers"] = dict(request.headers)
        captured["body"] = body
        return httpx.Response(
            200,
            request=request,
            json={
                "turn": {
                    "id": captured["body"]["runId"],
                    "items": [],
                    "itemsView": "notLoaded",
                    "status": "inProgress",
                }
            },
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=database,
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
        capability_catalog_client=_StaticCatalogClient(),
    )
    try:
        readonly_capabilities = {
            "knowledge": {"dataset_ids": ["dataset-a"]},
            "attachments": {"refs": ["art_1111111111111111"]},
            "web_search": {"enabled": True, "max_results": 3},
        }
        launch = await resolve_agent_launch(
            entrypoint="assistant",
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            model_service=plane.model_service,
            readonly_capabilities=readonly_capabilities,
            reasoning_option="auto",
            max_tokens=1024,
            temperature=0.2,
            memory_mode="auto",
        )
        turn = await plane.start_turn(
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            message="你好",
            model_id="qwen3.7-plus",
            reasoning_option="auto",
            legacy_thinking_level=None,
            max_tokens=1024,
            temperature=0.2,
            readonly_capabilities=readonly_capabilities,
            resolved_agent_launch=launch,
        )
    finally:
        await client.aclose()

    assert captured["url"].endswith(f"/internal/v1/threads/{runtime_thread_id}/turns")
    assert captured["body"]["runId"] == turn.run_id
    assert captured["body"]["effort"] == "minimal"
    readonly = captured["body"]["readonly"]
    assert readonly["schema_version"] == "agent-readonly-capability/v1"
    assert readonly["tenant_id"] == "tenant-a"
    assert readonly["capability_revision"] == 7
    assert [item["kind"] for item in readonly["items"]] == [
        "knowledge",
        "attachment",
        "context",
    ]
    assert readonly["tools"] == []
    assert readonly["mcp"] == []
    assert readonly["deferred"] == []
    assert readonly["attachment_tools"] == []
    assert "capability_allowlist" not in readonly
    assert captured["headers"]["x-ai-tenant-id"] == "tenant-a"
    assert captured["requests"][0][0] == f"/internal/v1/threads/{runtime_thread_id}/resume"
    assert captured["requests"][0][1]["model"] == "qwen3.7-plus"
    assert captured["requests"][0][1]["modelPlaneBaseUrl"] == (
        "http://gateway.test/internal/v1/agent-model-plane"
    )
    base_instructions = captured["requests"][0][1]["baseInstructions"]
    assert "Use only tools exposed for the turn" in base_instructions and "Keep retrieval bounded" in base_instructions
    assert "When tool_search and tool_call are exposed" in base_instructions
    assert "Discover tools that are not listed" not in base_instructions and "use tool_search then tool_call" not in base_instructions
    assert captured["requests"][0][1]["developerInstructions"]
    assert database.issued_snapshot is not None
    assert [
        item["kind"] for item in database.issued_snapshot["readonly_capabilities"]["items"]
    ] == [
        "knowledge",
        "attachment",
        "context",
    ]
    assert database.issued_snapshot["model"]["wire_protocol"] == "responses_v1"
    assert database.issued_snapshot["parameters"] == {"temperature": 0.2}
    assert database.issued_snapshot["input"] == {"message": "你好"}
    frozen = database.issued_snapshot["eval_fingerprint"]
    assert frozen["system_prompt_hash"] == runtime_sha256(database.issued_snapshot["instructions"]).removeprefix("sha256:")
    assert frozen["tool_schema_hash"] == runtime_sha256(
        AgentRuntimeControlPlane._dynamic_tools(database.issued_snapshot["readonly_capabilities"])
    ).removeprefix("sha256:")
    assert database.issued_snapshot["memory"]["policy"] == {
        "authoritative_profile": "basic",
        "agent_memory_mode": "user",
        "memory_principal": "user-a",
    }
    assert database.issued_snapshot["reasoning"] == {
        "requested_option": "auto",
        "effective_option": "minimal",
        "adapter_id": "reasoning/dashscope-responses-effort-v1",
        "canonical_effort": "minimal",
        "settings": {
            "effort": "minimal",
            "chat_enabled": True,
            "chat_budget_tokens": 128,
        },
        "fallback_reason": None,
    }


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("developer_instructions", "expected_instructions"),
    [
        ("Version one instructions: R3_OLD", "Version one instructions: R3_OLD"),
        ("", GENERIC_AGENT_INSTRUCTIONS_V1),
    ],
)
async def test_catalog_is_fetched_before_first_thread_and_dynamic_tools_are_pinned(
    developer_instructions: str, expected_instructions: str,
) -> None:
    runtime_thread_id = uuid.uuid4()
    requests: list[tuple[str, dict[str, Any]]] = []

    class FreshDatabase:
        fingerprint: str | None = None

        async def fetchrow(self, query: str, *_args: Any):
            if "assistant_runtime_threads" in query:
                return None
            raise AssertionError(f"unexpected query: {query}")

        async def execute(self, query: str, *args: Any):
            assert "dynamic_tool_fingerprint" in query
            self.fingerprint = args[0]
            return "UPDATE 1"

    async def handler(request: httpx.Request) -> httpx.Response:
        body = json.loads(request.content)
        requests.append((request.url.path, body))
        if request.url.path.endswith("/catalog"):
            return httpx.Response(
                200,
                request=request,
                json={
                    "tools": [
                        {
                            "name": "search_knowledge_base",
                            "id": "search_knowledge_base",
                            "version": None,
                            "schema_hash": "sha256:" + "a" * 64,
                            "description": "Read Knowledge.",
                            "schema": {"type": "object", "properties": {}},
                            "source": "knowledge",
                            "kind": "knowledge",
                            "read_only": True,
                            "tenant_id": "tenant-a",
                            "capability_revision": 7,
                        },
                        {
                            "name": "tool_search",
                            "id": "tool_search",
                            "version": None,
                            "schema_hash": "sha256:" + "b" * 64,
                            "description": "Search the stable tool catalog.",
                            "schema": {"type": "object", "properties": {}},
                            "source": "assistant",
                            "kind": "platform_tool_discovery",
                            "read_only": True,
                            "tenant_id": "tenant-a",
                            "capability_revision": 7,
                        },
                    ]
                },
            )
        assert request.url.path.endswith("/threads")
        return httpx.Response(
            200,
            request=request,
            json={"thread": {"id": str(runtime_thread_id)}},
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=FreshDatabase(),
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    plane.capability_plane_url = "http://capability.test/internal/v1/capabilities"
    try:
        readonly = plane._readonly_capability_payload(
            {"knowledge": {"dataset_ids": ["dataset-a"]}},
            tenant_id="tenant-a",
            capability_revision=7,
        )
        await plane._fetch_capability_catalog(
            readonly,
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            capability_revision=7,
        )
        await plane.ensure_thread(
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            readonly_capabilities=readonly,
            developer_instructions=developer_instructions,
        )
    finally:
        await client.aclose()

    thread_request = next(body for path, body in requests if path.endswith("/threads"))
    assert thread_request["start"]["approvalPolicy"] == "on-request"
    assert thread_request["start"]["baseInstructions"] == BASE_AGENT_INSTRUCTIONS_V1
    assert thread_request["start"]["developerInstructions"] == expected_instructions
    assert thread_request["start"]["dynamicTools"][0]["name"] == "search_knowledge_base"
    assert len(thread_request["start"]["dynamicTools"]) == 1
    assert all(tool["name"] != "tool_search" for tool in thread_request["start"]["dynamicTools"])
    assert thread_request["start"]["dynamicTools"][0]["inputSchema"] == {
        "type": "object",
        "properties": {},
    }
    assert "parameters" not in thread_request["start"]["dynamicTools"][0]
    assert readonly["capability_allowlist"] == [
        {
            "type": "knowledge",
            "name": "search_knowledge_base",
            "id": "search_knowledge_base",
            "version": None,
            "schema_hash": "sha256:" + "a" * 64,
        },
        {
            "type": "platform_tool_discovery",
            "name": "tool_search",
            "id": "tool_search",
            "version": None,
            "schema_hash": "sha256:" + "b" * 64,
        },
    ]
    assert len(plane.database.fingerprint or "") == 64


@pytest.mark.asyncio
async def test_generic_assistant_exposes_worker_writes_only_when_worker_is_ready(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    descriptor = {
        "name": "generate_document",
        "id": "generate_document",
        "version": "1",
        "schema_hash": "sha256:" + "c" * 64,
        "description": "Generate an Office document.",
        "schema": {"type": "object", "properties": {}},
        "source": "capability_worker",
        "kind": "tool",
        "read_only": False,
        "tenant_id": "tenant-a",
        "capability_revision": 7,
    }

    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            request=request,
            json={"tools": [], "mcp": [], "deferred": [descriptor]},
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=_Database(uuid.uuid4()),
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    plane.capability_plane_url = "http://capability.test/internal/v1/capabilities"
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_ENABLED", "true")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED", "true")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_URL", "http://worker")
    monkeypatch.setenv("AI_PLATFORM_CAPABILITY_LEASE_SIGNING_SECRET", "configured")
    readonly = plane._readonly_capability_payload(
        None,
        tenant_id="tenant-a",
        capability_revision=7,
    )
    try:
        await plane._fetch_capability_catalog(
            readonly,
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            capability_revision=7,
        )
        monkeypatch.setenv("AI_PLATFORM_CAPABILITY_WORKER_WRITES_ENABLED", "false")
        degraded = plane._readonly_capability_payload(
            None,
            tenant_id="tenant-a",
            capability_revision=7,
        )
        with pytest.raises(
            AgentRuntimeControlError,
            match="AI_PLATFORM_AGENT_RUNTIME_WRITE_CAPABILITY_DEGRADED",
        ):
            await plane._fetch_capability_catalog(
                degraded,
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
                model_id="qwen3.7-plus",
                capability_revision=7,
            )
    finally:
        await client.aclose()

    assert readonly["deferred"] == [descriptor]
    assert readonly["capability_allowlist"] == [
        {
            "type": "tool",
            "name": "generate_document",
            "id": "generate_document",
            "version": "1",
            "schema_hash": "sha256:" + "c" * 64,
        }
    ]
    assert [tool["name"] for tool in plane._dynamic_tools(readonly)] == ["generate_document"]


@pytest.mark.asyncio
async def test_catalog_refresh_preserves_connector_binding_and_fills_live_schema() -> None:
    runtime_thread_id = uuid.uuid4()

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/catalog")
        return httpx.Response(
            200,
            request=request,
            json={
                "tools": [
                    {
                        "name": "confluence_read",
                        "id": "confluence_read",
                        "version": None,
                        "schema_hash": "sha256:" + "b" * 64,
                        "description": "Read Confluence.",
                        "schema": {"type": "object", "properties": {}},
                        "kind": "tool",
                        "read_only": True,
                        "tenant_id": "tenant-a",
                        "capability_revision": 7,
                    }
                ],
                "mcp": [],
                "deferred": [],
            },
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=_Database(runtime_thread_id),
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    plane.capability_plane_url = "http://capability.test/internal/v1/capabilities"
    readonly = {
        "tools": [],
        "mcp": [],
        "capability_allowlist": [],
    }
    binding = {
        "type": "connector",
        "name": "confluence_read",
        "id": "confluence_read",
        "version": None,
        "schema_hash": None,
        "connector_binding": {
            "binding_type": "catalog",
            "provider": "confluence",
            "tool_name": "confluence_read",
            "principal_type": None,
            "grant_id": None,
            "channel": "api",
        },
    }
    try:
        await plane._fetch_capability_catalog(
            readonly,
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            capability_revision=7,
            capability_allowlist=[binding],
        )
    finally:
        await client.aclose()
    result = readonly["capability_allowlist"][0]
    assert result["schema_hash"] == "sha256:" + "b" * 64
    assert result["connector_binding"] == binding["connector_binding"]
    assert "secret_ref" not in result and "api_token" not in result


@pytest.mark.asyncio
async def test_existing_thread_uses_persisted_dynamic_tool_fingerprint_after_restart() -> None:
    runtime_thread_id = uuid.uuid4()
    readonly = {
        "tools": [
            {
                "name": "search_knowledge_base",
                "description": "Read Knowledge.",
                "schema": {"type": "object", "properties": {}},
                "kind": "knowledge",
                "read_only": True,
                "tenant_id": "tenant-a",
                "capability_revision": 7,
            }
        ],
        "mcp": [],
    }

    class ExistingDatabase:
        async def fetchrow(self, query: str, *_args: Any):
            assert "assistant_runtime_threads" in query
            return {
                "runtime_thread_id": runtime_thread_id,
                "last_sequence": 3,
                "dynamic_tool_fingerprint": AgentRuntimeControlPlane._dynamic_tool_fingerprint(
                    readonly
                ),
            }

    plane = AgentRuntimeControlPlane(
        database=ExistingDatabase(),
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=httpx.AsyncClient(transport=httpx.MockTransport(lambda _request: None)),
    )
    try:
        thread = await plane.ensure_thread(
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            readonly_capabilities=readonly,
        )
    finally:
        await plane.http_client.aclose()
    assert thread["runtime_thread_id"] == runtime_thread_id


@pytest.mark.asyncio
async def test_existing_unbound_thread_requires_recreation_instead_of_catalog_adoption() -> None:
    runtime_thread_id = uuid.uuid4()
    readonly = {
        "tools": [
            {
                "name": "search_knowledge_base",
                "description": "Read Knowledge.",
                "schema": {"type": "object", "properties": {}},
                "kind": "knowledge",
                "read_only": True,
                "tenant_id": "tenant-a",
                "capability_revision": 7,
            }
        ],
        "mcp": [],
    }

    class DatabaseWithUnboundThread:
        async def fetchrow(self, query: str, *_args: Any):
            assert "assistant_runtime_threads" in query
            return {
                "runtime_thread_id": runtime_thread_id,
                "last_sequence": 3,
                "dynamic_tool_fingerprint": None,
            }

    database = DatabaseWithUnboundThread()
    plane = AgentRuntimeControlPlane(
        database=database,
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=httpx.AsyncClient(
            transport=httpx.MockTransport(lambda _request: httpx.Response(500))
        ),
    )
    try:
        with pytest.raises(
            AgentRuntimeControlError,
            match="AI_PLATFORM_AGENT_RUNTIME_CAPABILITY_THREAD_RECREATE_REQUIRED",
        ):
            await plane.ensure_thread(
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
                model_id="qwen3.7-plus",
                readonly_capabilities=readonly,
            )
    finally:
        await plane.http_client.aclose()


@pytest.mark.asyncio
async def test_control_plane_fails_before_lease_issue_when_runtime_resume_rejects() -> None:
    runtime_thread_id = uuid.uuid4()
    database = _Database(runtime_thread_id)

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/resume")
        return httpx.Response(400, request=request)

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=database,
        model_service=_ModelService(),
        provider_service=_ProviderService(),
        assignment_store=_AssignmentStore(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
        capability_catalog_client=_StaticCatalogClient(),
    )
    try:
        launch = await resolve_agent_launch(
            entrypoint="assistant",
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            model_id="qwen3.7-plus",
            model_service=plane.model_service,
            reasoning_option="auto",
            max_tokens=1024,
        )
        with pytest.raises(
            AgentRuntimeControlError,
            match="AI_PLATFORM_AGENT_RUNTIME_THREAD_RESUME_FAILED",
        ):
            await plane.start_turn(
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
                message="你好",
                model_id="qwen3.7-plus",
                reasoning_option="auto",
                legacy_thinking_level=None,
                max_tokens=1024,
                resolved_agent_launch=launch,
            )
    finally:
        await client.aclose()

    assert database.issued_snapshot is None


@pytest.mark.asyncio
async def test_control_plane_relays_and_enriches_run_started_frame() -> None:
    run_id = str(uuid.uuid4())
    snapshot_id = str(uuid.uuid4())
    runtime_thread_id = str(uuid.uuid4())
    started = {
        "schema_version": "assistant-turn-contract/v1",
        "sequence": 0,
        "event_type": "run_started",
        "data": {"run_id": run_id},
        "timestamp": "2026-08-21T00:00:00Z",
    }
    finished = {
        "schema_version": "assistant-turn-contract/v1",
        "sequence": 1,
        "event_type": "run_finished",
        "data": {"run_id": run_id, "status": "succeeded"},
        "timestamp": "2026-08-21T00:00:01Z",
    }

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith(f"/{runtime_thread_id}/events")
        payload = (
            f"event: run_started\ndata: {json.dumps(started)}\n\n"
            f"event: run_finished\ndata: {json.dumps(finished)}\n\n"
        ).encode()
        return httpx.Response(
            200,
            request=request,
            headers={"content-type": "text/event-stream"},
            content=payload,
        )

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(),
        model_service=SimpleNamespace(),
        provider_service=SimpleNamespace(),
        assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    completed: list[tuple[uuid.UUID, str]] = []

    async def complete(completed_run_id: uuid.UUID, status: str) -> None:
        completed.append((completed_run_id, status))

    plane._complete_run = complete  # type: ignore[method-assign]
    turn = AgentTurn(
        runtime_thread_id=runtime_thread_id,
        run_id=run_id,
        snapshot_id=snapshot_id,
        lease_id=str(uuid.uuid4()),
        after_sequence=-1,
        requested_reasoning_option="auto",
        effective_reasoning_option="minimal",
        reasoning_adapter_id="reasoning/dashscope-responses-effort-v1",
        capability_revision=7,
        fallback_reason=None,
    )
    try:
        chunks = [
            chunk
            async for chunk in plane.stream_events(
                turn=turn,
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
            )
        ]
    finally:
        await client.aclose()

    assert len(chunks) == 2
    started_payload = json.loads(chunks[0].decode().split("data: ", 1)[1])
    assert started_payload["data"] == {
        "run_id": run_id,
        "requested_reasoning_option": "auto",
        "effective_reasoning_option": "minimal",
        "reasoning_adapter_id": "reasoning/dashscope-responses-effort-v1",
        "capability_revision": 7,
        "reasoning_fallback_reason": None,
        "kernel": "agent",
    }
    assert completed == [(uuid.UUID(run_id), "succeeded")]


@pytest.mark.asyncio
async def test_interrupt_uses_strict_empty_runtime_body_and_closes_run() -> None:
    runtime_thread_id = str(uuid.uuid4())
    run_id = str(uuid.uuid4())
    captured: dict[str, Any] = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        captured["body"] = json.loads(request.content)
        return httpx.Response(200, request=request, json={})

    client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    plane = AgentRuntimeControlPlane(
        database=SimpleNamespace(),
        model_service=SimpleNamespace(),
        provider_service=SimpleNamespace(),
        assignment_store=SimpleNamespace(),
        lease_signer=RuntimeModelLeaseSigner("x" * 32),
        runtime_url="http://runtime.test",
        runtime_internal_token="runtime-token",
        model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
        kernel_revision="kernel-1",
        http_client=client,
    )
    completed: list[tuple[uuid.UUID, str]] = []

    async def complete(completed_run_id: uuid.UUID, status: str) -> None:
        completed.append((completed_run_id, status))

    plane._complete_run = complete  # type: ignore[method-assign]
    try:
        await plane.interrupt_turn(
            runtime_thread_id=runtime_thread_id,
            turn_id=run_id,
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            reason="client_interrupt",
        )
    finally:
        await client.aclose()

    assert captured["body"] == {}
    assert completed == [(uuid.UUID(run_id), "cancelled")]


@pytest.mark.asyncio
async def test_dynamic_tool_catalog_does_not_depend_on_turn_scoped_bindings() -> None:
    """The thread's tool catalog must be identical with and without a dataset.

    ``ensure_thread`` pins ``dynamic_tool_fingerprint`` from a catalog fetched
    with no readonly input, while ``start_turn`` recomputes it from the turn's
    bindings. If the catalog varies with a turn-scoped binding the two
    fingerprints diverge and every KB-bound turn is rejected with
    ``CAPABILITY_THREAD_RECREATE_REQUIRED`` — which is what happened when
    ``search_knowledge_base`` was withheld for turns with no dataset selected.
    """

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path.endswith("/catalog")
        return httpx.Response(
            200,
            request=request,
            json={
                "tools": [
                    {
                        "name": "search_knowledge_base",
                        "id": "search_knowledge_base",
                        "version": None,
                        "schema_hash": "sha256:" + "a" * 64,
                        "description": "Read Knowledge.",
                        "schema": {"type": "object", "properties": {}},
                        "source": "knowledge",
                        "kind": "knowledge",
                        "read_only": True,
                        "tenant_id": "tenant-a",
                        "capability_revision": 7,
                    }
                ]
            },
        )

    async def fingerprint_for(readonly_input: dict[str, Any] | None) -> str:
        client = httpx.AsyncClient(transport=httpx.MockTransport(handler))
        plane = AgentRuntimeControlPlane(
            database=SimpleNamespace(),
            model_service=SimpleNamespace(),
            provider_service=SimpleNamespace(),
            assignment_store=SimpleNamespace(),
            lease_signer=RuntimeModelLeaseSigner("x" * 32),
            runtime_url="http://runtime.test",
            runtime_internal_token="runtime-token",
            model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
            kernel_revision="kernel-1",
            http_client=client,
        )
        plane.capability_plane_url = "http://capability.test/internal/v1/capabilities"
        try:
            readonly = plane._readonly_capability_payload(
                readonly_input, tenant_id="tenant-a", capability_revision=7
            )
            await plane._fetch_capability_catalog(
                readonly,
                tenant_id="tenant-a",
                user_id="user-a",
                session_id="session-a",
                model_id="qwen3.7-plus",
                capability_revision=7,
            )
        finally:
            await client.aclose()
        return plane._dynamic_tool_fingerprint(readonly)

    unbound = await fingerprint_for(None)
    bound = await fingerprint_for({"knowledge": {"dataset_ids": ["dataset-a"]}})
    assert unbound == bound


@pytest.mark.asyncio
async def test_recovery_only_wakes_original_scoped_turn_without_new_authority() -> None:
    thread, run = str(uuid.uuid4()), str(uuid.uuid4())
    requests = []

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.path == f"/internal/v1/threads/{thread}/turns/{run}/recover"
        assert json.loads(request.content) == {}
        assert request.headers["x-ai-tenant-id"] == "tenant-a"
        assert request.headers["x-ai-user-id"] == "user-a"
        assert request.headers["x-ai-session-id"] == "session-a"
        requests.append(request)
        return httpx.Response(200, request=request, json={"runId": run, "turnId": run, "status": "recovery_requested"})

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        plane = AgentRuntimeControlPlane(
            database=SimpleNamespace(), model_service=SimpleNamespace(), provider_service=SimpleNamespace(),
            assignment_store=SimpleNamespace(), lease_signer=RuntimeModelLeaseSigner("x" * 32),
            runtime_url="http://runtime.test", runtime_internal_token="fixture-token", model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
            kernel_revision="kernel-1", http_client=client,
        )
        for _ in range(2):
            result = await plane.recover_turn(runtime_thread_id=thread, turn_id=run, tenant_id="tenant-a", user_id="user-a", session_id="session-a")
            assert result["turnId"] == run
    assert len(requests) == 2
