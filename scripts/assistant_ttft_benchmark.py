#!/usr/bin/env python3
"""Measure real Assistant first-event, first-thinking, and first-visible latency.

Optional PPR-00 reconciliation: pass `--timing-log-command`, a read-only
command that takes the trial-start ISO-8601 timestamp as its single trailing
argument and prints gateway log lines — e.g. a wrapper script
``read-gateway-timing.sh`` running
``docker compose logs --no-color --since "$1" gateway``. Every
single-model-call trial then joins the client-observed TTFT with the
gateway's additive server timing line
(`Agent model-plane timing schema=ppr-timing/v1`, joined on sha256(run_id)).
Reconciliation passes only when *every* gate-set trial joins cleanly;
multi-call, missing, or failing trials are recorded honestly and fail the
report — they are never silently excluded from the pass decision.
"""

from __future__ import annotations

import argparse
import hashlib
import math
import os
import random
import re
import shlex
import subprocess
import time
import uuid
from collections.abc import Callable
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from dotenv import dotenv_values

from scripts.native_agent_parity_benchmark import (
    AIPlatformAdapter,
    BenchmarkError,
    _write_private_json,
)
from src.services.agent_runtime.timing import (
    CLIENT_LOWER_BOUND_SECONDS,
    CLIENT_RESIDUAL_ABS_SECONDS,
    CLIENT_RESIDUAL_RELATIVE,
    REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS,
    client_residual_within_tolerance,
)

# G3/v2 certification bounds (see run_benchmark): the residual upper bound
# moved from a per-trial floor to p99 + a hard defect ceiling.
CERT_RESIDUAL_P99_SECONDS = 0.250
CERT_RESIDUAL_MAX_SECONDS = 0.500
# G5 (pre-declared methodology note 4): a certified baseline needs >=100
# post-warm-up gate-set trials. Client review MINOR-3: enforce that floor
# inside `recordable` so a small exploratory run can never self-certify
# (tightening only — no certified run so far had fewer).
MIN_RECORDABLE_GATE_SET_TRIALS = 100


def _write_private_json_atomic(path: Path, value: Any) -> None:
    """Evidence write that never leaves a half-written file at `path`.

    Client review MINOR-4: the shared `_write_private_json` uses O_TRUNC in
    place, so a crash mid-write leaves a truncated file at a fresh path that
    the refuse-existing guard then locks out of repair. Write to a sibling
    `.tmp` (0600) and `os.replace` — atomic on the same filesystem — so the
    final path only ever appears fully written; a stale `.tmp` is harmless
    and the run can simply be retried.
    """
    tmp = path.with_name(path.name + ".tmp")
    _write_private_json(tmp, value)
    os.replace(tmp, path)


class PprTtftAdapter(AIPlatformAdapter):
    """Adapter bound to the current stack's assistant-turn-contract/v1 stream.

    The Rust assistant cutover (commits 4968068/fe2e1b8) removed
    ``context_snapshot`` from ``run_started`` events, so the shared parity
    adapter's echo check (model_id/provider read off that snapshot) can never
    pass on today's stack — every call fails as
    ``gateway_runtime_model_mismatch``. Validation stays fail-closed against
    what the contract still emits; model identity is instead enforced per
    trial against the gateway's ``ppr-timing/v1`` receipt (see
    ``run_benchmark``), which is gateway-process truth for the model the
    provider call actually used — strictly better evidence than the old echo.
    """

    def _validate_runtime_stream(self, stream: dict[str, Any]) -> None:
        if stream["failover_decisions"]:
            raise BenchmarkError("gateway_model_failover_invalidates_comparison")


ROOT = Path(__file__).resolve().parents[1]

_SERVER_COMPONENT_KEYS = (
    "local_pre_provider_seconds",
    "provider_wait_seconds",
    "local_projection_seconds",
    "local_overhead_seconds",
    "model_plane_ttft_seconds",
)

_TIMING_LINE_RE = re.compile(
    r"Agent model-plane timing schema=ppr-timing/v1 "
    r"wire=(?P<wire>\S+) run_id=(?P<run_id>\S+) call_id=(?P<call_id>\S+) "
    r"model=(?P<model>\S+) (?P<components>.*)$"
)
_TIMING_PAIR_RE = re.compile(r"(\w+_seconds)=(-?\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)")


