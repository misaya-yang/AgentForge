from __future__ import annotations

import asyncio
import inspect

import pytest

from src.services.agent_runtime import control_plane
from src.services.agent_runtime.control import event_stream, thread_lifecycle, turn_start
from src.services.agent_runtime.control import types as control_types
from src.services.assistant_entry.launch_resolution import resolve_agent_launch


def test_facade_preserves_type_identity_and_descriptors() -> None:
    plane = control_plane.AgentRuntimeControlPlane

    assert control_plane.AgentTurn is control_types.AgentTurn
    assert control_plane.AgentRuntimeControlError is control_types.AgentRuntimeControlError
    for name in {
        "_assert_dynamic_tool_fingerprint",
        "_dynamic_tools",
        "_validate_catalog_descriptor",
        "_allowlisted_catalog_descriptors",
        "_snapshot_capability_allowlist",
        "_dynamic_tool_fingerprint",
        "_attachment_tool_descriptor",
        "_worker_ready_for_writes",
        "_readonly_capability_payload",
        "_turn_prompt_readonly",
    }:
        assert isinstance(inspect.getattr_static(plane, name), staticmethod)
    assert isinstance(
        inspect.getattr_static(plane, "_attach_read_attachment_descriptors"),
        classmethod,
    )


@pytest.mark.asyncio
async def test_facade_cleanup_wrapper_delegates_to_authority(monkeypatch) -> None:
    instance = object.__new__(control_plane.AgentRuntimeControlPlane)
    seen: dict[str, object] = {}

    async def fake_cleanup(plane, **kwargs):
        seen.update(plane=plane, **kwargs)
        return True

    monkeypatch.setattr(thread_lifecycle, "cleanup_session", fake_cleanup)

    assert await instance.cleanup_session(
        tenant_id="tenant", user_id="user", session_id="session"
    )
    assert seen == {
        "plane": instance,
        "tenant_id": "tenant",
        "user_id": "user",
        "session_id": "session",
    }


@pytest.mark.asyncio
async def test_start_turn_wrapper_forwards_live_private_seams(monkeypatch) -> None:
    instance = object.__new__(control_plane.AgentRuntimeControlPlane)
    sentinel = object()
    seen: dict[str, object] = {}

    async def fake_start(plane, **kwargs):
        seen.update(plane=plane, **kwargs)
        return sentinel

    monkeypatch.setattr(turn_start, "start_turn", fake_start)

    result = await instance.start_turn(
        tenant_id="tenant",
        user_id="user",
        session_id="session",
        message="hello",
        model_id="model",
        reasoning_option=None,
        legacy_thinking_level=None,
        max_tokens=None,
    )

    assert result is sentinel
    assert seen["_logger"] is control_plane.logger
    assert seen["_provider_revision_func"] is control_plane._provider_revision


def test_module_projector_wrapper_keeps_monkeypatch_seam(monkeypatch) -> None:
    sentinel = {"event_type": "patched"}

    def fake_projector(envelope, parent_turn_id):
        assert envelope == {"data": {}}
        assert parent_turn_id == "parent"
        return sentinel

    monkeypatch.setattr(event_stream, "project_child_runtime_event", fake_projector)

    assert control_plane._project_child_runtime_event({"data": {}}, "parent") is sentinel


@pytest.mark.asyncio
async def test_thread_creation_lock_serializes_waiters_and_cleans_up() -> None:
    instance = object.__new__(control_plane.AgentRuntimeControlPlane)
    instance._thread_locks = {}
    instance._thread_lock_users = {}
    key = ("tenant", "user", "session")
    order: list[str] = []

    holder_entered = asyncio.Event()
    waiter_entered = asyncio.Event()
    third_entered = asyncio.Event()
    release_holder = asyncio.Event()
    release_waiter = asyncio.Event()
    release_third = asyncio.Event()

    async def contender(name, entered, release):
        async with thread_lifecycle._thread_creation_lock(instance, key):
            order.append(name)
            entered.set()
            await release.wait()

    holder = asyncio.create_task(contender("holder", holder_entered, release_holder))
    await holder_entered.wait()
    first_lock = instance._thread_locks[key]
    waiter = asyncio.create_task(contender("waiter", waiter_entered, release_waiter))
    await asyncio.sleep(0)
    assert instance._thread_locks[key] is first_lock
    assert instance._thread_lock_users[first_lock] == 2

    release_holder.set()
    await waiter_entered.wait()
    third = asyncio.create_task(contender("third", third_entered, release_third))
    await asyncio.sleep(0)
    assert instance._thread_locks[key] is first_lock
    assert instance._thread_lock_users[first_lock] == 2

    release_waiter.set()
    await third_entered.wait()
    release_third.set()
    await asyncio.gather(holder, waiter, third)

    assert order == ["holder", "waiter", "third"]
    assert instance._thread_locks == {}
    assert instance._thread_lock_users == {}


@pytest.mark.asyncio
async def test_control_plane_rejects_launch_scope_before_io() -> None:
    launch = await resolve_agent_launch(
        entrypoint="assistant",
        tenant_id="other-tenant",
        user_id="user",
        session_id="session",
        model_id="model",
        model_service=None,
    )
    instance = object.__new__(control_plane.AgentRuntimeControlPlane)

    with pytest.raises(
        control_plane.AgentRuntimeControlError,
        match="AI_PLATFORM_AGENT_RUNTIME_LAUNCH_SCOPE_MISMATCH",
    ):
        await instance.start_turn(
            tenant_id="tenant",
            user_id="user",
            session_id="session",
            message="hello",
            model_id="model",
            reasoning_option=None,
            legacy_thinking_level=None,
            max_tokens=None,
            resolved_agent_launch=launch,
        )


@pytest.mark.asyncio
async def test_control_plane_never_invents_default_agent_spec() -> None:
    instance = object.__new__(control_plane.AgentRuntimeControlPlane)

    with pytest.raises(
        control_plane.AgentRuntimeControlError,
        match="AI_PLATFORM_AGENT_RUNTIME_LAUNCH_REQUIRED",
    ):
        await instance.start_turn(
            tenant_id="tenant",
            user_id="user",
            session_id="session",
            message="hello",
            model_id="model",
            reasoning_option=None,
            legacy_thinking_level=None,
            max_tokens=None,
        )
