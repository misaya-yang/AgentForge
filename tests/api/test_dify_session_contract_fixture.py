"""Actual session routes must match the fixture consumed by the Web contract gate."""
from __future__ import annotations

import asyncio
import json
from datetime import datetime
from pathlib import Path
from uuid import UUID

import pytest
from fastapi.encoders import jsonable_encoder

from src.api.v1.sessions import SessionCreate, create_session, get_session
from src.core.auth.user_resolver import UserContext
from src.services.session.session_manager import SessionManager

FIXTURE = Path(__file__).resolve().parents[1] / "fixtures/adapters/dify_session_detail.json"


async def build_dify_session_fixture() -> dict:
    manager = SessionManager()
    user = UserContext(user_id="contract-user", tenant_id="contract-tenant", is_authenticated=True)
    responses = {}
    cases = [
        ("fresh", "service", {}),
        ("resumed", "service", {"dify_conversation": {"service_id": "service", "conversation_id": "saved-conversation"}}),
        ("mismatch", "other-service", {"dify_conversation": {"service_id": "service", "conversation_id": "must-not-use"}}),
        ("null", None, {}),
        ("wrong_binding", "service", {"dify_conversation": {"service_id": "other-service", "conversation_id": "must-not-use"}}),
    ]
    for index, (key, service, metadata) in enumerate(cases, 1):
        created = await create_session(
            SessionCreate(session_id=str(UUID(int=index)), service_id=service, metadata=metadata),
            manager, user,
        )
        # Control only nondeterministic fixture identity/time, not route output.
        session = await manager.get(created["session_id"])
        assert session is not None
        session.created_at = session.updated_at = datetime(2026, 1, 1)
        responses[key] = await get_session(created["session_id"], manager, user)
    return jsonable_encoder(responses)


@pytest.mark.asyncio
async def test_actual_session_routes_match_dify_web_fixture():
    assert await build_dify_session_fixture() == json.loads(FIXTURE.read_text())


if __name__ == "__main__":
    print(json.dumps(asyncio.run(build_dify_session_fixture()), indent=2, sort_keys=True))
