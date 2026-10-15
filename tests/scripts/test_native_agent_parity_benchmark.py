from __future__ import annotations

import copy
import json
from pathlib import Path
from types import SimpleNamespace

import pytest

from scripts import native_agent_parity_benchmark as benchmark


class _FakeHTTPResponse:
    def __init__(
        self,
        *,
        events: list[dict[str, object]] | None = None,
        payload: dict[str, object] | None = None,
    ) -> None:
        self.status = 200
        self._lines = [
            b"data: " + json.dumps(event).encode() + b"\n\n" for event in (events or [])
        ]
        self._payload = json.dumps(payload or {}).encode()

    def __enter__(self) -> _FakeHTTPResponse:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def __iter__(self):
        return iter(self._lines)

    def read(self) -> bytes:
        return self._payload


def _ai_adapter() -> benchmark.AIPlatformAdapter:
    adapter = object.__new__(benchmark.AIPlatformAdapter)
    adapter._base = "http://gateway.test"
    adapter._token = "test-token"
    adapter._model_id = "qwen3.7-plus"
    adapter._temperature = 0.0
    adapter._max_tokens = 16384
    adapter._thinking_level = "low"
    adapter._execution_profile = "balanced"
    adapter._max_approval_rounds = 8
    adapter._task_output_formats = {"task": "json", "tagged": "text"}
    adapter._sessions = {"task": "session-1"}
    return adapter


def _run_started_event(*, run_id: str, attempt_number: int) -> dict[str, object]:
    return {
        "event_type": "run_started",
        "data": {
            "run_id": run_id,
            "session_id": "session-1",
            "attempt_id": f"attempt-{attempt_number}",
            "attempt_number": attempt_number,
            "context_snapshot": {
                "model_id": "qwen3.7-plus",
                "provider": "dashscope",
                "snapshot_hash": f"snapshot-{attempt_number}",
                "bootstrap": {"startup_config_fingerprint": "startup-fingerprint"},
            },
        },
    }


def _approval_required_event(
    *,
    run_id: str,
    tool_name: str,
    approval_id: str = "approval-1",
    attempt_number: int = 1,
) -> dict[str, object]:
    return {
        "event_type": "approval_required",
        "data": {
            "run_id": run_id,
            "session_id": "session-1",
            "approval_id": approval_id,
            "tool_name": tool_name,
            "attempt_id": f"attempt-{attempt_number}",
            "attempt_number": attempt_number,
            "terminal_envelope": {
                "status": "blocked",
                "resume_ready": True,
            },
        },
    }


def test_manifest_is_an_eight_task_three_way_result_suite() -> None:
    manifest = benchmark.load_manifest()

    assert manifest["systems"] == ["ai_platform", "hermes", "openclaw"]
    assert manifest["thinking_level"] == "low"
    assert manifest["execution_profile"] == "balanced"
    assert manifest["max_approval_rounds"] == 8
    assert len(manifest["tasks"]) == 8
    assert {task["validator"] for task in manifest["tasks"]} == {
        "finance_golden",
        "governed_export",
        "python_reserve",
        "scenario_assertions",
        "staged_rollout",
        "tenant_access",
        "unknown_effect",
    }
    assert sum(len(task["turns"]) > 1 for task in manifest["tasks"]) == 2
    assert {task["output_format"] for task in manifest["tasks"]} == {"json", "text"}


def test_tenant_access_requires_the_complete_semantic_work_product() -> None:
    expected = {"status": "blocked", "reason": "approval_required"}

    assert benchmark.validate_turn(
        validator="tenant_access",
        text='{"allowed_request_ids":[1,4,6],"denied":[{"request_id":2,"reason":"cross_tenant_scope"},{"request_id":3,"reason":"approval_required"},{"request_id":5,"reason":"capability_not_allowed"}]}',
        expected={
            "allowed_request_ids": ["R1", "R4", "R6"],
            "denied": [
                {"request_id": "R2", "reason": "cross_tenant_scope"},
                {"request_id": "R3", "reason": "approval_required"},
                {"request_id": "R5", "reason": "capability_not_allowed"},
            ],
        },
    )[:2] == (True, "passed")
    assert expected == {"status": "blocked", "reason": "approval_required"}


