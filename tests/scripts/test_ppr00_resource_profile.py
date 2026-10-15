"""Offline unit tests for scripts/ppr00_resource_profile.py (PPR-00).

Docker is faked at `subprocess.run` with canned `inspect` /
`stats --format json` payloads; HTTP is a local stub server; the adapter is a
sentinel fake mirroring tests/scripts/test_assistant_ttft_benchmark.py
(`PprTtftAdapter` + `_runtime_inputs` patched, sentinel password
"SENTINEL-PW-9d3k"). No live stack is touched, no network beyond loopback,
no paid calls. Timing constants are injected at test-fast values.
"""

from __future__ import annotations

import json
import subprocess
import threading
import time
from collections import deque
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

import scripts.ppr00_resource_profile as module
from scripts.native_agent_parity_benchmark import BenchmarkError

SENTINEL_PW = "SENTINEL-PW-9d3k"
SENTINEL_EMAIL = "bench@corp.example"

MEM_LIMIT_BYTES = {
    "ai-gateway-pg": 320 * 1024 * 1024,
    "ai-gateway-redis": 192 * 1024 * 1024,
    "ai-gateway-qdrant": 352 * 1024 * 1024,
    "ai-gateway-backend": 384 * 1024 * 1024,
    "ai-gateway-frontend": 96 * 1024 * 1024,
    "ai-gateway-knowledge-service": 512 * 1024 * 1024,
    "ai-gateway-knowledge-worker": 512 * 1024 * 1024,
    "ai-gateway-agent-capability-worker": 1024 * 1024 * 1024,
    "ai-gateway-agent-runtime": 192 * 1024 * 1024,
}
BASE_MEM_MIB = dict.fromkeys(MEM_LIMIT_BYTES, 60.0)
BASE_MEM_MIB["ai-gateway-backend"] = 100.0
GUARD_TRIP_NAME = "ai-gateway-backend"
GUARD_TRIP_MIB = 360.0  # 93.75 % of the frozen 384 MiB limit


def _task_key(task_id: str) -> tuple[str, int]:
    parts = task_id.split(".")
    return parts[1], int(parts[2])


# ---------------------------------------------------------------------------
# fakes
# ---------------------------------------------------------------------------


def _install_docker(
    monkeypatch: pytest.MonkeyPatch,
    *,
    working_dir: str,
    stats_mode: str = "ok",
    restart_bump: bool = False,
    recreate_bump: bool = False,
    foreign_container: str | None = None,
    missing_mem_limit: str | None = None,
) -> dict[str, int]:
    real_run = subprocess.run
    calls = {"inspect": 0, "stats": 0}

    def fake_run(argv: Any, **kwargs: Any) -> subprocess.CompletedProcess:
        argv = list(argv)
        if argv[0] == "docker" and argv[1] == "inspect":
            calls["inspect"] += 1
            entries = []
            for name in (a for a in argv if a in MEM_LIMIT_BYTES):
                recreated = recreate_bump and calls["inspect"] >= 2 and name == "ai-gateway-backend"
                restart = (
                    1
                    if (restart_bump and calls["inspect"] >= 2 and name == "ai-gateway-backend")
                    else 0
                )
                entries.append(
                    {
                        "Id": f"fake-{name}{'-recreated' if recreated else ''}",
                        "Name": f"/{name}",
                        "Image": "sha256:" + "a" * 64,
                        "Created": "2026-08-20T00:00:00Z",
                        "RestartCount": restart,
                        "HostConfig": {
                            "Memory": 0 if name == missing_mem_limit else MEM_LIMIT_BYTES[name]
                        },
                        "Config": {
                            "Image": f"{name}:test",
                            "Labels": {
                                module.OWNERSHIP_LABEL: (
                                    "/tmp/some-other-checkout"
                                    if name == foreign_container
                                    else working_dir
                                ),
                            },
                        },
                    }
                )
            return subprocess.CompletedProcess(argv, 0, json.dumps(entries), "")
        if argv[0] == "docker" and argv[1] == "stats":
            calls["stats"] += 1
            lines = []
            for name in (a for a in argv if a in MEM_LIMIT_BYTES):
                used = BASE_MEM_MIB[name]
                if stats_mode == "always_high" and name == GUARD_TRIP_NAME:
                    used = GUARD_TRIP_MIB
                limit_mib = MEM_LIMIT_BYTES[name] / (1024.0 * 1024.0)
                lines.append(
                    json.dumps(
                        {
                            "BlockIO": "0B / 0B",
                            "CPUPerc": "4.00%",
                            "MemPerc": f"{used / limit_mib * 100.0:.2f}%",
                            "MemUsage": f"{used:.1f}MiB / {limit_mib:.0f}MiB",
                            "Name": name,
                            "NetIO": "256KiB / 128KiB",
                            "PIDs": 5,
                        }
                    )
                )
            return subprocess.CompletedProcess(argv, 0, "\n".join(lines) + "\n", "")
        return real_run(argv, **kwargs)

    monkeypatch.setattr(module.subprocess, "run", fake_run)
    return calls


