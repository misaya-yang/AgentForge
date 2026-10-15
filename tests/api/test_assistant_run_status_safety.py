from __future__ import annotations

from types import SimpleNamespace

import pytest

from src.api.v1._assistant_routes import runs
from src.core.auth.user_resolver import UserContext


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("status", "stored_error", "expected_reason"),
    [
        ("cancelled", "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED", "runtime_restart_interrupted"),
        ("failed", "private provider exception with credential", None),
    ],
)
async def test_run_status_projects_only_safe_terminal_reason(
    monkeypatch: pytest.MonkeyPatch,
    status: str,
    stored_error: str,
    expected_reason: str | None,
) -> None:
    async def fetch_run(_database, _run_id, _tenant_id, _user_id):
        return {"run_id": "run-1", "status": status, "error": stored_error, "usage": None}

    monkeypatch.setattr(runs, "fetch_agent_runtime_run", fetch_run)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(database=object())))
    user = UserContext(
        user_id="user-1", tenant_id="tenant-1", tier="normal", roles=[], is_authenticated=True,
    )
    result = await runs.get_run_status("run-1", request, user)

    assert result.run["error"] is None
    assert result.run["terminal_reason"] == expected_reason