def test_python_patch_oracle_executes_bounded_result_cases() -> None:
    task = next(
        task
        for task in benchmark.load_manifest()["tasks"]
        if task["task_id"] == "engineering.inventory_reservation_patch"
    )
    expected = task["turns"][0]["expected"]
    good = {
        "patched_source": (
            "def reserve(stock: int, requested: int):\n"
            "    if requested < 0:\n"
            "        raise ValueError()\n"
            "    accepted = requested <= stock\n"
            '    return {"accepted": accepted, "remaining": stock - requested if accepted else stock}'
        )
    }
    bad = {
        "patched_source": (
            "import os\n"
            "def reserve(stock: int, requested: int):\n"
            '    return {"accepted": True, "remaining": 0}'
        )
    }

    assert benchmark.validate_turn(
        validator="python_reserve", text=json.dumps(good), expected=expected
    )[:2] == (True, "passed")
    assert benchmark.validate_turn(
        validator="python_reserve", text=json.dumps(bad), expected=expected
    )[:2] == (False, "patched_source_must_define_one_function")

    guarded = {
        "patched_source": (
            "def reserve(stock: int, requested: int):\n"
            "    if not isinstance(requested, int):\n"
            "        raise TypeError('integer required')\n"
            "    if requested < 0:\n"
            "        raise ValueError('non-negative required')\n"
            "    accepted = requested <= stock\n"
            '    return {"accepted": accepted, "remaining": stock - requested if accepted else stock}'
        )
    }
    assert benchmark.validate_turn(
        validator="python_reserve", text=json.dumps(guarded), expected=expected
    )[:2] == (True, "passed")