def _parse_model_plane_timing_line(line: str) -> dict[str, Any] | None:
    """Parse one gateway timing log line into a reconciliation record."""
    match = _TIMING_LINE_RE.search(line)
    if match is None:
        return None
    components: dict[str, float | None] = dict.fromkeys(_SERVER_COMPONENT_KEYS)
    found = _TIMING_PAIR_RE.findall(match.group("components"))
    for key, value in found:
        if key in components:
            components[key] = float(value)
    return {
        "wire": match.group("wire"),
        "run_id_sha256": hashlib.sha256(match.group("run_id").encode()).hexdigest(),
        "call_id": match.group("call_id"),
        "model": match.group("model"),
        **components,
    }


def _shell_timing_reader(command: str) -> Callable[[str], list[str]]:
    """Build a reader that runs the operator-supplied read-only log command."""

    def read(since_iso: str) -> list[str]:
        completed = subprocess.run(  # noqa: S603 - operator-declared read-only command
            [*shlex.split(command), since_iso],
            capture_output=True,
            text=True,
            check=False,
        )
        if completed.returncode != 0:
            raise BenchmarkError("timing_log_command_failed")
        return completed.stdout.splitlines()

    return read


def _run_id_sha256(lifecycle_events: list[dict[str, Any]]) -> str | None:
    for event in lifecycle_events:
        if event.get("event_type") == "run_started":
            value = event.get("run_id_sha256")
            if isinstance(value, str) and value:
                return value
    return None


def _reconcile_trial(
    reader: Callable[[str], list[str]] | None,
    since_iso: str,
    run_id_sha: str | None,
    client_ttft_seconds: float | None,
) -> dict[str, Any]:
    """Join one completed trial's client TTFT with the server-side decomposition.

    Status vocabulary (recorded in the phase-00 methodology note, 2026-08-28):
    `ok` (exactly one model call matched, server identity holds, G3 holds),
    `tolerance_exceeded`, `identity_violation` (server components fail G1's
    real-clock bound — a defect, never waived), `incomplete`,
    `multi_call_excluded`, `missing` (no matching line, or the log command
    itself failed: an infrastructure miss, never a trial failure),
    `not_collected` (no reader), `unmatched` (no run_id evidence). Only `ok`
    counts toward the pass decision. A log-command failure is contained here
    so it cannot mark an otherwise-successful trial as infrastructure error.
    """
    if reader is None:
        return {"status": "not_collected"}
    if run_id_sha is None:
        return {"status": "unmatched"}
    seen: dict[str, dict[str, Any]] = {}

    def harvest() -> None:
        try:
            lines = reader(since_iso)
        except (BenchmarkError, OSError):
            # A failing or missing wrapper is an infrastructure miss, never a
            # trial failure and never a crash of the whole benchmark.
            return
        for parsed in map(_parse_model_plane_timing_line, lines):
            if parsed is not None and parsed["run_id_sha256"] == run_id_sha:
                seen[parsed["call_id"]] = parsed

    for _ in range(2):
        harvest()
        if seen:
            break
        time.sleep(0.5)
    if not seen:
        return {"status": "missing"}
    # The timing line is emitted after the call completes, so it can land in
    # the log a beat after the client sees the terminal event; take one more
    # delayed read so a second model call's line has a chance to race in
    # *before* we declare a single match. This bound is ~0.5 s, NOT absolute:
    # two lines of one trial are written server-side before `run_finished`,
    # so detection only fails if the log pipeline lags >0.5 s, and the raw
    # receipts permit post-hoc re-audit. Duplicate reads of one call_id are
    # deduped by call_id, so this is safe to repeat.
    time.sleep(0.5)
    harvest()
    if len(seen) > 1:
        return {"status": "multi_call_excluded", "call_count": len(seen)}
    record = next(iter(seen.values()))
    server_ttft = record["model_plane_ttft_seconds"]
    if server_ttft is None or client_ttft_seconds is None:
        return {"status": "incomplete", "server": record}
    parts = (
        record["local_pre_provider_seconds"],
        record["provider_wait_seconds"],
        record["local_projection_seconds"],
    )
    if any(value is None for value in parts):
        return {"status": "incomplete", "server": record}
    identity_residual = abs(sum(float(v) for v in parts) - server_ttft)
    if identity_residual > REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS:
        return {
            "status": "identity_violation",
            "server": record,
            "identity_residual_seconds": round(identity_residual, 9),
            "identity_tolerance_seconds": REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS,
        }
    residual = client_ttft_seconds - server_ttft
    within = client_residual_within_tolerance(server_ttft, client_ttft_seconds)
    return {
        "status": "ok" if within else "tolerance_exceeded",
        "server": record,
        "residual_seconds": round(residual, 6),
        "identity_residual_seconds": round(identity_residual, 9),
        "tolerance_seconds": round(
            max(CLIENT_RESIDUAL_ABS_SECONDS, CLIENT_RESIDUAL_RELATIVE * client_ttft_seconds), 6
        ),
        "lower_bound_seconds": CLIENT_LOWER_BOUND_SECONDS,
    }


