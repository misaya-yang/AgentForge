import importlib
from datetime import datetime, timezone
from unittest.mock import AsyncMock, patch

import pytest

from tests.conftest import create_test_token


@pytest.mark.asyncio
async def test_usage_summary_includes_status(async_client, test_app):
    token = create_test_token(user_id="admin", roles=["admin"])

    async def fake_summary(*args, **kwargs):
        return {
            "total_requests": 10,
            "success_rate": 90.0,
            "total_input_tokens": 100,
            "total_output_tokens": 200,
            "total_tokens": 300,
            "total_cost_usd": 1.23,
            "avg_latency_ms": 120,
            "start_date": "2026-01-08",
            "end_date": "2026-01-15",
        }

    usage_module = importlib.import_module("src.api.v1.usage")

    with patch.object(usage_module, "get_usage_recorder") as get_recorder:
        recorder = AsyncMock()
        recorder.get_usage_summary = AsyncMock(side_effect=fake_summary)
        recorder.get_last_ingested_at = AsyncMock(return_value=datetime.now(timezone.utc))
        get_recorder.return_value = recorder

        response = await async_client.get(
            "/api/v1/usage/summary",
            headers={"Authorization": f"Bearer {token}"},
        )

    assert response.status_code == 200
    body = response.json()
    assert "data_status" in body
    assert "data_freshness_minutes" in body
    assert "last_ingested_at" in body
