"""Shared fixtures for the event-bus test suite.

The repo's top-level ``tests/conftest.py`` mounts ``mock_redis`` as an
``AsyncMock`` for the gateway's HTTP layer. We need the *real* protocol
surface (XADD, XREADGROUP, XPENDING, XAUTOCLAIM) — ``fakeredis.aioredis``
is the right tool: it implements the streams commands in-process, no
broker required.

Each test gets a fresh in-memory server so streams from one test cannot
bleed into another.
"""

from __future__ import annotations

import fakeredis.aioredis
import pytest
import pytest_asyncio


@pytest_asyncio.fixture
async def fake_redis():
    """Fresh in-memory async Redis client per test.

    ``FakeRedis`` carries its own backing dict so each fixture instance
    is isolated. We close it after the test to release the underlying
    connection pool.
    """
    client = fakeredis.aioredis.FakeRedis(decode_responses=False)
    try:
        yield client
    finally:
        try:
            await client.aclose()
        except Exception:
            # Some fakeredis builds raise on a second close — harmless.
            pass


# ----- pytest-asyncio mode override ---------------------------------------
#
# The repo-level ``pyproject.toml`` sets ``asyncio_mode = "auto"`` which
# auto-marks every coroutine test. That's fine for us. No further config
# needed here.


@pytest.fixture
def usage_payload_dict() -> dict:
    """Minimal valid ``UsageRecordedV1`` payload as a plain dict.

    Used by tests that want to construct envelopes inline without
    importing the model class repeatedly.
    """
    return {
        "tenant_id": "t1",
        "user_id": "u1",
        "model": "qwen-3.6",
        "provider": "dashscope",
        "service_id": "svc-x",
        "input_tokens": 100,
        "output_tokens": 200,
        "latency_ms": 1500,
        "first_token_ms": 300,
        "request_total_duration_ms": 1500,
        "status": "success",
        "request_type": "chat",
        "timestamp": 1714200000.0,
        "metadata": {"k": "v"},
    }