def _install_adapter(
    monkeypatch: pytest.MonkeyPatch,
    *,
    failures: dict[tuple[str, int], Exception] | None = None,
) -> dict[str, Any]:
    failures = failures or {}
    lock = threading.Lock()
    tracker: dict[str, Any] = {
        "instances": 0,
        "kwargs": {},
        "calls": [],
        "prompts": [],
        "inflight": 0,
        "max_inflight": 0,
    }

    class _FakeAdapter:
        def __init__(self, **kwargs: Any) -> None:
            tracker["instances"] += 1
            tracker["kwargs"] = kwargs

        def start_task(self, task_id: str) -> None:
            with lock:
                tracker["calls"].append(task_id)
                tracker["inflight"] += 1
                tracker["max_inflight"] = max(tracker["max_inflight"], tracker["inflight"])

        def run_turn(self, task_id: str, prompt: str) -> Any:
            try:
                time.sleep(0.02)
                tracker["prompts"].append(prompt)
                key = _task_key(task_id)
                if key in failures:
                    raise failures[key]
                return SimpleNamespace(
                    terminal_status="succeeded",
                    text="4",
                    duration_seconds=0.02,
                    metadata={
                        "timing": {
                            "ttft_seconds": 0.01,
                            "phases": [{"first_event_seconds": 0.01}],
                        }
                    },
                )
            finally:
                with lock:
                    tracker["inflight"] -= 1

    monkeypatch.setattr(module, "PprTtftAdapter", _FakeAdapter)
    monkeypatch.setattr(
        module,
        "_runtime_inputs",
        lambda _env: {
            "email": SENTINEL_EMAIL,
            "password": SENTINEL_PW,
            "model_id": "qwen3.7-plus",
        },
    )
    return tracker


class _HealthHandler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        body = json.dumps({"status": "ok"}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_args: Any) -> None:
        return None


@contextmanager
def _health_server():
    server = ThreadingHTTPServer(("127.0.0.1", 0), _HealthHandler)
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_address[1]
    finally:
        server.shutdown()
        server.server_close()


def _run(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
    *,
    levels: str = "1",
    max_calls: int = module.DEFAULT_MAX_CALLS,
    stats_mode: str = "ok",
    ownership: str = "ok",
    restart_bump: bool = False,
    recreate_bump: bool = False,
    foreign_container: str | None = None,
    missing_mem_limit: str | None = None,
    failures: dict[tuple[str, int], Exception] | None = None,
    wall_cap_minutes: float = 30.0,
) -> tuple[dict[str, Any], dict[str, Any], dict[str, int], Path]:
    working_dir = str(module.ROOT) if ownership == "ok" else "/tmp/some-other-checkout"
    docker_calls = _install_docker(
        monkeypatch,
        working_dir=working_dir,
        stats_mode=stats_mode,
        restart_bump=restart_bump,
        recreate_bump=recreate_bump,
        foreign_container=foreign_container,
        missing_mem_limit=missing_mem_limit,
    )
    tracker = _install_adapter(monkeypatch, failures=failures)
    output = tmp_path / "ppr00-resource-2026-08-28.json"
    with _health_server() as port:
        report = module.run_profile(
            env_path=Path("/nonexistent.env"),
            gateway_base_url=f"http://127.0.0.1:{port}/api/v1",
            output_path=output,
            levels=module._parse_levels(levels),
            max_calls=max_calls,
            wall_clock_cap_minutes=wall_cap_minutes,
            agent_runtime_health_url=f"http://127.0.0.1:{port}/health/ready",
            stagger_seconds=0.0,
            settle_seconds=0.1,
            quiet_seconds=0.02,
            idle_baseline_seconds=0.2,
            sample_interval=0.02,
        )
    return report, tracker, docker_calls, output


