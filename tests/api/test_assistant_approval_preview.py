"""Approval preview is scoped, hash-bound, and omits hidden tool bytes."""

from __future__ import annotations

import hashlib
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.v1._assistant_routes.runs import approve_tool_call
from src.api.v1._assistant_routes.schemas import ApprovalRequest
from src.core.auth.user_resolver import UserContext
from src.services.assistant_entry.approval_preview import owner_approval_preview


class _DB:
    def __init__(self, arguments, tool_name="execute_python_code"):
        self.arguments = arguments
        self.tool_name = tool_name

    async def fetchrow(self, sql, *args):
        assert "JOIN assistant_runs" in sql
        assert args == ("approval-a", "tenant-a", "user-a", "session-a", "thread-a")
        return {
            "run_id": "run-a", "tool_name": self.tool_name,
            "arguments": self.arguments, "status": "pending",
        }


def _summary(arguments, tool_name="execute_python_code"):
    digest = hashlib.sha256(
        json.dumps(arguments, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    return {
        "run_id": "run-a", "tool_name": tool_name, "status": "pending",
        "arguments_hash": digest, "expires_in_seconds": 120,
    }


async def _preview(database, summary):
    return await owner_approval_preview(
        database, approval_id="approval-a", runtime_thread_id="thread-a",
        tenant_id="tenant-a", user_id="user-a", session_id="session-a",
        runtime_summary=summary,
    )


@pytest.mark.asyncio
async def test_python_preview_shows_code_and_input_names_without_file_bytes() -> None:
    arguments = {"code": "print(2 + 2)", "inputs": [{
        "filename": "input.csv", "size_bytes": 10, "content_base64": "SECRET_BYTES",
    }]}
    preview = await _preview(_DB(arguments), _summary(arguments))
    assert preview["can_approve"] is True
    assert preview["target"] == "Isolated Python execution"
    assert "print(2 + 2)" in json.dumps(preview)
    assert "input.csv" in json.dumps(preview)
    assert "SECRET_BYTES" not in json.dumps(preview)
    assert preview["authorization_scope"] == "this action only"


@pytest.mark.asyncio
async def test_changed_arguments_or_missing_target_cannot_be_approved() -> None:
    arguments = {"code": "print(2)"}
    changed = await _preview(_DB({"code": "print(3)"}), _summary(arguments))
    assert changed["can_approve"] is False

    confluence = {"action": "delete_page"}
    missing = await _preview(_DB(confluence, "confluence_write"), _summary(confluence, "confluence_write"))
    assert missing["can_approve"] is False


@pytest.mark.asyncio
async def test_local_node_secret_parameter_fails_closed() -> None:
    arguments = {"operation": "send", "device_id": "node-a", "arguments": {"api_token": "private"}}
    preview = await _preview(_DB(arguments, "local_node_action"), _summary(arguments, "local_node_action"))
    assert preview["can_approve"] is False
    assert "private" not in json.dumps(preview)


@pytest.mark.asyncio
async def test_quiz_and_document_generation_show_complete_reviewable_parameters() -> None:
    quiz = {"title": "Math quiz", "questions": [{"question_text": "1+1?", "correct_answer": ["B"]}]}
    checked = await _preview(_DB(quiz, "generate_quiz"), _summary(quiz, "generate_quiz"))
    assert checked["can_approve"] is True
    assert "1+1?" in json.dumps(checked)
    document = {"format": "docx", "title": "Report", "goal": "Summarize test", "body_markdown": "# Report"}
    checked_doc = await _preview(_DB(document, "mcp_docgen__generate_document"), _summary(document, "mcp_docgen__generate_document"))
    assert checked_doc["can_approve"] is True
    assert checked_doc["target"] == "Report"


@pytest.mark.asyncio
async def test_v1_decision_cannot_approve_unverified_action(monkeypatch) -> None:
    decisions = []

    class _Control:
        async def get_approval(self, **_kwargs):
            return {"status": "pending"}

        async def decide_approval(self, **kwargs):
            decisions.append(kwargs)

    async def _owner(*_args):
        return {"engine": "agent_runtime", "session_id": "session-a", "harness_thread_id": "thread-a"}

    async def _unverified(*_args, **kwargs):
        assert kwargs["runtime_thread_id"] == "thread-a"
        return {"can_approve": False}

    monkeypatch.setattr("src.api.v1._assistant_routes.runs.fetch_approval_run_owner", _owner)
    monkeypatch.setattr("src.api.v1._assistant_routes.runs.owner_approval_preview", _unverified)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        database=SimpleNamespace(
            fetch=AsyncMock(return_value=[]), fetchrow=AsyncMock(return_value=None),
        ), agent_runtime_control=_Control(),
    )))
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    with pytest.raises(HTTPException) as exc_info:
        await approve_tool_call("approval-a", ApprovalRequest(approved=True), request, user)
    assert exc_info.value.status_code == 409
    assert decisions == []

    await approve_tool_call("approval-a", ApprovalRequest(approved=False), request, user)
    assert len(decisions) == 1 and decisions[0]["approved"] is False

    async def _verified(*_args, **_kwargs):
        return {"can_approve": True}

    monkeypatch.setattr("src.api.v1._assistant_routes.runs.owner_approval_preview", _verified)
    await approve_tool_call("approval-a", ApprovalRequest(approved=True), request, user)
    assert decisions[-1]["approved"] is True
