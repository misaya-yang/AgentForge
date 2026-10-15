import hashlib
from pathlib import Path

import pytest

from scripts.assistant_ttft_benchmark import (
    _bootstrap_ci,
    _iqr,
    _metric_summary,
    _parse_model_plane_timing_line,
    _percentile,
    _reconcile_trial,
    _run_id_sha256,
    run_benchmark,
)
from scripts.native_agent_parity_benchmark import BenchmarkError


def test_percentile_uses_nearest_rank() -> None:
    values = [float(value) for value in range(1, 11)]

    assert _percentile(values, 0.50) == 5.0
    assert _percentile(values, 0.95) == 10.0
    assert _percentile([], 0.50) is None


def test_metric_summary_ignores_failed_or_missing_measurements() -> None:
    summary = _metric_summary(
        [{"ttft_seconds": 2.0}, {"ttft_seconds": None}, {"ttft_seconds": 1.0}],
        "ttft_seconds",
    )

    assert summary == {"p50": 1.0, "p95": 2.0, "min": 1.0, "max": 2.0}


_RUN_ID = "0f6f3ac8-9f1e-4d05-9b8a-0000000000aa"


def _timing_line(
    run_id: str = _RUN_ID,
    *,
    ttft: float | None = 2.54,
    call_id: str = "call-1",
    components: str | None = None,
    model: str = "qwen3.7-plus",
) -> str:
    if components is None:
        components = (
            "local_pre_provider_seconds=0.04 provider_wait_seconds=2.0 "
            "local_projection_seconds=0.5 local_overhead_seconds=0.54 "
            f"model_plane_ttft_seconds={'None' if ttft is None else ttft}"
        )
    return (
        "gateway | Agent model-plane timing schema=ppr-timing/v1 "
        f"wire=chat_completions run_id={run_id} call_id={call_id} model={model} {components}"
    )


def test_parse_model_plane_timing_line_joins_on_run_id_hash() -> None:
    parsed = _parse_model_plane_timing_line(_timing_line())
    assert parsed is not None
    assert parsed["wire"] == "chat_completions"
    assert parsed["run_id_sha256"] == hashlib.sha256(_RUN_ID.encode()).hexdigest()
    assert parsed["model_plane_ttft_seconds"] == 2.54
    assert parsed["provider_wait_seconds"] == 2.0


def test_parse_accepts_scientific_notation_values() -> None:
    # Reviewer regression: str(float) can emit 9.7e-05; the parser must not
    # silently truncate that exponent to 9.7 seconds.
    line = _timing_line(
        components=(
            "local_pre_provider_seconds=0.04 provider_wait_seconds=9.7e-05 "
            "local_projection_seconds=1e-3 local_overhead_seconds=0.041 "
            "model_plane_ttft_seconds=0.0507"
        )
    )
    parsed = _parse_model_plane_timing_line(line)
    assert parsed is not None
    assert parsed["provider_wait_seconds"] == pytest.approx(0.000097)
    assert parsed["local_projection_seconds"] == pytest.approx(0.001)
    assert parsed["model_plane_ttft_seconds"] == pytest.approx(0.0507)


def test_parse_rejects_unrelated_lines_and_none_ttft_yields_missing_component() -> None:
    assert _parse_model_plane_timing_line("GET /health 200") is None
    parsed = _parse_model_plane_timing_line(_timing_line(ttft=None))
    assert parsed is not None
    assert parsed["model_plane_ttft_seconds"] is None
    assert parsed["provider_wait_seconds"] == 2.0


def test_run_id_sha256_reads_first_run_started() -> None:
    digest = "a" * 64
    events = [
        {"event_type": "approval_required", "run_id_sha256": "b" * 64},
        {"event_type": "run_started", "run_id_sha256": digest},
    ]
    assert _run_id_sha256(events) == digest
    assert _run_id_sha256([]) is None


def test_reconcile_trial_statuses() -> None:
    digest = hashlib.sha256(_RUN_ID.encode()).hexdigest()
    assert _reconcile_trial(None, "t", digest, 3.0)["status"] == "not_collected"
    assert _reconcile_trial(lambda _s: [_timing_line()], "t", "f" * 64, 3.0)["status"] == "missing"
    multi = _reconcile_trial(
        lambda _s: [_timing_line(), _timing_line(call_id="call-2")], "t", digest, 2.6
    )
    assert multi["status"] == "multi_call_excluded"
    assert multi["call_count"] == 2
    ok = _reconcile_trial(lambda _s: [_timing_line()], "t", digest, 2.6)
    assert ok["status"] == "ok"
    assert ok["residual_seconds"] == 0.06
    assert ok["identity_residual_seconds"] == pytest.approx(0.0, abs=1e-9)
    assert ok["server"]["provider_wait_seconds"] == 2.0
    exceeded = _reconcile_trial(lambda _s: [_timing_line()], "t", digest, 5.0)
    assert exceeded["status"] == "tolerance_exceeded"
    incomplete = _reconcile_trial(lambda _s: [_timing_line(ttft=None)], "t", digest, 2.6)
    assert incomplete["status"] == "incomplete"