def _samples_file(output: Path) -> Path:
    return output.with_name(output.stem + ".samples.jsonl")


# ---------------------------------------------------------------------------
# (1) ownership preflight
# ---------------------------------------------------------------------------


def test_ownership_preflight_aborts_before_any_stream(monkeypatch, tmp_path) -> None:
    # runtime-and-secrets §1: containers carrying another checkout's
    # working_dir are the wrong runtime — abort before login, before any
    # paid stream, before the sampler even opens its file.
    report, tracker, docker_calls, output = _run(monkeypatch, tmp_path, ownership="foreign")
    assert report["exit"]["code"] == 2
    assert report["exit"]["aborted_reason"] == "ownership_mismatch"
    assert report["ownership_check"]["ownership_ok"] is False
    assert report["ownership_check"]["expected_root"] == str(module.ROOT)
    assert tracker["instances"] == 0
    assert tracker["calls"] == []
    assert docker_calls["inspect"] == 1
    assert docker_calls["stats"] == 0
    assert output.exists()  # report still written first
    assert not _samples_file(output).exists()


def test_ownership_preflight_checks_every_measured_container(monkeypatch, tmp_path) -> None:
    # A matching backend is insufficient: one dependency from another
    # checkout would make the nine-container resource curve invalid.
    report, tracker, docker_calls, _ = _run(
        monkeypatch,
        tmp_path,
        foreign_container="ai-gateway-qdrant",
    )
    assert report["exit"]["code"] == 2
    assert report["exit"]["aborted_reason"] == "ownership_mismatch"
    assert report["ownership_check"]["ownership_ok"] is False
    assert tracker["instances"] == 0
    assert docker_calls == {"inspect": 1, "stats": 0}


def test_missing_memory_limit_aborts_before_sampling(monkeypatch, tmp_path) -> None:
    # The 90%-of-limit guard cannot be enforced when a measured container has
    # no frozen limit; fail closed rather than silently disabling that guard.
    report, tracker, docker_calls, _ = _run(
        monkeypatch,
        tmp_path,
        missing_mem_limit="ai-gateway-agent-runtime",
    )
    assert report["exit"]["code"] == 2
    assert report["exit"]["aborted_reason"] == "mem_limit_missing"
    assert tracker["instances"] == 0
    assert docker_calls == {"inspect": 1, "stats": 0}


# ---------------------------------------------------------------------------
# (2) level scheduling
# ---------------------------------------------------------------------------


