from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from threading import Event
from typing import Any

from fastapi.testclient import TestClient

from tests.api.test_agent_runtime_api import PUBLICATION_ID, _Repository

pytest_plugins = ("tests.api.test_agent_runtime_api",)


def test_identical_concurrent_idempotent_requests_invoke_downstream_once(
    runtime_client: tuple[TestClient, _Repository, list[dict[str, Any]]],
) -> None:
    client, repository, captured = runtime_client
    headers = {"Authorization": "Bearer agt_valid", "Idempotency-Key": "concurrent-turn"}
    turn_started = Event()
    allow_completion = Event()
    control = client.app.state.agent_runtime_control
    original_start_turn = control.start_turn

    async def gated_start_turn(**kwargs: Any) -> Any:
        result = await original_start_turn(**kwargs)
        turn_started.set()
        if not allow_completion.wait(timeout=5):
            raise AssertionError("timed out waiting to complete the idempotent request")
        return result

    control.start_turn = gated_start_turn

    def send() -> Any:
        # TestClient owns a synchronous transport that is not safe to share
        # across worker threads. Independent clients still exercise the same
        # application and repository state, which is the tested boundary.
        with TestClient(client.app) as concurrent_client:
            return concurrent_client.post(
                f"/api/v1/agent-runtime/{PUBLICATION_ID}/chat/stream",
                headers=headers,
                json={"message": "execute once"},
            )

    with ThreadPoolExecutor(max_workers=2) as executor:
        winner_future = executor.submit(send)
        assert turn_started.wait(timeout=5)
        try:
            follower = executor.submit(send).result(timeout=5)
        finally:
            allow_completion.set()
        winner = winner_future.result(timeout=5)

    assert winner.status_code == 200
    assert follower.status_code == 409
    assert follower.json()["detail"]["code"] == "AGENT_RUNTIME_IDEMPOTENCY_IN_PROGRESS"
    assert len(captured) == 1

    replay = send()
    assert replay.status_code == 200
    assert replay.headers["x-idempotent-replay"] == "true"
    assert replay.text == winner.text
    assert len(captured) == 1
    row = next(iter(repository.idempotency.values()))
    assert row["status"] == "completed"
    assert list(client.app.state.session_manager.items) == [row["session_id"]]
