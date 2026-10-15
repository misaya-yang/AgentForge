from __future__ import annotations

import importlib.util
import json
from pathlib import Path
from typing import Any


def _load_eval_golden_module():
    script_path = Path(__file__).resolve().parents[2] / "scripts" / "eval_golden.py"
    spec = importlib.util.spec_from_file_location("eval_golden_live_script", script_path)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


eval_golden = _load_eval_golden_module()


def _args() -> list[str]:
    return [
        "live",
        "experiment-1",
        "--evaluator-id",
        "evaluator-1",
        "--base-url",
        "http://gateway.test",
        "--poll-interval",
        "0",
        "--timeout",
        "1",
    ]


def test_live_starts_polls_and_gates_against_experiment_baseline(
    monkeypatch,
    capsys,
) -> None:
    calls: list[tuple[str, str, dict[str, Any] | None]] = []
    run_responses = iter(
        [
            {
                "run_id": "candidate-1",
                "run_mode": "live_candidate",
                "status": "running",
            },
            {
                "run_id": "candidate-1",
                "run_mode": "live_candidate",
                "status": "succeeded",
            },
        ]
    )

    def fake_request(
        _base_url: str,
        method: str,
        path: str,
        *,
        payload: dict[str, Any] | None = None,
        timeout: float = 30.0,
    ) -> dict[str, Any]:
        assert timeout > 0
        calls.append((method, path, payload))
        if path == "/experiments/experiment-1":
            return {"experiment_id": "experiment-1", "baseline_run_id": "baseline-1"}
        if path == "/experiments/experiment-1:run":
            return {"jobs": [{"run_id": "candidate-1", "status": "queued"}]}
        if path == "/experiment-runs/candidate-1":
            return next(run_responses)
        assert path.startswith("/experiment-runs:compare?")
        return {
            "compatibility": {"status": "compatible", "compatible": True},
            "gate": {"status": "pass", "failures": []},
            "candidate_summary": {"overall_score": 0.95, "note": "not emitted"},
            "deltas": {"overall_score": 0.01},
        }

    monkeypatch.setattr(eval_golden, "_request_json", fake_request)

    assert eval_golden.main(_args()) == 0

    started = next(call for call in calls if call[0:2] == ("POST", "/experiments/experiment-1:run"))
    assert started[2] == {
        "run_mode": "live_candidate",
        "evaluator_ids": ["evaluator-1"],
        "repetitions": 3,
        "baseline_run_id": "baseline-1",
        "metadata": {"source": "eval_golden_live_cli"},
    }
    output = json.loads(capsys.readouterr().out)
    assert output["gate"]["status"] == "pass"
    assert output["candidate_metrics"] == {"overall_score": 0.95}
    assert output["deltas"] == {"overall_score": 0.01}


def test_live_fails_closed_for_incompatible_comparison(monkeypatch, capsys) -> None:
    def fake_request(
        _base_url: str,
        method: str,
        path: str,
        **_kwargs: Any,
    ) -> dict[str, Any]:
        if path == "/experiments/experiment-1":
            return {"baseline_run_id": "baseline-1"}
        if method == "POST":
            return {"run_id": "candidate-1"}
        if path == "/experiment-runs/candidate-1":
            return {"run_mode": "live_candidate", "status": "succeeded"}
        return {
            "compatibility": {"status": "incompatible", "compatible": False},
            "gate": {"status": "pass", "failures": []},
        }

    monkeypatch.setattr(eval_golden, "_request_json", fake_request)

    assert eval_golden.main(_args()) == 1
    assert json.loads(capsys.readouterr().out)["compatible"] is False


def test_live_fails_closed_when_server_does_not_return_gate(monkeypatch) -> None:
    def fake_request(
        _base_url: str,
        method: str,
        path: str,
        **_kwargs: Any,
    ) -> dict[str, Any]:
        if path == "/experiments/experiment-1":
            return {"baseline_run_id": "baseline-1"}
        if method == "POST":
            return {"run_id": "candidate-1"}
        if path == "/experiment-runs/candidate-1":
            return {"run_mode": "live_candidate", "status": "succeeded"}
        return {"compatibility": True}

    monkeypatch.setattr(eval_golden, "_request_json", fake_request)

    assert eval_golden.main(_args()) == 1


def test_live_run_failure_redacts_runtime_secret_and_skips_compare(
    monkeypatch,
    capsys,
) -> None:
    monkeypatch.setenv("GATEWAY_TOKEN", "private-token")
    paths: list[str] = []

    def fake_request(
        _base_url: str,
        method: str,
        path: str,
        **_kwargs: Any,
    ) -> dict[str, Any]:
        paths.append(path)
        if path == "/experiments/experiment-1":
            return {"baseline_run_id": "baseline-1"}
        if method == "POST":
            return {"run_id": "candidate-1"}
        return {
            "run_mode": "live_candidate",
            "status": "failed",
            "error_message": "provider rejected private-token",
        }

    monkeypatch.setattr(eval_golden, "_request_json", fake_request)

    assert eval_golden.main(_args()) == 1
    output = capsys.readouterr().out
    assert "private-token" not in output
    assert "[redacted]" in output
    assert not any(path.startswith("/experiment-runs:compare") for path in paths)


def test_live_requires_baseline_before_starting_candidate(monkeypatch, capsys) -> None:
    calls: list[tuple[str, str]] = []

    def fake_request(
        _base_url: str,
        method: str,
        path: str,
        **_kwargs: Any,
    ) -> dict[str, Any]:
        calls.append((method, path))
        return {"experiment_id": "experiment-1", "baseline_run_id": None}

    monkeypatch.setattr(eval_golden, "_request_json", fake_request)

    assert eval_golden.main(_args()) == 1
    assert calls == [("GET", "/experiments/experiment-1")]
    assert "No baseline is configured" in capsys.readouterr().err
