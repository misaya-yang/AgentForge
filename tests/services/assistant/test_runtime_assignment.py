from __future__ import annotations

import pytest

from src.services.assistant_runtime_assignment import (
    AssistantRuntimeAssignmentStore,
    RuntimeAssignmentPolicy,
    runtime_assignment_policy_from_env,
)


def test_single_kernel_policy_is_stable_and_prompt_agnostic() -> None:
    policy = RuntimeAssignmentPolicy(default_owner="agent_runtime", kernel_revision="kernel-1")
    assert policy.choose(tenant_id="tenant-a", session_id="session-a") == (
        "agent_runtime", "single_kernel"
    )
    assert policy.choose(tenant_id="other", session_id="other") == (
        "agent_runtime", "single_kernel"
    )


def test_rollout_environment_cannot_select_a_second_owner(monkeypatch) -> None:
    monkeypatch.setenv("ASSISTANT_RUNTIME_DEFAULT_OWNER", "python_control")
    monkeypatch.setenv("ASSISTANT_RUNTIME_CANARY_PERCENT", "25")
    monkeypatch.setenv("AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION", "kernel-1")
    policy = RuntimeAssignmentPolicy.from_env()
    assert policy.default_owner == "agent_runtime"
    assert policy.kernel_revision == "kernel-1"


class _FakeDatabase:
    def __init__(self) -> None:
        self.rows: dict[tuple[str, str, str], dict[str, object]] = {}

    async def fetchrow(self, query: str, *args):
        key = (str(args[0]), str(args[1]), str(args[2]))
        if "INSERT INTO" in query:
            row = {
                "tenant_id": key[0],
                "user_id": key[1],
                "session_id": key[2],
                "runtime_owner": args[3],
                "kernel_revision": args[4],
                "assignment_reason": args[5],
            }
            return self.rows.setdefault(key, row)
        return self.rows.get(key)


@pytest.mark.asyncio
async def test_runtime_assignment_is_idempotent_and_scope_bound() -> None:
    store = AssistantRuntimeAssignmentStore(_FakeDatabase())
    first = await store.bind(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        runtime_owner="agent_runtime", kernel_revision=None,
    )
    replay = await store.bind(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        runtime_owner="agent_runtime", kernel_revision=None,
    )
    assert first == replay
    assert await store.resolve(
        tenant_id="tenant-b", user_id="user-a", session_id="session-a"
    ) is None

def test_runtime_assignment_policy_uses_new_revision_name(monkeypatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_AGENT_RUNTIME_KERNEL_REVISION", "kernel-1")
    assert runtime_assignment_policy_from_env() == ("agent_runtime", "kernel-1")


@pytest.mark.asyncio
async def test_bind_new_session_is_single_kernel_and_existing_assignment_wins() -> None:
    store = AssistantRuntimeAssignmentStore(_FakeDatabase())
    first = await store.bind_new_session(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        policy=RuntimeAssignmentPolicy(default_owner="agent_runtime", kernel_revision="kernel-1"),
    )
    replay = await store.bind_new_session(
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        policy=RuntimeAssignmentPolicy(default_owner="agent_runtime", kernel_revision="kernel-2"),
    )
    assert first.runtime_owner == "agent_runtime"
    assert replay == first