def test_reconcile_trial_dedups_repeated_reads_of_one_call() -> None:
    # Reviewer regression #3 (race hardening): the same call's line observed in
    # two delayed reads is one call, not a multi-call run.
    digest = hashlib.sha256(_RUN_ID.encode()).hexdigest()
    result = _reconcile_trial(lambda _s: [_timing_line(), _timing_line()], "t", digest, 2.6)
    assert result["status"] == "ok"


def test_reconcile_trial_enforces_server_identity_bound() -> None:
    # Reviewer regression #5: components that do not add up to the server TTFT
    # within the real-clock tolerance are a defect, not a waiver.
    digest = hashlib.sha256(_RUN_ID.encode()).hexdigest()
    line = _timing_line(
        components=(
            "local_pre_provider_seconds=0.04 provider_wait_seconds=2.0 "
            "local_projection_seconds=0.5 local_overhead_seconds=0.54 "
            "model_plane_ttft_seconds=3.00"
        )
    )
    result = _reconcile_trial(lambda _s: [line], "t", digest, 3.1)
    assert result["status"] == "identity_violation"
    assert result["identity_residual_seconds"] == pytest.approx(0.46)


def test_reconcile_trial_contains_log_command_failures() -> None:
    # Reviewer regression #2: a failing reader must yield an honest `missing`,
    # never re-raise and mark a successful trial as infrastructure_error.
    digest = hashlib.sha256(_RUN_ID.encode()).hexdigest()

    def failing(_since: str) -> list[str]:
        raise BenchmarkError("timing_log_command_failed")

    assert _reconcile_trial(failing, "t", digest, 2.6) == {"status": "missing"}


# ---------------------------------------------------------------------------
# Pre-declared statistical policy helpers (phase-00 report requirements)
# ---------------------------------------------------------------------------


def test_iqr_and_bootstrap_are_deterministic() -> None:
    values = [float(value) for value in range(1, 21)]
    assert _iqr(values) == pytest.approx(10.0)
    first = _bootstrap_ci(values, 0.50)
    second = _bootstrap_ci(values, 0.50)
    assert first == second
    assert first["low"] <= 10.5 and first["high"] >= 10.5
    assert _iqr([]) is None
    assert _bootstrap_ci([], 0.50) == {"low": None, "high": None}


def test_warmup_rejects_impossible_budgets(tmp_path) -> None:
    with pytest.raises(BenchmarkError, match="ttft_warmup_out_of_range"):
        run_benchmark(
            env_path=Path("/nonexistent.env"),
            gateway_base_url="http://127.0.0.1:1/api/v1",
            output_path=tmp_path / "out.json",
            prompt="x",
            trials=3,
            thinking_level="low",
            p50_ceiling_seconds=4.0,
            p95_ceiling_seconds=5.0,
            warmup_trials=3,
        )


class _FakeTurnResult:
    def __init__(self, *, run_id_sha: str) -> None:
        self.terminal_status = "succeeded"
        self.text = "pong"
        self.duration_seconds = 2.7
        self.metadata = {
            "event_types": ["run_started", "thinking_delta", "text_delta", "run_finished"],
            "lifecycle_events": [{"event_type": "run_started", "run_id_sha256": run_id_sha}],
            "timing": {"ttft_seconds": 2.6, "phases": [{}]},
            "usage": {},
        }


