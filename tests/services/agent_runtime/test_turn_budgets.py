import httpx
import pytest

from src.services.agent_runtime.control.turn_start import _resolve_output_limit
from src.services.agent_runtime.control_plane import AgentRuntimeControlPlane


def test_default_output_budget_is_bounded_below_model_capability() -> None:
    assert _resolve_output_limit(None, 131_072) == 32_768
    assert _resolve_output_limit(None, 131_072) * 24 < 1_000_000
    assert _resolve_output_limit(None, 4_096) == 4_096
    assert _resolve_output_limit(32_768, 131_072) == 32_768
    assert _resolve_output_limit(32_768, 16_384) == 16_384


@pytest.mark.asyncio
async def test_default_model_call_budget_has_retry_headroom(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("AI_PLATFORM_AGENT_RUNTIME_MAX_MODEL_CALLS_PER_TURN", raising=False)
    async with httpx.AsyncClient() as client:
        plane = AgentRuntimeControlPlane(
            database=object(),
            model_service=object(),
            provider_service=object(),
            assignment_store=object(),
            lease_signer=object(),
            runtime_url="http://runtime.test",
            runtime_internal_token="runtime-token",
            model_plane_base_url="http://gateway.test/internal/v1/agent-model-plane",
            kernel_revision="kernel-1",
            memory_service=object(),
            http_client=client,
        )
        assert plane.max_model_calls == 24