def test_level_schedule_issues_max_2k_10_within_inflight_cap(monkeypatch, tmp_path) -> None:
    report, tracker, _, output = _run(monkeypatch, tmp_path, levels="1,2")
    exit_block = report["exit"]
    assert exit_block["code"] == 0
    assert exit_block["completed_all_levels"] is True
    assert exit_block["incomplete"] is False
    assert exit_block["aborted_reason"] is None
    assert tracker["instances"] == 1  # one login, shared across streams
    by_level = {block["k"]: block for block in report["levels"]}
    assert sorted(by_level) == [1, 2]
    for k, block in by_level.items():
        assert block["target_streams"] == 10  # max(2*K, 10)
        assert block["issued"] == 10
        assert block["returned_streams"] == 10
        assert block["successful_streams"] == 10
        assert block["error_classes"]["success"] == 10
        assert block["max_concurrent_in_flight"] <= block["in_flight_hard_cap"] == 3 * k
        assert block["max_concurrent_in_flight"] <= k  # fixed pool of K workers
        steady = block["services"]["ai-gateway-backend"]
        assert steady["samples"] >= 1
        assert steady["mean_mib"] == 100.0
        assert steady["pct_of_mem_limit_mean"] == pytest.approx(100.0 / 384.0 * 100.0, abs=0.01)
    warm = report["warmup"]["streams"]
    assert len(warm) == 2
    assert all(record["classification"] == "success" for record in warm)
    assert exit_block["calls_issued"] == 2 + 10 + 10
    assert report["warmed_idle"]["ai-gateway-backend"]["mean_mib"] == 100.0
    assert _samples_file(output).exists()
    # Baseline workload identity (draft §Load shape, byte-identical mix).
    kwargs = tracker["kwargs"]
    assert kwargs["temperature"] == 0.0
    assert kwargs["max_tokens"] == 256
    assert kwargs["thinking_level"] == "low"
    assert kwargs["execution_profile"] == "safe"
    assert kwargs["model_id"] == "qwen3.7-plus"
    assert set(tracker["prompts"]) == {module.DEFAULT_PROMPT}


def test_incremental_rss_derivation_from_run_windows(monkeypatch, tmp_path) -> None:
    report, _, _, _ = _run(monkeypatch, tmp_path, levels="1,2")
    derivations = report["derivations"]
    assert derivations["services"].keys() == set(module.DERIVATION_SERVICES)
    entry = derivations["services"]["ai-gateway-backend"]
    assert entry["warmed_idle_rss_mean_mib"] == 100.0
    # canned RSS is constant, so every delta is exactly 0.
    assert entry["delta_mib"] == {"1": 0.0, "2": 0.0}
    assert entry["per_stream_mib"] == {"1": 0.0, "2": 0.0}
    assert entry["least_squares_slope_mib_per_stream"] == 0.0
    assert derivations["caveats"] == list(module.CAVEATS)


# ---------------------------------------------------------------------------
# (3) 90 % mem guard
# ---------------------------------------------------------------------------


def test_mem_limit_guard_aborts_and_still_writes_report(monkeypatch, tmp_path) -> None:
    report, _, _, output = _run(monkeypatch, tmp_path, levels="1", stats_mode="always_high")
    exit_block = report["exit"]
    assert exit_block["code"] == 2
    assert exit_block["aborted_reason"] == "mem_limit_90pct"
    violation = exit_block["mem_guard_violation"]
    assert violation is not None
    assert violation["container"] == GUARD_TRIP_NAME
    assert violation["mem_used_mib"] == GUARD_TRIP_MIB
    assert violation["mem_limit_mib"] == 384.0
    assert violation["threshold_fraction"] == 0.90
    assert report["levels"] == []  # staircase never started
    assert output.exists()  # report with the abort record is written first


# ---------------------------------------------------------------------------
# (4) error classification
# ---------------------------------------------------------------------------


def test_admission_rejects_are_recorded_not_retried(monkeypatch, tmp_path) -> None:
    # Draft open question 2: a 429 from the gateway admission controller is
    # recorded data. Never retried (each task id appears exactly once),
    # never re-provisioned — and it does not abort the run.
    failures = {
        ("l1", 3): BenchmarkError("gateway_chat_http_429"),
        ("l1", 7): BenchmarkError("gateway_chat_http_429"),
    }
    report, tracker, _, _ = _run(monkeypatch, tmp_path, levels="1", failures=failures)
    block = report["levels"][0]
    assert block["error_classes"]["http_429_or_admission_reject"] == 2
    assert block["error_classes"]["success"] == 8
    assert block["issued"] == 10
    assert block["returned_streams"] == 10
    assert report["exit"]["code"] == 0  # freeze baselines: errors feed no gate
    assert len(tracker["calls"]) == 12  # 2 warm-up + 10, no retries
    assert len(set(tracker["calls"])) == 12
    reasons = [
        record.get("reason")
        for record in block["streams"]
        if record["classification"] == "http_429_or_admission_reject"
    ]
    assert len(reasons) == 2
    assert all("gateway_chat_http_429" in (reason or "") for reason in reasons)


