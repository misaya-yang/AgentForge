from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from src.services.agent_runtime.capability_catalog import CapabilityCatalogError
from src.services.agent_runtime.control.capability_catalog import fetch_capability_catalog
from src.services.agent_runtime.control.types import AgentRuntimeControlError


async def test_explicit_empty_agent_binding_has_no_catalog_or_inherited_tools():
    client = SimpleNamespace(fetch_catalog=AsyncMock(side_effect=AssertionError("no catalog grant")))
    plane = SimpleNamespace(capability_catalog_client=client)
    readonly = {"tools": [{"name": "must-not-inherit"}], "items": [], "attachment_tools": []}
    await fetch_capability_catalog(
        plane, readonly, tenant_id="tenant-a", user_id="anonymous-a", session_id="session-a",
        model_id="model-a", capability_revision=1, capability_allowlist=[],
    )
    client.fetch_catalog.assert_not_awaited()
    assert readonly["tools"] == readonly["mcp"] == readonly["deferred"] == []
    assert readonly["capability_allowlist"] == []
    assert readonly["_thread_capabilities"]["tools"] == []


@pytest.mark.parametrize("allowlist", [None, [{"id": "tool-a", "name": "tool-a"}]])
async def test_inherited_or_selected_capabilities_still_require_authorized_catalog(allowlist):
    client = SimpleNamespace(fetch_catalog=AsyncMock(side_effect=CapabilityCatalogError("CAPABILITY_CATALOG_UNAVAILABLE", "not authorized")))
    with pytest.raises(AgentRuntimeControlError, match="CAPABILITY_CATALOG_UNAVAILABLE"):
        await fetch_capability_catalog(
            SimpleNamespace(capability_catalog_client=client), {}, tenant_id="tenant-a",
            user_id="user-a", session_id="session-a", model_id="model-a",
            capability_revision=1, capability_allowlist=allowlist,
        )
    client.fetch_catalog.assert_awaited_once()