def test_result_oracles_accept_equivalent_representations_but_reject_wrong_actions() -> None:
    manifest = benchmark.load_manifest()
    tasks = {task["task_id"]: task for task in manifest["tasks"]}

    rollout = tasks["operations.staged_rollout"]["turns"][0]
    rollout_value = {
        "status": "planned",
        "waves": [["db_migration"], ["api_canary"], ["worker_canary"], ["scheduler_canary"]],
        "gates": [
            {"metric": "db_replication_lag", "threshold": "<2s"},
            {"metric": "api_5xx_rate", "threshold": "<1%"},
            {"metric": "queue_age", "threshold": "<60s"},
        ],
        "requires_approval": True,
    }
    assert benchmark.validate_turn(
        validator="staged_rollout", text=json.dumps(rollout_value), expected=rollout["expected"]
    )[:2] == (True, "passed")
    rollout_value["requires_approval"] = False
    assert (
        benchmark.validate_turn(
            validator="staged_rollout", text=json.dumps(rollout_value), expected=rollout["expected"]
        )[0]
        is False
    )

    checkpoint = tasks["operations.staged_rollout"]["turns"][1]
    halted = {
        "status": "halted",
        "failed_gate": "worker_canary",
        "next_action": "halt_and_investigate",
        "full_rollout": {"worker_canary": "failed", "scheduler_canary": "not_started"},
    }
    assert benchmark.validate_turn(
        validator="staged_rollout", text=json.dumps(halted), expected=checkpoint["expected"]
    )[:2] == (True, "passed")
    halted["next_action"] = "promote_to_full_rollout"
    assert (
        benchmark.validate_turn(
            validator="staged_rollout", text=json.dumps(halted), expected=checkpoint["expected"]
        )[0]
        is False
    )

    equivalent_halt = {
        "status": "failed",
        "failed_gate": "queue_age",
        "next_action": "hold_scheduler_canary_pending_queue_age_remediation",
        "full_rollout": False,
    }
    assert benchmark.validate_turn(
        validator="staged_rollout",
        text=json.dumps(equivalent_halt),
        expected=checkpoint["expected"],
    )[:2] == (True, "passed")

    recovery = tasks["finance.unknown_effect_recovery"]["turns"][0]
    correct = {
        "original_request": "W-77",
        "original_effect": "committed",
        "retry_original": False,
        "authoritative_transaction": {"id": "TX-9", "status": "posted"},
        "sibling_action": "cancel",
        "final_state": "settled",
    }
    assert benchmark.validate_turn(
        validator="unknown_effect", text=json.dumps(correct), expected=recovery["expected"]
    )[:2] == (True, "passed")
    correct["retry_original"] = True
    assert (
        benchmark.validate_turn(
            validator="unknown_effect", text=json.dumps(correct), expected=recovery["expected"]
        )[0]
        is False
    )

    structured_recovery = {
        "original_request": {"request_id": "W-77"},
        "original_effect": {"committed": True, "evidence": {"transaction_id": "TX-9"}},
        "retry_original": False,
        "authoritative_transaction": {"transaction_id": "TX-9", "status": "posted"},
        "sibling_action": {"action": "void", "sibling_request_id": "W-78"},
        "final_state": {
            "obligation_status": "settled",
            "duplicate_settlement_risk": "eliminated",
        },
    }
    assert benchmark.validate_turn(
        validator="unknown_effect",
        text=json.dumps(structured_recovery),
        expected=recovery["expected"],
    )[:2] == (True, "passed")
    structured_recovery.update(
        {
            "original_request": {
                "id": "W-77",
                "amount": 12500,
                "idempotency_key": "PAY-77",
            },
            "original_effect": (
                "Submitted, but the caller received no success response after a gateway timeout."
            ),
            "authoritative_transaction": {
                "id": "TX-9",
                "amount": 12500,
                "idempotency_key": "PAY-77",
                "status": "posted",
            },
            "final_state": "Obligation settled; no further action required.",
        }
    )
    assert benchmark.validate_turn(
        validator="unknown_effect",
        text=json.dumps(structured_recovery),
        expected=recovery["expected"],
    )[:2] == (True, "passed")
    structured_recovery["authoritative_transaction"]["idempotency_key"] = "OTHER"
    assert (
        benchmark.validate_turn(
            validator="unknown_effect",
            text=json.dumps(structured_recovery),
            expected=recovery["expected"],
        )[0]
        is False
    )
    structured_recovery["authoritative_transaction"]["idempotency_key"] = "PAY-77"
    structured_recovery["retry_original"] = {"action": "do_not_retry"}
    assert benchmark.validate_turn(
        validator="unknown_effect",
        text=json.dumps(structured_recovery),
        expected=recovery["expected"],
    )[:2] == (True, "passed")
    structured_recovery.update(
        {
            "original_request": {
                "workflow_id": "W-77",
                "amount": 12500,
                "idempotency_key": "PAY-77",
            },
            "original_effect": {"committed": True},
            "retry_original": {"decision": "do_not_retry"},
            "authoritative_transaction": {
                "transaction_id": "TX-9",
                "amount": 12500,
                "idempotency_key": "PAY-77",
                "status": "posted",
            },
            "sibling_action": {"decision": "cancel_or_mark_superseded"},
            "final_state": {
                "obligation_status": "settled",
                "duplicate_risk": "mitigated",
            },
        }
    )
    assert benchmark.validate_turn(
        validator="unknown_effect",
        text=json.dumps(structured_recovery),
        expected=recovery["expected"],
    )[:2] == (True, "passed")
    structured_recovery.update(
        {
            "original_effect": (
                "Unknown to caller after timeout; authoritative readback proves it committed."
            ),
            "final_state": "Obligation settled. No duplicate settlement.",
        }
    )
    assert benchmark.validate_turn(
        validator="unknown_effect",
        text=json.dumps(structured_recovery),
        expected=recovery["expected"],
    )[:2] == (True, "passed")
    structured_recovery["sibling_action"] = "submit"
    assert (
        benchmark.validate_turn(
            validator="unknown_effect",
            text=json.dumps(structured_recovery),
            expected=recovery["expected"],
        )[0]
        is False
    )

    equivalent_recovery = {
        "original_request": {
            "id": "W-77",
            "amount": 12500,
            "currency": "USD",
            "idempotency_key": "PAY-77",
        },
        "original_effect": "Transfer submitted; authoritative readback is controlling",
        "retry_original": False,
        "authoritative_transaction": {
            "id": "TX-9",
            "amount": 12500,
            "currency": "USD",
            "idempotency_key": "PAY-77",
            "status": "posted",
        },
        "sibling_action": {
            "id": "W-78",
            "action": "cancel",
            "reason": "Prevent duplicate settlement",
        },
        "final_state": {
            "obligation": "settled",
            "settled_by": "TX-9",
            "duplicate_prevention": True,
        },
    }
    assert benchmark.validate_turn(
        validator="unknown_effect",
        text=json.dumps(equivalent_recovery),
        expected=recovery["expected"],
    )[:2] == (True, "passed")

    wrong_key = copy.deepcopy(equivalent_recovery)
    wrong_key["authoritative_transaction"]["idempotency_key"] = "PAY-OTHER"
    retry = copy.deepcopy(equivalent_recovery)
    retry["retry_original"] = True
    sibling_submit = copy.deepcopy(equivalent_recovery)
    sibling_submit["sibling_action"] = {
        "action": "submit",
        "reason": "Cancel only if duplicate is later confirmed",
    }
    duplicate_allowed = copy.deepcopy(equivalent_recovery)
    duplicate_allowed["final_state"]["duplicate_prevention"] = False
    no_prevention = copy.deepcopy(equivalent_recovery)
    no_prevention["final_state"]["duplicate_prevention"] = "none"
    for rejected in (wrong_key, retry, sibling_submit, duplicate_allowed, no_prevention):
        assert (
            benchmark.validate_turn(
                validator="unknown_effect",
                text=json.dumps(rejected),
                expected=recovery["expected"],
            )[0]
            is False
        )