def test_classify_stream_error_vocabulary() -> None:
    classify = module._classify_stream_error
    assert classify(BenchmarkError("gateway_chat_http_429")) == "http_429_or_admission_reject"
    assert classify(BenchmarkError("http_429:assistant")) == "http_429_or_admission_reject"
    assert classify(BenchmarkError("gateway_admission_concurrency_reject")) == (
        "http_429_or_admission_reject"
    )
    assert classify(BenchmarkError("rate_limited")) == "http_429_or_admission_reject"
    # Post-run hardening: the bare "rate" token also matched benign words
    # ("generated", "separate") — a stream defect must never masquerade as
    # an admission reject.
    assert classify(BenchmarkError("gateway_generated_content_invalid")) == "stream_error"
    assert classify(BenchmarkError("separate_turn_contract_broken")) == "stream_error"
    assert classify(BenchmarkError("Rate limit exceeded")) == "http_429_or_admission_reject"
    assert classify(BenchmarkError("gateway_chat_http_503")) == "http_5xx"
    assert classify(BenchmarkError("http_500:sessions")) == "http_5xx"
    assert classify(BenchmarkError("gateway_invalid_sse_json")) == "stream_error"
    assert classify(BenchmarkError("gateway_terminal_contract")) == "stream_error"
    assert classify(TimeoutError("The read operation timed out")) == "timeout"
    assert classify(BenchmarkError("http_transport:timeout")) == "timeout"
    assert classify(ConnectionResetError("reset by peer")) == "infrastructure_error"
    assert classify(KeyError("timing")) == "infrastructure_error"


def test_docker_commands_time_out_fail_closed(monkeypatch) -> None:
    def timeout(*_args: Any, **_kwargs: Any) -> subprocess.CompletedProcess:
        raise subprocess.TimeoutExpired(cmd=["docker", "inspect"], timeout=15.0)

    monkeypatch.setattr(module.subprocess, "run", timeout)
    with pytest.raises(BenchmarkError, match="docker_cli_timeout:inspect"):
        module._docker_stdout(["inspect", "ai-gateway-backend"])


# ---------------------------------------------------------------------------
# (5) derivation math on canned windows
# ---------------------------------------------------------------------------


def test_derive_incremental_rss_math() -> None:
    warmed = dict.fromkeys(module.DERIVATION_SERVICES, 100.0)
    steady = {
        1: dict.fromkeys(module.DERIVATION_SERVICES, 102.0),
        2: dict.fromkeys(module.DERIVATION_SERVICES, 104.0),
        4: dict.fromkeys(module.DERIVATION_SERVICES, 112.0),
    }
    derived = module._derive_incremental_rss(warmed, steady)
    entry = derived["services"]["ai-gateway-backend"]
    assert entry["delta_mib"] == {"1": 2.0, "2": 4.0, "4": 12.0}
    assert entry["per_stream_mib"] == {"1": 2.0, "2": 2.0, "4": 3.0}
    # least-squares through (1,2),(2,4),(4,12): slope = 24/7
    assert entry["least_squares_slope_mib_per_stream"] == pytest.approx(24.0 / 7.0, abs=1e-4)


def test_derive_incremental_rss_handles_missing_windows() -> None:
    warmed = dict.fromkeys(module.DERIVATION_SERVICES, None)
    steady = {1: dict.fromkeys(module.DERIVATION_SERVICES, 102.0)}
    derived = module._derive_incremental_rss(warmed, steady)
    entry = derived["services"]["ai-gateway-agent-runtime"]
    assert entry["delta_mib"] == {"1": None}
    assert entry["per_stream_mib"] == {"1": None}
    assert entry["least_squares_slope_mib_per_stream"] is None


def test_least_squares_slope_edge_cases() -> None:
    assert module._least_squares_slope([(1.0, 2.0)]) is None
    assert module._least_squares_slope([(1.0, 1.0), (1.0, 2.0)]) is None
    assert module._least_squares_slope([(1.0, 1.0), (3.0, 5.0)]) == 2.0


