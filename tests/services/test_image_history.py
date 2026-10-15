from datetime import datetime, timedelta, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from starlette.requests import Request

from src.core.auth.user_resolver import UserContext
from src.services.assistant_entry.source_access import ConversationSources
from src.services.images.history import image_history, merge_image_history

START = datetime(2026, 9, 27, tzinfo=timezone.utc)
TASK = "imt_" + "a" * 20
TURN = "turn-a"


def turn(status="unknown", **overrides):
    return {
        "turn_id": TURN,
        "task_id": TASK,
        "status": status,
        "task_status": status,
        "created_at": START + timedelta(seconds=1),
        "prompt": "fixture",
        "result": {},
        **overrides,
    }


def user_message():
    return {
        "role": "user",
        "content": "🎨 fixture",
        "timestamp": START.isoformat(),
        "metadata": None,
    }


@pytest.mark.parametrize(
    "status,expected,uncertain",
    [
        ("running", "running", False),
        ("completed", "succeeded", False),
        ("failed", "failed", False),
        ("unknown", "failed", True),
        ("cancelled", "cancelled", False),
    ],
)
def test_image_recovery_preserves_original_identity_and_actual_outcome(status, expected, uncertain):
    artifacts = (
        [{"turn_id": TURN, "artifact_id": "art_display", "variant": "display"}]
        if status == "completed"
        else []
    )
    history = merge_image_history([user_message()], [turn(status)], artifacts, None, {})
    assert len(history) == 2
    meta = history[1]["metadata"]
    summary = meta["process_summary"]
    assert meta["image_task_id"] == TASK and meta["image_turn_id"] == TURN
    assert summary["status"] == expected and summary["outcome_uncertain"] == uncertain
    assert "run_id" not in summary and "provider_error" not in str(history)
    assert meta["image_generating"] == (status == "running")


def test_display_and_raw_family_deduplicates_legacy_success_and_preserves_body():
    old = {
        "role": "assistant",
        "content": "original image body",
        "metadata": {"artifact_ids": ["art_display"]},
    }
    artifacts = [
        {"turn_id": TURN, "artifact_id": "art_raw", "variant": "raw"},
        {"turn_id": TURN, "artifact_id": "art_display", "variant": "display"},
    ]
    result = merge_image_history([user_message(), old], [turn("completed")], artifacts, None, {})
    assert len(result) == 2 and result[1]["content"] == "original image body"
    assert result[1]["metadata"]["artifact_ids"] == ["art_display"]
    assert len(merge_image_history(result, [turn("completed")], artifacts, None, {})) == 2


def test_task_terminal_replaces_stale_running_turn_and_legacy_unknown_deduplicates():
    old = {
        "role": "assistant",
        "content": "Earlier public guidance",
        "metadata": {"process_summary": {"diagnostic_id": TASK}},
    }
    result = merge_image_history(
        [user_message(), old], [turn("running", task_status="unknown")], [], None, {}
    )
    assert len(result) == 2 and result[1]["metadata"]["process_summary"]["outcome_uncertain"]
    assert not result[1]["metadata"]["image_generating"]


def test_completed_without_nonzero_artifacts_stays_unknown():
    result = merge_image_history([user_message()], [turn("completed")], [], None, {})
    assert result[1]["metadata"]["process_summary"]["outcome_uncertain"]
    assert "artifact_ids" not in result[1]["metadata"]


def test_revoked_source_hides_legacy_body_and_deduplicates_with_original_identity():
    old = {"role": "assistant", "content": "", "metadata": {"source_access_revoked": True}}
    original = [None, {"artifact_ids": ["art_display"], "private": "PRIVATE_MARKER"}]
    artifacts = [{"turn_id": TURN, "artifact_id": "art_display", "variant": "display"}]
    sources = ConversationSources(frozenset({"private"}), {}, frozenset({"private"}))
    result = merge_image_history(
        [user_message(), old], [turn("completed")], artifacts, sources, {}, original
    )
    assert len(result) == 2 and result[1]["content"] == ""
    assert result[1]["metadata"]["source_access_revoked"]
    assert "PRIVATE_MARKER" not in str(result) and "artifact_ids" not in result[1]["metadata"]


def test_two_same_prompts_remain_distinct_and_mixed_history_keeps_order():
    later = {
        "role": "user",
        "content": "later text",
        "timestamp": (START + timedelta(seconds=4)).isoformat(),
    }
    result = merge_image_history(
        [user_message(), later],
        [
            turn(),
            turn(
                turn_id="turn-b", task_id="imt_" + "b" * 20, created_at=START + timedelta(seconds=2)
            ),
        ],
        [],
        None,
        {},
    )
    assert [m["role"] for m in result] == ["user", "assistant", "assistant", "user"]
    assert result[1]["metadata"]["image_task_id"] != result[2]["metadata"]["image_task_id"]


@pytest.mark.asyncio
async def test_ledger_queries_are_owner_scoped_and_do_not_execute():
    db = SimpleNamespace(fetch=AsyncMock(side_effect=[[turn()], []]))
    req = Request(
        {"type": "http", "headers": [], "app": SimpleNamespace(state=SimpleNamespace(database=db))}
    )
    user = UserContext(user_id="user-a", tenant_id="tenant-a", is_authenticated=True)
    result = await image_history(req, user, "session-a", [user_message()], None, {}, limit=200)
    assert len(result) == 2 and db.fetch.await_count == 2
    for call in db.fetch.call_args_list:
        sql, *args = call.args
        assert args[:3] == ["session-a", "tenant-a", "user-a"]
        assert "SELECT" in sql and "INSERT" not in sql and "UPDATE" not in sql
