"""SDK integration test — runs against the live server."""
from __future__ import annotations

import asyncio
import os
import sys

import pytest

# Add SDK to path
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))

from ai_assistant import AssistantClient
from ai_assistant.models.events import EventType

SERVER = os.environ.get("SDK_TEST_SERVER", "http://127.0.0.1:8080")
API_KEY = os.environ.get("SDK_TEST_API_KEY", "")

pytestmark = pytest.mark.skipif(
    not API_KEY,
    reason="live SDK tests require SDK_TEST_API_KEY",
)


async def test_non_streaming_chat():
    """Test basic non-streaming chat."""
    async with AssistantClient(api_key=API_KEY, base_url=SERVER) as client:
        resp = await client.chat.send("Say just OK", session_id="sdk-test-chat")
        assert resp.content, "Expected non-empty content"
        assert resp.session_id, "Expected session_id"
        print(f"  PASS: chat.send() → {resp.content[:50]}")


async def test_streaming_chat():
    """Test SSE streaming chat."""
    async with AssistantClient(api_key=API_KEY, base_url=SERVER) as client:
        events = []
        text = ""
        async for event in client.chat.stream("Say hello in 5 words", session_id="sdk-test-stream"):
            events.append(event)
            if event.event_type == EventType.TEXT_DELTA:
                text += (
                    str(event.data)
                    if isinstance(event.data, str)
                    else event.data.get("text", event.data.get("data", ""))
                )
            if event.event_type == EventType.DONE:
                break

        assert len(events) > 0, "Expected at least one event"
        assert text, "Expected text from text_delta events"
        print(f"  PASS: chat.stream() → {len(events)} events, text={text[:50]}")


async def test_sessions():
    """Test session CRUD."""
    async with AssistantClient(api_key=API_KEY, base_url=SERVER) as client:
        # List
        sessions = await client.sessions.list(limit=5)
        assert isinstance(sessions, list), "Expected list"
        print(f"  PASS: sessions.list() → {len(sessions)} sessions")


async def test_models_and_tools():
    """Test discovery endpoints."""
    async with AssistantClient(api_key=API_KEY, base_url=SERVER) as client:
        tools = await client.tools.list_tools()
        assert len(tools) >= 10, f"Expected 10+ tools, got {len(tools)}"
        print(f"  PASS: tools.list() → {len(tools)} tools")


async def test_knowledge():
    """Test KB dataset listing."""
    async with AssistantClient(api_key=API_KEY, base_url=SERVER) as client:
        datasets = await client.knowledge.list_datasets()
        assert isinstance(datasets, list), "Expected list"
        print(f"  PASS: knowledge.list_datasets() → {len(datasets)} datasets")


async def main():
    print(f"Testing SDK against {SERVER}")
    print("=" * 50)

    tests = [
        ("Non-streaming chat", test_non_streaming_chat),
        ("Streaming chat", test_streaming_chat),
        ("Sessions", test_sessions),
        ("Models & Tools", test_models_and_tools),
        ("Knowledge", test_knowledge),
    ]

    passed = 0
    failed = 0
    for name, test_fn in tests:
        try:
            await test_fn()
            passed += 1
        except Exception as e:
            print(f"  FAIL: {name} — {e}")
            failed += 1

    print("=" * 50)
    print(f"Results: {passed}/{passed + failed} passed")
    if failed == 0:
        print("ALL PASS")
    return failed == 0


if __name__ == "__main__":
    success = asyncio.run(main())
    sys.exit(0 if success else 1)