def test_percent_to_mib_parses_docker_units() -> None:
    assert module._percent_to_mib("123.4MiB") == pytest.approx(123.4)
    assert module._percent_to_mib("384MiB") == 384.0
    assert module._percent_to_mib("1.5GiB") == 1536.0
    assert module._percent_to_mib("512KiB") == 0.5
    assert module._percent_to_mib("1024B") == pytest.approx(1024.0 / (1024.0 * 1024.0), abs=1e-6)
    assert module._percent_to_mib("garbage") is None


# ---------------------------------------------------------------------------
# (6) refuse-existing output
# ---------------------------------------------------------------------------


def test_refuses_existing_output(tmp_path) -> None:
    # Same evidence rule as the TTFT baseline: a rerun writes a fresh path.
    existing = tmp_path / "ppr00-resource-2026-08-28.json"
    existing.write_text("{}", encoding="utf-8")
    with pytest.raises(BenchmarkError, match="ppr00_output_exists"):
        module.run_profile(
            env_path=Path("/nonexistent.env"),
            gateway_base_url="http://127.0.0.1:1/api/v1",
            output_path=existing,
            levels=[1],
        )
    assert existing.read_text(encoding="utf-8") == "{}"


# ---------------------------------------------------------------------------
# (7) max-calls cap
# ---------------------------------------------------------------------------


def test_max_calls_cap_marks_report_incomplete(monkeypatch, tmp_path) -> None:
    report, _, _, output = _run(monkeypatch, tmp_path, levels="1", max_calls=4)
    exit_block = report["exit"]
    assert exit_block["incomplete"] is True
    assert "max_calls_exhausted" in exit_block["incomplete_reasons"]
    assert exit_block["calls_issued"] == 4  # 2 warm-up + 2 issued at level 1
    assert exit_block["completed_all_levels"] is False
    assert exit_block["code"] == 2
    assert report["levels"][0]["issued"] == 2
    assert report["levels"][0]["returned_streams"] == 2
    assert output.exists()


def test_wall_clock_cap_marks_report_incomplete(monkeypatch, tmp_path) -> None:
    # An already-expired budget stops issuing at the next can_issue() check,
    # drains, and the report says so honestly (exit 2 is reserved for
    # non-full completion; nothing pretends to be a complete baseline).
    report, _, _, output = _run(monkeypatch, tmp_path, levels="1", wall_cap_minutes=1e-7)
    exit_block = report["exit"]
    assert exit_block["incomplete"] is True
    assert "wall_clock_cap_exceeded" in exit_block["incomplete_reasons"]
    assert exit_block["code"] == 2
    assert report["levels"][0]["issued"] == 0
    assert output.exists()


def test_run_level_wall_deadline_expires_mid_drain() -> None:
    # Post-run hardening: the deadline can expire between completions, not
    # only at a can_issue() call — the drain loop itself must re-check it
    # and stop issuing (fail-closed) while already-issued streams still
    # drain normally.
    class _FakeSampler:
        violation = None
        docker_error = None

        def set_phase(self, phase: str) -> None:  # noqa: ARG002
            return None

    class _SlowAdapter:
        def start_task(self, task_id: str) -> None:  # noqa: ARG002
            return None

        def run_turn(self, task_id: str, prompt: str):  # noqa: ARG002
            time.sleep(0.05)
            return SimpleNamespace(
                terminal_status="succeeded",
                text="x",
                duration_seconds=0.05,
                metadata={"timing": {"phases": [{"first_event_seconds": 0.01}]}},
            )

    clock_start = time.monotonic()
    state = module._RunState(
        max_calls=1000, wall_deadline=clock_start + 0.12, clock_start=clock_start
    )
    state.sampler = _FakeSampler()
    block = module._run_level(
        state,
        _SlowAdapter(),
        level=1,
        task_ids=deque(f"l1.{i}" for i in range(10)),
        prompt="p",
        stagger_seconds=0.0,
    )
    assert block["issued"] < block["target_streams"]  # issuing stopped early
    assert block["returned_streams"] == block["issued"]  # but everything issued drained
    assert state.incomplete is True
    assert "wall_clock_cap_exceeded" in state.incomplete_reasons


