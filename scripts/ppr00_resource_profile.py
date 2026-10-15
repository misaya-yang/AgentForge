#!/usr/bin/env python3
"""PPR-00 named-concurrency resource-load profile (`ppr00-stream-load-v1`).

Implements the frozen design in ``tmp/ppr00-resource-profile-draft.md``: a
threaded pool of K blocking adapter streams per concurrency level plus a
1 Hz ``docker stats`` sampler thread, freezing per-service RSS/CPU baselines
under concurrent assistant streams. This phase records numbers — it asserts
no timing ceiling. Exit 0 on full completion; exit 2 on any guard,
pre-flight, or infrastructure abort, always writing the report first.

Docker usage is observational (``stats``, ``inspect``, plus a container-local
readiness GET through ``docker exec``); the only persistent writes are the
report JSON and the raw samples JSONL. Credentials and raw response bodies are
never serialized.

Ambiguity resolutions against the draft (minimal choices, recorded here per
the task instruction):
- The draft's own cost table (levels 1,5,10,20 -> 10,10,20,40 streams) shows
  the per-level issued count is exactly ``max(2K, 10)``; the "(hard cap
  3*K)" clause is therefore read as a bound on *concurrently outstanding*
  streams, never as a target reduction. The pool of K workers trivially
  keeps in-flight <= K <= 3K.
- The implemented error vocabulary (success / http_429_or_admission_reject /
  http_5xx / stream_error / timeout / infrastructure_error) supersedes the
  draft's five-class list; a container restart is recorded as abort
  evidence (``restart_evidence``), not as a per-stream class.
- Wall-clock / max-calls cap trips mark the report ``incomplete`` and exit 2
  (exit 0 is reserved for full completion).
- A warm-up stream that does not succeed aborts the run (the baseline
  protocol itself failed); warm-up streams are excluded from per-level
  error stats but recorded in ``warmup.streams``.
- Raw samples land at ``<output-dir>/<output-stem>.samples.jsonl``, which
  equals the draft's
  ``reports/performance/ppr00-resource-<date>.samples.jsonl`` for the
  documented single command.
- The draft requires a plain urllib GET to the agent-runtime
  ``/health/ready`` on localhost; the base compose publishes that port only
  inside the container network, so the URL is overridable via
  ``--agent-runtime-health-url`` (default ``http://127.0.0.1:8094``).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request
import uuid
from collections import deque
from concurrent.futures import FIRST_COMPLETED, Future, ThreadPoolExecutor, wait
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from scripts.assistant_ttft_benchmark import (
    PprTtftAdapter,
    _configuration_fingerprint,
    _percentile,
    _runtime_inputs,
    _write_private_json_atomic,
)
from scripts.native_agent_parity_benchmark import BenchmarkError

ROOT = Path(__file__).resolve().parents[1]

PROFILE_NAME = "ppr00-stream-load-v1"
SCHEMA_VERSION = "ppr00-stream-load/v1"

# Frozen container budget (draft table): the 9 resident default-profile
# services. Names are hardcoded by design so the sampler can never silently
# widen or narrow the measured set.
CONTAINERS = (
    "ai-gateway-pg",
    "ai-gateway-redis",
    "ai-gateway-qdrant",
    "ai-gateway-backend",
    "ai-gateway-frontend",
    "ai-gateway-knowledge-service",
    "ai-gateway-knowledge-worker",
    "ai-gateway-agent-capability-worker",
    "ai-gateway-agent-runtime",
)
DERIVATION_SERVICES = (
    "ai-gateway-backend",
    "ai-gateway-agent-runtime",
    "ai-gateway-pg",
    "ai-gateway-redis",
    "ai-gateway-qdrant",
)
OWNERSHIP_LABEL = "com.docker.compose.project.working_dir"
DOCKER_COMMAND_TIMEOUT_SECONDS = 15.0

# Workload byte-identical to the TTFT baseline request (same adapter body
# builder via PprTtftAdapter/AIPlatformAdapter).
DEFAULT_PROMPT = "只回答数字：2+2等于多少？"
DEFAULT_LEVELS = "1,5,10,20"
AGENT_RUNTIME_HEALTH_URL = "http://127.0.0.1:8094/health/ready"
# The base compose only `expose`s 8094 (no host mapping), so the host URL can
# never answer; when a GET to the configured URL fails, fall back to the
# container-local probe mirroring compose's own healthcheck
# (docker-compose.yml: curl -f http://localhost:8094/health/ready).
AGENT_RUNTIME_CONTAINER = "ai-gateway-agent-runtime"
AGENT_RUNTIME_EXEC_HEALTH_URL = "http://localhost:8094/health/ready"

# Tunable timing constants (module-level so tests can monkeypatch, and every
# one is also a run_profile() keyword override).
LAUNCH_STAGGER_SECONDS = 0.5
SETTLE_GAP_SECONDS = 60.0
QUIET_SECONDS = 60.0
IDLE_BASELINE_SECONDS = 60.0
SAMPLE_INTERVAL_SECONDS = 1.0
HEALTH_PROBE_ATTEMPTS = 5
HEALTH_PROBE_INTERVAL_SECONDS = 2.0

MEM_GUARD_FRACTION = 0.90
DEFAULT_MAX_CALLS = 90
DEFAULT_WALL_CLOCK_CAP_MINUTES = 30.0

ERROR_CLASSES = (
    "success",
    "http_429_or_admission_reject",
    "http_5xx",
    "stream_error",
    "timeout",
    "infrastructure_error",
)
# A 429 from the gateway admission controller surfaces through the adapter as
# BenchmarkError("gateway_chat_http_429") (_stream_turn) or
# BenchmarkError("http_429:<endpoint>") (_request_json). Draft open question
# 2: admission rejects are RECORDED DATA — never retried, never re-provisioned.
_ADMISSION_TOKENS = ("429", "rate_limit", "rate limit", "concurrency", "admission")

# Recorded verbatim in every report (draft §Sampler and §Metrics).
CAVEATS = (
    "docker stats reports the memory working set (cgroup `memory.current` "
    "minus inactive file cache ~= anon RSS), not strict process RSS.",
    "docker stats gives container granularity only.",
    "delta/K mixes true per-stream state (SSE buffers, run state, session "
    "JSONB copies) with shared warm-up growth (caches, fragmentation, Python "
    "arenas that never return memory to the OS).",
    "post-level cooldown RSS is therefore also recorded (retained vs in-flight split).",
)

_HUMAN_SIZE_RE = re.compile(r"^\s*([0-9]+(?:\.[0-9]+)?)\s*([KMGT])?i?B\s*$", re.IGNORECASE)


# ---------------------------------------------------------------------------
# small pure helpers
# ---------------------------------------------------------------------------


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _mean(values: list[float]) -> float | None:
    return round(sum(values) / len(values), 6) if values else None


def _finite_or_none(value: Any) -> float | None:
    if (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(float(value))
    ):
        return float(value)
    return None


def _percent_to_mib(value: Any) -> float | None:
    if not isinstance(value, str):
        return None
    match = _HUMAN_SIZE_RE.match(value)
    if match is None:
        return None
    number = float(match.group(1))
    if match.group(2):
        number *= 1024.0 ** (1 + "KMGT".index(match.group(2).upper()))
    return round(number / (1024.0 * 1024.0), 6)


def _percent_or_none(value: Any) -> float | None:
    if not isinstance(value, str):
        return None
    try:
        return float(value.rstrip("%").strip())
    except ValueError:
        return None


def _parse_levels(text: str) -> list[int]:
    try:
        levels = sorted({int(part) for part in text.split(",") if part.strip()})
    except ValueError as exc:
        raise BenchmarkError("ppr00_levels_invalid") from exc
    if not levels or any(k < 1 for k in levels):
        raise BenchmarkError("ppr00_levels_invalid")
    return levels


def _level_target(level: int) -> int:
    """Per-level issued count (draft §Load shape); 3*K is an in-flight bound."""
    return max(2 * level, 10)


def _configure_loopback_proxy_bypass() -> None:
    """Same macOS-proxy loopback bypass as the TTFT baseline (urllib honours
    no_proxy/NO_PROXY; a system proxy otherwise 502s on 127.0.0.1)."""
    bypass = os.environ.get("no_proxy") or os.environ.get("NO_PROXY") or ""
    parts = [
        *[part.strip() for part in bypass.split(",") if part.strip()],
        "127.0.0.1",
        "localhost",
        "::1",
    ]
    joined = ",".join(dict.fromkeys(parts))
    os.environ["no_proxy"] = os.environ["NO_PROXY"] = joined


def _classify_stream_error(exc: BaseException) -> str:
    reason = str(exc).lower()
    if isinstance(exc, TimeoutError) or "timed out" in reason or "timeout" in reason:
        return "timeout"
    if any(token in reason for token in _ADMISSION_TOKENS):
        return "http_429_or_admission_reject"
    if "http_5" in reason:
        return "http_5xx"
    if isinstance(exc, BenchmarkError):
        return "stream_error"
    return "infrastructure_error"


def _least_squares_slope(points: list[tuple[float, float]]) -> float | None:
    if len(points) < 2:
        return None
    mean_x = sum(x for x, _ in points) / len(points)
    mean_y = sum(y for _, y in points) / len(points)
    denominator = sum((x - mean_x) ** 2 for x, _ in points)
    if denominator == 0:
        return None
    numerator = sum((x - mean_x) * (y - mean_y) for x, y in points)
    return round(numerator / denominator, 6)


def _derive_incremental_rss(
    warmed_means: dict[str, float | None],
    steady_means: dict[int, dict[str, float | None]],
) -> dict[str, Any]:
    """Draft §Metrics derivation, per service.

    ``delta_K = mean_RSS(steady window, K) - warmed_idle_RSS``;
    ``per_stream_K = delta_K / K``; plus the least-squares slope through
    (K, delta_K) over K > 0, which is robust to allocator retention.
    """
    services: dict[str, Any] = {}
    for name in DERIVATION_SERVICES:
        base = warmed_means.get(name)
        deltas: dict[str, float | None] = {}
        per_stream: dict[str, float | None] = {}
        points: list[tuple[float, float]] = []
        for level in sorted(steady_means):
            steady_mean = steady_means[level].get(name)
            if base is None or steady_mean is None:
                deltas[str(level)] = None
                per_stream[str(level)] = None
                continue
            delta = steady_mean - base
            deltas[str(level)] = round(delta, 6)
            per_stream[str(level)] = round(delta / level, 6)
            points.append((float(level), delta))
        services[name] = {
            "warmed_idle_rss_mean_mib": base,
            "delta_mib": deltas,
            "per_stream_mib": per_stream,
            "least_squares_slope_mib_per_stream": _least_squares_slope(points),
        }
    return {
        "policy": (
            "delta_K = mean_RSS(steady window, K) - warmed_idle_RSS; "
            "per_stream_K = delta_K / K; slope = least-squares line through "
            "(K, delta_K) over K > 0"
        ),
        "services": services,
        "caveats": list(CAVEATS),
    }


def _window_aggregate(
    samples: list[dict[str, Any]],
    phase: str,
    name: str,
    *,
    limit_mib: float | None,
) -> dict[str, Any]:
    """Aggregate one sampler phase window for one container."""
    rows = [
        sample["containers"][name]
        for sample in samples
        if sample.get("phase") == phase and name in sample.get("containers", {})
    ]
    mem_values = [
        value
        for value in (_finite_or_none(row.get("mem_used_mib")) for row in rows)
        if value is not None
    ]
    cpu_values = [
        value
        for value in (_finite_or_none(row.get("cpu_pct")) for row in rows)
        if value is not None
    ]
    window_start: float | None = None
    window_end: float | None = None
    for sample in samples:
        if sample.get("phase") != phase:
            continue
        value = _finite_or_none(sample.get("elapsed_seconds"))
        if value is None:
            continue
        window_start = value if window_start is None else min(window_start, value)
        window_end = value if window_end is None else max(window_end, value)
    mean_mib = _mean(mem_values)
    peak_mib = max(mem_values) if mem_values else None
    return {
        "samples": len(rows),
        "window_seconds": (
            round(window_end - window_start, 3)
            if window_start is not None and window_end is not None
            else None
        ),
        "mean_mib": mean_mib,
        "p50_mib": _percentile(mem_values, 0.50),
        "p95_mib": _percentile(mem_values, 0.95),
        "peak_mib": peak_mib,
        "pct_of_mem_limit_mean": (
            round(mean_mib / limit_mib * 100.0, 2) if mean_mib is not None and limit_mib else None
        ),
        "pct_of_mem_limit_peak": (
            round(peak_mib / limit_mib * 100.0, 2) if peak_mib is not None and limit_mib else None
        ),
        "peak_cpu_pct": max(cpu_values) if cpu_values else None,
        "net_in_mib_cumulative_mean": _mean(
            [
                value
                for value in (_finite_or_none(row.get("net_in_mib")) for row in rows)
                if value is not None
            ]
        ),
        "net_out_mib_cumulative_mean": _mean(
            [
                value
                for value in (_finite_or_none(row.get("net_out_mib")) for row in rows)
                if value is not None
            ]
        ),
    }


# ---------------------------------------------------------------------------
# read-only docker surface
# ---------------------------------------------------------------------------


def _docker_stdout(args: list[str]) -> str:
    try:
        completed = subprocess.run(  # noqa: S603 - observational Docker commands only
            ["docker", *args],
            capture_output=True,
            text=True,
            check=False,
            timeout=DOCKER_COMMAND_TIMEOUT_SECONDS,
        )
    except subprocess.TimeoutExpired as exc:
        raise BenchmarkError(f"docker_cli_timeout:{args[0] if args else 'none'}") from exc
    if completed.returncode != 0:
        raise BenchmarkError(f"docker_cli_failed:{args[0] if args else 'none'}")
    return completed.stdout


def _inspect_containers() -> dict[str, dict[str, Any]]:
    """mem_limit + RestartCount + image identity + ownership label, at once."""
    entries = json.loads(_docker_stdout(["inspect", *CONTAINERS]))
    if not isinstance(entries, list):
        raise BenchmarkError("docker_inspect_unexpected_shape")
    by_name: dict[str, dict[str, Any]] = {}
    for entry in entries:
        if not isinstance(entry, dict):
            continue
        name = str(entry.get("Name", "")).lstrip("/")
        config = entry.get("Config") if isinstance(entry.get("Config"), dict) else {}
        labels = config.get("Labels") if isinstance(config.get("Labels"), dict) else {}
        state = entry.get("State") if isinstance(entry.get("State"), dict) else {}
        host_config = entry.get("HostConfig") if isinstance(entry.get("HostConfig"), dict) else {}
        restart = entry.get("RestartCount")
        if not isinstance(restart, int):
            restart = state.get("RestartCount")
        memory = host_config.get("Memory")
        by_name[name] = {
            "container_id": entry.get("Id"),
            "image_tag": config.get("Image"),
            "image_id": entry.get("Image"),
            "created": entry.get("Created"),
            "mem_limit_bytes": memory,
            "mem_limit_mib": (
                round(float(memory) / (1024.0 * 1024.0), 4)
                if isinstance(memory, (int, float)) and memory
                else None
            ),
            "restart_count": restart if isinstance(restart, int) else None,
            "compose_working_dir": labels.get(OWNERSHIP_LABEL),
        }
    missing = sorted(set(CONTAINERS) - set(by_name))
    if missing:
        raise BenchmarkError(f"docker_inspect_missing_containers:{','.join(missing)}")
    return by_name


def _collect_stats() -> dict[str, dict[str, Any]]:
    """One `docker stats --no-stream --format json` cycle over the 9 names."""
    raw = _docker_stdout(["stats", "--no-stream", "--format", "json", *CONTAINERS])
    rows: dict[str, dict[str, Any]] = {}
    for line in raw.splitlines():
        if not line.strip():
            continue
        row = json.loads(line)
        name = str(row.get("Name", "")).lstrip("/")
        used_text, _, limit_text = str(row.get("MemUsage", "")).partition("/")
        rx_text, _, tx_text = str(row.get("NetIO", "")).partition("/")
        rows[name] = {
            "cpu_pct": _percent_or_none(row.get("CPUPerc")),
            "mem_used_mib": _percent_to_mib(used_text),
            "mem_limit_mib_stats": _percent_to_mib(limit_text),
            "mem_pct": _percent_or_none(row.get("MemPerc")),
            # docker stats NetIO is since-container-start cumulative; kept
            # as a secondary signal only (draft: "net I/O ... as secondary").
            "net_in_mib": _percent_to_mib(rx_text),
            "net_out_mib": _percent_to_mib(tx_text),
            "pids": row.get("PIDs"),
        }
    return rows


# ---------------------------------------------------------------------------
# health probes (plain urllib GET, baseline's no_proxy bypass)
# ---------------------------------------------------------------------------


def _http_get_ok(url: str, timeout: float = 5.0) -> bool:
    try:
        with urllib.request.urlopen(url, timeout=timeout) as response:
            response.read()
            return 200 <= int(response.status) < 300
    except urllib.error.HTTPError as exc:
        exc.read()
        return False
    except (OSError, ValueError):
        return False


def _wait_for_health(url: str, attempts: int, interval: float) -> bool:
    for attempt in range(attempts):
        if _http_get_ok(url):
            return True
        if attempt + 1 < attempts:
            time.sleep(interval)
    return False


def _docker_exec_health_ok() -> bool:
    try:
        _docker_stdout(
            [
                "exec",
                AGENT_RUNTIME_CONTAINER,
                "curl",
                "-fsS",
                "-o",
                "/dev/null",
                "--max-time",
                "5",
                AGENT_RUNTIME_EXEC_HEALTH_URL,
            ]
        )
        return True
    except BenchmarkError:
        return False


def _wait_for_agent_runtime_ready(url: str, attempts: int, interval: float) -> bool:
    """Host GET first; on refusal (8094 unpublished) probe container-local.

    A dead port refuses instantly, so the fallback costs no retry sleeps in
    the live default path; an operator-provided reachable mapping short-
    circuits on the GET alone. Both failing for every attempt is an honest
    infrastructure abort.
    """
    for attempt in range(attempts):
        if _http_get_ok(url) or _docker_exec_health_ok():
            return True
        if attempt + 1 < attempts:
            time.sleep(interval)
    return False


# ---------------------------------------------------------------------------
# sampler thread
# ---------------------------------------------------------------------------


class _Sampler(threading.Thread):
    """1 Hz docker-stats sampler: writes raw JSONL lines and in-memory
    phase-tagged windows, and evaluates the 90 %-of-frozen-mem_limit guard
    after every cycle."""

    def __init__(
        self,
        *,
        interval: float,
        limits_mib: dict[str, float | None],
        samples_path: Path,
        clock_start: float,
    ) -> None:
        super().__init__(daemon=True, name="ppr00-sampler")
        self.interval = interval
        self.limits_mib = limits_mib
        self.clock_start = clock_start
        self.samples: list[dict[str, Any]] = []
        self.violation: dict[str, Any] | None = None
        self.docker_error: str | None = None
        self._phase = "preflight"
        self._phase_lock = threading.Lock()
        self._exit = threading.Event()
        fd = os.open(
            samples_path,
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
            0o600,
        )
        self._file = os.fdopen(fd, "w", encoding="utf-8")

    def set_phase(self, phase: str) -> None:
        with self._phase_lock:
            self._phase = phase

    def stop(self) -> None:
        self._exit.set()
        self.join(timeout=30.0)
        self._file.close()

    def run(self) -> None:
        while not self._exit.is_set():
            cycle_started = time.monotonic()
            try:
                stats = _collect_stats()
            except (BenchmarkError, OSError, ValueError) as exc:
                self.docker_error = str(exc)
                return
            with self._phase_lock:
                phase = self._phase
            missing = sorted(set(self.limits_mib) - set(stats))
            if missing:
                self.docker_error = f"docker_stats_missing_containers:{','.join(missing)}"
                return
            record = {
                "ts": _now_iso(),
                "elapsed_seconds": round(time.monotonic() - self.clock_start, 3),
                "phase": phase,
                "containers": stats,
            }
            self.samples.append(record)
            self._file.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")
            self._file.flush()
            for name, row in stats.items():
                limit_mib = self.limits_mib.get(name)
                used = _finite_or_none(row.get("mem_used_mib"))
                if limit_mib and used is not None and used > limit_mib * MEM_GUARD_FRACTION:
                    self.violation = {
                        "container": name,
                        "mem_used_mib": used,
                        "mem_limit_mib": limit_mib,
                        "threshold_fraction": MEM_GUARD_FRACTION,
                        "phase": phase,
                        "elapsed_seconds": record["elapsed_seconds"],
                        "ts": record["ts"],
                    }
                    return
            remaining = self.interval - (time.monotonic() - cycle_started)
            if remaining > 0:
                self._exit.wait(remaining)


# ---------------------------------------------------------------------------
# driver state + stream scheduling
# ---------------------------------------------------------------------------


class _RunState:
    def __init__(self, *, max_calls: int, wall_deadline: float, clock_start: float) -> None:
        self.max_calls = max_calls
        self.wall_deadline = wall_deadline
        self.clock_start = clock_start
        self.sampler: _Sampler | None = None
        self.calls_issued = 0
        self.incomplete = False
        self.incomplete_reasons: list[str] = []
        self.interrupted = False
        self.abort_reason: str | None = None
        self.mem_guard_violation: dict[str, Any] | None = None
        self.sampler_docker_error: str | None = None
        self.runtime: dict[str, str] = {}
        self.stop_issuing = threading.Event()
        self.lock = threading.Lock()

    def mark_incomplete(self, reason: str) -> None:
        self.incomplete = True
        if reason not in self.incomplete_reasons:
            self.incomplete_reasons.append(reason)
        self.stop_issuing.set()

    def check_sampler_abort(self) -> None:
        sampler = self.sampler
        if sampler is None:
            return
        if sampler.violation is not None and self.mem_guard_violation is None:
            self.mem_guard_violation = sampler.violation
            self.abort_reason = "mem_limit_90pct"
            self.stop_issuing.set()
        if sampler.docker_error is not None and self.sampler_docker_error is None:
            self.sampler_docker_error = sampler.docker_error
            self.abort_reason = self.abort_reason or "sampler_docker_error"
            self.stop_issuing.set()

    def can_issue(self) -> bool:
        if self.stop_issuing.is_set():
            return False
        self.check_sampler_abort()
        if self.stop_issuing.is_set():
            return False
        if time.monotonic() > self.wall_deadline:
            self.mark_incomplete("wall_clock_cap_exceeded")
            return False
        with self.lock:
            if self.calls_issued >= self.max_calls:
                self.mark_incomplete("max_calls_exhausted")
                return False
        return True

    def sleep_guarded(self, seconds: float) -> None:
        """Interruptible sleep that aborts early on a sampler guard trip, on
        stop_issuing, or on Ctrl-C (partial report stays honest)."""
        deadline = time.monotonic() + seconds
        while True:
            self.check_sampler_abort()
            remaining = deadline - time.monotonic()
            if remaining <= 0 or self.stop_issuing.is_set():
                return
            try:
                time.sleep(min(0.05, remaining))
            except KeyboardInterrupt:
                self.interrupted = True
                self.abort_reason = self.abort_reason or "interrupted"
                self.stop_issuing.set()
                return


def _run_stream(
    adapter: Any,
    *,
    task_id: str,
    prompt: str,
    level: Any,
    ordinal: int,
) -> dict[str, Any]:
    """One blocking adapter stream. Never retried — an admission reject is
    recorded data (draft open question 2)."""
    try:
        adapter.start_task(task_id)
        result = adapter.run_turn(task_id, prompt)
        first_event = None
        try:
            phases = result.metadata["timing"]["phases"]
            first_event = phases[0].get("first_event_seconds") if phases else None
        except (KeyError, IndexError, TypeError, AttributeError):
            first_event = None
        succeeded = result.terminal_status == "succeeded" and bool(result.text)
        record: dict[str, Any] = {
            "level": level,
            "ordinal": ordinal,
            "classification": "success" if succeeded else "stream_error",
            "terminal_status": result.terminal_status,
            "total_seconds": round(float(result.duration_seconds), 6),
            "first_event_seconds": first_event,
        }
        if not succeeded:
            record["reason"] = f"gateway_terminal_status:{result.terminal_status}"[:300]
        return record
    except Exception as exc:  # one stream must never crash the paid run
        return {
            "level": level,
            "ordinal": ordinal,
            "classification": _classify_stream_error(exc),
            "total_seconds": None,
            "first_event_seconds": None,
            "reason": f"{type(exc).__name__}: {exc}"[:300],
        }


def _run_level(
    state: _RunState,
    adapter: Any,
    *,
    level: int,
    task_ids: deque[str],
    prompt: str,
    stagger_seconds: float,
) -> dict[str, Any]:
    """Ramp K streams staggered ``stagger_seconds`` apart, replace every
    completing stream until ``max(2K, 10)`` have been issued, then drain.
    In-flight <= K <= 3*K by construction (fixed pool of K workers)."""
    target = _level_target(level)
    records: list[dict[str, Any]] = []
    issued = 0
    max_in_flight = 0
    inflight = 0
    counters = threading.Lock()
    level_started = time.monotonic()
    steady_started_after: float | None = None
    sampler = state.sampler
    assert sampler is not None
    sampler.set_phase(f"ramp:{level}")
    with ThreadPoolExecutor(max_workers=level, thread_name_prefix=f"ppr00-k{level}") as pool:
        pending: set[Future] = set()

        def tracked(task_id: str, ordinal: int) -> dict[str, Any]:
            nonlocal inflight, max_in_flight
            with counters:
                inflight += 1
                max_in_flight = max(max_in_flight, inflight)
            try:
                return _run_stream(
                    adapter, task_id=task_id, prompt=prompt, level=level, ordinal=ordinal
                )
            finally:
                with counters:
                    inflight -= 1

        def spawn() -> None:
            nonlocal issued
            if issued >= target or not state.can_issue():
                return
            ordinal = issued + 1
            issued += 1
            with state.lock:
                state.calls_issued += 1
            pending.add(pool.submit(tracked, task_ids.popleft(), ordinal))

        initial = min(level, target)
        for position in range(initial):
            spawn()
            if position + 1 < initial and not state.stop_issuing.is_set():
                state.sleep_guarded(stagger_seconds)
        sampler.set_phase(f"steady:{level}")
        while pending:
            try:
                done, pending = wait(pending, timeout=0.25, return_when=FIRST_COMPLETED)
            except KeyboardInterrupt:
                state.interrupted = True
                state.abort_reason = state.abort_reason or "interrupted"
                state.stop_issuing.set()
                continue
            state.check_sampler_abort()
            if time.monotonic() > state.wall_deadline:
                # Drain-loop re-check: the deadline can expire between
                # spawns, not just at a can_issue() call. Marking incomplete
                # stops issuing; already-issued streams still drain (each is
                # individually bounded by its own timeouts).
                state.mark_incomplete("wall_clock_cap_exceeded")
            for future in done:
                if steady_started_after is None:
                    steady_started_after = round(time.monotonic() - level_started, 3)
                records.append(future.result())
            if state.stop_issuing.is_set():
                continue
            for _ in done:
                spawn()
    completed = sum(1 for record in records if record["classification"] == "success")
    classes = dict.fromkeys(ERROR_CLASSES, 0)
    for record in records:
        classes[record["classification"]] = classes.get(record["classification"], 0) + 1
    level_seconds = time.monotonic() - level_started
    totals = [
        value
        for value in (_finite_or_none(record.get("total_seconds")) for record in records)
        if value is not None
    ]
    first_events = [
        value
        for value in (_finite_or_none(record.get("first_event_seconds")) for record in records)
        if value is not None
    ]
    return {
        "k": level,
        "target_streams": target,
        "issued": issued,
        "returned_streams": len(records),
        "successful_streams": completed,
        "max_concurrent_in_flight": max_in_flight,
        "in_flight_hard_cap": 3 * level,
        "level_seconds": round(level_seconds, 3),
        "steady_started_after_seconds": steady_started_after,
        # Driver-side throughput (informational; this profile gates nothing).
        "completed_streams_per_second": (
            round(len(records) / level_seconds, 4) if level_seconds > 0 else None
        ),
        "error_classes": classes,
        "total_seconds": {
            "p50": _percentile(totals, 0.50),
            "p95": _percentile(totals, 0.95),
        },
        "time_to_first_event_seconds": {
            "p50": _percentile(first_events, 0.50),
            "p95": _percentile(first_events, 0.95),
        },
        "streams": records,
    }


# ---------------------------------------------------------------------------
# the profile run
# ---------------------------------------------------------------------------


def run_profile(
    *,
    env_path: Path,
    gateway_base_url: str,
    output_path: Path,
    levels: list[int],
    max_calls: int = DEFAULT_MAX_CALLS,
    wall_clock_cap_minutes: float = DEFAULT_WALL_CLOCK_CAP_MINUTES,
    agent_runtime_health_url: str = AGENT_RUNTIME_HEALTH_URL,
    stagger_seconds: float = LAUNCH_STAGGER_SECONDS,
    settle_seconds: float = SETTLE_GAP_SECONDS,
    quiet_seconds: float = QUIET_SECONDS,
    idle_baseline_seconds: float = IDLE_BASELINE_SECONDS,
    sample_interval: float = SAMPLE_INTERVAL_SECONDS,
    prompt: str = DEFAULT_PROMPT,
) -> dict[str, Any]:
    if output_path.exists():
        # Baseline history is evidence (same rule as the TTFT report).
        raise BenchmarkError("ppr00_output_exists")
    if not levels or any(k < 1 for k in levels):
        raise BenchmarkError("ppr00_levels_invalid")
    if max_calls < 1:
        raise BenchmarkError("ppr00_max_calls_invalid")
    if wall_clock_cap_minutes <= 0:
        raise BenchmarkError("ppr00_wall_clock_cap_invalid")
    # Samples ride next to the report: for the documented command (output in
    # reports/performance/) this is exactly the draft's fixed path.
    samples_path = output_path.parent / f"{output_path.stem}.samples.jsonl"
    _configure_loopback_proxy_bypass()
    clock_start = time.monotonic()
    state = _RunState(
        max_calls=max_calls,
        wall_deadline=clock_start + wall_clock_cap_minutes * 60.0,
        clock_start=clock_start,
    )
    prompt_sha256 = hashlib.sha256(prompt.encode()).hexdigest()
    containers: dict[str, dict[str, Any]] = {}
    ownership_ok: bool | None = None
    warmup_streams: list[dict[str, Any]] = []
    level_blocks: list[dict[str, Any]] = []
    restart_evidence: list[dict[str, Any]] = []
    aborted_at_level: int | None = None

    def finalize(*, completed_all_levels: bool) -> dict[str, Any]:
        sampler = state.sampler
        if sampler is not None:
            sampler.stop()
            samples = list(sampler.samples)
        else:
            samples = []
        if not samples:
            # Fail-closed: a zero-sample report can never certify a complete
            # baseline, whatever else went right.
            state.mark_incomplete("no_samples")
        state.check_sampler_abort()
        limits = {
            name: (_finite_or_none(info.get("mem_limit_mib"))) for name, info in containers.items()
        }
        warmed = {
            name: _window_aggregate(samples, "warmed_idle", name, limit_mib=limits.get(name))
            for name in CONTAINERS
        }
        warmed_means = {name: warmed[name]["mean_mib"] for name in CONTAINERS}
        steady_means_by_level: dict[int, dict[str, float | None]] = {}
        for block in level_blocks:
            phase = f"steady:{block['k']}"
            block["services"] = {
                name: _window_aggregate(samples, phase, name, limit_mib=limits.get(name))
                for name in CONTAINERS
            }
            steady_means_by_level[block["k"]] = {
                name: block["services"][name]["mean_mib"] for name in CONTAINERS
            }
            block["cooldown_services"] = {
                name: _window_aggregate(
                    samples, f"settle:{block['k']}", name, limit_mib=limits.get(name)
                )
                for name in CONTAINERS
            }
        exit_code = 0
        if (
            not completed_all_levels
            or state.incomplete
            or state.interrupted
            or state.abort_reason is not None
        ):
            exit_code = 2
        report: dict[str, Any] = {
            "schema_version": SCHEMA_VERSION,
            "profile": PROFILE_NAME,
            "generated_at": _now_iso(),
            "status": "completed" if exit_code == 0 else "aborted_or_incomplete",
            "configuration_fingerprint": {
                "git_commit": _configuration_fingerprint(),
                "git_commit_scope": (
                    "client-side operator checkout only; the running stack's identity is "
                    "pinned by the container/image identity below"
                ),
                "model_id": state.runtime.get("model_id"),
                "prompt_sha256": prompt_sha256,
                "prompt_chars": len(prompt),
                "thinking_level": "low",
                "temperature": 0.0,
                "max_tokens": 256,
                "execution_profile": "safe",
                "memory_mode": "off",
                "skills_enabled": False,
                "gateway_base_url": gateway_base_url,
            },
            "ownership_check": {
                "label": OWNERSHIP_LABEL,
                "expected_root": str(ROOT),
                "checked_containers": list(CONTAINERS),
                "observed": {
                    name: (containers.get(name) or {}).get("compose_working_dir")
                    for name in CONTAINERS
                },
                "ownership_ok": ownership_ok,
            },
            "container_identity": {
                name: {
                    key: (containers.get(name) or {}).get(key)
                    for key in (
                        "container_id",
                        "image_tag",
                        "image_id",
                        "created",
                        "mem_limit_bytes",
                        "mem_limit_mib",
                        "restart_count",
                    )
                }
                for name in CONTAINERS
            },
            "levels_requested": list(levels),
            "schedule_policy": {
                "per_level_streams": "max(2*K, 10) issued per level",
                "launch_stagger_seconds": stagger_seconds,
                "in_flight_hard_cap": "concurrent outstanding streams never exceed 3*K "
                "(fixed pool of K workers)",
                "settle_gap_seconds": settle_seconds,
                "sample_interval_seconds": sample_interval,
                "stream_mix": (
                    "byte-identical to the TTFT baseline request (same adapter, prompt, "
                    "thinking low, temperature 0.0, max_tokens 256, execution_profile safe, "
                    "memory off, skills off)"
                ),
            },
            "warmup_policy": {
                "health_checks": [
                    f"{gateway_base_url.removesuffix('/api/v1').rstrip('/')}/health",
                    agent_runtime_health_url,
                ],
                "warmup_streams": 2,
                "quiet_seconds": quiet_seconds,
                "idle_baseline_window_seconds": idle_baseline_seconds,
                "warmed_rss_definition": "mean RSS over the idle baseline sample window",
                "first_two_ordinals_excluded": (
                    "not applicable — this profile carries no latency gates; warm-up "
                    "streams are excluded from per-level error stats instead"
                ),
            },
            "warmup": {
                "streams": warmup_streams,
                "excluded_from_level_error_stats": True,
            },
            "warmed_idle": warmed,
            "levels": level_blocks,
            "derivations": _derive_incremental_rss(warmed_means, steady_means_by_level),
            "raw_samples_file": str(samples_path),
            "raw_sample_count": len(samples),
            "exit": {
                "code": exit_code,
                "completed_all_levels": completed_all_levels,
                "incomplete": state.incomplete,
                "incomplete_reasons": list(state.incomplete_reasons),
                "interrupted": state.interrupted,
                "aborted_reason": state.abort_reason,
                "aborted_at_level": aborted_at_level,
                "mem_guard_violation": state.mem_guard_violation,
                "sampler_docker_error": state.sampler_docker_error,
                "restart_evidence": restart_evidence,
                "calls_issued": state.calls_issued,
                "max_calls": max_calls,
                "wall_clock_cap_minutes": wall_clock_cap_minutes,
                "elapsed_seconds": round(time.monotonic() - clock_start, 3),
            },
        }
        _write_private_json_atomic(output_path, report)
        return report

    try:
        # 1. Ownership preflight (runtime-and-secrets §1) + frozen identity.
        try:
            containers = _inspect_containers()
        except (BenchmarkError, OSError, ValueError) as exc:
            state.abort_reason = f"preflight_docker_failed:{type(exc).__name__}"
            return finalize(completed_all_levels=False)
        expected_root = str(ROOT)
        observed_owners = {
            name: (containers.get(name) or {}).get("compose_working_dir") for name in CONTAINERS
        }
        ownership_ok = all(owner == expected_root for owner in observed_owners.values())
        if not ownership_ok:
            state.abort_reason = "ownership_mismatch"
            return finalize(completed_all_levels=False)
        missing_limits = [
            name
            for name in CONTAINERS
            if _finite_or_none((containers.get(name) or {}).get("mem_limit_mib")) is None
        ]
        if missing_limits:
            state.abort_reason = "mem_limit_missing"
            return finalize(completed_all_levels=False)
        # 2. Health-green checks (plain urllib GET, loopback bypass applied).
        gateway_health = f"{gateway_base_url.removesuffix('/api/v1').rstrip('/')}/health"
        if not _wait_for_health(
            gateway_health, HEALTH_PROBE_ATTEMPTS, HEALTH_PROBE_INTERVAL_SECONDS
        ):
            state.abort_reason = "gateway_health_check_failed"
            return finalize(completed_all_levels=False)
        if not _wait_for_agent_runtime_ready(
            agent_runtime_health_url, HEALTH_PROBE_ATTEMPTS, HEALTH_PROBE_INTERVAL_SECONDS
        ):
            state.abort_reason = "agent_runtime_health_check_failed"
            return finalize(completed_all_levels=False)
        # 3. Sampler thread runs across the whole measurement protocol.
        samples_path.parent.mkdir(parents=True, exist_ok=True)
        limits_mib = {name: info.get("mem_limit_mib") for name, info in containers.items()}
        try:
            sampler = _Sampler(
                interval=sample_interval,
                limits_mib=limits_mib,
                samples_path=samples_path,
                clock_start=clock_start,
            )
        except OSError as exc:
            state.abort_reason = f"samples_file_open_failed:{type(exc).__name__}"
            return finalize(completed_all_levels=False)
        state.sampler = sampler
        sampler.start()
        sampler.set_phase("warmup_streams")
        # 4. Runtime inputs + adapter (one login; auth via /auth/login, one
        # session per stream via POST /assistant/sessions inside start_task).
        try:
            runtime_inputs = _runtime_inputs(env_path)
            task_ids = deque(
                [f"ppr00.warmup.{n}.{uuid.uuid4().hex}" for n in (1, 2)]
                + [
                    f"ppr00.l{level}.{ordinal}.{uuid.uuid4().hex}"
                    for level in levels
                    for ordinal in range(1, _level_target(level) + 1)
                ]
            )
            adapter = PprTtftAdapter(
                gateway_base_url=gateway_base_url,
                email=runtime_inputs["email"],
                password=runtime_inputs["password"],
                model_id=runtime_inputs["model_id"],
                temperature=0.0,
                max_tokens=256,
                thinking_level="low",
                execution_profile="safe",
                max_approval_rounds=1,
                task_output_formats=dict.fromkeys(task_ids, "text"),
            )
        except (BenchmarkError, OSError, ValueError) as exc:
            state.abort_reason = "adapter_login_failed"
            warmup_streams.append(
                {
                    "level": "warmup",
                    "ordinal": 0,
                    "classification": _classify_stream_error(exc),
                    "total_seconds": None,
                    "first_event_seconds": None,
                    "reason": f"{type(exc).__name__}: {exc}"[:300],
                }
            )
            return finalize(completed_all_levels=False)
        state.runtime = {"model_id": runtime_inputs["model_id"]}
        # 5. Two sequential warm-up streams; a failure aborts (the baseline
        # protocol itself is broken). Warm-up excluded from level error stats.
        warmup_ok = True
        for ordinal in (1, 2):
            state.check_sampler_abort()
            if state.stop_issuing.is_set():
                warmup_ok = False
                break
            with state.lock:
                if state.calls_issued >= max_calls:
                    state.mark_incomplete("max_calls_exhausted")
                    warmup_ok = False
                    break
                state.calls_issued += 1
            record = _run_stream(
                adapter,
                task_id=task_ids.popleft(),
                prompt=prompt,
                level="warmup",
                ordinal=ordinal,
            )
            warmup_streams.append(record)
            if record["classification"] != "success":
                warmup_ok = False
                state.abort_reason = "warmup_stream_failed"
                break
        if not warmup_ok or state.stop_issuing.is_set():
            return finalize(completed_all_levels=False)
        # 6. Quiet gap, then the warmed-idle baseline window.
        sampler.set_phase("quiet")
        state.sleep_guarded(quiet_seconds)
        state.check_sampler_abort()
        if state.stop_issuing.is_set():
            return finalize(completed_all_levels=False)
        sampler.set_phase("warmed_idle")
        state.sleep_guarded(idle_baseline_seconds)
        state.check_sampler_abort()
        if state.stop_issuing.is_set():
            return finalize(completed_all_levels=False)
        # 7. The staircase.
        completed_all_levels = True
        for index, level in enumerate(levels):
            level_blocks.append(
                _run_level(
                    state,
                    adapter,
                    level=level,
                    task_ids=task_ids,
                    prompt=prompt,
                    stagger_seconds=stagger_seconds,
                )
            )
            # A guard/cap trip inside the level drains it and stops the
            # staircase; in-flight streams were already drained above.
            if state.stop_issuing.is_set():
                completed_all_levels = False
                break
            # Restart-count delta after each level (frozen at start).
            try:
                recheck = _inspect_containers()
            except (BenchmarkError, OSError, ValueError) as exc:
                state.abort_reason = f"post_level_inspect_failed:{type(exc).__name__}"
                completed_all_levels = False
                break
            increments = []
            recreations = []
            for name in CONTAINERS:
                before = (containers.get(name) or {}).get("restart_count")
                after = (recheck.get(name) or {}).get("restart_count")
                before_id = (containers.get(name) or {}).get("container_id")
                after_id = (recheck.get(name) or {}).get("container_id")
                if before_id != after_id:
                    recreations.append(
                        {
                            "container": name,
                            "container_id_before": before_id,
                            "container_id_after": after_id,
                        }
                    )
                if isinstance(before, int) and isinstance(after, int) and after > before:
                    increments.append(
                        {"container": name, "restart_before": before, "restart_after": after}
                    )
            if recreations or increments:
                # An honest current-topology finding, never a silent pass.
                aborted_at_level = level
                state.abort_reason = "container_recreated" if recreations else "container_restart"
                restart_evidence.extend(recreations or increments)
                completed_all_levels = False
                break
            if index + 1 < len(levels):
                # Settle gap only before the next level (draft §Load shape);
                # its window doubles as this level's post-load cooldown RSS.
                sampler.set_phase(f"settle:{level}")
                state.sleep_guarded(settle_seconds)
                state.check_sampler_abort()
                if state.stop_issuing.is_set():
                    completed_all_levels = False
                    break
        return finalize(completed_all_levels=completed_all_levels)
    except KeyboardInterrupt:
        state.interrupted = True
        state.abort_reason = state.abort_reason or "interrupted"
        state.stop_issuing.set()
        return finalize(completed_all_levels=False)


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "PPR-00 named concurrency / resource-load profile "
            f"({PROFILE_NAME}). Read-only against Docker; costs paid provider "
            "calls — requires explicit operator authorization before execution."
        )
    )
    parser.add_argument("--env-file", type=Path, required=True)
    parser.add_argument("--gateway-base-url", default="http://127.0.0.1:8080/api/v1")
    parser.add_argument("--levels", default=DEFAULT_LEVELS, help='comma list, e.g. "1,5,10,20"')
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--max-calls", type=int, default=DEFAULT_MAX_CALLS)
    parser.add_argument(
        "--wall-clock-cap-minutes", type=float, default=DEFAULT_WALL_CLOCK_CAP_MINUTES
    )
    parser.add_argument(
        "--agent-runtime-health-url",
        default=AGENT_RUNTIME_HEALTH_URL,
        help=(
            "GET probe for the agent-runtime readiness endpoint; the base "
            "compose does not publish 8094 to the host, so point this at a "
            "reachable mapping when needed"
        ),
    )
    args = parser.parse_args()
    try:
        levels = _parse_levels(args.levels)
        report = run_profile(
            env_path=args.env_file,
            gateway_base_url=args.gateway_base_url,
            output_path=args.output,
            levels=levels,
            max_calls=args.max_calls,
            wall_clock_cap_minutes=args.wall_clock_cap_minutes,
            agent_runtime_health_url=args.agent_runtime_health_url,
        )
    except BenchmarkError as exc:
        print({"status": "infrastructure_error", "reason": str(exc)})
        return 2
    exit_block = report["exit"]
    print(
        {
            "output": str(args.output),
            "exit_code": exit_block["code"],
            "completed_all_levels": exit_block["completed_all_levels"],
            "incomplete": exit_block["incomplete"],
            "aborted_reason": exit_block["aborted_reason"],
            "aborted_at_level": exit_block["aborted_at_level"],
            "calls_issued": exit_block["calls_issued"],
            "levels": [block["k"] for block in report["levels"]],
        }
    )
    return int(exit_block["code"])


if __name__ == "__main__":
    raise SystemExit(main())