def _percentile(values: list[float], percentile: float) -> float | None:
    """Return the nearest-rank percentile used by the release report."""
    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return round(ordered[rank - 1], 6)


def _iqr(values: list[float]) -> float | None:
    p75 = _percentile(values, 0.75)
    p25 = _percentile(values, 0.25)
    if p75 is None or p25 is None:
        return None
    return round(p75 - p25, 6)


def _bootstrap_ci(
    values: list[float],
    percentile: float,
    *,
    iterations: int = 2000,
    seed: int = 0,
) -> dict[str, float | None]:
    """Deterministic percentile-bootstrap 95 % CI for a nearest-rank quantile."""
    if not values:
        return {"low": None, "high": None}
    rng = random.Random(seed)
    size = len(values)
    draws = sorted(
        _percentile(rng.choices(values, k=size), percentile) or 0.0 for _ in range(iterations)
    )
    return {
        "low": draws[min(iterations - 1, math.floor(0.025 * iterations))],
        "high": draws[min(iterations - 1, math.ceil(0.975 * iterations) - 1)],
    }


def _configuration_fingerprint() -> str:
    completed = subprocess.run(  # noqa: S603 - read-only identity lookup
        ["git", "rev-parse", "HEAD"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )
    commit = completed.stdout.strip() if completed.returncode == 0 else ""
    return commit or "unknown"


def _runtime_inputs(env_path: Path) -> dict[str, str]:
    file_env = dotenv_values(env_path) if env_path.exists() else {}

    def value(*names: str) -> str:
        for name in names:
            candidate = os.environ.get(name) or file_env.get(name)
            if isinstance(candidate, str) and candidate:
                return candidate
        return ""

    email = value("ASSISTANT_TTFT_EMAIL", "ASSISTANT_ISOLATION_EMAIL")
    password = value("ASSISTANT_TTFT_PASSWORD", "ASSISTANT_ISOLATION_PASSWORD")
    if not email or not password:
        # Same documented local-bootstrap account as
        # native_agent_parity_benchmark._load_runtime_inputs: the stack's
        # admin@<AUTH_ALLOWED_EMAIL_DOMAIN> with DEFAULT_USER_PASSWORD.
        domain = value("AUTH_ALLOWED_EMAIL_DOMAIN") or "example.com"
        default_password = value("DEFAULT_USER_PASSWORD")
        if default_password:
            email = email or f"admin@{domain}"
            password = password or default_password
    model_id = value("ASSISTANT_TTFT_MODEL", "ASSISTANT_ISOLATION_MODEL") or "qwen3.7-plus"
    if not email or not password:
        raise BenchmarkError("ttft_credentials_missing")
    return {"email": email, "password": password, "model_id": model_id}


def _metric_summary(trials: list[dict[str, Any]], key: str) -> dict[str, float | None]:
    values = [float(trial[key]) for trial in trials if isinstance(trial.get(key), (int, float))]
    return {
        "p50": _percentile(values, 0.50),
        "p95": _percentile(values, 0.95),
        "min": round(min(values), 6) if values else None,
        "max": round(max(values), 6) if values else None,
    }


def _metric_summary_nested(
    trials: list[dict[str, Any]],
    section: str,
    key: str,
) -> dict[str, float | None]:
    values = [
        float(trial[section][key])
        for trial in trials
        if isinstance(trial.get(section), dict)
        and isinstance(trial[section].get(key), (int, float))
    ]
    return {
        "p50": _percentile(values, 0.50),
        "p95": _percentile(values, 0.95),
        "min": round(min(values), 6) if values else None,
        "max": round(max(values), 6) if values else None,
    }


def run_benchmark(
    *,
    env_path: Path,
    gateway_base_url: str,
    output_path: Path,
    prompt: str,
    trials: int,
    thinking_level: str,
    p50_ceiling_seconds: float,
    p95_ceiling_seconds: float,
    timing_line_reader: Callable[[str], list[str]] | None = None,
    warmup_trials: int = 2,
    min_recordable_gate_set: int = MIN_RECORDABLE_GATE_SET_TRIALS,
) -> dict[str, Any]:
    if not 1 <= trials <= 200:
        raise BenchmarkError("ttft_trials_out_of_range")
    if not 0 <= warmup_trials < trials:
        raise BenchmarkError("ttft_warmup_out_of_range")
    if thinking_level not in {"low", "medium", "high"}:
        raise BenchmarkError("ttft_thinking_must_be_enabled")
    # Refuse to clobber an existing report (security review A2): the baseline
    # history is evidence; a rerun must write a new path (e.g. run2.json).
    if output_path.exists():
        raise BenchmarkError("ttft_output_exists")
    # This benchmark only ever talks to the local gateway. A macOS system
    # proxy (or *_proxy env) otherwise intercepts loopback and returns 502;
    # urllib honours no_proxy/NO_PROXY for the bypass.
    bypass = os.environ.get("no_proxy") or os.environ.get("NO_PROXY") or ""
    parts = [
        *[part.strip() for part in bypass.split(",") if part.strip()],
        "127.0.0.1",
        "localhost",
        "::1",
    ]
    joined = ",".join(dict.fromkeys(parts))
    os.environ["no_proxy"] = os.environ["NO_PROXY"] = joined
    runtime = _runtime_inputs(env_path)
    # The adapter indexes task_output_formats strictly per task_id; the TTFT
    # task ids are dynamic, so mint them up front and bind each to "text".
    task_ids = {ordinal: f"ttft.{ordinal}.{uuid.uuid4().hex}" for ordinal in range(1, trials + 1)}
    adapter = PprTtftAdapter(
        gateway_base_url=gateway_base_url,
        email=runtime["email"],
        password=runtime["password"],
        model_id=runtime["model_id"],
        temperature=0.0,
        max_tokens=256,
        thinking_level=thinking_level,
        execution_profile="safe",
        max_approval_rounds=1,
        task_output_formats=dict.fromkeys(task_ids.values(), "text"),
    )
    prompt_sha256 = hashlib.sha256(prompt.encode()).hexdigest()
    results: list[dict[str, Any]] = []
    started_at = time.time()
    for ordinal in range(1, trials + 1):
        task_id = task_ids[ordinal]
        trial_started_iso = datetime.now(timezone.utc).isoformat()
        try:
            adapter.start_task(task_id)
            result = adapter.run_turn(task_id, prompt)
            phase = result.metadata["timing"]["phases"][0]
            event_types = result.metadata["event_types"]
            thinking_observed = any(
                event_type in {"thinking_start", "thinking_delta"} for event_type in event_types
            )
            success = (
                result.terminal_status == "succeeded" and bool(result.text) and thinking_observed
            )
            usage = result.metadata.get("usage") or {}
            client_ttft = result.metadata["timing"].get("ttft_seconds")
            trial: dict[str, Any] = {
                "ordinal": ordinal,
                "success": success,
                "terminal_status": result.terminal_status,
                "thinking_observed": thinking_observed,
                "first_event_seconds": phase.get("first_event_seconds"),
                "first_thinking_seconds": phase.get("first_thinking_seconds"),
                "ttft_seconds": client_ttft,
                "first_text_seconds": result.metadata["timing"].get("first_text_seconds"),
                "thinking_to_text_seconds": phase.get("thinking_to_text_seconds"),
                "total_seconds": result.duration_seconds,
                "input_tokens": usage.get("input_tokens"),
                "cached_input_tokens": usage.get("cached_input_tokens"),
                "output_tokens": usage.get("output_tokens"),
            }
            if success:
                # G3 reconciliation is declared for completed trials only.
                trial["timing_reconciliation"] = _reconcile_trial(
                    timing_line_reader,
                    trial_started_iso,
                    _run_id_sha256(result.metadata.get("lifecycle_events") or []),
                    client_ttft,
                )
                # Model identity is enforced here (PprTtftAdapter docstring):
                # the gateway receipt's model must be the requested one. A
                # mismatch fails the trial closed — same semantics as the
                # pre-cutover adapter echo — so it can never enter a gate or
                # a baseline set.
                receipt_model = (trial["timing_reconciliation"].get("server") or {}).get("model")
                if receipt_model is not None and receipt_model != runtime["model_id"]:
                    trial["success"] = False
                    trial["terminal_status"] = "infrastructure_error"
                    trial["reason"] = "gateway_runtime_model_mismatch"
            results.append(trial)
        except (BenchmarkError, IndexError, KeyError, TypeError) as exc:
            # Client review MINOR-5: a malformed adapter receipt (missing
            # timing/phases keys) must fail THIS trial, never crash a paid
            # run mid-way with no output. No reconciliation is recorded, so
            # the run can never certify (collected fails).
            reason = str(exc) if isinstance(exc, BenchmarkError) else f"{type(exc).__name__}: {exc}"
            results.append(
                {
                    "ordinal": ordinal,
                    "success": False,
                    "terminal_status": "infrastructure_error",
                    "reason": reason,
                }
            )
    successful = [trial for trial in results if trial.get("success") is True]
    # Pre-declared statistical policy: the first `warmup_trials` ordinals are
    # warm-up; gates and summaries run on the remaining successful trials only.
    gate_set = [trial for trial in successful if int(trial["ordinal"]) > warmup_trials]
    metrics = {
        key: _metric_summary(gate_set, key)
        for key in (
            "first_event_seconds",
            "first_thinking_seconds",
            "ttft_seconds",
            "first_text_seconds",
            "thinking_to_text_seconds",
            "total_seconds",
        )
    }
    ttft_values = [
        float(trial["ttft_seconds"])
        for trial in gate_set
        if isinstance(trial.get("ttft_seconds"), (int, float))
    ]
    reconciled = [
        trial["timing_reconciliation"]
        for trial in gate_set
        if isinstance(trial.get("timing_reconciliation"), dict)
        and trial["timing_reconciliation"].get("status") in {"ok", "tolerance_exceeded"}
    ]
    for key in _SERVER_COMPONENT_KEYS:
        metrics[key] = _metric_summary_nested(reconciled, "server", key)
    residuals = [
        abs(float(record["residual_seconds"]))
        for record in reconciled
        if "residual_seconds" in record
    ]
    reconciliation_counts = {
        status: sum(
            1
            for trial in gate_set
            if isinstance(trial.get("timing_reconciliation"), dict)
            and trial["timing_reconciliation"].get("status") == status
        )
        for status in (
            "ok",
            "tolerance_exceeded",
            "identity_violation",
            "multi_call_excluded",
            "missing",
            "incomplete",
            "unmatched",
            "not_collected",
        )
    }
    ttft_p50 = metrics["ttft_seconds"]["p50"]
    ttft_p95 = metrics["ttft_seconds"]["p95"]
    passed = (
        len(successful) == trials
        and len(gate_set) == trials - warmup_trials
        and ttft_p50 is not None
        and ttft_p95 is not None
        and ttft_p50 <= p50_ceiling_seconds
        and ttft_p95 <= p95_ceiling_seconds
    )
    # G3/v2 certification (user-authorized 2026-08-28, methodology review F2
    # and the run1/run2 evidence): the per-trial 0.200 s residual floor was a
    # ~97th-percentile cut through a stable client<->server inventory, so a
    # "100 % of trials individually ok" rule could not certify a baseline.
    # v2 keeps everything deterministic and structural per trial — every
    # trial must have a collected, identity-clean reconciliation, no
    # exclusions, and the client lower bound (client >= server - 0.010) holds
    # for every trial — and moves only the residual *upper* bound to a
    # quantile: p99 of |residual| <= 0.250 over all trials (warm-ups
    # included), with a hard defect ceiling |residual| <= 0.500 for every
    # trial (a tightening beyond what p99 alone rejects, so a single gross
    # outlier is still structural, not statistical). The per-trial
    # `tolerance_exceeded` status is kept as informational detail.
    reconciliation_passed: bool | None = None
    abs_residuals: list[float] = []
    if timing_line_reader is not None:
        records = [
            trial["timing_reconciliation"]
            for trial in results
            if isinstance(trial.get("timing_reconciliation"), dict)
        ]
        collected = len(records) == trials and all(
            record.get("status") in {"ok", "tolerance_exceeded"} for record in records
        )
        trial_residuals = [record.get("residual_seconds") for record in records]
        lower_bounds_ok = all(
            isinstance(value, (int, float)) and value >= -CLIENT_LOWER_BOUND_SECONDS
            for value in trial_residuals
        )
        abs_residuals = [
            abs(float(value)) for value in trial_residuals if isinstance(value, (int, float))
        ]
        p99_ok = bool(abs_residuals) and (
            (_percentile(abs_residuals, 0.99) or 1.0) <= CERT_RESIDUAL_P99_SECONDS
        )
        ceiling_ok = bool(abs_residuals) and max(abs_residuals) <= CERT_RESIDUAL_MAX_SECONDS
        reconciliation_passed = collected and lower_bounds_ok and p99_ok and ceiling_ok
    # G5 pre-declared sample floor (client review MINOR-3): certification
    # requires >=100 post-warm-up gate trials, enforced here rather than by
    # operator discipline, so an exploratory N=1 run can never self-certify.
    gate_size_ok = len(gate_set) >= min_recordable_gate_set
    recordable = passed and reconciliation_passed is True and gate_size_ok
    summary = {
        "schema_version": "assistant-ttft-benchmark/v3",
        "ttft_definition": "first_user_visible_thinking_or_text_token",
        "model_id": runtime["model_id"],
        "thinking_level": thinking_level,
        "thinking_required": True,
        "execution_profile": "safe",
        "memory_mode": "off",
        "skills_enabled": False,
        "prompt_sha256": prompt_sha256,
        "prompt_chars": len(prompt),
        "trial_count": trials,
        "successful_trials": len(successful),
        "gate_trials": len(gate_set),
        "failure_count": trials - len(successful),
        "p50_ceiling_seconds": p50_ceiling_seconds,
        "p95_ceiling_seconds": p95_ceiling_seconds,
        "passed": passed,
        "recordable": recordable,
        "statistical_policy": {
            "warmup_trials": warmup_trials,
            "min_recordable_gate_set_trials": min_recordable_gate_set,
            "percentile_method": "nearest-rank",
            "ttft_iqr_seconds": _iqr(ttft_values),
            "ttft_p50_ci95_bootstrap": _bootstrap_ci(ttft_values, 0.50),
            "ttft_p95_ci95_bootstrap": _bootstrap_ci(ttft_values, 0.95),
            "bootstrap_iterations": 2000,
            "bootstrap_seed": 0,
            "client_clock_source": "time.monotonic (adapter process)",
            "server_clock_source": "time.perf_counter (gateway process, single worker)",
        },
        "configuration_fingerprint": {
            "git_commit": _configuration_fingerprint(),
            "git_commit_scope": (
                "client-side operator checkout only; the running gateway "
                "stack's identity is pinned separately by "
                "deploy/runbooks/platform-plane-restructure/rollback-bundle.json "
                "and the hot-update provenance recorded in loop-state evidence"
            ),
            "model_id": runtime["model_id"],
            "thinking_level": thinking_level,
            "temperature": 0.0,
            "max_tokens": 256,
            "prompt_sha256": prompt_sha256,
        },
        "metrics": metrics,
        "timing_reconciliation": {
            "policy": {
                "server_timing_schema": "ppr-timing/v1",
                "join": "sha256(run_id) against client run_started",
                "pass_rule": (
                    "G3/v2 (user-authorized 2026-08-28): every trial needs a "
                    "collected identity-clean reconciliation (exclusions fail "
                    "the report); the client lower bound holds per trial; "
                    "the residual upper bound is certified at p99 <= "
                    f"{CERT_RESIDUAL_P99_SECONDS} with a hard per-trial "
                    f"defect ceiling <= {CERT_RESIDUAL_MAX_SECONDS}; v1's "
                    "per-trial 0.200 floor stays as informational status; "
                    f"and recordable additionally requires a gate-set of at "
                    f"least {MIN_RECORDABLE_GATE_SET_TRIALS} trials (G5)"
                ),
                "identity_tolerance_seconds": REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS,
                "client_lower_bound_seconds": CLIENT_LOWER_BOUND_SECONDS,
                "residual_abs_seconds": CLIENT_RESIDUAL_ABS_SECONDS,
                "residual_relative": CLIENT_RESIDUAL_RELATIVE,
                "attribution_caveat": (
                    "provider token pacing after the first upstream frame is "
                    "attributed to local_projection_seconds by definition; "
                    "provider_wait is comparable only at single-stream load "
                    "(downstream backpressure stalls the generator inside the "
                    "provider-wait window)"
                ),
                "residual_inventory": (
                    "client<->gateway transport, gateway pre-stream auth/"
                    "session/run-creation, the agent-runtime container hop "
                    "including Rust-side event processing and persistence, "
                    "and client parse time"
                ),
                "survivorship": (
                    "timing lines exist only for completed model calls; "
                    "failed or aborted calls never enter the component "
                    "distributions (the gate set also requires all-trial "
                    "success)"
                ),
            },
            "field_scope": (
                "status_counts and gate_set_max_abs_residual_seconds cover the "
                "gate-set only (warm-ups excluded); the all_trials_* fields "
                "are the G3/v2 certification inputs and include every trial"
            ),
            "status_counts": reconciliation_counts,
            "gate_set_max_abs_residual_seconds": (round(max(residuals), 6) if residuals else None),
            "gate_set_meets_recordable_min": gate_size_ok,
            "all_trials_abs_residual_p99_seconds": (
                _percentile(abs_residuals, 0.99) if abs_residuals else None
            ),
            "all_trials_abs_residual_max_seconds": (
                round(max(abs_residuals), 6) if abs_residuals else None
            ),
            "certification_bounds": {
                "rule": "g3/v2",
                "residual_p99_seconds": CERT_RESIDUAL_P99_SECONDS,
                "residual_max_seconds": CERT_RESIDUAL_MAX_SECONDS,
                "client_lower_bound_seconds": CLIENT_LOWER_BOUND_SECONDS,
            },
            "reconciliation_passed": reconciliation_passed,
        },
        "trials": results,
        "wall_seconds": round(time.time() - started_at, 6),
    }
    _write_private_json_atomic(output_path, summary)
    return summary


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env-file", type=Path, required=True)
    parser.add_argument("--gateway-base-url", default="http://127.0.0.1:8080/api/v1")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prompt", default="只回答数字：2+2等于多少？")
    parser.add_argument("--trials", type=int, default=10)
    parser.add_argument("--thinking-level", default="low")
    parser.add_argument("--p50-ceiling-seconds", type=float, default=4.0)
    parser.add_argument("--p95-ceiling-seconds", type=float, default=5.0)
    parser.add_argument("--warmup-trials", type=int, default=2)
    parser.add_argument(
        "--timing-log-command",
        default="",
        help=(
            "read-only command that takes the trial-start ISO-8601 timestamp "
            "as its single trailing argument and prints gateway log lines; "
            "e.g. a wrapper script: docker compose logs --no-color "
            '--since "$1" gateway'
        ),
    )
    args = parser.parse_args()
    try:
        summary = run_benchmark(
            env_path=args.env_file,
            gateway_base_url=args.gateway_base_url,
            output_path=args.output,
            prompt=args.prompt,
            trials=args.trials,
            thinking_level=args.thinking_level,
            p50_ceiling_seconds=args.p50_ceiling_seconds,
            p95_ceiling_seconds=args.p95_ceiling_seconds,
            timing_line_reader=(
                _shell_timing_reader(args.timing_log_command) if args.timing_log_command else None
            ),
            warmup_trials=args.warmup_trials,
        )
    except (BenchmarkError, FileExistsError) as exc:
        print({"status": "infrastructure_error", "reason": str(exc)})
        return 2
    reconciliation = summary["timing_reconciliation"]
    print(
        {
            "passed": summary["passed"],
            "recordable": summary["recordable"],
            "successful_trials": summary["successful_trials"],
            "ttft": summary["metrics"]["ttft_seconds"],
            "local_overhead_p95": summary["metrics"]["local_overhead_seconds"]["p95"],
            "provider_wait_p95": summary["metrics"]["provider_wait_seconds"]["p95"],
            "reconciliation_passed": reconciliation["reconciliation_passed"],
            "reconciliation_statuses": reconciliation["status_counts"],
        }
    )
    # A baseline is only recordable when the ceilings pass AND reconciliation
    # is present and fully clean; a reader-less run can never certify one
    # (delta review finding 1: the old `is not False` let receipt-less runs
    # exit 0 with zero model verification).
    return 0 if summary["recordable"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