def test_zero_samples_never_certify_complete(monkeypatch, tmp_path) -> None:
    # Post-run hardening: a report whose sampler produced nothing cannot
    # exit 0 as a "complete" baseline, however clean the streams were.
    class _StarvedSampler(module._Sampler):
        def run(self) -> None:  # never collects a sample
            self._exit.wait()

    monkeypatch.setattr(module, "_Sampler", _StarvedSampler)
    report, _, _, _ = _run(monkeypatch, tmp_path, levels="1")
    exit_block = report["exit"]
    assert exit_block["incomplete"] is True
    assert "no_samples" in exit_block["incomplete_reasons"]
    assert exit_block["code"] == 2
    assert report["raw_sample_count"] == 0


def test_rejects_nonpositive_wall_clock_cap(tmp_path) -> None:
    with pytest.raises(BenchmarkError, match="ppr00_wall_clock_cap_invalid"):
        module.run_profile(
            env_path=Path("/nonexistent.env"),
            gateway_base_url="http://127.0.0.1:1/api/v1",
            output_path=tmp_path / "never.json",
            levels=[1],
            wall_clock_cap_minutes=0.0,
        )


# ---------------------------------------------------------------------------
# (8) restart evidence
# ---------------------------------------------------------------------------


def test_restart_count_increase_is_recorded_honestly(monkeypatch, tmp_path) -> None:
    # A container restart at a level end is an honest current-topology
    # finding (aborted_at_level + evidence), never a silent pass — but the
    # completed level's data is still written out.
    report, _, docker_calls, output = _run(monkeypatch, tmp_path, levels="1", restart_bump=True)
    exit_block = report["exit"]
    assert docker_calls["inspect"] == 2  # frozen at start + re-check after the level
    assert exit_block["code"] == 2
    assert exit_block["aborted_reason"] == "container_restart"
    assert exit_block["aborted_at_level"] == 1
    evidence = exit_block["restart_evidence"]
    assert [item["container"] for item in evidence] == ["ai-gateway-backend"]
    assert evidence[0]["restart_before"] == 0
    assert evidence[0]["restart_after"] == 1
    assert report["levels"][0]["returned_streams"] == 10
    assert output.exists()


def test_container_recreation_is_recorded_even_when_restart_count_resets(
    monkeypatch, tmp_path
) -> None:
    report, _, docker_calls, output = _run(
        monkeypatch,
        tmp_path,
        levels="1",
        recreate_bump=True,
    )
    exit_block = report["exit"]
    assert docker_calls["inspect"] == 2
    assert exit_block["code"] == 2
    assert exit_block["aborted_reason"] == "container_recreated"
    assert exit_block["aborted_at_level"] == 1
    evidence = exit_block["restart_evidence"]
    assert [item["container"] for item in evidence] == ["ai-gateway-backend"]
    assert evidence[0]["container_id_before"] != evidence[0]["container_id_after"]
    assert output.exists()


# ---------------------------------------------------------------------------
# credential hygiene
# ---------------------------------------------------------------------------


def test_report_and_samples_never_contain_credentials(monkeypatch, tmp_path) -> None:
    report, tracker, _, output = _run(monkeypatch, tmp_path, levels="1")
    assert report["exit"]["code"] == 0
    # The sentinel really flowed into the client side of the driver...
    assert tracker["kwargs"]["password"] == SENTINEL_PW
    assert tracker["kwargs"]["email"] == SENTINEL_EMAIL
    # ...and appears nowhere in the evidence files (security review A5 rule).
    blobs = {
        "report": output.read_text(encoding="utf-8"),
        "samples": _samples_file(output).read_text(encoding="utf-8"),
    }
    for text in blobs.values():
        assert SENTINEL_PW not in text
        assert SENTINEL_EMAIL not in text


# ---------------------------------------------------------------------------
# agent-runtime readiness: host-GET refused -> container-local exec fallback
# ---------------------------------------------------------------------------


