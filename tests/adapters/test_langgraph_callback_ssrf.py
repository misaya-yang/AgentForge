from __future__ import annotations

import pytest
from ai_gateway_core.exceptions import ValidationFailedError

from src.adapters.langgraph import LangGraphAdapter
from src.models.request import UnifiedRequest


class _FakeService:
    session_enabled = False
    assistant_id = "asst"


@pytest.mark.asyncio
async def test_remote_wait_rejects_private_callback_url(monkeypatch) -> None:
    adapter = LangGraphAdapter.__new__(LangGraphAdapter)
    adapter.assistant_id = "asst"
    adapter.service = _FakeService()
    adapter.invoke_endpoint = "/runs/wait"
    adapter.thread_invoke_endpoint = "/threads/{thread_id}/runs/wait"
    adapter._build_base_run_config = lambda *_args, **_kwargs: {}
    adapter._prepare_remote_run_payload = lambda _request, payload, _path: payload
    adapter._build_auth_headers = lambda _request: {}

    request = UnifiedRequest(
        request_id="req-1",
        service_id="svc",
        inputs=[],
        user_id="user",
        tenant_id="tenant",
        callback_url="http://127.0.0.1/steal",
    )

    with pytest.raises(ValidationFailedError, match="callback_url"):
        await adapter._remote_wait(request, messages=[])
