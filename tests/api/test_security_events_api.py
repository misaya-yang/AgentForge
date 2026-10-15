from datetime import datetime, timezone
from unittest.mock import AsyncMock, MagicMock

import pytest

from tests.conftest import create_test_token


@pytest.mark.asyncio
async def test_security_breakdown_includes_status(async_client, test_app):
    token = create_test_token(user_id="admin", roles=["admin"])

    test_app.state.database = AsyncMock(enabled=True)
    test_app.state.database.get_security_event_breakdown = AsyncMock(return_value=[])
    test_app.state.database.get_security_event_last_ingested_at = AsyncMock(
        return_value=datetime.now(timezone.utc)
    )
    dispatcher = MagicMock()
    dispatcher.rbac.require = MagicMock(return_value=None)
    test_app.state.dispatcher = dispatcher

    response = await async_client.get(
        "/api/v1/metrics/security/breakdown?dimension=user&event_type=auth_failed",
        headers={"Authorization": f"Bearer {token}"},
    )

    assert response.status_code == 200
    body = response.json()
    assert "data_status" in body
    assert "data_freshness_minutes" in body
    assert "last_ingested_at" in body
