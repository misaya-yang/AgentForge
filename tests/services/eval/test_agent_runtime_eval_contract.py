from __future__ import annotations

from typing import Any

import pytest

from scripts.harness.agent_runtime_eval_contract import (
    validate_runtime_observation,
)

THREAD = {"thread_id": "root", "runtime": {"owner": "agent_runtime"}}


def _event(sequence: int, event_type: str, **data: Any) -> dict[str, Any]:
    return {
        "schema_version": "agent-event/v2",
        "sequence": sequence,
        "event": {
            "payload": {"event_type": event_type, "data": data},
        },
    }


def test_simple_and_complex_runtime_turns_have_v2_terminal_contract() -> None:
    simple = [
        _event(1, "run_started", run_id="turn-simple"),
        _event(2, "text_delta", content="200"),
        _event(3, "run_finished", run_id="turn-simple", status="succeeded"),
    ]
    complex_turn = [
        _event(1, "run_started", run_id="turn-complex"),
        _event(2, "tool_call_start", call_id="call-1", tool_name="search"),
        _event(3, "tool_call_result", call_id="call-1", status="succeeded"),
        _event(4, "tool_call_end", call_id="call-1", status="succeeded"),
        _event(5, "text_delta", content="grounded answer"),
        _event(6, "run_finished", run_id="turn-complex", status="succeeded"),
    ]
    assert validate_runtime_observation(THREAD, simple) == []
    assert validate_runtime_observation(THREAD, complex_turn) == []


def test_multi_turn_stop_and_continuation_keep_tool_receipts_closed() -> None:
    stopped = [
        _event(1, "run_started", run_id="turn-stop"),
        _event(2, "tool_call_start", call_id="call-stop", tool_name="read"),
        _event(3, "tool_call_result", call_id="call-stop", status="cancelled"),
        _event(4, "tool_call_end", call_id="call-stop", status="cancelled"),
        _event(5, "run_stopped", run_id="turn-stop", status="cancelled"),
    ]
    continuation = [
        _event(5, "run_started", run_id="turn-resume"),
        _event(6, "text_delta", content="continued"),
        _event(7, "run_finished", run_id="turn-resume", status="succeeded"),
    ]
    assert validate_runtime_observation(THREAD, stopped) == []
    assert validate_runtime_observation(THREAD, continuation) == []


def test_multi_agent_root_child_receipts_are_required_and_paired() -> None:
    events = [
        _event(1, "run_started", run_id="turn-agent"),
        _event(2, "subagent_started", agent_id="child-research"),
        _event(3, "subagent_finished", agent_id="child-research", status="succeeded"),
        _event(4, "text_delta", content="merged"),
        _event(5, "run_finished", run_id="turn-agent", status="succeeded"),
    ]
    assert validate_runtime_observation(THREAD, events) == []

    orphan = [
        events[0],
        events[1],
        _event(3, "text_delta", content="merged"),
        _event(4, "run_finished", run_id="turn-agent", status="succeeded"),
    ]
    assert "child lifecycle receipts must pair exactly" in validate_runtime_observation(THREAD, orphan)


def test_compaction_requires_retention_receipt() -> None:
    retained = [
        _event(1, "run_started", run_id="turn-compact"),
        _event(2, "compaction_started"),
        _event(3, "compaction_completed", retained_item_ids=["item-1", "item-2"]),
        _event(4, "text_delta", content="after compact"),
        _event(5, "run_finished", run_id="turn-compact", status="succeeded"),
    ]
    assert validate_runtime_observation(THREAD, retained) == []
    dropped = retained.copy()
    dropped[2] = _event(3, "compaction_completed", retained_item_ids=[])
    assert "compaction must emit retained item ids" in validate_runtime_observation(THREAD, dropped)


def test_native_context_compaction_matches_runtime_projection() -> None:
    events = [
        _event(1, "run_started", run_id="turn-compact"),
        _event(2, "context_compaction", compacted=True),
        _event(3, "text_delta", content="after compact"),
        _event(4, "run_finished", run_id="turn-compact", status="succeeded"),
    ]
    assert validate_runtime_observation(THREAD, events) == []

    events[1] = _event(2, "context_compaction", compacted=False)
    assert "context_compaction must confirm compacted=true" in validate_runtime_observation(
        THREAD, events
    )


def test_contract_rejects_python_owner_and_unpaired_tool_result() -> None:
    failures = validate_runtime_observation(
        {"runtime": {"owner": "python"}},
        [
            _event(1, "run_started", run_id="turn-invalid"),
            _event(2, "tool_call_result", call_id="orphan", status="succeeded"),
            _event(3, "run_finished", run_id="turn-invalid", status="succeeded"),
        ],
    )
    assert "thread.runtime.owner must be agent_runtime" in failures
    assert "orphan tool results: orphan" in failures


@pytest.mark.parametrize("bad_sequences", [[1, 1, 2], [2, 1, 3]])
def test_contract_rejects_non_monotonic_event_sequences(bad_sequences: list[int]) -> None:
    events = [
        _event(bad_sequences[0], "run_started", run_id="turn-invalid"),
        _event(bad_sequences[1], "text_delta", content="answer"),
        _event(
            bad_sequences[2], "run_finished", run_id="turn-invalid", status="succeeded"
        ),
    ]
    assert "event sequences must be strictly increasing and unique" in validate_runtime_observation(
        THREAD, events
    )