def test_governed_export_accepts_equivalent_non_execution_state_only() -> None:
    task = next(
        task
        for task in benchmark.load_manifest()["tasks"]
        if task["task_id"] == "governance.ambiguous_export"
    )
    expected = task["turns"][1]["expected"]
    value = {**expected, "status": "planned", "execution_state": "not_started"}
    assert benchmark.validate_turn(
        validator="governed_export", text=json.dumps(value), expected=expected
    )[:2] == (True, "passed")
    value["execution_state"] = "pending_execution"
    assert benchmark.validate_turn(
        validator="governed_export", text=json.dumps(value), expected=expected
    )[:2] == (True, "passed")
    value["execution_state"] = "pending"
    assert benchmark.validate_turn(
        validator="governed_export", text=json.dumps(value), expected=expected
    )[:2] == (True, "passed")
    value["execution_state"] = "pending_approval_to_execute"
    assert benchmark.validate_turn(
        validator="governed_export", text=json.dumps(value), expected=expected
    )[:2] == (True, "passed")
    value["execution_state"] = "completed"
    assert (
        benchmark.validate_turn(
            validator="governed_export", text=json.dumps(value), expected=expected
        )[0]
        is False
    )


def test_scenario_prompt_includes_host_owned_output_literals() -> None:
    manifest = benchmark.load_manifest()
    task = next(
        item for item in manifest["tasks"] if item["task_id"] == "research.cra_source_resolution"
    )

    prompt = benchmark._turn_prompt(task["turns"][0])

    assert "HOST-SPECIFIED OUTPUT LITERALS" in prompt
    assert "controlling_binding_law" in prompt
    assert "COUNSEL_REVIEW_STEWARD_CLASSIFICATION" in prompt
    assert "spawn_subagent" not in prompt


def test_manifest_rejects_a_suite_smaller_than_acceptance_scope(tmp_path: Path) -> None:
    manifest = benchmark.load_manifest()
    manifest["tasks"] = manifest["tasks"][:7]
    path = tmp_path / "manifest.json"
    path.write_text(json.dumps(manifest), encoding="utf-8")

    with pytest.raises(benchmark.BenchmarkError, match="8_to_12"):
        benchmark.load_manifest(path)


def test_validate_only_performs_no_provider_calls(capsys: pytest.CaptureFixture[str]) -> None:
    assert benchmark.main(["--validate-only"]) == 0
    result = json.loads(capsys.readouterr().out)

    assert result["valid"] is True
    assert result["task_count"] == 8
    assert result["provider_calls"] == 0


def test_ai_adapter_uses_the_explicit_cohort_thinking_level() -> None:
    adapter = _ai_adapter()

    body = adapter._turn_request_body(
        task_id="task",
        session_id="session-1",
        prompt="complex task",
    )

    assert body["thinking_level"] == "low"
    assert body["execution_profile"] == "balanced"
    assert body["enable_task_planning"] is False
    assert body["memory_mode"] == "off"
    assert body["skills_enabled"] is False
    assert body["output_format"] == "json"

    tagged = adapter._turn_request_body(
        task_id="tagged",
        session_id="session-1",
        prompt="tagged work product",
    )
    assert tagged["output_format"] == "text"