def _run_with_fake_adapter(
    monkeypatch,
    tmp_path,
    *,
    receipt_model: str,
    trials: int = 1,
    warmup: int = 0,
    with_reader: bool = True,
    reader=None,
    min_gate: int = 1,
    result_factory=None,
):
    import scripts.assistant_ttft_benchmark as module

    run_id = "run-1"
    run_id_sha = hashlib.sha256(run_id.encode()).hexdigest()
    captured: dict[str, object] = {}

    class _FakeAdapter:
        def __init__(self, **kwargs):
            captured["model_id"] = kwargs["model_id"]
            captured["task_output_formats"] = kwargs["task_output_formats"]
            captured["email"] = kwargs["email"]
            captured["password"] = kwargs["password"]

        def start_task(self, _task_id: str) -> None:
            return None

        def run_turn(self, _task_id: str, _prompt: str):
            if result_factory is not None:
                return result_factory(run_id_sha)
            return _FakeTurnResult(run_id_sha=run_id_sha)

    monkeypatch.setattr(module, "PprTtftAdapter", _FakeAdapter)
    monkeypatch.setattr(
        module,
        "_runtime_inputs",
        lambda _env: {
            "email": "bench@corp.example",
            "password": "SENTINEL-PW-9d3k",
            "model_id": "qwen3.7-plus",
        },
    )
    line = (
        "gateway | Agent model-plane timing schema=ppr-timing/v1 "
        f"wire=chat_completions run_id={run_id} call_id=call-1 model={receipt_model} "
        "local_pre_provider_seconds=0.04 provider_wait_seconds=2.0 "
        "local_projection_seconds=0.5 local_overhead_seconds=0.54 "
        "model_plane_ttft_seconds=2.54"
    )
    if reader is None and with_reader:
        reader = lambda _since: [line]  # noqa: E731
    summary = module.run_benchmark(
        env_path=Path("/nonexistent.env"),
        gateway_base_url="http://127.0.0.1:1/api/v1",
        output_path=tmp_path / "out.json",
        prompt="x",
        trials=trials,
        thinking_level="low",
        p50_ceiling_seconds=9.0,
        p95_ceiling_seconds=9.0,
        timing_line_reader=reader,
        warmup_trials=warmup,
        min_recordable_gate_set=min_gate,
    )
    return summary, captured


def test_run_benchmark_enforces_model_identity_from_timing_receipt(monkeypatch, tmp_path) -> None:
    # The Rust cutover dropped run_started.context_snapshot, so model identity
    # is enforced against the gateway ppr-timing/v1 receipt instead. A receipt
    # naming a different model must fail the trial closed (never enter a gate).
    summary, captured = _run_with_fake_adapter(monkeypatch, tmp_path, receipt_model="evil-model")
    trial = summary["trials"][0]
    assert trial["success"] is False
    assert trial["terminal_status"] == "infrastructure_error"
    assert trial["reason"] == "gateway_runtime_model_mismatch"
    assert summary["passed"] is False
    # Dynamic ttft.* task ids are bound to "text" up front (strict adapter index).
    assert captured["model_id"] == "qwen3.7-plus"
    assert set(captured["task_output_formats"].values()) == {"text"}  # type: ignore[union-attr]


def test_run_benchmark_keeps_trial_when_receipt_model_matches(monkeypatch, tmp_path) -> None:
    summary, _ = _run_with_fake_adapter(monkeypatch, tmp_path, receipt_model="qwen3.7-plus")
    trial = summary["trials"][0]
    assert trial["success"] is True
    assert trial["timing_reconciliation"]["status"] == "ok"
    assert summary["passed"] is True
    # With a present, fully clean reconciliation the run certifies as
    # recordable — the single bit main() gates exit 0 on.
    assert summary["timing_reconciliation"]["reconciliation_passed"] is True
    assert summary["recordable"] is True


def test_run_benchmark_readerless_run_is_never_recordable(monkeypatch, tmp_path) -> None:
    # Delta review finding 1: the old exit rule (`is not False`) let a
    # receipt-less run exit 0 with zero model verification. recordable must
    # be False whenever no reconciliation was collected.
    summary, _ = _run_with_fake_adapter(
        monkeypatch, tmp_path, receipt_model="qwen3.7-plus", with_reader=False
    )
    assert summary["passed"] is True  # ceilings alone are not certification
    assert summary["timing_reconciliation"]["reconciliation_passed"] is None
    assert summary["recordable"] is False


