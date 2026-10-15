"""Known provider failures retain their code without retrying dispatched work."""

from unittest.mock import AsyncMock

import httpx
import pytest
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner

from src.services.agent_runtime.model_plane import AgentModelPlane, AgentModelPlaneError
from tests.services.agent_runtime.test_model_plane import (
    _call,
    _Database,
    _profile,
    _ProviderService,
)


@pytest.mark.parametrize(
    ("wire", "content", "code"),
    [
        ("responses_v1", b'data: {"type":"response.created","sequence_number":2}\n\n',
         "RUNTIME_PROVIDER_STREAM_INVALID"),
        ("chat_completions", b'data: {"choices":[{"delta":{"content":"partial"}}]}\n\n',
         "RUNTIME_PROVIDER_STREAM_INCOMPLETE"),
    ],
)
async def test_known_stream_failure_is_recorded_once_without_retry(wire, content, code):
    requests = []

    async def handler(request):
        requests.append(request)
        return httpx.Response(200, request=request, content=content)

    async with httpx.AsyncClient(transport=httpx.MockTransport(handler)) as client:
        plane = AgentModelPlane(
            database=_Database(), provider_service=_ProviderService(),
            lease_signer=RuntimeModelLeaseSigner("x" * 32), http_client=client,
        )
        plane._fail_call = AsyncMock()
        plane._complete_call = AsyncMock()
        plane._mark_unknown_if_dispatched = AsyncMock()
        call = _call(_profile(), wire_protocol=wire)
        with pytest.raises(AgentModelPlaneError, match=code):
            _ = [chunk async for chunk in plane.stream(
                body={"input": "hello"}, turn_metadata={}, authorized_call=call,
            )]
        assert len(requests) == 1
        plane._fail_call.assert_awaited_once_with(call.call_id, code, dispatched=True)
        plane._complete_call.assert_not_awaited()
        plane._mark_unknown_if_dispatched.assert_awaited_once_with(call.call_id)