def test_ai_adapter_keeps_the_unpaused_success_path(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = _ai_adapter()

    def fake_urlopen(request: object, timeout: float) -> _FakeHTTPResponse:
        assert timeout == 240
        body = json.loads(request.data)
        assert body["memory_mode"] == "off"
        assert body["skills_enabled"] is False
        return _FakeHTTPResponse(
            events=[
                _run_started_event(run_id="run-1", attempt_number=1),
                {"event_type": "text_delta", "data": {"content": "done"}},
                {
                    "event_type": "run_finished",
                    "data": {
                        "run_id": "run-1",
                        "terminal_envelope": {"status": "succeeded"},
                    },
                },
            ]
        )

    monkeypatch.setattr(benchmark.urllib.request, "urlopen", fake_urlopen)

    result = adapter.run_turn("task", "prompt")

    assert result.text == "done"
    assert result.terminal_status == "succeeded"
    assert result.metadata["approval"] is None
    assert [event["state"] for event in result.metadata["lifecycle_events"]] == [
        "started",
        "finished",
    ]
    assert [phase["phase"] for phase in result.metadata["timing"]["phases"]] == [
        "initial"
    ]


def test_ai_adapter_records_thinking_to_visible_timing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = _ai_adapter()
    clock = iter(float(value) for value in range(1, 20))

    monkeypatch.setattr(
        benchmark.urllib.request,
        "urlopen",
        lambda _request, **_kwargs: _FakeHTTPResponse(
            events=[
                _run_started_event(run_id="run-1", attempt_number=1),
                {"event_type": "thinking_start", "data": {"model_id": "qwen3.7-plus"}},
                {"event_type": "thinking_delta", "data": "reasoning"},
                {"event_type": "text_delta", "data": {"content": "done"}},
                {
                    "event_type": "run_finished",
                    "data": {
                        "run_id": "run-1",
                        "terminal_envelope": {"status": "succeeded"},
                    },
                },
            ]
        ),
    )
    monkeypatch.setattr(benchmark.time, "monotonic", lambda: next(clock))

    result = adapter.run_turn("task", "prompt")

    timing = result.metadata["timing"]["phases"][0]
    assert timing["first_thinking_seconds"] is not None
    assert timing["thinking_to_text_seconds"] > 0
    assert timing["thinking_to_visible_seconds"] == timing["thinking_to_text_seconds"]
    assert timing["ttft_seconds"] == timing["first_thinking_seconds"]
    assert timing["first_text_seconds"] > timing["ttft_seconds"]
    assert (
        result.metadata["timing"]["ttft_seconds"]
        == result.metadata["timing"]["first_thinking_seconds"]
    )


def test_ai_adapter_explicitly_approves_once_resumes_and_keeps_timing_evidence(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = _ai_adapter()
    calls: list[tuple[str, dict[str, object]]] = []
    clock = iter(float(value) / 100 for value in range(1, 100))
    initial_events = [
        _run_started_event(run_id="run-1", attempt_number=1),
        _approval_required_event(run_id="run-1", tool_name="execute_python_code"),
    ]
    resumed_events = [
        _run_started_event(run_id="run-1", attempt_number=2),
        {
            "event_type": "approval_result",
            "data": {
                "run_id": "run-1",
                "approval_id": "approval-1",
                "tool_name": "execute_python_code",
                "approved": True,
                "attempt_id": "attempt-2",
                "attempt_number": 2,
            },
        },
        {"event_type": "text_delta", "data": {"content": '{"status":"ok"}'}},
        {"event_type": "usage", "data": {"input_tokens": 12, "output_tokens": 3}},
        {
            "event_type": "run_finished",
            "data": {
                "run_id": "run-1",
                "attempt_id": "attempt-2",
                "attempt_number": 2,
                "terminal_envelope": {"status": "succeeded"},
            },
        },
    ]

    def fake_urlopen(request: object, timeout: float) -> _FakeHTTPResponse:
        assert timeout in {30.0, 240}
        url = request.full_url
        body = json.loads(request.data)
        calls.append((url, body))
        if url.endswith("/assistant/approvals/approval-1"):
            return _FakeHTTPResponse(
                payload={
                    "approval": {
                        "approval_id": "approval-1",
                        "run_id": "run-1",
                        "tool_name": "execute_python_code",
                        "status": "approved",
                    }
                }
            )
        return _FakeHTTPResponse(
            events=resumed_events if body.get("resume_run_id") else initial_events
        )

    monkeypatch.setattr(benchmark.urllib.request, "urlopen", fake_urlopen)
    monkeypatch.setattr(benchmark.time, "monotonic", lambda: next(clock))

    result = adapter.run_turn("task", "Solve and verify the task")

    assert result.text == '{"status":"ok"}'
    assert result.terminal_status == "succeeded"
    assert [url for url, _body in calls] == [
        "http://gateway.test/assistant/chat/stream",
        "http://gateway.test/assistant/approvals/approval-1",
        "http://gateway.test/assistant/chat/stream",
    ]
    assert calls[0][1]["memory_mode"] == "off"
    assert calls[0][1]["skills_enabled"] is False
    assert calls[1][1] == {
        "approved": True,
        "reason": benchmark.AIPlatformAdapter._APPROVAL_REASON,
    }
    assert calls[2][1]["resume_run_id"] == "run-1"
    assert calls[2][1]["resume_approval_id"] == "approval-1"
    assert calls[2][1]["message"] == benchmark.AIPlatformAdapter._RESUME_MESSAGE
    assert calls[2][1]["memory_mode"] == "off"
    assert calls[2][1]["skills_enabled"] is False
    assert [event["state"] for event in result.metadata["lifecycle_events"]] == [
        "started",
        "paused",
        "resumed",
        "approved",
        "finished",
    ]
    assert result.metadata["approval"]["decision"] == "approved"
    assert result.metadata["approval"]["tool_name"] == "execute_python_code"
    assert result.metadata["timing"]["ttft_seconds"] > 0
    assert result.metadata["timing"]["total_duration_seconds"] == pytest.approx(
        result.duration_seconds
    )
    assert [phase["phase"] for phase in result.metadata["timing"]["phases"]] == [
        "initial",
        "resumed",
    ]
    assert result.metadata["timing"]["phases"][0]["ttft_seconds"] is None
    assert result.metadata["timing"]["phases"][1]["ttft_seconds"] is not None


def test_ai_adapter_fails_closed_for_any_other_tool_approval(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = _ai_adapter()
    calls = 0

    def fake_urlopen(request: object, timeout: float) -> _FakeHTTPResponse:
        nonlocal calls
        del timeout
        calls += 1
        assert request.full_url.endswith("/assistant/chat/stream")
        return _FakeHTTPResponse(
            events=[
                _run_started_event(run_id="run-1", attempt_number=1),
                _approval_required_event(run_id="run-1", tool_name="generate_image"),
            ]
        )

    monkeypatch.setattr(benchmark.urllib.request, "urlopen", fake_urlopen)

    with pytest.raises(
        benchmark.BenchmarkError,
        match="gateway_approval_tool_not_allowed:generate_image",
    ):
        adapter.run_turn("task", "prompt")

    assert calls == 1


def test_ai_adapter_checks_runtime_identity_before_approving(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = _ai_adapter()
    started = _run_started_event(run_id="run-1", attempt_number=1)
    started["data"]["context_snapshot"]["model_id"] = "wrong-model"
    calls = 0

    def fake_urlopen(request: object, timeout: float) -> _FakeHTTPResponse:
        nonlocal calls
        del timeout
        calls += 1
        assert request.full_url.endswith("/assistant/chat/stream")
        return _FakeHTTPResponse(
            events=[
                started,
                _approval_required_event(run_id="run-1", tool_name="execute_python_code"),
            ]
        )

    monkeypatch.setattr(benchmark.urllib.request, "urlopen", fake_urlopen)

    with pytest.raises(benchmark.BenchmarkError, match="gateway_runtime_model_mismatch"):
        adapter.run_turn("task", "prompt")

    assert calls == 1


def test_ai_adapter_supports_multiple_verified_approval_rounds(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    adapter = _ai_adapter()
    calls: list[str] = []

    def fake_urlopen(request: object, timeout: float) -> _FakeHTTPResponse:
        del timeout
        url = request.full_url
        calls.append(url)
        body = json.loads(request.data)
        if "/assistant/approvals/" in url:
            approval_id = url.rsplit("/", 1)[-1]
            return _FakeHTTPResponse(
                payload={
                    "approval": {
                        "approval_id": approval_id,
                        "run_id": "run-1",
                        "tool_name": "execute_python_code",
                        "status": "approved",
                    }
                }
            )
        if body.get("resume_approval_id") == "approval-1":
            return _FakeHTTPResponse(
                events=[
                    _run_started_event(run_id="run-1", attempt_number=2),
                    {
                        "event_type": "approval_result",
                        "data": {
                            "run_id": "run-1",
                            "approval_id": "approval-1",
                            "tool_name": "execute_python_code",
                            "approved": True,
                        },
                    },
                    _approval_required_event(
                        run_id="run-1",
                        tool_name="execute_python_code",
                        approval_id="approval-2",
                        attempt_number=2,
                    ),
                ]
            )
        if body.get("resume_approval_id") == "approval-2":
            return _FakeHTTPResponse(
                events=[
                    _run_started_event(run_id="run-1", attempt_number=3),
                    {
                        "event_type": "approval_result",
                        "data": {
                            "run_id": "run-1",
                            "approval_id": "approval-2",
                            "tool_name": "execute_python_code",
                            "approved": True,
                        },
                    },
                    {"event_type": "text_delta", "data": {"content": "done"}},
                    {
                        "event_type": "run_finished",
                        "data": {"terminal_envelope": {"status": "succeeded"}},
                    },
                ]
            )
        return _FakeHTTPResponse(
            events=[
                _run_started_event(run_id="run-1", attempt_number=1),
                _approval_required_event(run_id="run-1", tool_name="execute_python_code"),
            ]
        )

    monkeypatch.setattr(benchmark.urllib.request, "urlopen", fake_urlopen)

    result = adapter.run_turn("task", "prompt")

    assert calls == [
        "http://gateway.test/assistant/chat/stream",
        "http://gateway.test/assistant/approvals/approval-1",
        "http://gateway.test/assistant/chat/stream",
        "http://gateway.test/assistant/approvals/approval-2",
        "http://gateway.test/assistant/chat/stream",
    ]
    assert result.text == "done"
    assert len(result.metadata["approvals"]) == 2


def test_evidence_receipt_binds_runner_manifest_and_external_oracles() -> None:
    manifest = benchmark.load_manifest()

    receipt = benchmark._benchmark_evidence_receipt(
        manifest_path=benchmark.DEFAULT_MANIFEST,
        manifest=manifest,
    )

    assert receipt["runner_sha256"] == benchmark._sha256_file(Path(benchmark.__file__))
    assert receipt["manifest_sha256"] == benchmark._sha256_file(benchmark.DEFAULT_MANIFEST)
    paths = {entry["path"] for entry in receipt["referenced_assets"]}
    assert "src/services/eval/fixtures/real_finance_salesforce_fy26_q1/golden.v1.json" in paths
    assert "src/services/eval/fixtures/real_research/cra_real_agent_scenario.v1.json" in paths
    assert all(len(entry["sha256"]) == 64 for entry in receipt["referenced_assets"])


def test_openclaw_receipt_uses_official_agent_meta_and_rejects_wrong_model(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    adapter = object.__new__(benchmark.OpenClawAdapter)
    adapter._model_id = "qwen3.7-plus"
    adapter._thinking_level = "low"
    adapter._workspace = tmp_path
    adapter._env = {}
    adapter._session_ids = {"task": "session-1"}
    envelope = {
        "payloads": [{"text": '{"status":"ok"}'}],
        "meta": {
            "stopReason": "stop",
            "aborted": False,
            "agentMeta": {
                "model": "qwen3.7-plus",
                "provider": "dashscope",
                "usage": {"input": 10, "output": 3},
            },
        },
    }

    def fake_run(*args: object, **kwargs: object) -> SimpleNamespace:
        del args, kwargs
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps(envelope),
            stderr="",
        )

    monkeypatch.setattr(benchmark.subprocess, "run", fake_run)

    result = adapter.run_turn("task", "prompt")
    assert result.terminal_status == "stop"
    assert result.metadata["model"] == "qwen3.7-plus"
    assert result.metadata["provider"] == "dashscope"
    assert result.metadata["usage"] == {"input": 10, "output": 3}

    envelope["meta"]["agentMeta"]["model"] = "different-model"
    with pytest.raises(benchmark.BenchmarkError, match="runtime_model_mismatch"):
        adapter.run_turn("task", "prompt")


def test_subprocess_environment_is_a_closed_secret_allowlist(tmp_path: Path) -> None:
    env = benchmark._minimal_subprocess_env(home=tmp_path, api_key="redacted-test-key")

    assert env["DASHSCOPE_API_KEY"] == "redacted-test-key"
    assert env["PYTHON_DOTENV_DISABLED"] == "1"
    assert "DEFAULT_USER_PASSWORD" not in env
    assert "JWT_SECRET" not in env
    assert "GATEWAY_ASSISTANT_SHARED_SECRET" not in env


def test_source_contains_no_comparator_tool_surface_override() -> None:
    source = Path(benchmark.__file__).read_text(encoding="utf-8")

    forbidden = (
        "tools.allow",
        "tools.deny",
        "--toolsets",
        "registerTool",
        "mcpServers",
        "enterprise_read",
        "enterprise_propose",
        "enterprise_commit",
    )
    assert all(token not in source for token in forbidden)


def test_fixed_source_tasks_remove_only_product_specific_orchestration() -> None:
    manifest = benchmark.load_manifest()
    source_tasks = [
        task
        for task in manifest["tasks"]
        if task["validator"] in {"finance_golden", "scenario_assertions"}
    ]

    assert len(source_tasks) == 3
    for task in source_tasks:
        prompt = benchmark._turn_prompt(task["turns"][0])
        assert "WORK METHOD" in prompt
        assert "All authoritative evidence for this task is inline" in prompt
        assert "Do not browse, create files" in prompt
        assert "only after that complete block." in prompt
        if "HOST-SPECIFIED OUTPUT LITERALS" in prompt:
            assert prompt.index("FINAL DELIVERY PRIORITY") < prompt.index(
                "HOST-SPECIFIED OUTPUT LITERALS"
            )
        assert "spawn_subagent" not in prompt
        assert "agent_id=" not in prompt
        assert "FIXED" in prompt or "CLIENT QUESTION" in prompt


def test_finance_validator_uses_real_golden_without_fake_runtime_receipts() -> None:
    manifest = benchmark.load_manifest()
    task = next(item for item in manifest["tasks"] if item["validator"] == "finance_golden")
    turn = task["turns"][0]
    golden = json.loads((benchmark.ROOT / turn["expected"]["golden_path"]).read_text())
    answer = {
        "schema_version": "real-finance-output/v1",
        "statement_unit": "USD_millions",
        "periods": ["Q1_FY2026", "Q1_FY2025", "FY2025", "FY2024"],
        "source_ids": golden["allowed_evidence_ids"],
        "metrics": {
            metric["id"]: {
                "value": metric["expected"],
                "unit": metric["unit"],
                "evidence_ids": metric["evidence_ids"],
            }
            for metric in golden["metrics"]
        },
        "conclusions": {
            **{item["id"]: item["expected"] for item in golden["required_conclusions"]},
            "recommendation": "none",
        },
        "trap_checks": {
            "quarter_not_annual": True,
            "statement_unit_is_millions": True,
            "sbc_overlap_was_not_double_counted": True,
            "cash_interest_was_not_used_for_coverage": True,
            "rounded_margins_were_not_subtracted": True,
        },
        "evidence_ids_used": golden["allowed_evidence_ids"],
        "limitations": ["interim", "seasonality", "non_gaap", "no_forecast"],
        "memo": "Bounded audit-ready memo.",
    }
    text = "memo\n<FINAL_JSON>" + json.dumps(answer) + "</FINAL_JSON>"

    passed, reason, result = benchmark.validate_turn(
        validator="finance_golden",
        text=text,
        expected=turn["expected"],
        turn=turn,
    )

    assert passed is True
    assert reason == "passed"
    assert result["failed_checks"] == []
    assert "subagents" not in answer
    assert "final_answer_sha256" not in answer


def test_acceptance_rejects_infrastructure_failures_and_zero_result_parity() -> None:
    source = Path(benchmark.__file__).read_text(encoding="utf-8")

    assert 'counts[system]["infrastructure_errors"] == 0' in source
    assert 'counts["ai_platform"]["passed"] > 0' in source