def test_run_benchmark_warmup_exclusion_fails_report_under_v2(monkeypatch, tmp_path) -> None:
    # G3/v2 clause (1): exclusions still fail the report for *every* trial,
    # warm-ups included — a warm-up whose receipt was never collected means
    # the p99 claim covers 106/107 trials, so the report is not certified.
    receipt_line = _timing_line("run-1")  # matches the fake adapter's run_id
    first_since: list[str] = []

    def reader(since: str) -> list[str]:
        # The reconciliation reads retry with the same since stamp; key per
        # trial, not per call: the first trial's window yields nothing.
        if not first_since:
            first_since.append(since)
        if since == first_since[0]:
            return []  # warm-up trial: receipt never observed
        return [receipt_line]

    summary, _ = _run_with_fake_adapter(
        monkeypatch,
        tmp_path,
        receipt_model="qwen3.7-plus",
        trials=2,
        warmup=1,
        reader=reader,
    )
    warmup_trial, gate_trial = summary["trials"]
    assert warmup_trial["timing_reconciliation"]["status"] == "missing"
    assert gate_trial["timing_reconciliation"]["status"] == "ok"
    assert summary["passed"] is True
    assert summary["timing_reconciliation"]["reconciliation_passed"] is False
    assert summary["recordable"] is False


def _receipt_with_server_ttft(server_ttft: float, parts: tuple[float, float, float]) -> str:
    pre, wait, proj = parts
    assert abs(pre + wait + proj - server_ttft) < 1e-9
    return (
        "gateway | Agent model-plane timing schema=ppr-timing/v1 "
        f"wire=chat_completions run_id=run-1 call_id=call-1 model=qwen3.7-plus "
        f"local_pre_provider_seconds={pre} provider_wait_seconds={wait} "
        f"local_projection_seconds={proj} local_overhead_seconds={pre + proj} "
        f"model_plane_ttft_seconds={server_ttft}"
    )


def test_run_benchmark_v2_isolated_tolerance_exceeded_still_certifies(
    monkeypatch, tmp_path
) -> None:
    # G3/v2 clause (2): the residual bound is a p99 quantile over all trials.
    # Client ttft is 2.6; a trial whose server ttft is 2.36 has residual
    # 0.24 > the v1 0.200 floor (status tolerance_exceeded) but < the 0.250
    # p99 bound — with 3 trials the nearest-rank p99 is the max — so the run
    # certifies, and the informational status is preserved.
    lines = [
        _receipt_with_server_ttft(2.36, (0.04, 2.0, 0.32)),  # residual 0.24
        _receipt_with_server_ttft(2.54, (0.04, 2.0, 0.5)),  # residual 0.06
        _receipt_with_server_ttft(2.5, (0.04, 2.0, 0.46)),  # residual 0.1
    ]
    assigned: dict[str, str] = {}

    def reader(since: str) -> list[str]:
        # Idempotent per trial (the joiner reads the same window twice):
        # map the trial's since-stamp to the next receipt in the list.
        if since not in assigned:
            assigned[since] = lines[len(assigned)]
        return [assigned[since]]

    summary, _ = _run_with_fake_adapter(
        monkeypatch,
        tmp_path,
        receipt_model="qwen3.7-plus",
        trials=3,
        warmup=0,
        reader=reader,
    )
    statuses = [t["timing_reconciliation"]["status"] for t in summary["trials"]]
    assert statuses == ["tolerance_exceeded", "ok", "ok"]
    assert summary["passed"] is True
    assert summary["timing_reconciliation"]["reconciliation_passed"] is True
    assert summary["recordable"] is True


def test_run_benchmark_v2_lower_bound_violation_blocks_certification(monkeypatch, tmp_path) -> None:
    # G3/v2 clause (1): client TTFT earlier than server minus 0.010 is
    # structurally impossible (the server window is a sub-interval of the
    # client window) and blocks certification per trial, never quantiled.
    # Server 2.65 vs client 2.6 => residual -0.05 < -0.010.
    line = _receipt_with_server_ttft(2.65, (0.04, 2.0, 0.61))
    summary, _ = _run_with_fake_adapter(
        monkeypatch, tmp_path, receipt_model="qwen3.7-plus", reader=lambda _s: [line]
    )
    assert summary["trials"][0]["timing_reconciliation"]["status"] == "tolerance_exceeded"
    assert summary["timing_reconciliation"]["reconciliation_passed"] is False
    assert summary["recordable"] is False


def test_run_benchmark_v2_defect_ceiling_blocks_certification(monkeypatch, tmp_path) -> None:
    # G3/v2 clause (3): a single gross outlier (residual 0.6 > the 0.500
    # ceiling) fails as structural even though a 1-trial p99 would be it.
    line = _receipt_with_server_ttft(2.0, (0.04, 1.5, 0.46))
    summary, _ = _run_with_fake_adapter(
        monkeypatch, tmp_path, receipt_model="qwen3.7-plus", reader=lambda _s: [line]
    )
    assert summary["trials"][0]["timing_reconciliation"]["status"] == "tolerance_exceeded"
    assert summary["timing_reconciliation"]["reconciliation_passed"] is False
    assert summary["recordable"] is False