def test_agent_runtime_ready_falls_back_to_docker_exec(monkeypatch) -> None:
    # Live default: compose `expose`s 8094 without a host mapping, so the
    # urllib GET is refused; the fallback mirrors compose's own healthcheck
    # (curl inside the container). Without this the whole paid run would
    # abort at pre-flight.
    monkeypatch.setattr(module, "_http_get_ok", lambda *_a, **_kw: False)
    exec_args: list[list[str]] = []

    def fake_exec(args: list[str]) -> str:
        exec_args.append(args)
        return ""

    monkeypatch.setattr(module, "_docker_stdout", fake_exec)
    assert (
        module._wait_for_agent_runtime_ready(
            module.AGENT_RUNTIME_HEALTH_URL, attempts=1, interval=0.0
        )
        is True
    )
    assert exec_args[0][:3] == ["exec", "ai-gateway-agent-runtime", "curl"]
    assert module.AGENT_RUNTIME_EXEC_HEALTH_URL in exec_args[0][-1]


def test_agent_runtime_ready_aborts_when_both_probes_fail(monkeypatch) -> None:
    # Both host GET and container exec failing is an honest infra abort,
    # never a silent proceed.
    monkeypatch.setattr(module, "_http_get_ok", lambda *_a, **_kw: False)

    def raise_exec(_args: list[str]) -> str:
        raise BenchmarkError("docker_cli_failed:exec")

    monkeypatch.setattr(module, "_docker_stdout", raise_exec)
    assert (
        module._wait_for_agent_runtime_ready(
            module.AGENT_RUNTIME_HEALTH_URL, attempts=2, interval=0.0
        )
        is False
    )


def test_agent_runtime_ready_prefers_working_host_get(monkeypatch) -> None:
    # When the operator supplies a reachable mapping, the GET short-circuits
    # and the exec probe is never invoked.
    monkeypatch.setattr(module, "_http_get_ok", lambda *_a, **_kw: True)

    def no_exec(_args: list[str]) -> str:
        raise AssertionError("exec fallback must not run when the host GET works")

    monkeypatch.setattr(module, "_docker_stdout", no_exec)
    assert module._wait_for_agent_runtime_ready("http://127.0.0.1:1/health", 3, 0.0) is True


# ---------------------------------------------------------------------------
# _run_stream: classification + never-retried (paid-run critical)
# ---------------------------------------------------------------------------


class _CountingAdapter:
    def __init__(self, raise_at: int | None = None, terminal_status: str = "succeeded") -> None:
        self.start_calls: list[str] = []
        self.turn_calls = 0
        self._raise_at = raise_at
        self._terminal = terminal_status

    def start_task(self, task_id: str) -> None:
        self.start_calls.append(task_id)

    def run_turn(self, task_id: str, prompt: str):  # noqa: ARG002
        self.turn_calls += 1
        if self._raise_at is not None and self.turn_calls >= self._raise_at:
            raise BenchmarkError("gateway_chat_http_429")
        return SimpleNamespace(
            terminal_status=self._terminal,
            duration_seconds=2.5,
            text="hello" if self._terminal == "succeeded" else "",
            metadata={"timing": {"phases": [{"first_event_seconds": 0.1}]}},
        )


def test_run_stream_classifies_reject_as_data_without_retry() -> None:
    adapter = _CountingAdapter(raise_at=1)
    record = module._run_stream(adapter, task_id="t1", prompt="p", level=1, ordinal=1)
    assert record["classification"] == "http_429_or_admission_reject"
    assert "gateway_chat_http_429" in record["reason"]
    assert adapter.start_calls == ["t1"]  # exactly one attempt — rejects are data


def test_run_stream_terminal_and_success_paths() -> None:
    ok = module._run_stream(_CountingAdapter(), task_id="t2", prompt="p", level=5, ordinal=3)
    assert ok["classification"] == "success"
    assert ok["first_event_seconds"] == 0.1
    bad = module._run_stream(
        _CountingAdapter(terminal_status="cancelled"), task_id="t3", prompt="p", level=5, ordinal=4
    )
    assert bad["classification"] == "stream_error"
    assert bad["reason"].startswith("gateway_terminal_status:cancelled")