def test_run_benchmark_report_never_contains_credentials(monkeypatch, tmp_path) -> None:
    # Security review A5: the serialized report must not leak the benchmark
    # account. The sentinel is what _runtime_inputs is monkeypatched to
    # return; the fake adapter proves it actually flowed into the client.
    summary, captured = _run_with_fake_adapter(monkeypatch, tmp_path, receipt_model="qwen3.7-plus")
    assert summary["recordable"] is True
    assert captured["password"] == "SENTINEL-PW-9d3k"
    assert captured["email"] == "bench@corp.example"
    report_text = (tmp_path / "out.json").read_text(encoding="utf-8")
    assert "SENTINEL-PW-9d3k" not in report_text
    assert "bench@corp.example" not in report_text


def test_run_benchmark_g5_gate_set_floor_blocks_recordable(monkeypatch, tmp_path) -> None:
    # Client review MINOR-3: certification must enforce G5's "gate-set >=100
    # successful trials" in code, not operator discipline — a fully clean
    # exploratory run stays NOT recordable under the default floor.
    summary, _ = _run_with_fake_adapter(
        monkeypatch, tmp_path, receipt_model="qwen3.7-plus", trials=3, min_gate=100
    )
    assert summary["passed"] is True
    assert summary["timing_reconciliation"]["reconciliation_passed"] is True
    assert summary["timing_reconciliation"]["gate_set_meets_recordable_min"] is False
    assert summary["recordable"] is False


def test_run_benchmark_identity_violation_receipt_blocks_recordable(monkeypatch, tmp_path) -> None:
    # Client review NIT: end-to-end (not just _reconcile_trial unit) proof
    # that a receipt whose additive components don't sum to the server TTFT
    # (G1 violation) fails the report closed and can never certify.
    line = _timing_line(
        "run-1",
        components=(
            "local_pre_provider_seconds=0.04 provider_wait_seconds=2.0 "
            "local_projection_seconds=0.5 local_overhead_seconds=0.54 "
            "model_plane_ttft_seconds=3.00"  # components sum to 2.54 — lie
        ),
    )
    summary, _ = _run_with_fake_adapter(
        monkeypatch, tmp_path, receipt_model="qwen3.7-plus", reader=lambda _s: [line]
    )
    assert summary["trials"][0]["timing_reconciliation"]["status"] == "identity_violation"
    assert summary["timing_reconciliation"]["status_counts"]["identity_violation"] == 1
    assert summary["timing_reconciliation"]["reconciliation_passed"] is False
    assert summary["recordable"] is False


def test_run_benchmark_malformed_receipt_fails_trial_not_run(monkeypatch, tmp_path) -> None:
    # Client review MINOR-5: a malformed adapter receipt (missing timing
    # keys) must mark ONE trial as infrastructure_error — never crash the
    # whole paid run mid-way with no output.
    class _BrokenResult:
        def __init__(self) -> None:
            self.terminal_status = "succeeded"
            self.text = "pong"
            self.duration_seconds = 2.7
            self.metadata = {"event_types": [], "lifecycle_events": []}  # no timing

    summary, _ = _run_with_fake_adapter(
        monkeypatch,
        tmp_path,
        receipt_model="qwen3.7-plus",
        result_factory=lambda _sha: _BrokenResult(),
    )
    trial = summary["trials"][0]
    assert trial["success"] is False
    assert trial["terminal_status"] == "infrastructure_error"
    assert "KeyError" in trial["reason"]
    assert summary["passed"] is False
    assert summary["recordable"] is False


def test_run_benchmark_refuses_existing_output(tmp_path) -> None:
    # Security review A2: baseline history is evidence — a rerun must write
    # a fresh path, never silently overwrite an earlier report.
    import scripts.assistant_ttft_benchmark as module

    existing = tmp_path / "out.json"
    existing.write_text("{}", encoding="utf-8")
    with pytest.raises(BenchmarkError, match="ttft_output_exists"):
        module.run_benchmark(
            env_path=Path("/nonexistent.env"),
            gateway_base_url="http://127.0.0.1:1/api/v1",
            output_path=existing,
            prompt="x",
            trials=1,
            thinking_level="low",
            p50_ceiling_seconds=9.0,
            p95_ceiling_seconds=9.0,
            warmup_trials=0,
        )
    assert existing.read_text(encoding="utf-8") == "{}"
