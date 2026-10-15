from __future__ import annotations

import hashlib
import json
import math
import random
import statistics
import uuid
from contextlib import contextmanager
from contextvars import ContextVar
from datetime import datetime, timezone
from typing import Any

from ai_gateway_core.billing import build_pricing_snapshot
from ai_gateway_core.eval.dataset_manifest import build_eval_dataset_manifest

from .base import BaseRepository
from .runtime_trace_dimensions import bind_runtime_trace_dimensions

EXAMPLE_METADATA_PATCH_KEYS = (
    "expected_trajectory",
    "assertions",
    "tags",
    "difficulty",
    "owner",
    "review_status",
)


class EvalCaseRevisionConflict(ValueError):
    def __init__(self, current_revision: int):
        self.current_revision = current_revision
        super().__init__(f"Eval case revision changed; current revision is {current_revision}")


def _kb_case_revision(example: dict[str, Any]) -> int:
    metadata = example.get("metadata") if isinstance(example.get("metadata"), dict) else {}
    value = metadata.get("case_revision")
    return value if type(value) is int and value > 0 else 1


def _latest_kb_case_revision(examples: list[dict[str, Any]]) -> dict[str, Any] | None:
    if not examples:
        return None
    return max(
        examples,
        key=lambda example: (
            _kb_case_revision(example),
            example.get("created_at") or datetime.min.replace(tzinfo=timezone.utc),
            str(example.get("example_id") or ""),
        ),
    )
EVAL_GATE_METRICS_SCHEMA_VERSION = "eval-gate-metrics/v2"
EVAL_GATE_METRICS_REQUIRED_FIELDS = frozenset(
    {
        "case_count",
        "critical_case_count",
        "critical_failed_count",
        "critical_pass_rate",
        "failed_case_count",
        "overall_score",
        "pass_rate",
        "score_sum",
        "stateful_case_count",
        "stateful_failed_count",
        "stateful_pass_rate",
        "trajectory_case_count",
        "trajectory_failed_count",
        "trajectory_pass_rate",
    }
)
EVAL_GATE_RATE_ABS_TOLERANCE = 0.00005


def _canonical_hash(value: Any) -> str:
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def _percentile(values: list[float], percentile: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    position = (len(ordered) - 1) * percentile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def _paired_bootstrap_ci(deltas: list[float], *, samples: int = 10_000) -> list[float] | None:
    if not deltas:
        return None
    rng = random.Random(42)
    count = len(deltas)
    means = [
        sum(deltas[rng.randrange(count)] for _ in range(count)) / count for _ in range(samples)
    ]
    low = _percentile(means, 0.025)
    high = _percentile(means, 0.975)
    if low is None or high is None:
        return None
    return [round(low, 4), round(high, 4)]


def _known_number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    number = float(value)
    return number if math.isfinite(number) else None


def _has_versioned_gate_metrics(value: Any) -> bool:
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != EVAL_GATE_METRICS_SCHEMA_VERSION
        or not EVAL_GATE_METRICS_REQUIRED_FIELDS.issubset(value)
    ):
        return False
    count_fields = (
        "case_count",
        "failed_case_count",
        "trajectory_case_count",
        "trajectory_failed_count",
        "critical_case_count",
        "critical_failed_count",
        "stateful_case_count",
        "stateful_failed_count",
    )
    counts: dict[str, int] = {}
    for field in count_fields:
        raw = value.get(field)
        if isinstance(raw, bool) or not isinstance(raw, int) or raw < 0:
            return False
        counts[field] = raw
    case_count = counts["case_count"]
    if (
        case_count <= 0
        or counts["failed_case_count"] > case_count
        or counts["trajectory_case_count"] != case_count
        or counts["trajectory_failed_count"] > counts["trajectory_case_count"]
        or counts["critical_case_count"] > case_count
        or counts["critical_failed_count"] > counts["critical_case_count"]
        or counts["stateful_case_count"] > case_count
        or counts["stateful_failed_count"] > counts["stateful_case_count"]
    ):
        return False

    score_sum = _known_number(value.get("score_sum"))
    overall_score = _known_number(value.get("overall_score"))
    if (
        score_sum is None
        or overall_score is None
        or not 0.0 <= score_sum <= float(case_count)
        or not 0.0 <= overall_score <= 1.0
        or not math.isclose(
            overall_score,
            score_sum / case_count,
            rel_tol=0.0,
            abs_tol=EVAL_GATE_RATE_ABS_TOLERANCE,
        )
    ):
        return False

    def rate_matches(rate_field: str, count_field: str, failed_field: str) -> bool:
        count = counts[count_field]
        failed = counts[failed_field]
        raw_rate = value.get(rate_field)
        if count == 0:
            return raw_rate is None
        rate = _known_number(raw_rate)
        return (
            rate is not None
            and 0.0 <= rate <= 1.0
            and math.isclose(
                rate,
                (count - failed) / count,
                rel_tol=0.0,
                abs_tol=EVAL_GATE_RATE_ABS_TOLERANCE,
            )
        )

    return all(
        (
            rate_matches("pass_rate", "case_count", "failed_case_count"),
            rate_matches(
                "trajectory_pass_rate",
                "trajectory_case_count",
                "trajectory_failed_count",
            ),
            rate_matches(
                "critical_pass_rate",
                "critical_case_count",
                "critical_failed_count",
            ),
            rate_matches(
                "stateful_pass_rate",
                "stateful_case_count",
                "stateful_failed_count",
            ),
        )
    )


def _average(values: list[float]) -> float | None:
    return round(sum(values) / len(values), 4) if values else None


def _average_complete_metric(rows: list[dict[str, Any]], key: str) -> float | None:
    values = [_known_number(row.get(key)) for row in rows]
    if not values or any(value is None for value in values):
        return None
    return _average([value for value in values if value is not None])


def _aggregate_live_case_rows(rows: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    grouped: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        grouped.setdefault(str(row.get("case_id") or ""), []).append(row)

    aggregated: dict[str, dict[str, Any]] = {}
    for case_id, trials in grouped.items():
        trial_statuses = [str(row.get("status") or "queued") for row in trials]
        observed = [
            row.get("observed_metrics") if isinstance(row.get("observed_metrics"), dict) else {}
            for row in trials
        ]
        scores = [
            value
            for item in observed
            if (value := _known_number(item.get("aggregate_score"))) is not None
        ]
        behavior_labels = [
            item.get("behavior_pass")
            for item in observed
            if isinstance(item.get("behavior_pass"), bool)
        ]
        execution_labels = [item.get("execution_succeeded") is True for item in observed]
        # A judge may fail after the candidate has completed. Keep candidate
        # execution separate from score availability in the public case row.
        execution_complete = bool(trials) and all(execution_labels)
        quality_complete = (
            execution_complete
            and all(status == "succeeded" for status in trial_statuses)
            and len(behavior_labels) == len(trials)
        )
        if any(
            status == "failed" and not executed
            for status, executed in zip(trial_statuses, execution_labels, strict=True)
        ):
            execution_status = "failed"
        elif any(status == "skipped" for status in trial_statuses):
            execution_status = "cancelled" if any(
                item.get("execution_outcome") == "cancelled" for item in observed
            ) else "skipped"
        elif any(status == "running" for status in trial_statuses):
            execution_status = "running"
        elif any(status == "queued" for status in trial_statuses):
            execution_status = "queued"
        else:
            execution_status = "succeeded" if execution_complete else "failed"
        representative = next(
            (item for item in observed if item.get("trace_id")),
            observed[0] if observed else {},
        )
        representative_row = next(
            (
                row
                for row in trials
                if str(row.get("candidate_trace_id") or "")
                == str(representative.get("trace_id") or "")
            ),
            trials[0],
        )
        status = "unscored"
        if quality_complete:
            status = "passed" if all(behavior_labels) else "failed"
        aggregated[case_id] = {
            "case_id": case_id,
            "example_id": trials[0].get("example_id"),
            "input": trials[0].get("input") or {},
            "expected_output": trials[0].get("expected_output") or {},
            "assertions": trials[0].get("assertions") or [],
            "metadata": trials[0].get("metadata") or {},
            "candidate_trace_id": representative.get("trace_id"),
            "trace_ids": [str(item.get("trace_id")) for item in observed if item.get("trace_id")],
            "status": status,
            "execution_status": execution_status,
            "behavior_pass": all(behavior_labels) if quality_complete else None,
            "execution_succeeded": execution_complete,
            "critical": bool((trials[0].get("metadata") or {}).get("critical")),
            "aggregate_score": _average(scores) if quality_complete and len(scores) == len(trials) else None,
            "score_stddev": (
                round(statistics.pstdev(scores), 4) if quality_complete and len(scores) > 1
                else 0.0 if quality_complete and scores else None
            ),
            "flaky": quality_complete and len(set(behavior_labels)) > 1,
            "trial_count": len(trials),
            "observed_metrics": {
                key: _average_complete_metric(observed, key)
                for key in (
                    "latency_ms",
                    "input_tokens",
                    "output_tokens",
                    "total_tokens",
                    "cost_cents",
                )
            },
            "output_preview": representative.get("output_preview") or "",
            "trace": {
                "trace_family": representative_row.get("trace_family") or "assistant",
                "status": representative_row.get("trace_status"),
                "model_id": representative_row.get("model_id"),
                "provider": representative_row.get("provider"),
                "total_latency_ms": representative.get("latency_ms"),
                "total_tokens": representative.get("total_tokens"),
                "output_preview": representative.get("output_preview") or "",
            },
            "tool_trajectory": representative.get("tool_trajectory") or [],
            "rag_evidence": representative.get("rag_evidence") or [],
            "exit_reason": representative.get("exit_reason"),
            "contract_failures": sorted(
                {
                    str(failure)
                    for item in observed
                    for failure in item.get("contract_failures") or []
                }
            ),
            "errors": sorted({str(item.get("error")) for item in observed if item.get("error")}),
        }
    return aggregated


def _retry_source_cases(
    rows: list[dict[str, Any]], case_ids: list[str],
) -> list[dict[str, Any]]:
    """Select one failed frozen attempt per case without replaying uncertain dispatches."""
    if not case_ids or len(case_ids) != len(set(case_ids)) or any(not item.strip() for item in case_ids):
        raise ValueError("eval_retry_case_ids_invalid")
    by_case: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        by_case.setdefault(str(row.get("case_id") or ""), []).append(row)
    selected: list[dict[str, Any]] = []
    for case_id in case_ids:
        if case_id not in by_case:
            raise ValueError(f"eval_retry_case_not_found:{case_id}")
        eligible = [
            row for row in by_case[case_id]
            if row.get("status") == "failed"
            or (row.get("status") == "succeeded"
                and (row.get("observed_metrics") or {}).get("behavior_pass") is False)
        ]
        if not eligible:
            raise ValueError(f"eval_retry_case_not_failed:{case_id}")
        if any(
            str((row.get("metadata") or {}).get("source_kind") or "").startswith(("kb", "knowledge"))
            or any(key in (row.get("metadata") or {}) for key in (
                "kb_dataset_id", "kb_source_versions", "kb_trace_id",
            ))
            for row in eligible
        ):
            raise ValueError(f"eval_retry_kb_source_requires_fresh_authorization:{case_id}")
        if any(
            row.get("dispatch_state") in {"dispatching", "reconcile_required"}
            or (row.get("observed_metrics") or {}).get("exit_reason") == "side_effect_unknown"
            for row in eligible
        ):
            raise ValueError(f"eval_retry_side_effect_unconfirmed:{case_id}")
        source = eligible[0]
        observed = source.get("observed_metrics") or {}
        model_only_complete = (
            observed.get("execution_succeeded") is True
            and (source.get("candidate_trace_id") or observed.get("trace_id"))
            and observed.get("tool_trajectory") == []
            and observed.get("exit_reason") in {"completed", "succeeded"}
        )
        if source.get("dispatch_state") != "not_started" and not model_only_complete:
            raise ValueError(f"eval_retry_side_effect_unconfirmed:{case_id}")
        selected.append(source)
    return selected


def _example_metadata_patch(payload: dict[str, Any]) -> dict[str, Any]:
    patch = dict(payload.get("metadata") or {})
    for key in EXAMPLE_METADATA_PATCH_KEYS:
        if payload.get(key) is not None:
            patch[key] = payload[key]
    return patch


def _import_example_metadata(example: dict[str, Any]) -> dict[str, Any]:
    return {
        "case_id": example.get("case_id"),
        "expected_trajectory": example.get("expected_trajectory") or {},
        "assertions": example.get("assertions") or [],
        **(example.get("metadata") or {}),
    }


def _experiment_score_id(
    *,
    tenant_id: str,
    trace_id: str,
    trace_family: str,
    payload: dict[str, Any],
) -> str | None:
    metadata = payload.get("metadata")
    if not isinstance(metadata, dict):
        return None
    experiment_run_id = str(metadata.get("experiment_run_id") or "").strip()
    if not experiment_run_id:
        return None

    span_id = payload.get("span_id")
    target_type = payload.get("target_type") or ("span" if span_id else "trace")
    target_id = payload.get("target_id") or span_id or trace_id
    identity = {
        "evaluator_id": str(payload.get("evaluator_id") or ""),
        "evaluator_name": str(payload.get("evaluator_name") or ""),
        "evaluator_version": str(payload.get("evaluator_version") or ""),
        "experiment_run_id": experiment_run_id,
        "score_name": str(payload.get("score_name") or ""),
        "score_source": str(payload.get("score_source") or payload.get("scorer_type") or "human"),
        "scorer_type": str(payload.get("scorer_type") or "human"),
        "span_id": str(span_id or ""),
        "target_id": str(target_id),
        "target_type": str(target_type),
        "tenant_id": tenant_id,
        "trace_family": trace_family,
        "trace_id": trace_id,
    }
    identity_json = json.dumps(identity, sort_keys=True, separators=(",", ":"))
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"ai-gateway/eval-score/{identity_json}"))


def _coerce_timestamptz(value: Any) -> datetime | None:
    if value is None or value == "":
        return None
    if isinstance(value, datetime):
        return value if value.tzinfo is not None else value.replace(tzinfo=timezone.utc)
    if isinstance(value, bool):
        return None
    if isinstance(value, int | float):
        if value <= 0:
            return None
        return datetime.fromtimestamp(value, tz=timezone.utc)
    if isinstance(value, str):
        text = value.strip()
        if not text:
            return None
        if text.endswith("Z"):
            text = f"{text[:-1]}+00:00"
        try:
            parsed = datetime.fromisoformat(text)
        except ValueError:
            return None
        return parsed if parsed.tzinfo is not None else parsed.replace(tzinfo=timezone.utc)
    return None


class TraceIngestNotFound(ValueError):
    """The requested Trace or child identity is outside the write scope."""


class EvalLeaseLost(RuntimeError):
    """The job was cancelled, expired or reclaimed; stale writes are forbidden."""


_OUTBOX_CLAIM: ContextVar[tuple[Any, dict[str, Any]] | None] = ContextVar("eval_outbox_claim", default=None)


class AgentTraceRepository(BaseRepository):
    """Tenant-scoped persistence helper for Agent Trace Eval APIs."""

    @contextmanager
    def bind_outbox_claim(self, job: dict[str, Any]):
        if not all(job.get(key) for key in ("job_id", "tenant_id", "owner_id", "claim_token")):
            raise EvalLeaseLost("eval_outbox_claim_missing")
        token = _OUTBOX_CLAIM.set((self, dict(job)))
        try:
            yield
        finally:
            _OUTBOX_CLAIM.reset(token)

    async def _fenced_call(self, method: str, query: str, *args: Any) -> Any:
        claim = _OUTBOX_CLAIM.get()
        if claim is None or claim[0] is not self:
            return await getattr(super(), method)(query, *args)
        if not self.enabled:
            raise EvalLeaseLost("eval_outbox_database_unavailable")
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_outbox_claim(conn)
            # Validation and the domain write share the same short transaction;
            # a concurrent reclaim/cancel cannot slip between these statements.
            result = await getattr(conn, method)(query, *args)
        if method == "fetchrow":
            return dict(result) if result else None
        if method == "fetch":
            return [dict(row) for row in result]
        return result

    async def _lock_outbox_claim(self, conn: Any) -> None:
        claim = _OUTBOX_CLAIM.get()
        if claim is None or claim[0] is not self:
            return
        job = claim[1]
        owned = await conn.fetchrow(
            """SELECT job_id FROM agent_trace_outbox
               WHERE job_id = $1::uuid AND tenant_id = $2 AND owner_id = $3
               AND claim_token = $4::uuid AND status = 'running'
               AND lease_until > clock_timestamp() FOR UPDATE""",
            str(job["job_id"]), str(job["tenant_id"]), str(job["owner_id"]), str(job["claim_token"]),
        )
        if not owned:
            raise EvalLeaseLost("eval_outbox_lease_lost")

    async def abandon_outbox_claim(self) -> None:
        claim = _OUTBOX_CLAIM.get()
        if claim is None or claim[0] is not self:
            raise EvalLeaseLost("eval_outbox_claim_missing")
        await self.execute(
            """UPDATE agent_trace_outbox SET status = 'queued', available_at = NOW(),
               lease_until = NULL, updated_at = NOW() WHERE job_id = $1::uuid""",
            str(claim[1]["job_id"]),
        )

    async def fetchrow(self, query: str, *args: Any) -> dict[str, Any] | None:
        return await self._fenced_call("fetchrow", query, *args)

    async def fetch(self, query: str, *args: Any) -> list[dict[str, Any]]:
        return await self._fenced_call("fetch", query, *args)

    async def execute(self, query: str, *args: Any) -> str:
        return await self._fenced_call("execute", query, *args)

    async def executemany(self, query: str, args: list[tuple]) -> None:
        await self._fenced_call("executemany", query, args)

    async def list_traces(
        self,
        *,
        tenant_id: str,
        user_id: str | None = None,
        trace_family: str = "assistant",
        status: str | None = None,
        model_id: str | None = None,
        session_id: str | None = None,
        run_id: str | None = None,
        request_id: str | None = None,
        agent_id: str | None = None,
        agent_version_id: str | None = None,
        publication_id: str | None = None,
        channel: str | None = None,
        transcript_query: str | None = None,
        turn_index: int | None = None,
        span_kind: str | None = None,
        score_name: str | None = None,
        score_label: str | None = None,
        min_score: float | None = None,
        max_score: float | None = None,
        min_latency_ms: int | None = None,
        max_latency_ms: int | None = None,
        dataset_id: str | None = None,
        metadata_dataset_id: str | None = None,
        started_after: Any | None = None,
        started_before: Any | None = None,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        params: list[Any] = [tenant_id, trace_family]
        filters = ["t.tenant_id = $1", "t.trace_family = $2"]

        if user_id:
            params.append(user_id)
            filters.append(f"t.user_id = ${len(params)}")
        if status:
            params.append(status)
            filters.append(f"t.status = ${len(params)}")
        if model_id:
            params.append(model_id)
            filters.append(f"t.model_id = ${len(params)}")
        if session_id:
            params.append(session_id)
            filters.append(f"t.session_id = ${len(params)}")
        if run_id:
            params.append(run_id)
            filters.append(f"t.run_id = ${len(params)}")
        if request_id:
            params.append(request_id)
            filters.append(f"t.request_id = ${len(params)}")
        if agent_id:
            params.append(agent_id)
            filters.append(f"t.agent_id = ${len(params)}::uuid")
        if agent_version_id:
            params.append(agent_version_id)
            filters.append(f"t.agent_version_id = ${len(params)}::uuid")
        if publication_id:
            params.append(publication_id)
            filters.append(f"t.publication_id = ${len(params)}::uuid")
        if channel:
            params.append(channel)
            filters.append(f"t.channel = ${len(params)}")
        if turn_index is not None:
            params.append(str(turn_index))
            filters.append(f"t.metadata->'transcript_locator'->>'turn_index' = ${len(params)}")
        if transcript_query:
            params.append(f"%{transcript_query}%")
            query_param = f"${len(params)}"
            filters.append(
                f"""
                (
                    t.trace_id::text ILIKE {query_param}
                    OR COALESCE(t.request_id, '') ILIKE {query_param}
                    OR COALESCE(t.run_id, '') ILIKE {query_param}
                    OR COALESCE(t.session_id, '') ILIKE {query_param}
                    OR COALESCE(t.user_id, '') ILIKE {query_param}
                    OR COALESCE(t.model_id, '') ILIKE {query_param}
                    OR COALESCE(t.provider, '') ILIKE {query_param}
                    OR COALESCE(t.input_preview, '') ILIKE {query_param}
                    OR COALESCE(t.output_preview, '') ILIKE {query_param}
                    OR COALESCE(
                        t.metadata->'transcript_locator'->>'current_message_preview',
                        ''
                    ) ILIKE {query_param}
                    OR COALESCE(
                        t.metadata->'transcript_locator'->>'transcript_excerpt',
                        ''
                    ) ILIKE {query_param}
                    OR COALESCE(
                        t.metadata->'transcript_locator'->>'turn_id',
                        ''
                    ) ILIKE {query_param}
                )
                """
            )
        if span_kind:
            params.append(span_kind)
            filters.append(
                f"""
                EXISTS (
                    SELECT 1
                    FROM agent_trace_spans st
                    WHERE st.trace_id = t.trace_id AND st.span_kind = ${len(params)}
                )
                """
            )
        if min_latency_ms is not None:
            params.append(min_latency_ms)
            filters.append(f"t.total_latency_ms >= ${len(params)}")
        if max_latency_ms is not None:
            params.append(max_latency_ms)
            filters.append(f"t.total_latency_ms <= ${len(params)}")
        if started_after is not None:
            params.append(started_after)
            filters.append(f"t.started_at >= ${len(params)}")
        if started_before is not None:
            params.append(started_before)
            filters.append(f"t.started_at <= ${len(params)}")
        if score_name:
            params.append(score_name)
            filters.append(
                f"""
                EXISTS (
                    SELECT 1
                    FROM agent_trace_scores ss
                    WHERE ss.trace_id = t.trace_id AND ss.score_name = ${len(params)}
                )
                """
            )
        if score_label:
            params.append(score_label)
            filters.append(
                f"""
                EXISTS (
                    SELECT 1
                    FROM agent_trace_scores sl
                    WHERE sl.trace_id = t.trace_id AND sl.label = ${len(params)}
                )
                """
            )
        if min_score is not None:
            params.append(min_score)
            filters.append(
                f"""
                EXISTS (
                    SELECT 1
                    FROM agent_trace_scores smn
                    WHERE smn.trace_id = t.trace_id AND smn.numeric_value >= ${len(params)}
                )
                """
            )
        if max_score is not None:
            params.append(max_score)
            filters.append(
                f"""
                EXISTS (
                    SELECT 1
                    FROM agent_trace_scores smx
                    WHERE smx.trace_id = t.trace_id AND smx.numeric_value <= ${len(params)}
                )
                """
            )
        if dataset_id:
            params.append(dataset_id)
            filters.append(
                f"""
                EXISTS (
                    SELECT 1
                    FROM eval_examples ex
                    WHERE ex.source_trace_id = t.trace_id AND ex.dataset_id = ${len(params)}::uuid
                )
                """
            )
        if metadata_dataset_id:
            params.append(metadata_dataset_id)
            filters.append(f"t.metadata->>'dataset_id' = ${len(params)}")

        where_clause = " AND ".join(filters)
        count_row = await self.fetchrow(
            f"SELECT COUNT(*) AS total FROM agent_traces t WHERE {where_clause}",
            *params,
        )
        total = int(count_row.get("total") or 0) if count_row else 0

        page_params = [*params, limit, offset]
        rows = await self.fetch(
            f"""
            SELECT
                t.*,
                COUNT(s.score_id)::int AS scores_count
            FROM agent_traces t
            LEFT JOIN agent_trace_scores s ON s.trace_id = t.trace_id
            WHERE {where_clause}
            GROUP BY t.trace_id
            ORDER BY t.created_at DESC
            LIMIT ${len(params) + 1}
            OFFSET ${len(params) + 2}
            """,
            *page_params,
        )
        return [self._decode_trace_row(row) for row in rows], total

    async def get_trace_detail(
        self,
        *,
        tenant_id: str,
        trace_id: str,
        user_id: str | None = None,
        trace_family: str = "assistant",
    ) -> dict[str, Any] | None:
        params: list[Any] = [trace_id, tenant_id, trace_family]
        filters = ["trace_id = $1", "tenant_id = $2", "trace_family = $3"]
        if user_id:
            params.append(user_id)
            filters.append(f"user_id = ${len(params)}")

        trace = await self.fetchrow(
            f"SELECT *, 0::int AS scores_count FROM agent_traces WHERE {' AND '.join(filters)}",
            *params,
        )
        if not trace:
            return None

        spans = await self.fetch(
            """
            SELECT *
            FROM agent_trace_spans
            WHERE trace_id = $1
            ORDER BY sequence_no ASC, started_at ASC
            """,
            trace_id,
        )
        events = await self.fetch(
            """
            SELECT *
            FROM agent_trace_events
            WHERE trace_id = $1
            ORDER BY sequence_no ASC, occurred_at ASC
            """,
            trace_id,
        )
        scores = await self.fetch(
            """
            SELECT *
            FROM agent_trace_scores
            WHERE trace_id = $1
            ORDER BY created_at DESC
            """,
            trace_id,
        )

        decoded_trace = self._decode_trace_row(trace)
        decoded_trace["scores_count"] = len(scores)
        return {
            "trace": decoded_trace,
            "spans": [self._decode_span_row(row) for row in spans],
            "events": [self._decode_event_row(row) for row in events],
            "scores": [self._decode_score_row(row) for row in scores],
        }

    async def get_trace_details(
        self,
        *,
        tenant_id: str,
        trace_ids: list[str],
        user_id: str | None = None,
        trace_family: str = "assistant",
    ) -> dict[str, dict[str, Any]]:
        requested_ids = list(dict.fromkeys(str(trace_id) for trace_id in trace_ids if trace_id))
        if not requested_ids:
            return {}

        params: list[Any] = [requested_ids, tenant_id, trace_family]
        filters = ["trace_id = ANY($1::uuid[])", "tenant_id = $2", "trace_family = $3"]
        if user_id:
            params.append(user_id)
            filters.append(f"user_id = ${len(params)}")

        traces = await self.fetch(
            f"""
            SELECT *, 0::int AS scores_count
            FROM agent_traces
            WHERE {" AND ".join(filters)}
            ORDER BY array_position($1::uuid[], trace_id)
            """,
            *params,
        )
        allowed_ids = [str(row["trace_id"]) for row in traces]
        if not allowed_ids:
            return {}

        spans = await self.fetch(
            """
            SELECT * FROM agent_trace_spans
            WHERE trace_id = ANY($1::uuid[])
            ORDER BY trace_id, sequence_no ASC, started_at ASC
            """,
            allowed_ids,
        )
        events = await self.fetch(
            """
            SELECT * FROM agent_trace_events
            WHERE trace_id = ANY($1::uuid[])
            ORDER BY trace_id, sequence_no ASC, occurred_at ASC
            """,
            allowed_ids,
        )
        scores = await self.fetch(
            """
            SELECT * FROM agent_trace_scores
            WHERE trace_id = ANY($1::uuid[])
            ORDER BY trace_id, created_at DESC
            """,
            allowed_ids,
        )

        details = {
            trace_id: {
                "trace": self._decode_trace_row(row),
                "spans": [],
                "events": [],
                "scores": [],
            }
            for row in traces
            if (trace_id := str(row["trace_id"]))
        }
        for key, rows, decoder in (
            ("spans", spans, self._decode_span_row),
            ("events", events, self._decode_event_row),
            ("scores", scores, self._decode_score_row),
        ):
            for row in rows:
                trace_id = str(row["trace_id"])
                if trace_id in details:
                    details[trace_id][key].append(decoder(row))
        for detail in details.values():
            detail["trace"]["scores_count"] = len(detail["scores"])
        return details

    async def create_score(
        self,
        *,
        tenant_id: str,
        trace_id: str,
        created_by: str,
        payload: dict[str, Any],
        user_id: str | None = None,
        trace_family: str = "assistant",
    ) -> dict[str, Any] | None:
        params: list[Any] = [trace_id, tenant_id, trace_family]
        filters = ["trace_id = $1", "tenant_id = $2", "trace_family = $3"]
        if user_id:
            params.append(user_id)
            filters.append(f"user_id = ${len(params)}")

        trace = await self.fetchrow(
            f"SELECT trace_id FROM agent_traces WHERE {' AND '.join(filters)}",
            *params,
        )
        if not trace:
            return None

        score_id = _experiment_score_id(
            tenant_id=tenant_id,
            trace_id=trace_id,
            trace_family=trace_family,
            payload=payload,
        )
        row = await self.fetchrow(
            """
            INSERT INTO agent_trace_scores (
                score_id, trace_id, span_id, score_name, score_type, numeric_value,
                boolean_value, categorical_value, text_value, label, explanation,
                scorer_type, evaluator_version, target_type, target_id,
                evaluator_id, evaluator_name, score_source, confidence,
                created_by, metadata
            ) VALUES (
                COALESCE($1::uuid, gen_random_uuid()), $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11,
                $12, $13, $14, $15,
                $16::uuid, $17, $18, $19,
                $20, $21::jsonb
            )
            ON CONFLICT (score_id)
            DO UPDATE SET
                score_type = EXCLUDED.score_type,
                numeric_value = EXCLUDED.numeric_value,
                boolean_value = EXCLUDED.boolean_value,
                categorical_value = EXCLUDED.categorical_value,
                text_value = EXCLUDED.text_value,
                label = EXCLUDED.label,
                explanation = EXCLUDED.explanation,
                scorer_type = EXCLUDED.scorer_type,
                evaluator_version = EXCLUDED.evaluator_version,
                target_type = EXCLUDED.target_type,
                target_id = EXCLUDED.target_id,
                evaluator_id = EXCLUDED.evaluator_id,
                evaluator_name = EXCLUDED.evaluator_name,
                score_source = EXCLUDED.score_source,
                confidence = EXCLUDED.confidence,
                created_by = EXCLUDED.created_by,
                metadata = EXCLUDED.metadata
            RETURNING *
            """,
            score_id,
            trace_id,
            payload.get("span_id"),
            payload["score_name"],
            payload.get("score_type") or "numeric",
            payload.get("numeric_value"),
            payload.get("boolean_value"),
            payload.get("categorical_value"),
            payload.get("text_value"),
            payload.get("label"),
            payload.get("explanation"),
            payload.get("scorer_type") or "human",
            payload.get("evaluator_version"),
            payload.get("target_type") or ("span" if payload.get("span_id") else "trace"),
            payload.get("target_id") or payload.get("span_id") or trace_id,
            payload.get("evaluator_id"),
            payload.get("evaluator_name"),
            payload.get("score_source") or payload.get("scorer_type") or "human",
            payload.get("confidence"),
            created_by,
            self._json_dumps(payload.get("metadata") or {}),
        )
        return self._decode_score_row(row) if row else None

    async def ingest_trace(
        self,
        *,
        tenant_id: str,
        created_by: str,
        payload: dict[str, Any],
        enqueue: bool = True,
        scope_user_id: str | None = None,
    ) -> dict[str, Any]:
        if not tenant_id or scope_user_id == "":
            raise TraceIngestNotFound("Trace not found")
        if not self.enabled:
            raise RuntimeError("Eval database is unavailable")
        claim = _OUTBOX_CLAIM.get()
        if claim is not None and claim[0] is self and claim[1]["tenant_id"] != tenant_id:
            raise EvalLeaseLost("eval_outbox_lease_lost")
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_outbox_claim(conn)
            return await self._ingest_trace_in(
                conn, tenant_id=tenant_id, created_by=created_by, payload=payload,
                enqueue=enqueue, scope_user_id=scope_user_id,
            )

    async def _ingest_trace_in(
        self, conn: Any, *, tenant_id: str, created_by: str,
        payload: dict[str, Any], enqueue: bool, scope_user_id: str | None,
    ) -> dict[str, Any]:
        trace = payload["trace"]
        if scope_user_id is not None and (trace.get("user_id") or created_by) != scope_user_id:
            raise TraceIngestNotFound("Trace not found")
        trace_id = str(trace.get("trace_id") or uuid.uuid4())
        thread_id = trace.get("thread_id") or trace.get("session_id")
        metrics = trace.get("metrics") or {}
        privacy = trace.get("privacy") or {}
        metadata = {
            "schema_version": "ate-03",
            "source_adapter": trace.get("source_adapter") or "api",
            **(trace.get("metadata") or {}),
        }
        saved = await conn.fetchrow(
            """
            INSERT INTO agent_traces (
                trace_id, trace_family, workflow_kind, tenant_id, user_id,
                session_id, thread_id, run_id, request_id, otel_trace_id, traceparent,
                model_id, provider,
                status, started_at, ended_at, total_latency_ms, first_token_latency_ms,
                input_tokens, output_tokens, total_tokens, total_cost_cents,
                input_preview, output_preview, redaction_state, metadata,
                metrics, privacy, source_adapter, retention_expires_at
            ) VALUES (
                $1::uuid, $2, $3, $4, $5,
                $6, $7, $8, $9, $10, $11,
                $12, $13,
                $14, COALESCE($15::timestamptz, NOW()), $16::timestamptz, $17, $18,
                $19, $20, $21, $22,
                $23, $24, $25::jsonb, $26::jsonb,
                $27::jsonb, $28::jsonb, $29, $30::timestamptz
            )
            ON CONFLICT (trace_id)
            DO UPDATE SET
                trace_family = EXCLUDED.trace_family,
                workflow_kind = EXCLUDED.workflow_kind,
                user_id = EXCLUDED.user_id,
                session_id = EXCLUDED.session_id,
                thread_id = EXCLUDED.thread_id,
                run_id = EXCLUDED.run_id,
                request_id = EXCLUDED.request_id,
                model_id = EXCLUDED.model_id,
                provider = EXCLUDED.provider,
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_traces.ended_at),
                total_latency_ms = GREATEST(agent_traces.total_latency_ms, EXCLUDED.total_latency_ms),
                first_token_latency_ms = GREATEST(agent_traces.first_token_latency_ms, EXCLUDED.first_token_latency_ms),
                input_tokens = GREATEST(agent_traces.input_tokens, EXCLUDED.input_tokens),
                output_tokens = GREATEST(agent_traces.output_tokens, EXCLUDED.output_tokens),
                total_tokens = GREATEST(agent_traces.total_tokens, EXCLUDED.total_tokens),
                total_cost_cents = GREATEST(agent_traces.total_cost_cents, EXCLUDED.total_cost_cents),
                input_preview = COALESCE(NULLIF(EXCLUDED.input_preview, ''), agent_traces.input_preview),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''), agent_traces.output_preview),
                redaction_state = agent_traces.redaction_state || EXCLUDED.redaction_state,
                metadata = agent_traces.metadata || EXCLUDED.metadata,
                metrics = agent_traces.metrics || EXCLUDED.metrics,
                privacy = agent_traces.privacy || EXCLUDED.privacy,
                source_adapter = COALESCE(EXCLUDED.source_adapter, agent_traces.source_adapter),
                updated_at = NOW()
            WHERE agent_traces.tenant_id = EXCLUDED.tenant_id
              AND ($31::text IS NULL OR agent_traces.user_id = $31)
            RETURNING trace_id
            """,
            trace_id,
            trace.get("trace_family") or "assistant",
            trace.get("workflow_kind") or "ai_assistant_chat",
            tenant_id,
            trace.get("user_id") or created_by,
            trace.get("session_id"),
            thread_id,
            trace.get("run_id"),
            trace.get("request_id"),
            trace.get("otel_trace_id"),
            trace.get("traceparent"),
            trace.get("model_id"),
            trace.get("provider"),
            trace.get("status") or "succeeded",
            _coerce_timestamptz(trace.get("started_at")),
            _coerce_timestamptz(trace.get("ended_at")),
            int(metrics.get("total_latency_ms") or trace.get("total_latency_ms") or 0),
            int(metrics.get("first_token_latency_ms") or trace.get("first_token_latency_ms") or 0),
            int(metrics.get("input_tokens") or trace.get("input_tokens") or 0),
            int(metrics.get("output_tokens") or trace.get("output_tokens") or 0),
            int(metrics.get("total_tokens") or trace.get("total_tokens") or 0),
            int(metrics.get("total_cost_cents") or trace.get("total_cost_cents") or 0),
            trace.get("input_preview") or "",
            trace.get("output_preview") or "",
            self._json_dumps(trace.get("redaction_state") or {}),
            self._json_dumps(metadata),
            self._json_dumps(metrics),
            self._json_dumps(privacy),
            trace.get("source_adapter") or "api",
            _coerce_timestamptz(trace.get("retention_expires_at")),
            scope_user_id,
        )
        if not saved:
            raise TraceIngestNotFound("Trace not found")
        if trace.get("source_adapter") == "gateway.agent_runtime":
            await bind_runtime_trace_dimensions(
                conn, trace_id=trace_id, tenant_id=tenant_id,
                user_id=trace.get("user_id") or created_by,
                session_id=trace.get("session_id"),
            )
        for span in trace.get("spans") or []:
            await self._upsert_ingested_span(conn, trace_id, span)
        for event in trace.get("events") or []:
            await self._upsert_ingested_event(conn, trace_id, event)
        if enqueue:
            job = await self._create_trace_ingested_outbox_job_in(
                conn, tenant_id=tenant_id,
                trace_id=trace_id,
                trace_family=str(trace.get("trace_family") or "assistant"),
                status=str(trace.get("status") or "succeeded"),
                source_adapter=str(trace.get("source_adapter") or "api"),
            )
            return {
                "trace_id": trace_id,
                "status": "stored",
                "job_id": job.get("job_id") if job else None,
            }
        return {"trace_id": trace_id, "status": "stored", "job_id": None}

    async def _upsert_ingested_span(self, conn: Any, trace_id: str, span: dict[str, Any]) -> None:
        span_id = str(
            span.get("span_id")
            or uuid.uuid5(uuid.UUID(trace_id), span.get("name") or str(uuid.uuid4()))
        )
        await self._require_ingested_span(conn, trace_id, span.get("parent_span_id"))
        saved = await conn.fetchrow(
            """
            INSERT INTO agent_trace_spans (
                span_id, trace_id, parent_span_id, span_kind, name, status,
                sequence_no, started_at, ended_at, duration_ms, input_preview,
                output_preview, attributes, error_type, error_message
            ) VALUES (
                $1::uuid, $2::uuid, $3::uuid, $4, $5, $6,
                $7, COALESCE($8::timestamptz, NOW()), $9::timestamptz, $10, $11,
                $12, $13::jsonb, $14, $15
            )
            ON CONFLICT (span_id)
            DO UPDATE SET
                status = EXCLUDED.status,
                ended_at = COALESCE(EXCLUDED.ended_at, agent_trace_spans.ended_at),
                duration_ms = GREATEST(agent_trace_spans.duration_ms, EXCLUDED.duration_ms),
                output_preview = COALESCE(NULLIF(EXCLUDED.output_preview, ''), agent_trace_spans.output_preview),
                attributes = agent_trace_spans.attributes || EXCLUDED.attributes,
                error_type = COALESCE(EXCLUDED.error_type, agent_trace_spans.error_type),
                error_message = COALESCE(EXCLUDED.error_message, agent_trace_spans.error_message)
            WHERE agent_trace_spans.trace_id = EXCLUDED.trace_id
            RETURNING span_id
            """,
            span_id,
            trace_id,
            span.get("parent_span_id"),
            span.get("span_kind") or "custom",
            str(span.get("name") or "span")[:160],
            span.get("status") or "succeeded",
            int(span.get("sequence_no") or 0),
            _coerce_timestamptz(span.get("started_at")),
            _coerce_timestamptz(span.get("ended_at")),
            int(span.get("duration_ms") or 0),
            span.get("input_preview") or "",
            span.get("output_preview") or "",
            self._json_dumps(span.get("attributes") or {}),
            span.get("error_type"),
            span.get("error_message"),
        )
        if not saved:
            raise TraceIngestNotFound("Trace not found")

    async def _upsert_ingested_event(self, conn: Any, trace_id: str, event: dict[str, Any]) -> None:
        await self._require_ingested_span(conn, trace_id, event.get("span_id"))
        saved = await conn.fetchrow(
            """
            INSERT INTO agent_trace_events (
                trace_id, span_id, event_type, sequence_no, occurred_at,
                payload, payload_size_bytes, redacted
            ) VALUES (
                $1::uuid, $2::uuid, $3, $4, COALESCE($5::timestamptz, NOW()),
                $6::jsonb, $7, $8
            )
            ON CONFLICT (trace_id, sequence_no)
            DO UPDATE SET
                span_id = EXCLUDED.span_id,
                event_type = EXCLUDED.event_type,
                payload = EXCLUDED.payload,
                payload_size_bytes = EXCLUDED.payload_size_bytes,
                redacted = EXCLUDED.redacted
            RETURNING event_id
            """,
            trace_id,
            event.get("span_id"),
            event.get("event_type") or "event",
            int(event.get("sequence_no") or 0),
            _coerce_timestamptz(event.get("occurred_at")),
            self._json_dumps(event.get("payload") or {}),
            int(event.get("payload_size_bytes") or 0),
            bool(event.get("redacted", True)),
        )
        if not saved:
            raise TraceIngestNotFound("Trace not found")

    @staticmethod
    async def _require_ingested_span(conn: Any, trace_id: str, span_id: str | None) -> None:
        if span_id and not await conn.fetchrow(
            "SELECT span_id FROM agent_trace_spans WHERE trace_id = $1::uuid "
            "AND span_id = $2::uuid FOR KEY SHARE", trace_id, span_id,
        ):
            raise TraceIngestNotFound("Trace not found")

    async def get_thread(
        self,
        *,
        tenant_id: str,
        thread_id: str,
        user_id: str | None = None,
        limit: int = 100,
    ) -> dict[str, Any]:
        params: list[Any] = [tenant_id, thread_id]
        filters = ["t.tenant_id = $1", "COALESCE(t.thread_id, t.session_id) = $2"]
        if user_id:
            params.append(user_id)
            filters.append(f"t.user_id = ${len(params)}")
        rows = await self.fetch(
            f"""
            SELECT t.*, COUNT(s.score_id)::int AS scores_count
            FROM agent_traces t
            LEFT JOIN agent_trace_scores s ON s.trace_id = t.trace_id
            WHERE {" AND ".join(filters)}
            GROUP BY t.trace_id
            ORDER BY t.started_at ASC, t.created_at ASC
            LIMIT ${len(params) + 1}
            """,
            *params,
            limit,
        )
        traces = [self._decode_trace_row(row) for row in rows]
        metrics = {
            "trace_count": len(traces),
            "total_latency_ms": sum(int(trace.get("total_latency_ms") or 0) for trace in traces),
            "total_tokens": sum(int(trace.get("total_tokens") or 0) for trace in traces),
            "failed_count": sum(1 for trace in traces if trace.get("status") == "failed"),
        }
        return {"thread_id": thread_id, "traces": traces, "total": len(traces), "metrics": metrics}

    async def create_dataset(
        self,
        *,
        tenant_id: str,
        created_by: str,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        row = await self.fetchrow(
            """
            INSERT INTO eval_datasets (
                tenant_id, name, description, version, schema, metadata, created_by
            ) VALUES ($1, $2, $3, $4, $5::jsonb, $6::jsonb, $7)
            RETURNING *
            """,
            tenant_id,
            payload["name"],
            payload.get("description") or "",
            payload.get("version") or "v1",
            self._json_dumps(payload.get("schema") or payload.get("json_schema") or {}),
            self._json_dumps(payload.get("metadata") or {}),
            created_by,
        )
        return self._decode_eval_row(row) if row else {}

    async def create_example_from_trace(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        created_by: str,
        payload: dict[str, Any],
        user_id: str | None = None,
        trace_family: str = "assistant",
    ) -> dict[str, Any] | None:
        from .trace_example_import import import_trace_example

        return await import_trace_example(
            self, tenant_id=tenant_id, dataset_id=dataset_id, created_by=created_by,
            payload=payload, user_id=user_id, trace_family=trace_family,
        )

    async def create_example(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        created_by: str,
        payload: dict[str, Any],
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            """
            INSERT INTO eval_examples (
                dataset_id, tenant_id, split, input, expected_output, metadata,
                source_trace_id, source_span_id, created_by
            ) VALUES (
                $1::uuid, $2, $3, $4::jsonb, $5::jsonb, $6::jsonb,
                $7::uuid, $8::uuid, $9
            )
            RETURNING *
            """,
            dataset_id,
            tenant_id,
            payload.get("split") or "regression",
            self._json_dumps(payload.get("input") or {}),
            self._json_dumps(payload.get("expected_output") or {}),
            self._json_dumps(payload.get("metadata") or {}),
            payload.get("source_trace_id"),
            payload.get("source_span_id"),
            created_by,
        )
        return self._decode_eval_row(row) if row else None

    async def update_example(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        example_id: str,
        payload: dict[str, Any],
    ) -> dict[str, Any] | None:
        metadata_patch = _example_metadata_patch(payload)
        row = await self.fetchrow(
            """
            UPDATE eval_examples
            SET split = COALESCE($4, split),
                input = COALESCE($5::jsonb, input),
                expected_output = COALESCE($6::jsonb, expected_output),
                metadata = COALESCE(metadata, '{}'::jsonb) || $7::jsonb
            WHERE tenant_id = $1
              AND dataset_id = $2::uuid
              AND example_id = $3::uuid
            RETURNING *
            """,
            tenant_id,
            dataset_id,
            example_id,
            payload.get("split"),
            self._json_dumps(payload["input"]) if payload.get("input") is not None else None,
            self._json_dumps(payload["expected_output"])
            if payload.get("expected_output") is not None
            else None,
            self._json_dumps(metadata_patch),
        )
        return self._decode_eval_row(row) if row else None

    async def get_example(
        self, *, tenant_id: str, dataset_id: str, example_id: str,
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            """
            SELECT * FROM eval_examples
            WHERE tenant_id = $1 AND dataset_id = $2::uuid AND example_id = $3::uuid
            """,
            tenant_id, dataset_id, example_id,
        )
        return self._decode_eval_row(row) if row else None

    async def review_kb_failure_example(
        self, *, tenant_id: str, dataset_id: str, example_id: str,
        review_status: str, reviewed_from: str,
    ) -> dict[str, Any] | None:
        if not self.enabled:
            raise RuntimeError("Eval database is unavailable")
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                SELECT * FROM eval_examples
                WHERE tenant_id = $1 AND dataset_id = $2::uuid AND example_id = $3::uuid
                """,
                tenant_id, dataset_id, example_id,
            )
            if not row:
                return None
            target = self._decode_eval_row(dict(row))
            metadata = target.get("metadata") or {}
            case_id = metadata.get("case_id")
            if metadata.get("source_kind") != "kb_failure" or not case_id:
                raise ValueError("Not a KB failure case")
            await conn.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))",
                f"eval-kb-failure:{tenant_id}:{dataset_id}:{case_id}",
            )
            rows = await conn.fetch(
                """
                SELECT * FROM eval_examples
                WHERE tenant_id = $1 AND dataset_id = $2::uuid
                  AND metadata->>'source_kind' = 'kb_failure'
                  AND metadata->>'case_id' = $3
                ORDER BY created_at DESC, example_id DESC
                """,
                tenant_id, dataset_id, case_id,
            )
            latest = _latest_kb_case_revision(
                [self._decode_eval_row(dict(item)) for item in rows]
            )
            if not latest or latest["example_id"] != example_id:
                raise EvalCaseRevisionConflict(_kb_case_revision(latest) if latest else 0)
            if review_status == "approved" and not latest.get("source_trace_id"):
                raise ValueError("A confirmed source trace is required before approval")
            expected = latest.get("expected_output") or {}
            if review_status == "approved" and not str(expected.get("answer") or "").strip():
                raise ValueError("An expected answer is required before approval")
            updated = await conn.fetchrow(
                """
                UPDATE eval_examples
                SET split = $4,
                    metadata = metadata || $5::jsonb
                WHERE tenant_id = $1 AND dataset_id = $2::uuid AND example_id = $3::uuid
                RETURNING *
                """,
                tenant_id, dataset_id, example_id,
                "review",
                self._json_dumps({
                    "review_status": review_status,
                    "reviewed_from": reviewed_from,
                }),
            )
            return self._decode_eval_row(dict(updated)) if updated else None

    async def delete_example(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        example_id: str,
    ) -> bool:
        row = await self.fetchrow(
            """
            DELETE FROM eval_examples
            WHERE tenant_id = $1
              AND dataset_id = $2::uuid
              AND example_id = $3::uuid
            RETURNING example_id
            """,
            tenant_id,
            dataset_id,
            example_id,
        )
        return row is not None

    async def list_dataset_example_case_ids(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> set[str]:
        rows = await self.fetch(
            """
            SELECT DISTINCT metadata->>'case_id' AS case_id
            FROM eval_examples
            WHERE tenant_id = $1
              AND dataset_id = $2::uuid
              AND COALESCE(metadata->>'case_id', '') <> ''
            """,
            tenant_id,
            dataset_id,
        )
        return {str(row["case_id"]) for row in rows if row.get("case_id")}

    async def import_examples(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        created_by: str,
        examples: list[dict[str, Any]],
        mode: str = "skip_duplicates",
    ) -> dict[str, Any]:
        existing_case_ids: set[str] = set()
        if mode == "skip_duplicates":
            existing_case_ids = await self.list_dataset_example_case_ids(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )
        seen_in_request: set[str] = set()
        imported: list[dict[str, Any]] = []
        skipped = 0
        for example in examples:
            case_id = str(example.get("case_id") or "").strip()
            if (
                mode == "skip_duplicates"
                and case_id
                and (case_id in existing_case_ids or case_id in seen_in_request)
            ):
                skipped += 1
                continue
            created = await self.create_example(
                tenant_id=tenant_id,
                dataset_id=dataset_id,
                created_by=created_by,
                payload={
                    "split": example.get("split") or "regression",
                    "input": example.get("input") or {},
                    "expected_output": example.get("expected_output") or {},
                    "metadata": _import_example_metadata(example),
                    "source_trace_id": example.get("source_trace_id"),
                    "source_span_id": example.get("source_span_id"),
                },
            )
            if created:
                imported.append(created)
                if case_id:
                    existing_case_ids.add(case_id)
                    seen_in_request.add(case_id)
        return {"imported": len(imported), "skipped": skipped, "examples": imported}

    async def get_latest_kb_failure_example(
        self, *, tenant_id: str, dataset_id: str, case_id: str,
    ) -> dict[str, Any] | None:
        rows = await self.fetch(
            """
            SELECT * FROM eval_examples
            WHERE tenant_id = $1 AND dataset_id = $2::uuid
              AND metadata->>'source_kind' = 'kb_failure'
              AND metadata->>'case_id' = $3
            ORDER BY created_at DESC, example_id DESC
            """,
            tenant_id, dataset_id, case_id,
        )
        return _latest_kb_case_revision([self._decode_eval_row(row) for row in rows])

    async def save_kb_failure_revision(
        self, *, tenant_id: str, dataset_id: str, created_by: str,
        payload: dict[str, Any],
    ) -> tuple[dict[str, Any], bool]:
        """Append an immutable revision under a per-case PostgreSQL lock.

        The expected revision is a CAS token. An exact retry returns the
        already stored revision without inserting another row.
        """
        if not self.enabled:
            raise RuntimeError("Eval database is unavailable")
        case_id = str(payload["case_id"])
        async with self._pool.acquire() as conn, conn.transaction():
            await conn.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended($1::text, 0))",
                f"eval-kb-failure:{tenant_id}:{dataset_id}:{case_id}",
            )
            dataset = await conn.fetchrow(
                "SELECT dataset_id FROM eval_datasets WHERE tenant_id = $1 AND dataset_id = $2::uuid",
                tenant_id, dataset_id,
            )
            if not dataset:
                raise ValueError("Eval dataset not found")
            rows = await conn.fetch(
                """
                SELECT * FROM eval_examples
                WHERE tenant_id = $1 AND dataset_id = $2::uuid
                  AND metadata->>'source_kind' = 'kb_failure'
                  AND metadata->>'case_id' = $3
                ORDER BY created_at DESC, example_id DESC
                """,
                tenant_id, dataset_id, case_id,
            )
            latest = _latest_kb_case_revision(
                [self._decode_eval_row(dict(row)) for row in rows]
            )
            if latest and (latest.get("metadata") or {}).get("kb_dataset_id") != payload["kb_dataset_id"]:
                raise ValueError("KB case belongs to a different knowledge dataset")
            source_versions = [
                {**source, "source_hash": str(source["source_hash"]).lower()}
                for source in payload.get("source_versions") or []
            ]
            semantic_metadata = {
                "source": payload["source"],
                "source_kind": "kb_failure",
                "kb_dataset_id": payload["kb_dataset_id"],
                "kb_trace_id": payload.get("kb_trace_id"),
                "kb_query_fingerprint": payload.get("query_fingerprint"),
                "kb_observed_segment_ids": payload.get("observed_segment_ids") or [],
                "kb_source_versions": source_versions,
                "kb_source_versions_verified": payload.get("source_versions_verified") is True,
                "kb_observed_answer": payload.get("observed_answer"),
                "kb_failure_reason": str(payload.get("failure_reason") or "").strip() or None,
            }
            query = str(payload["query"]).strip()
            expected_answer = str(payload["expected_answer"]).strip()
            source_trace_id = payload.get("source_trace_id")
            if latest:
                metadata = latest.get("metadata") or {}
                if (
                    latest.get("input") == {"query": query}
                    and latest.get("expected_output") == {"answer": expected_answer}
                    and latest.get("source_trace_id") == source_trace_id
                    and all(metadata.get(key) == value for key, value in semantic_metadata.items())
                ):
                    return latest, False
            current_revision = _kb_case_revision(latest) if latest else 0
            if payload["expected_revision"] != current_revision:
                raise EvalCaseRevisionConflict(current_revision)
            metadata = {
                **semantic_metadata,
                "case_id": case_id,
                "case_revision": current_revision + 1,
                "supersedes_example_id": latest.get("example_id") if latest else None,
                "review_status": "pending",
                "behavior_confirmed": False,
            }
            row = await conn.fetchrow(
                """
                INSERT INTO eval_examples (
                    dataset_id, tenant_id, split, input, expected_output, metadata,
                    source_trace_id, created_by
                ) VALUES ($1::uuid, $2, 'review', $3::jsonb, $4::jsonb, $5::jsonb,
                          $6::uuid, $7)
                RETURNING *
                """,
                dataset_id, tenant_id,
                self._json_dumps({"query": query}),
                self._json_dumps({"answer": expected_answer}),
                self._json_dumps(metadata),
                source_trace_id, created_by,
            )
            if not row:
                raise RuntimeError("Eval case revision insert returned no row")
            return self._decode_eval_row(dict(row)), True

    async def create_evaluator(
        self,
        *,
        tenant_id: str,
        created_by: str,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        row = await self.fetchrow(
            """
            INSERT INTO eval_evaluators (
                tenant_id, name, evaluator_type, rubric, version,
                sampling_config, filter_config, metadata, created_by
            ) VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7::jsonb, $8::jsonb, $9)
            RETURNING *
            """,
            tenant_id,
            payload["name"],
            payload.get("evaluator_type") or "human",
            payload.get("rubric") or "",
            payload.get("version") or "v1",
            self._json_dumps(payload.get("sampling_config") or {}),
            self._json_dumps(payload.get("filter_config") or {}),
            self._json_dumps(payload.get("metadata") or {}),
            created_by,
        )
        return self._decode_eval_row(row) if row else {}

    async def create_experiment(
        self,
        *,
        tenant_id: str,
        created_by: str,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        row = await self.fetchrow(
            """
            INSERT INTO eval_experiments (
                tenant_id, dataset_id, name, description, target_config, metadata, created_by
            ) VALUES ($1, $2::uuid, $3, $4, $5::jsonb, $6::jsonb, $7)
            RETURNING *
            """,
            tenant_id,
            payload.get("dataset_id"),
            payload["name"],
            payload.get("description") or "",
            self._json_dumps(payload.get("target_config") or {}),
            self._json_dumps(payload.get("metadata") or {}),
            created_by,
        )
        decoded = self._decode_eval_row(row) if row else {}
        decoded["runs"] = []
        return decoded

    async def get_experiment(
        self,
        *,
        tenant_id: str,
        experiment_id: str,
    ) -> dict[str, Any] | None:
        experiment = await self.fetchrow(
            "SELECT * FROM eval_experiments WHERE tenant_id = $1 AND experiment_id = $2::uuid",
            tenant_id,
            experiment_id,
        )
        if not experiment:
            return None
        runs = await self.fetch(
            """
            SELECT *
            FROM eval_experiment_runs
            WHERE tenant_id = $1 AND experiment_id = $2::uuid
            ORDER BY created_at DESC
            """,
            tenant_id,
            experiment_id,
        )
        decoded = self._decode_eval_row(experiment)
        decoded["runs"] = [self._decode_eval_row(run) for run in runs]
        return decoded

    async def has_active_evaluator_run_for_trace(
        self,
        *,
        tenant_id: str,
        evaluator_id: str,
        trace_id: str,
    ) -> bool:
        row = await self.fetchrow(
            """
            SELECT 1
            FROM eval_experiment_runs
            WHERE tenant_id = $1
              AND evaluator_id = $2::uuid
              AND status IN ('queued', 'running')
              AND target_snapshot->>'trace_id' = $3
            LIMIT 1
            """,
            tenant_id,
            evaluator_id,
            trace_id,
        )
        return row is not None

    async def count_pending_online_eval_runs(self, *, tenant_id: str) -> int:
        row = await self.fetchrow(
            """
            SELECT COUNT(*)::int AS pending
            FROM eval_experiment_runs
            WHERE tenant_id = $1
              AND status IN ('queued', 'running')
              AND target_snapshot->>'source' = 'online_sampling'
            """,
            tenant_id,
        )
        return int((row or {}).get("pending") or 0)

    async def has_pending_trace_ingested_job(
        self,
        *,
        tenant_id: str,
        trace_id: str,
    ) -> bool:
        row = await self.fetchrow(
            """
            SELECT 1
            FROM agent_trace_outbox
            WHERE tenant_id = $1
              AND job_type = 'trace.ingested'
              AND status IN ('queued', 'running')
              AND payload->>'trace_id' = $2
            LIMIT 1
            """,
            tenant_id,
            trace_id,
        )
        return row is not None

    async def create_trace_ingested_outbox_job(
        self, *, tenant_id: str, trace_id: str, trace_family: str,
        status: str, source_adapter: str,
    ) -> dict[str, Any] | None:
        claim = _OUTBOX_CLAIM.get()
        if claim is not None and claim[0] is self and claim[1]["tenant_id"] != tenant_id:
            raise EvalLeaseLost("eval_outbox_lease_lost")
        if not self.enabled:
            return None
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_outbox_claim(conn)
            owned = await conn.fetchrow(
                "SELECT trace_id FROM agent_traces WHERE trace_id = $1::uuid "
                "AND tenant_id = $2 FOR UPDATE", trace_id, tenant_id,
            )
            if not owned:
                return None
            return await self._create_trace_ingested_outbox_job_in(
                conn, tenant_id=tenant_id, trace_id=trace_id,
                trace_family=trace_family, status=status, source_adapter=source_adapter,
            )

    async def _create_trace_ingested_outbox_job_in(
        self, conn: Any, *, tenant_id: str, trace_id: str, trace_family: str,
        status: str, source_adapter: str,
    ) -> dict[str, Any] | None:
        payload = {"trace_id": trace_id, "trace_family": trace_family,
                   "status": status, "source_adapter": source_adapter}
        row = await conn.fetchrow(
            """INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
               SELECT $1::varchar, 'trace.ingested'::varchar, $2::jsonb
               WHERE NOT EXISTS (
                   SELECT 1 FROM agent_trace_outbox
                   WHERE tenant_id = $1::varchar AND job_type = 'trace.ingested'
                     AND payload->>'trace_id' = $3::text
                     AND ($4::boolean OR status IN ('queued', 'running'))
               ) RETURNING *""",
            tenant_id, self._json_dumps(payload), trace_id,
            source_adapter == "gateway.agent_runtime",
        )
        return self._decode_eval_row(dict(row)) if row else None

    async def list_assistant_runtime_trace_reconciliation_candidates(
        self,
        *,
        since: datetime,
        limit: int,
        after_finished_at: datetime | None = None,
        after_run_id: str | None = None,
    ) -> list[dict[str, Any]]:
        """Page recent terminal Runtime runs; never invoke the Runtime itself."""
        rows = await self.fetch(
            """SELECT r.run_id, r.tenant_id, r.user_id, r.session_id,
                      r.status, r.request_preview, r.usage, r.error, r.started_at,
                      COALESCE(r.finished_at, r.updated_at) AS ended_at,
                      s.runtime_thread_id, s.snapshot
               FROM assistant_runs r
               JOIN assistant_runtime_snapshots s
                 ON s.run_id = r.run_id AND s.tenant_id = r.tenant_id
                AND s.user_id = r.user_id AND s.session_id = r.session_id
                AND (r.runtime_snapshot_id IS NULL OR s.snapshot_id = r.runtime_snapshot_id)
               LEFT JOIN agent_traces t ON t.trace_id = r.run_id
               LEFT JOIN assistant.sessions a
                 ON a.session_id = r.session_id AND a.tenant_id = r.tenant_id
                AND a.user_id = r.user_id
               WHERE r.engine = 'agent_runtime'
                 AND r.status IN ('completed', 'succeeded', 'failed', 'cancelled')
                 AND COALESCE(r.finished_at, r.updated_at) >= $1::timestamptz
                 AND ($2::timestamptz IS NULL OR
                      (COALESCE(r.finished_at, r.updated_at), r.run_id) >
                      ($2::timestamptz, $3::uuid))
                 AND (t.trace_id IS NULL OR (
                     t.tenant_id = r.tenant_id AND t.user_id = r.user_id
                     AND t.session_id = r.session_id
                     AND t.workflow_kind = 'agent_runtime_turn'
                     AND (NOT EXISTS (
                         SELECT 1 FROM agent_trace_outbox o
                         WHERE o.tenant_id = r.tenant_id
                           AND o.job_type = 'trace.ingested'
                           AND o.payload->>'trace_id' = r.run_id::text
                     ) OR (
                         t.agent_id IS NULL AND a.agent_id IS NOT NULL
                         AND a.agent_id::text = s.snapshot->'agent_spec'->>'agentId'
                         AND a.agent_version_id::text IS NOT DISTINCT FROM
                             s.snapshot->'agent_spec'->>'agentVersionId'
                     ))
                 ))
               ORDER BY COALESCE(r.finished_at, r.updated_at), r.run_id
               LIMIT $4""",
            since, after_finished_at, after_run_id, max(1, min(limit, 500)),
        )
        return [
            {**row, "usage": self._decode_json(row.get("usage"), default={}),
             "snapshot": self._decode_json(row.get("snapshot"), default={})}
            for row in rows
        ]

    async def get_assistant_runtime_trace_terminal(
        self, *, tenant_id: str, user_id: str, session_id: str,
        runtime_thread_id: str, run_id: str,
    ) -> dict[str, Any] | None:
        return await self.fetchrow(
            """SELECT event_id, sequence, event_type, status, payload, created_at
               FROM assistant_runtime_items
               WHERE runtime_thread_id = $1::uuid AND tenant_id = $2
                 AND user_id = $3 AND session_id = $4 AND turn_id = $5
                 AND event_type IN ('compat/v1/run_finished', 'compat/v1/run_error', 'compat/v1/cancelled')
                 AND payload->'data'->>'run_id' = $5
               ORDER BY sequence DESC LIMIT 1""",
            runtime_thread_id, tenant_id, user_id, session_id, run_id,
        )

    async def count_assistant_runtime_trace_events(
        self, *, tenant_id: str, user_id: str, session_id: str,
        runtime_thread_id: str, run_id: str,
    ) -> dict[str, int]:
        rows = await self.fetch(
            """SELECT event_type, COUNT(*)::int AS event_count
               FROM assistant_runtime_items
               WHERE runtime_thread_id = $1::uuid AND tenant_id = $2
                 AND user_id = $3 AND session_id = $4 AND turn_id = $5
                 AND event_type LIKE 'compat/v1/%'
                 AND payload->'data'->>'run_id' = $5
               GROUP BY event_type""",
            runtime_thread_id, tenant_id, user_id, session_id, run_id,
        )
        return {str(row["event_type"]).removeprefix("compat/v1/"): int(row["event_count"])
                for row in rows}

    async def read_assistant_runtime_trace_text_page(
        self, *, tenant_id: str, user_id: str, session_id: str,
        runtime_thread_id: str, run_id: str, after_sequence: int,
        limit: int = 250,
    ) -> list[dict[str, Any]]:
        return await self.fetch(
            """SELECT sequence, payload->'data'->>'content' AS content
               FROM assistant_runtime_items
               WHERE runtime_thread_id = $1::uuid AND tenant_id = $2
                 AND user_id = $3 AND session_id = $4 AND turn_id = $5
                 AND sequence > $6 AND event_type = 'compat/v1/text_delta'
                 AND payload->'data'->>'run_id' = $5
                 AND NULLIF(payload->'data'->>'content', '') IS NOT NULL
               ORDER BY sequence LIMIT $7""",
            runtime_thread_id, tenant_id, user_id, session_id, run_id,
            after_sequence, max(1, min(limit, 500)),
        )

    async def get_assistant_runtime_trace_model_usage(
        self, *, tenant_id: str, user_id: str, session_id: str,
        runtime_thread_id: str, run_id: str,
    ) -> dict[str, Any]:
        """Return measured call usage only when every dispatched call is complete."""
        row = await self.fetchrow(
            """SELECT
                 COUNT(*) FILTER (WHERE c.dispatched_at IS NOT NULL)::int AS dispatched_calls,
                 COUNT(*) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                       AND c.input_tokens IS NOT NULL AND c.output_tokens IS NOT NULL
                 )::int AS measured_calls,
                 COALESCE(SUM(c.input_tokens) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                 ), 0)::bigint AS input_tokens,
                 COALESCE(SUM(c.output_tokens) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                 ), 0)::bigint AS output_tokens,
                 COALESCE(SUM(c.cost_microusd) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                 ), 0)::bigint AS cost_microusd,
                 COUNT(*) FILTER (
                     WHERE c.dispatched_at IS NOT NULL AND c.status = 'completed'
                       AND c.cost_microusd IS NOT NULL
                 )::int AS cost_measured_calls
               FROM assistant_runtime_model_calls c
               JOIN assistant_runtime_snapshots s
                 ON s.run_id = c.run_id AND s.tenant_id = c.tenant_id
                AND s.user_id = c.user_id AND s.session_id = c.session_id
                AND s.runtime_thread_id = $4::uuid
               WHERE c.tenant_id = $1 AND c.user_id = $2 AND c.session_id = $3
                 AND c.run_id = $5::uuid""",
            tenant_id, user_id, session_id, runtime_thread_id, run_id,
        )
        return dict(row or {})

    async def insert_reconciled_assistant_runtime_trace(
        self, *, run: dict[str, Any], terminal: dict[str, Any], trace: dict[str, Any],
    ) -> bool:
        """Atomically insert one scoped terminal trace, event and Eval outbox job."""
        if not self.enabled:
            return False
        run_id = str(run["run_id"])
        tenant_id = str(run["tenant_id"])
        user_id = str(run["user_id"])
        session_id = str(run["session_id"])
        status = str(run["status"])
        event_id = str(terminal["event_id"])
        metrics = trace["metrics"]
        async with self._pool.acquire() as conn, conn.transaction():
            current = await conn.fetchrow(
                """SELECT r.run_id FROM assistant_runs r
                   JOIN assistant_runtime_snapshots s
                     ON s.run_id = r.run_id AND s.tenant_id = r.tenant_id
                    AND s.user_id = r.user_id AND s.session_id = r.session_id
                    AND s.runtime_thread_id = $5::uuid
                    AND (r.runtime_snapshot_id IS NULL OR s.snapshot_id = r.runtime_snapshot_id)
                   WHERE r.run_id = $1::uuid AND r.tenant_id = $2
                     AND r.user_id = $3 AND r.session_id = $4
                     AND r.engine = 'agent_runtime' AND r.status = $6
                     AND r.status IN ('completed', 'succeeded', 'failed', 'cancelled')
                     AND EXISTS (
                         SELECT 1 FROM assistant_runtime_items i
                         WHERE i.runtime_thread_id = s.runtime_thread_id
                           AND i.tenant_id = r.tenant_id AND i.user_id = r.user_id
                           AND i.session_id = r.session_id AND i.turn_id = r.run_id::text
                           AND i.event_id = $7::uuid
                           AND i.event_type IN ('compat/v1/run_finished', 'compat/v1/run_error', 'compat/v1/cancelled')
                           AND i.payload->'data'->>'run_id' = r.run_id::text
                     ) FOR UPDATE OF r""",
                run_id, tenant_id, user_id, session_id,
                str(run["runtime_thread_id"]), status, event_id,
            )
            if not current:
                return False
            inserted = await conn.fetchrow(
                """INSERT INTO agent_traces (
                     trace_id, trace_family, workflow_kind, tenant_id, user_id,
                     session_id, thread_id, run_id, request_id, model_id, provider,
                     status, started_at, ended_at, total_latency_ms,
                     first_token_latency_ms, input_tokens, output_tokens, total_tokens,
                     input_preview, output_preview, redaction_state, metadata,
                     metrics, privacy, source_adapter, retention_expires_at
                   ) VALUES (
                     $1::uuid, 'assistant', 'agent_runtime_turn', $2, $3,
                     $4, $4, $5, $6, $7, $8,
                     $9, $10::timestamptz, $11::timestamptz, $12,
                     $13, $14, $15, $16,
                     $17, $18, $19::jsonb, $20::jsonb,
                     $21::jsonb, $22::jsonb, 'gateway.agent_runtime', $23::timestamptz
                   ) ON CONFLICT (trace_id) DO NOTHING RETURNING trace_id""",
                run_id, tenant_id, user_id, session_id, run_id,
                trace["request_id"], trace.get("model_id"), trace.get("provider"),
                trace["status"], _coerce_timestamptz(trace["started_at"]),
                _coerce_timestamptz(trace["ended_at"]),
                int(metrics["total_latency_ms"]), int(metrics["first_token_latency_ms"]),
                int(metrics["input_tokens"]), int(metrics["output_tokens"]),
                int(metrics["total_tokens"]), trace["input_preview"],
                trace["output_preview"], self._json_dumps(trace["redaction_state"]),
                self._json_dumps({"schema_version": "ate-03", **trace["metadata"]}),
                self._json_dumps(metrics), self._json_dumps(trace["privacy"]),
                _coerce_timestamptz(trace["retention_expires_at"]),
            )
            if not inserted:
                existing = await conn.fetchrow(
                    """SELECT trace_id FROM agent_traces
                       WHERE trace_id = $1::uuid AND tenant_id = $2 AND user_id = $3
                         AND session_id = $4 AND run_id = $5
                         AND trace_family = 'assistant'
                         AND workflow_kind = 'agent_runtime_turn'
                         AND status = $6
                       FOR UPDATE""",
                    run_id, tenant_id, user_id, session_id, run_id,
                    trace["status"],
                )
                if not existing:
                    return False
            # A crash during live ingestion may leave only the parent trace.
            # Fill missing children before enqueueing, without replacing evidence.
            await bind_runtime_trace_dimensions(
                conn, trace_id=run_id, tenant_id=tenant_id,
                user_id=user_id, session_id=session_id,
            )
            span = trace["spans"][0]
            await conn.execute(
                """INSERT INTO agent_trace_spans (
                     span_id, trace_id, span_kind, name, status, sequence_no,
                     started_at, ended_at, duration_ms, input_preview,
                     output_preview, attributes, error_type
                   ) VALUES ($1::uuid, $2::uuid, $3, $4, $5, $6,
                     $7::timestamptz, $8::timestamptz, $9, $10, $11,
                     $12::jsonb, $13) ON CONFLICT (span_id) DO NOTHING""",
                span["span_id"], run_id, span["span_kind"], span["name"],
                span["status"], span["sequence_no"],
                _coerce_timestamptz(span["started_at"]),
                _coerce_timestamptz(span["ended_at"]), span["duration_ms"],
                span["input_preview"], span["output_preview"],
                self._json_dumps(span["attributes"]), span["error_type"],
            )
            event = trace["events"][0]
            await conn.execute(
                """INSERT INTO agent_trace_events (
                     event_id, trace_id, event_type, sequence_no, occurred_at,
                     payload, payload_size_bytes, redacted
                   ) VALUES ($1::uuid, $2::uuid, $3, $4, $5::timestamptz,
                     $6::jsonb, 0, TRUE) ON CONFLICT (trace_id, sequence_no) DO NOTHING""",
                event_id, run_id, event["event_type"], event["sequence_no"],
                _coerce_timestamptz(terminal["created_at"]),
                self._json_dumps(event["payload"]),
            )
            payload = {
                "trace_id": run_id, "trace_family": "assistant",
                "status": trace["status"], "source_adapter": "gateway.agent_runtime",
            }
            await conn.fetchrow(
                """INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
                   SELECT $1::varchar, 'trace.ingested'::varchar, $2::jsonb
                   WHERE NOT EXISTS (
                       SELECT 1 FROM agent_trace_outbox
                       WHERE tenant_id = $1::varchar AND job_type = 'trace.ingested'
                         AND payload->>'trace_id' = $3::text
                   ) RETURNING job_id""",
                tenant_id, self._json_dumps(payload), run_id,
            )
        return bool(inserted)

    async def resolve_eval_job_actor(self, *, tenant_id: str) -> dict[str, str]:
        claim = _OUTBOX_CLAIM.get()
        if claim is None or claim[0] is not self or claim[1].get("tenant_id") != tenant_id:
            raise EvalLeaseLost("AGENT_EVAL_DELEGATION_CLAIM_REQUIRED")
        job_id = str(claim[1]["job_id"])
        row = await self.fetchrow(
            """SELECT u.user_id, u.tenant_id, r.run_id
               FROM agent_trace_outbox o JOIN eval_experiment_runs r
                 ON r.run_id::text = o.payload->>'run_id' AND r.tenant_id = o.tenant_id
               JOIN users u ON u.user_id = r.created_by AND u.tenant_id = r.tenant_id
               WHERE o.job_id = $1::uuid AND o.tenant_id = $2 AND u.status = 'active'""",
            job_id, tenant_id,
        )
        if not row:
            raise RuntimeError("AGENT_EVAL_DELEGATION_SUBJECT_UNAVAILABLE")
        return {"actor": "eval-worker", "subject": str(row["user_id"]),
                "tenant_id": str(row["tenant_id"]), "run_id": str(row["run_id"]), "job_id": job_id}

    async def freeze_eval_model_ref(
        self, *, tenant_id: str, model_id: str, provider_id: str | None = None,
    ) -> dict[str, Any]:
        rows = await self.fetch(
            """SELECT model_id, provider_id, capability_revision, input_price_per_1k, output_price_per_1k
               FROM llm_models WHERE tenant_id = $1 AND model_id = $2 AND is_enabled = TRUE
               AND ($3::text IS NULL OR provider_id = $3) LIMIT 2""", tenant_id, model_id, provider_id,
        )
        if len(rows) != 1:
            raise ValueError("eval_model_provider_required" if len(rows) > 1 else "eval_model_unavailable")
        row = rows[0]
        receipt = build_pricing_snapshot(
            tenant_id=tenant_id, provider_id=row["provider_id"], model_id=row["model_id"],
            input_price_per_1k=row["input_price_per_1k"], output_price_per_1k=row["output_price_per_1k"],
        )
        return {"tenant_id": tenant_id, "model_id": row["model_id"], "provider_id": row["provider_id"],
                "capability_revision": int(row["capability_revision"] or 1),
                "price_version": receipt["version"], "pricing_snapshot": receipt}

    async def freeze_eval_judge(self, *, tenant_id: str, evaluator: dict[str, Any]) -> dict[str, Any]:
        if evaluator.get("evaluator_type") not in {"llm", "llm_judge", "composite"}:
            return evaluator
        metadata = dict(evaluator.get("metadata") or {})
        ref = await self.freeze_eval_model_ref(
            tenant_id=tenant_id, model_id=str(metadata.get("judge_model_id") or "qwen3.7-plus"),
            provider_id=metadata.get("judge_provider_id"),
        )
        return {**evaluator, "metadata": {**metadata, "judge_model_id": ref["model_id"],
                "judge_provider_id": ref["provider_id"], "judge_model_ref": ref}}

    async def pin_outbox_evaluator(self, *, tenant_id: str, evaluator: dict[str, Any]) -> dict[str, Any]:
        evaluator = await self.freeze_eval_judge(tenant_id=tenant_id, evaluator=evaluator)
        claim = _OUTBOX_CLAIM.get()
        if claim is not None and claim[0] is self:
            await self.execute(
                """UPDATE agent_trace_outbox SET payload = jsonb_set(payload, '{evaluator_snapshot}', $2::jsonb)
                   WHERE job_id = $1::uuid""", str(claim[1]["job_id"]), self._json_dumps(evaluator),
            )
        return evaluator

    async def enqueue_evaluator_run(
        self,
        *,
        tenant_id: str,
        evaluator_id: str,
        created_by: str,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        evaluator = await self.get_evaluator(tenant_id=tenant_id, evaluator_id=evaluator_id)
        if evaluator is None:
            raise ValueError("eval_evaluator_not_found")
        evaluator = await self.freeze_eval_judge(tenant_id=tenant_id, evaluator=evaluator)
        target_snapshot = dict(payload.get("target_snapshot") or {})
        target_snapshot.pop("dataset_manifest", None)
        dataset_id = payload.get("dataset_id")
        dataset_manifest_hash = None
        manifest: list[dict[str, Any]] | None = None
        if dataset_id:
            from ai_gateway_core.eval.evaluator_executor import is_runnable_dataset_example

            dataset = await self.get_dataset(tenant_id=tenant_id, dataset_id=str(dataset_id))
            if dataset is None:
                raise ValueError("eval_dataset_not_found")
            examples = [
                example for example in await self.list_example_manifest(
                    tenant_id=tenant_id, dataset_id=str(dataset_id),
                ) if is_runnable_dataset_example(example)
            ]
            frozen_dataset = build_eval_dataset_manifest(dataset, examples)
            manifest = [
                {key: example.get(key) for key in (
                    "example_id", "split", "input", "expected_output", "metadata",
                    "source_trace_id", "source_span_id",
                )} for example in examples
            ]
            manifest.sort(key=lambda item: str(item.get("example_id") or ""))
            dataset_manifest_hash = _canonical_hash(frozen_dataset)
            target_snapshot.update({
                "dataset_version": dataset.get("version"),
                "dataset_manifest_hash": dataset_manifest_hash,
                "dataset_case_count": len(manifest),
            })
        target_snapshot["evaluator_snapshot"] = {
            key: evaluator.get(key)
            for key in (
                "evaluator_id", "name", "evaluator_type", "rubric", "version",
                "sampling_config", "filter_config",
            )
        }
        evaluator_suite_hash = _canonical_hash({
            **target_snapshot["evaluator_snapshot"],
            "metadata": evaluator.get("metadata") or {},
        })
        trace_id = payload.get("trace_id")
        if trace_id and "trace_id" not in target_snapshot:
            target_snapshot["trace_id"] = trace_id
        metadata = payload.get("metadata")
        if isinstance(metadata, dict) and metadata:
            target_snapshot.setdefault("metadata", metadata)
        if not target_snapshot and trace_id:
            target_snapshot = {"trace_id": trace_id, "metadata": metadata or {}}
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_outbox_claim(conn)
            run = await conn.fetchrow(
                """
                INSERT INTO eval_experiment_runs (
                    experiment_id, tenant_id, evaluator_id, dataset_id, status,
                    dataset_manifest_hash, evaluator_suite_hash,
                    target_snapshot, metrics, created_by
                ) VALUES (
                    $1::uuid, $2, $3::uuid, $4::uuid, 'queued',
                    $5, $6, $7::jsonb, $8::jsonb, $9
                )
                RETURNING *
                """,
                payload.get("experiment_id"),
                tenant_id,
                evaluator_id,
                dataset_id,
                dataset_manifest_hash,
                evaluator_suite_hash,
                self._json_dumps(target_snapshot),
                self._json_dumps({}),
                created_by,
            )
            decoded_run = self._decode_eval_row(run) if run else {}
            trace_family = "assistant"
            if isinstance(target_snapshot, dict):
                family = str(target_snapshot.get("trace_family") or "").strip()
                if family in {"assistant", "langgraph_proxy", "rag"}:
                    trace_family = family
            job_payload = {
                    "run_id": decoded_run.get("run_id"),
                    "evaluator_id": evaluator_id,
                    "evaluator_snapshot": evaluator,
                    "experiment_id": payload.get("experiment_id"),
                    "dataset_id": payload.get("dataset_id"),
                    "trace_id": payload.get("trace_id"),
                    "trace_family": trace_family,
                    "target_snapshot": target_snapshot if isinstance(target_snapshot, dict) else {},
                    "dataset_manifest": manifest,
            }
            job = await conn.fetchrow(
                """INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
                   VALUES ($1, 'eval.evaluator.run', $2::jsonb) RETURNING *""",
                tenant_id, self._json_dumps(job_payload),
            )

        return {"job_id": str(job["job_id"]), "status": "queued", "run_id": decoded_run.get("run_id")}

    async def enqueue_live_experiment_run(
        self,
        *,
        tenant_id: str,
        experiment_id: str,
        dataset_id: str,
        evaluator_snapshots: list[dict[str, Any]],
        examples: list[dict[str, Any]],
        repetitions: int,
        created_by: str,
        target_snapshot: dict[str, Any],
        execution_config: dict[str, Any],
        candidate_fingerprint: dict[str, Any],
        baseline_run_id: str | None = None,
    ) -> dict[str, Any]:
        """Freeze one live candidate run and enqueue it atomically."""
        dataset = await self.get_dataset(tenant_id=tenant_id, dataset_id=dataset_id)
        if dataset is None:
            raise ValueError("eval_dataset_not_found")
        frozen_dataset = build_eval_dataset_manifest(dataset, examples)
        manifest = frozen_dataset["examples"]
        seen_case_ids: set[str] = set()
        for case in manifest:
            case_id = case["case_id"].strip()
            if not case_id or case_id in seen_case_ids:
                raise ValueError(
                    f"Dataset contains missing or duplicate case_id: {case_id or '<empty>'}"
                )
            seen_case_ids.add(case_id)
        evaluator_manifest = sorted(
            [
                {
                    key: evaluator.get(key)
                    for key in (
                        "evaluator_id",
                        "name",
                        "evaluator_type",
                        "rubric",
                        "version",
                        "sampling_config",
                        "filter_config",
                        "metadata",
                    )
                }
                for evaluator in evaluator_snapshots
            ],
            key=lambda item: str(item.get("evaluator_id") or ""),
        )
        dataset_manifest_hash = _canonical_hash(frozen_dataset)
        evaluator_suite_hash = _canonical_hash(evaluator_manifest)
        public_snapshot = {
            **target_snapshot,
            "run_mode": "live_candidate",
            "repetitions": repetitions,
            "evaluator_ids": [item.get("evaluator_id") for item in evaluator_manifest],
            "dataset_manifest_hash": dataset_manifest_hash,
            "evaluator_suite_hash": evaluator_suite_hash,
        }
        candidate_ref = await self.freeze_eval_model_ref(
            tenant_id=tenant_id, model_id=str(execution_config.get("model_id") or ""),
            provider_id=execution_config.get("provider_id"),
        )
        evaluator_manifest = [await self.freeze_eval_judge(tenant_id=tenant_id, evaluator=item)
                              for item in evaluator_manifest]
        evaluator_suite_hash = _canonical_hash(evaluator_manifest)
        public_snapshot["evaluator_suite_hash"] = evaluator_suite_hash
        execution_config = {**execution_config, "model_id": candidate_ref["model_id"],
                            "provider_id": candidate_ref["provider_id"], "model_ref": candidate_ref}
        candidate_fingerprint = {**candidate_fingerprint, "model_ref": candidate_ref}
        private_config = {
            **execution_config,
            "evaluators": evaluator_manifest,
        }

        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_outbox_claim(conn)
            run = await conn.fetchrow(
                """
                INSERT INTO eval_experiment_runs (
                    experiment_id, tenant_id, evaluator_id, dataset_id, status,
                    run_mode, repetitions, baseline_run_id,
                    dataset_manifest_hash, evaluator_suite_hash,
                    candidate_fingerprint, execution_config,
                    target_snapshot, metrics, created_by
                ) VALUES (
                    $1::uuid, $2, $3::uuid, $4::uuid, 'queued',
                    'live_candidate', $5, $6::uuid,
                    $7, $8, $9::jsonb, $10::jsonb,
                    $11::jsonb, '{}'::jsonb, $12
                )
                RETURNING *
                """,
                experiment_id,
                tenant_id,
                evaluator_manifest[0]["evaluator_id"],
                dataset_id,
                repetitions,
                baseline_run_id,
                dataset_manifest_hash,
                evaluator_suite_hash,
                self._json_dumps(candidate_fingerprint),
                self._json_dumps(private_config),
                self._json_dumps(public_snapshot),
                created_by,
            )
            run_id = str(run["run_id"])
            rows = [
                (
                    run_id,
                    tenant_id,
                    case["case_id"],
                    case.get("example_id"),
                    trial_index,
                    self._json_dumps(case["input"]),
                    self._json_dumps(case["expected_output"]),
                    self._json_dumps(case["expected_trajectory"]),
                    self._json_dumps(case["assertions"]),
                    self._json_dumps(case["metadata"]),
                )
                for case in manifest
                for trial_index in range(1, repetitions + 1)
            ]
            await conn.executemany(
                """
                INSERT INTO eval_experiment_run_cases (
                    run_id, tenant_id, case_id, example_id, trial_index,
                    input, expected_output, expected_trajectory, assertions, metadata
                ) VALUES (
                    $1::uuid, $2, $3, $4::uuid, $5,
                    $6::jsonb, $7::jsonb, $8::jsonb, $9::jsonb, $10::jsonb
                )
                """,
                rows,
            )
            job = await conn.fetchrow(
                """
                INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
                VALUES (
                    $1,
                    'eval.evaluator.run',
                    jsonb_build_object(
                        'run_id', $2::text,
                        'experiment_id', $3::text,
                        'dataset_id', $4::text,
                        'evaluator_id', $5::text,
                        'evaluator_ids', $6::jsonb,
                        'run_mode', 'live_candidate',
                        'trace_family', 'assistant'
                    )
                )
                RETURNING *
                """,
                tenant_id,
                run_id,
                experiment_id,
                dataset_id,
                evaluator_manifest[0]["evaluator_id"],
                self._json_dumps([item["evaluator_id"] for item in evaluator_manifest]),
            )
        return {"job_id": str(job["job_id"]), "status": "queued", "run_id": run_id}

    async def list_experiment_run_cases(
        self,
        *,
        tenant_id: str,
        run_id: str,
        statuses: tuple[str, ...] | None = None,
    ) -> list[dict[str, Any]]:
        params: list[Any] = [tenant_id, run_id]
        status_sql = ""
        if statuses:
            params.append(list(statuses))
            status_sql = f" AND status = ANY(${len(params)}::varchar[])"
        rows = await self.fetch(
            f"""
            SELECT *
            FROM eval_experiment_run_cases
            WHERE tenant_id = $1 AND run_id = $2::uuid{status_sql}
            ORDER BY case_id, trial_index
            """,
            *params,
        )
        return [self._decode_eval_row(row) for row in rows]

    async def retry_failed_experiment_cases(
        self, *, tenant_id: str, run_id: str, case_ids: list[str], created_by: str,
        idempotency_key: str,
    ) -> dict[str, Any] | None:
        """Queue a separate one-attempt run using selected frozen case and candidate snapshots."""
        request_hash = hashlib.sha256(
            f"{tenant_id}:{created_by}:{run_id}:{idempotency_key}".encode()
        ).hexdigest()
        async with self._pool.acquire() as conn, conn.transaction():
            source_row = await conn.fetchrow(
                """SELECT * FROM eval_experiment_runs
                   WHERE tenant_id = $1 AND run_id = $2::uuid FOR UPDATE""",
                tenant_id, run_id,
            )
            if not source_row:
                return None
            source = self._decode_eval_row(dict(source_row))
            if source.get("run_mode") != "live_candidate" or source.get("status") not in {
                "succeeded", "failed", "cancelled",
            }:
                raise ValueError("eval_retry_requires_terminal_live_run")
            existing = await conn.fetchrow(
                """SELECT run_id, status, target_snapshot FROM eval_experiment_runs
                   WHERE tenant_id = $1 AND target_snapshot->>'retry_of_run_id' = $2
                   AND target_snapshot->>'retry_request_key_hash' = $3
                   LIMIT 1""",
                tenant_id, run_id, request_hash,
            )
            if existing:
                existing_snapshot = self._decode_json(existing["target_snapshot"], default={})
                if sorted(existing_snapshot.get("retry_case_ids") or []) != sorted(case_ids):
                    raise ValueError("eval_retry_idempotency_conflict")
                return {
                    "job_id": str(existing_snapshot["retry_job_id"]),
                    "run_id": str(existing["run_id"]),
                    "status": str(existing["status"]),
                }
            # A byte-for-byte idempotent replay only reads the already accepted
            # receipt. New attempts must still recheck current source grants.
            source_snapshot = source.get("target_snapshot") or {}
            if source_snapshot.get("dataset_kb_linked") is not False:
                raise ValueError("eval_retry_dataset_provenance_unverified")
            dataset = await conn.fetchrow(
                """SELECT metadata FROM eval_datasets
                   WHERE tenant_id = $1 AND dataset_id = $2::uuid""",
                tenant_id, source.get("dataset_id"),
            )
            if not dataset:
                raise ValueError("eval_retry_dataset_unavailable")
            dataset_metadata = self._decode_json(dataset.get("metadata"), default={})
            if dataset_metadata.get("kb_dataset_id"):
                raise ValueError("eval_retry_kb_source_requires_fresh_authorization")
            rows = await conn.fetch(
                """SELECT * FROM eval_experiment_run_cases
                   WHERE tenant_id = $1 AND run_id = $2::uuid
                   AND case_id = ANY($3::varchar[])
                   ORDER BY case_id, trial_index FOR UPDATE""",
                tenant_id, run_id, case_ids,
            )
            frozen_cases = _retry_source_cases(
                [self._decode_eval_row(dict(row)) for row in rows], case_ids,
            )
            for case in frozen_cases:
                original_turn = (case.get("runtime_handle") or {}).get("turn_id") or case.get("candidate_trace_id")
                if original_turn and await conn.fetchval(
                    """SELECT EXISTS (
                           SELECT 1 FROM assistant_capability_executions
                           WHERE tenant_id = $1 AND run_id = $2::uuid
                         ) OR EXISTS (
                           SELECT 1 FROM assistant_runtime_items
                           WHERE tenant_id = $1 AND turn_id = $2::text
                             AND event_type IN ('compat/v1/tool_call_start',
                                                'compat/v1/tool_call_end', 'compat/v1/tool_result')
                         )""",
                    tenant_id, str(original_turn),
                ):
                    # Older Trace projections can omit tools. A terminal model
                    # receipt alone never proves the original turn had no effects.
                    raise ValueError(f"eval_retry_side_effect_unconfirmed:{case['case_id']}")
            frozen_cases.sort(key=lambda row: row["case_id"])
            manifest = [
                {
                    "case_id": row["case_id"], "example_id": row.get("example_id"),
                    "input": row["input"], "expected_output": row["expected_output"],
                    "expected_trajectory": row["expected_trajectory"],
                    "assertions": row["assertions"], "metadata": row["metadata"],
                }
                for row in frozen_cases
            ]
            manifest_hash = _canonical_hash(manifest)
            source_config = source.get("execution_config") or {}
            evaluator_ids = [
                item.get("evaluator_id") for item in source_config.get("evaluators") or []
                if isinstance(item, dict) and item.get("evaluator_id")
            ]
            if not evaluator_ids:
                raise ValueError("eval_retry_frozen_evaluator_missing")
            snapshot = {
                **source_snapshot,
                "run_mode": "live_candidate",
                "repetitions": 1,
                "dataset_manifest_hash": manifest_hash,
                "retry_of_run_id": run_id,
                "retry_request_key_hash": request_hash,
                "retry_case_ids": sorted(case_ids),
                "retry_source_run_case_ids": [row["run_case_id"] for row in frozen_cases],
            }
            fingerprint = {
                **(source.get("candidate_fingerprint") or {}), "verification": "pending",
            }
            new_run = await conn.fetchrow(
                """INSERT INTO eval_experiment_runs (
                    experiment_id, tenant_id, evaluator_id, dataset_id, status, run_mode,
                    repetitions, baseline_run_id, dataset_manifest_hash, evaluator_suite_hash,
                    candidate_fingerprint, execution_config, target_snapshot, metrics, created_by
                ) VALUES (
                    $1::uuid, $2, $3::uuid, $4::uuid, 'queued', 'live_candidate', 1,
                    $5::uuid, $6, $7, $8::jsonb, $9::jsonb, $10::jsonb, '{}'::jsonb, $11
                ) RETURNING run_id""",
                source["experiment_id"], tenant_id, source["evaluator_id"], source["dataset_id"],
                source.get("baseline_run_id"), manifest_hash, source.get("evaluator_suite_hash"),
                self._json_dumps(fingerprint), self._json_dumps(source_config),
                self._json_dumps(snapshot), created_by,
            )
            new_run_id = str(new_run["run_id"])
            await conn.executemany(
                """INSERT INTO eval_experiment_run_cases (
                    run_id, tenant_id, case_id, example_id, trial_index, input,
                    expected_output, expected_trajectory, assertions, metadata
                ) VALUES (
                    $1::uuid, $2, $3, $4::uuid, 1, $5::jsonb, $6::jsonb, $7::jsonb,
                    $8::jsonb, $9::jsonb
                )""",
                [
                    (new_run_id, tenant_id, row["case_id"], row.get("example_id"),
                     self._json_dumps(row["input"]), self._json_dumps(row["expected_output"]),
                     self._json_dumps(row["expected_trajectory"]),
                     self._json_dumps(row["assertions"]), self._json_dumps(row["metadata"]))
                    for row in frozen_cases
                ],
            )
            job = await conn.fetchrow(
                """INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
                   VALUES ($1, 'eval.evaluator.run', jsonb_build_object(
                       'run_id', $2::text, 'experiment_id', $3::text,
                       'dataset_id', $4::text, 'evaluator_id', $5::text,
                       'evaluator_ids', $6::jsonb,
                       'run_mode', 'live_candidate', 'trace_family', 'assistant'))
                   RETURNING job_id""",
                tenant_id, new_run_id, source["experiment_id"], source["dataset_id"],
                source["evaluator_id"], self._json_dumps(evaluator_ids),
            )
            await conn.execute(
                """UPDATE eval_experiment_runs
                   SET target_snapshot = jsonb_set(target_snapshot, '{retry_job_id}', to_jsonb($3::text))
                   WHERE tenant_id = $1 AND run_id = $2::uuid""",
                tenant_id, new_run_id, str(job["job_id"]),
            )
        return {"job_id": str(job["job_id"]), "run_id": new_run_id, "status": "queued"}

    async def update_experiment_run_case(
        self,
        *,
        tenant_id: str,
        run_case_id: str,
        status: str,
        candidate_trace_id: str | None = None,
        observed_metrics: dict[str, Any] | None = None,
        error_message: str | None = None,
        runtime_handle: dict[str, Any] | None = None,
        dispatch_state: str | None = None,
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            """
            UPDATE eval_experiment_run_cases
            SET status = $3,
                candidate_trace_id = COALESCE($4::uuid, candidate_trace_id),
                observed_metrics = COALESCE($5::jsonb, observed_metrics),
                error_message = $6,
                runtime_handle = COALESCE($7::jsonb, runtime_handle),
                dispatch_state = COALESCE($8, dispatch_state),
                updated_at = NOW()
            WHERE tenant_id = $1 AND run_case_id = $2::uuid
            RETURNING *
            """,
            tenant_id,
            run_case_id,
            status,
            candidate_trace_id,
            self._json_dumps(observed_metrics) if observed_metrics is not None else None,
            error_message,
            self._json_dumps(runtime_handle) if runtime_handle is not None else None,
            dispatch_state,
        )
        return self._decode_eval_row(row) if row else None

    async def create_outbox_job(
        self,
        *,
        tenant_id: str,
        job_type: str,
        payload: dict[str, Any],
    ) -> dict[str, Any]:
        row = await self.fetchrow(
            """
            INSERT INTO agent_trace_outbox (tenant_id, job_type, payload)
            VALUES ($1, $2, $3::jsonb)
            RETURNING *
            """,
            tenant_id,
            job_type,
            self._json_dumps(payload),
        )
        return self._decode_eval_row(row) if row else {}

    async def claim_outbox_jobs(
        self, *, limit: int = 1, max_attempts: int = 5,
        owner_id: str, lease_seconds: int = 60,
    ) -> list[dict[str, Any]]:
        if not self.enabled:
            return []
        async with self._pool.acquire() as conn, conn.transaction():
            # Exhausted expired claims must become terminal, never stranded running.
            await conn.execute(
                """WITH expired AS (
                    UPDATE agent_trace_outbox SET status = 'failed',
                        last_error = 'eval_outbox_attempts_exhausted', updated_at = NOW()
                    WHERE status = 'running' AND lease_until <= NOW() AND attempts >= $1
                    RETURNING tenant_id, payload, job_type
                ) UPDATE eval_experiment_runs r SET status = 'failed', finished_at = NOW(),
                    error_message = 'eval_outbox_attempts_exhausted', updated_at = NOW()
                  FROM expired e WHERE e.job_type = 'eval.evaluator.run'
                  AND r.tenant_id = e.tenant_id AND r.run_id::text = e.payload->>'run_id'""",
                max_attempts,
            )
            rows = await conn.fetch(
                """WITH picked AS (
                    SELECT job_id FROM agent_trace_outbox
                    WHERE ((status = 'queued' AND available_at <= NOW())
                        OR (status = 'running' AND lease_until <= NOW())) AND attempts < $2
                    ORDER BY available_at, created_at LIMIT $1 FOR UPDATE SKIP LOCKED
                ) UPDATE agent_trace_outbox o SET status = 'running', attempts = o.attempts + 1,
                    owner_id = $3, claim_token = gen_random_uuid(),
                    lease_until = NOW() + ($4::int * INTERVAL '1 second'),
                    heartbeat_at = NOW(), updated_at = NOW()
                  FROM picked WHERE o.job_id = picked.job_id RETURNING o.*""",
                limit, max_attempts, owner_id, max(lease_seconds, 3),
            )
        return [self._decode_eval_row(dict(row)) for row in rows]

    async def renew_outbox_claim(self, job: dict[str, Any], *, lease_seconds: int = 60) -> bool:
        if not self.enabled:
            return False
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """UPDATE agent_trace_outbox
                   SET lease_until = NOW() + ($5::int * INTERVAL '1 second'), heartbeat_at = NOW()
                   WHERE job_id = $1::uuid AND tenant_id = $2 AND owner_id = $3
                   AND claim_token = $4::uuid AND status = 'running'
                   AND lease_until > clock_timestamp() RETURNING job_id""",
                str(job["job_id"]), str(job["tenant_id"]), str(job["owner_id"]),
                str(job["claim_token"]), max(lease_seconds, 3),
            )
        return bool(row)

    async def get_candidate_runtime_evidence(
        self, *, tenant_id: str, run_case_id: str, run_id: str,
    ) -> dict[str, Any] | None:
        """Read the issued model identity and actual calls for one Eval case."""
        row = await self.fetchrow(
            """SELECT s.tenant_id, s.session_id, s.run_id,
                      l.provider_id, l.model_id, l.capability_revision,
                      s.snapshot->>'kernel_revision' AS runtime_revision,
                      s.snapshot->'pricing'->'snapshot' AS pricing_snapshot,
                      s.snapshot->'parameters' AS parameters,
                      s.snapshot->'limits' AS limits,
                      s.snapshot->'eval_fingerprint' AS eval_fingerprint,
                      a.agent_id, a.agent_version_id, a.agent_spec_hash,
                      a.runtime_fingerprint AS agent_runtime_snapshot_hash
               FROM assistant_runtime_snapshots s
               LEFT JOIN assistant.sessions a
                 ON a.session_id = s.session_id AND a.tenant_id = s.tenant_id
                AND a.user_id = s.user_id
                AND a.agent_id::text = s.snapshot->'agent_spec'->>'agentId'
                AND a.agent_version_id::text = s.snapshot->'agent_spec'->>'agentVersionId'
               JOIN assistant_runtime_model_leases l
                 ON l.snapshot_id = s.snapshot_id AND l.run_id = s.run_id
                AND l.tenant_id = s.tenant_id AND l.session_id = s.session_id
                AND l.user_id = s.user_id
               WHERE s.tenant_id = $1 AND s.session_id = $2 AND s.run_id = $3::uuid""",
            tenant_id, run_case_id, run_id,
        )
        if row is None:
            return None
        result = dict(row)
        for key in ("pricing_snapshot", "parameters", "limits", "eval_fingerprint"):
            result[key] = self._decode_json(result.get(key), default={})
        result["calls"] = await self.fetch(
            """SELECT call_id, status, input_tokens, output_tokens, cost_microusd,
                      dispatched_at, completed_at, error_code
               FROM assistant_runtime_model_calls
               WHERE tenant_id = $1 AND session_id = $2 AND run_id = $3::uuid
               ORDER BY reserved_at, call_id""",
            tenant_id, run_case_id, run_id,
        )
        result["tool_executions"] = await self.fetch(
            """SELECT execution_id, capability_id, status, effect, approval_status,
                      dispatched_at, terminal_at, error_code,
                      result_summary->>'quiz_id' AS quiz_id
               FROM assistant_capability_executions
               WHERE tenant_id = $1 AND session_id = $2 AND run_id = $3::uuid
               ORDER BY created_at, execution_id""",
            tenant_id, run_case_id, run_id,
        )
        return result

    async def reconcile_candidate_handle(
        self, *, tenant_id: str, run_case_id: str, handle: dict[str, Any],
    ) -> dict[str, Any] | None:
        # A snapshot is persisted before Runtime dispatch. Reattach to its run;
        # absence does NOT prove POST was unsent, so callers must never replay it.
        rows = await self.fetch(
            """SELECT run_id, runtime_thread_id FROM assistant_runtime_snapshots
               WHERE tenant_id = $1 AND session_id = $2 AND user_id = $3
               AND runtime_thread_id = $4::uuid ORDER BY created_at LIMIT 2""",
            tenant_id, run_case_id, str(handle.get("user_id") or ""),
            str(handle.get("thread_id") or ""),
        )
        if len(rows) != 1:
            return None
        run_id = str(rows[0]["run_id"])
        thread_id = str(rows[0]["runtime_thread_id"])
        return {**handle, "turn_id": run_id,
                "events_url": f"/api/v2/agent/threads/{thread_id}/events?after_sequence=0&turn_id={run_id}"}

    def _require_outbox_claim(self, job_id: str) -> None:
        claim = _OUTBOX_CLAIM.get()
        if claim is None or claim[0] is not self or str(claim[1].get("job_id")) != job_id:
            raise EvalLeaseLost("eval_outbox_claim_missing")

    async def mark_outbox_succeeded(self, job_id: str) -> None:
        self._require_outbox_claim(job_id)
        await self.execute(
            """
            UPDATE agent_trace_outbox
            SET status = 'succeeded', last_error = NULL, updated_at = NOW()
            WHERE job_id = $1::uuid
            """,
            job_id,
        )

    async def mark_outbox_failed(
        self,
        job_id: str,
        *,
        error: str,
        retry_after_seconds: int | None = None,
        max_attempts: int = 5,
    ) -> None:
        self._require_outbox_claim(job_id)
        if retry_after_seconds is not None:
            await self.execute(
                """
                WITH updated_job AS (
                    UPDATE agent_trace_outbox
                    SET status = CASE
                            WHEN attempts >= $4 THEN 'failed'
                            ELSE 'queued'
                        END,
                        last_error = $2,
                        available_at = CASE
                            WHEN attempts >= $4 THEN available_at
                            ELSE NOW() + ($3::int * INTERVAL '1 second')
                        END,
                        updated_at = NOW()
                    WHERE job_id = $1::uuid
                    RETURNING tenant_id, job_type, payload, status
                )
                UPDATE eval_experiment_runs r
                SET status = 'failed',
                    error_message = $2,
                    finished_at = NOW(),
                    updated_at = NOW()
                FROM updated_job j
                WHERE j.status = 'failed'
                  AND j.job_type = 'eval.evaluator.run'
                  AND r.tenant_id = j.tenant_id
                  AND r.run_id = NULLIF(j.payload->>'run_id', '')::uuid
                """,
                job_id,
                error[:4000],
                retry_after_seconds,
                max_attempts,
            )
            return
        await self.execute(
            """
            WITH updated_job AS (
                UPDATE agent_trace_outbox
                SET status = 'failed', last_error = $2, updated_at = NOW()
                WHERE job_id = $1::uuid
                RETURNING tenant_id, job_type, payload
            )
            UPDATE eval_experiment_runs r
            SET status = 'failed',
                error_message = $2,
                finished_at = NOW(),
                updated_at = NOW()
            FROM updated_job j
            WHERE j.job_type = 'eval.evaluator.run'
              AND r.tenant_id = j.tenant_id
              AND r.run_id = NULLIF(j.payload->>'run_id', '')::uuid
            """,
            job_id,
            error[:4000],
        )

    async def update_experiment_run(
        self,
        *,
        tenant_id: str,
        run_id: str,
        status: str,
        score_summary: dict[str, Any] | None = None,
        metrics: dict[str, Any] | None = None,
        error_message: str | None = None,
        mark_started: bool = False,
        mark_finished: bool = False,
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            """
            UPDATE eval_experiment_runs
            SET status = $3,
                score_summary = COALESCE($4::jsonb, score_summary),
                metrics = COALESCE($5::jsonb, metrics),
                error_message = $6,
                started_at = CASE WHEN $7 THEN COALESCE(started_at, NOW()) ELSE started_at END,
                finished_at = CASE WHEN $8 THEN NOW() ELSE finished_at END,
                updated_at = NOW()
            WHERE tenant_id = $1 AND run_id = $2::uuid
            RETURNING *
            """,
            tenant_id,
            run_id,
            status,
            self._json_dumps(score_summary) if score_summary is not None else None,
            self._json_dumps(metrics) if metrics is not None else None,
            error_message,
            mark_started,
            mark_finished,
        )
        return self._decode_eval_row(row) if row else None

    async def cancel_experiment_run(
        self, *, tenant_id: str, run_id: str, cancelled_by: str,
    ) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn, conn.transaction():
            run = await conn.fetchrow(
                "SELECT * FROM eval_experiment_runs WHERE tenant_id = $1 AND run_id = $2::uuid",
                tenant_id, run_id,
            )
            if not run:
                return None
            if run["status"] == "succeeded":
                return {"run_id": run_id, "status": run["status"], "cases": []}
            # Lock jobs before domain rows, matching the worker fencing order.
            await conn.execute(
                """UPDATE agent_trace_outbox SET status = 'cancelled', lease_until = NULL,
                   updated_at = NOW() WHERE tenant_id = $1 AND payload->>'run_id' = $2
                   AND status IN ('queued', 'running')""", tenant_id, run_id,
            )
            await conn.execute(
                """UPDATE eval_experiment_runs SET status = 'cancelled', finished_at = NOW(),
                   metrics = metrics || jsonb_build_object('cancelled_by', $3::text), updated_at = NOW()
                   WHERE tenant_id = $1 AND run_id = $2::uuid AND status IN ('queued', 'running', 'failed', 'cancelled')""",
                tenant_id, run_id, cancelled_by,
            )
            current = await conn.fetchrow(
                "SELECT status FROM eval_experiment_runs WHERE tenant_id = $1 AND run_id = $2::uuid",
                tenant_id, run_id,
            )
            if current and current["status"] == "succeeded":
                return {"run_id": run_id, "status": "succeeded", "cases": []}
            await conn.execute(
                """UPDATE eval_experiment_run_cases SET status = 'skipped',
                   observed_metrics = observed_metrics || '{"execution_outcome":"cancelled"}'::jsonb,
                   error_message = 'eval_run_cancelled', updated_at = NOW()
                   WHERE tenant_id = $1 AND run_id = $2::uuid AND status IN ('queued', 'running')""",
                tenant_id, run_id,
            )
            cases = await conn.fetch(
                "SELECT run_case_id, runtime_handle, dispatch_state FROM eval_experiment_run_cases "
                "WHERE tenant_id = $1 AND run_id = $2::uuid AND dispatch_state <> 'not_started'",
                tenant_id, run_id,
            )
        return {"run_id": run_id, "status": "cancelled", "cases": [self._decode_eval_row(dict(row)) for row in cases]}

    async def get_experiment_run(
        self,
        *,
        tenant_id: str,
        run_id: str,
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            "SELECT * FROM eval_experiment_runs WHERE tenant_id = $1 AND run_id = $2::uuid",
            tenant_id,
            run_id,
        )
        return self._decode_eval_row(row) if row else None

    async def get_experiment_run_progress(
        self,
        *,
        tenant_id: str,
        run_id: str,
    ) -> dict[str, int]:
        row = await self.fetchrow(
            """
            SELECT
                COUNT(*)::int AS total_trials,
                COUNT(*) FILTER (
                    WHERE status IN ('succeeded', 'failed', 'skipped')
                )::int AS completed_trials,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_trials
            FROM eval_experiment_run_cases
            WHERE tenant_id = $1 AND run_id = $2::uuid
            """,
            tenant_id,
            run_id,
        )
        return {
            "total_trials": int((row or {}).get("total_trials") or 0),
            "completed_trials": int((row or {}).get("completed_trials") or 0),
            "failed_trials": int((row or {}).get("failed_trials") or 0),
        }

    async def promote_experiment_baseline(
        self,
        *,
        tenant_id: str,
        experiment_id: str,
        run_id: str,
        promoted_by: str,
        expected_previous_baseline_run_id: str | None = None,
    ) -> dict[str, Any] | None:
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_outbox_claim(conn)
            experiment = await conn.fetchrow(
                """
                SELECT baseline_run_id
                FROM eval_experiments
                WHERE tenant_id = $1 AND experiment_id = $2::uuid
                FOR UPDATE
                """,
                tenant_id,
                experiment_id,
            )
            if not experiment:
                return None
            current_baseline = experiment.get("baseline_run_id")
            current_baseline_id = str(current_baseline) if current_baseline else None
            if current_baseline_id != expected_previous_baseline_run_id:
                return None
            eligible = await conn.fetchval(
                """
                SELECT EXISTS (
                    SELECT 1 FROM eval_experiment_runs
                    WHERE tenant_id = $1
                      AND experiment_id = $2::uuid
                      AND run_id = $3::uuid
                      AND run_mode = 'live_candidate'
                      AND status = 'succeeded'
                )
                """,
                tenant_id,
                experiment_id,
                run_id,
            )
            if not eligible:
                return None
            row = await conn.fetchrow(
                """
                UPDATE eval_experiments
                SET baseline_run_id = $3::uuid,
                    baseline_promoted_by = $4,
                    baseline_promoted_at = NOW(),
                    updated_at = NOW()
                WHERE tenant_id = $1 AND experiment_id = $2::uuid
                RETURNING experiment_id, baseline_run_id,
                          baseline_promoted_by, baseline_promoted_at
                """,
                tenant_id,
                experiment_id,
                run_id,
                promoted_by,
            )
            await conn.execute(
                """
                INSERT INTO eval_baseline_promotions (
                    tenant_id, experiment_id, previous_baseline_run_id,
                    baseline_run_id, promoted_by
                ) VALUES ($1, $2::uuid, $3::uuid, $4::uuid, $5)
                """,
                tenant_id,
                experiment_id,
                current_baseline,
                run_id,
                promoted_by,
            )
        result = self._decode_eval_row(row)
        previous = current_baseline
        result["previous_baseline_run_id"] = str(previous) if previous else None
        result["promoted_by"] = result.pop("baseline_promoted_by", promoted_by)
        result["promoted_at"] = result.pop("baseline_promoted_at", None)
        return result

    async def list_experiment_run_case_results(
        self,
        *,
        tenant_id: str,
        run_id: str,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        run = await self.get_experiment_run(tenant_id=tenant_id, run_id=run_id)
        if run and run.get("run_mode") == "live_candidate":
            return await self._list_live_experiment_run_case_results(
                tenant_id=tenant_id,
                run_id=run_id,
                limit=limit,
                offset=offset,
            )

        case_key_sql = """
            COALESCE(
                NULLIF(s.metadata->>'example_id', ''),
                NULLIF(s.metadata->>'case_id', ''),
                s.trace_id::text
            )
        """
        target_key_sql = """
            COALESCE(NULLIF(s.target_id, ''), s.span_id::text, s.trace_id::text)
        """
        count_row = await self.fetchrow(
            f"""
            SELECT COUNT(DISTINCT {case_key_sql})::int AS total
            FROM agent_trace_scores s
            INNER JOIN agent_traces t
                ON t.trace_id = s.trace_id AND t.tenant_id = $1
            WHERE s.metadata->>'experiment_run_id' = $2
            """,
            tenant_id,
            run_id,
        )
        total = int((count_row or {}).get("total") or 0)
        if total == 0:
            return [], 0

        rows = await self.fetch(
            f"""
            WITH ranked_scores AS (
                SELECT
                    s.*,
                    {case_key_sql} AS case_key,
                    ROW_NUMBER() OVER (
                        PARTITION BY {case_key_sql}, s.score_name,
                            s.target_type, {target_key_sql}
                        ORDER BY s.created_at DESC, s.score_id DESC
                    ) AS score_revision
                FROM agent_trace_scores s
                INNER JOIN agent_traces t
                    ON t.trace_id = s.trace_id AND t.tenant_id = $1
                WHERE s.metadata->>'experiment_run_id' = $2
            ),
            paged_cases AS (
                SELECT case_key, MAX(created_at) AS latest_created_at
                FROM ranked_scores
                WHERE score_revision = 1
                GROUP BY case_key
                ORDER BY latest_created_at DESC, case_key
                LIMIT $3 OFFSET $4
            )
            SELECT
                s.case_key,
                s.score_id,
                s.trace_id AS candidate_trace_id,
                s.score_name,
                s.target_type,
                s.target_id,
                s.span_id,
                s.numeric_value,
                s.label AS score_label,
                s.explanation AS score_explanation,
                s.score_source,
                s.metadata AS score_metadata,
                e.example_id,
                e.source_trace_id,
                e.input,
                e.expected_output,
                e.metadata AS example_metadata,
                t.trace_family,
                t.status AS trace_status,
                t.model_id,
                t.provider,
                t.total_latency_ms,
                t.total_tokens,
                t.output_preview
            FROM ranked_scores s
            INNER JOIN paged_cases p ON p.case_key = s.case_key
            INNER JOIN agent_traces t
                ON t.trace_id = s.trace_id AND t.tenant_id = $1
            LEFT JOIN eval_examples e
                ON e.tenant_id = $1
               AND e.example_id::text = NULLIF(s.metadata->>'example_id', '')
            WHERE s.score_revision = 1
            ORDER BY p.latest_created_at DESC, s.case_key, s.score_name
            """,
            tenant_id,
            run_id,
            max(1, limit),
            max(0, offset),
        )

        grouped: dict[str, dict[str, Any]] = {}
        for raw_row in rows:
            row = dict(raw_row)
            score_metadata = self._decode_json(row.get("score_metadata"), default={})
            example_metadata = self._decode_json(row.get("example_metadata"), default={})
            case_key = str(row.get("case_key") or row.get("candidate_trace_id") or "")
            case = grouped.setdefault(
                case_key,
                {
                    "example_id": str(
                        row.get("example_id") or score_metadata.get("example_id") or ""
                    )
                    or None,
                    "case_id": str(
                        score_metadata.get("case_id")
                        or example_metadata.get("case_id")
                        or row.get("example_id")
                        or case_key
                    ),
                    "candidate_trace_id": str(row.get("candidate_trace_id") or ""),
                    "source_trace_id": str(row.get("source_trace_id") or "") or None,
                    "status": "unscored",
                    "aggregate_score": None,
                    "failure_reason": None,
                    "input": self._decode_json(row.get("input"), default={}),
                    "expected_output": self._decode_json(row.get("expected_output"), default={}),
                    "trace": {
                        "trace_family": row.get("trace_family"),
                        "status": row.get("trace_status"),
                        "model_id": row.get("model_id"),
                        "provider": row.get("provider"),
                        "total_latency_ms": int(row.get("total_latency_ms") or 0),
                        "total_tokens": int(row.get("total_tokens") or 0),
                        "output_preview": row.get("output_preview") or "",
                    },
                    "scores": [],
                },
            )
            case["scores"].append(
                {
                    "score_name": row.get("score_name"),
                    "target_type": row.get("target_type"),
                    "target_id": row.get("target_id"),
                    "span_id": str(row.get("span_id") or "") or None,
                    "numeric_value": row.get("numeric_value"),
                    "label": row.get("score_label"),
                    "explanation": row.get("score_explanation") or "",
                    "score_source": row.get("score_source"),
                    "failure_kind": score_metadata.get("failure_kind"),
                }
            )

        cases = list(grouped.values())
        for case in cases:
            scores = case["scores"]
            numeric_scores = [
                float(score["numeric_value"])
                for score in scores
                if score.get("label") in {"pass", "fail"}
                and isinstance(score.get("numeric_value"), int | float)
                and not isinstance(score.get("numeric_value"), bool)
            ]
            case["aggregate_score"] = (
                round(sum(numeric_scores) / len(numeric_scores), 4) if numeric_scores else None
            )
            failed = next(
                (
                    score
                    for score in scores
                    if score.get("label") == "fail" or score.get("failure_kind") == "infrastructure"
                ),
                None,
            )
            review = next(
                (score for score in scores if score.get("label") == "review"),
                None,
            )
            if failed:
                case["status"] = "failed"
                case["failure_reason"] = failed.get("explanation") or "Evaluation failed"
            elif review:
                case["status"] = "review"
                case["failure_reason"] = review.get("explanation") or "Manual review required"
            elif any(score.get("label") == "pass" for score in scores):
                case["status"] = "passed"
        return cases, total

    async def _list_live_experiment_run_case_results(
        self,
        *,
        tenant_id: str,
        run_id: str,
        limit: int,
        offset: int,
    ) -> tuple[list[dict[str, Any]], int]:
        count_row = await self.fetchrow(
            """
            SELECT COUNT(DISTINCT case_id)::int AS total
            FROM eval_experiment_run_cases
            WHERE tenant_id = $1 AND run_id = $2::uuid
            """,
            tenant_id,
            run_id,
        )
        total = int((count_row or {}).get("total") or 0)
        if total == 0:
            return [], 0
        rows = await self.fetch(
            """
            WITH paged_cases AS (
                SELECT case_id, MIN(created_at) AS first_created_at
                FROM eval_experiment_run_cases
                WHERE tenant_id = $1 AND run_id = $2::uuid
                GROUP BY case_id
                ORDER BY first_created_at, case_id
                LIMIT $3 OFFSET $4
            )
            SELECT
                c.*,
                t.trace_family,
                t.status AS trace_status,
                t.model_id,
                t.provider
            FROM eval_experiment_run_cases c
            INNER JOIN paged_cases p ON p.case_id = c.case_id
            LEFT JOIN agent_traces t
                ON t.tenant_id = c.tenant_id
               AND t.trace_id = c.candidate_trace_id
            WHERE c.tenant_id = $1 AND c.run_id = $2::uuid
            ORDER BY p.first_created_at, c.case_id, c.trial_index
            """,
            tenant_id,
            run_id,
            max(1, limit),
            max(0, offset),
        )
        decoded = [self._decode_eval_row(dict(row)) for row in rows]
        cases = _aggregate_live_case_rows(decoded)
        public_cases: list[dict[str, Any]] = []
        for case in cases.values():
            failure_reason = "; ".join(case["errors"] or case["contract_failures"]) or None
            if failure_reason is None and case["execution_status"] in {"cancelled", "skipped"}:
                failure_reason = f"Trial {case['execution_status']} before scoring"
            public_cases.append(
                {
                    "example_id": case["example_id"],
                    "case_id": case["case_id"],
                    "candidate_trace_id": case["candidate_trace_id"] or "",
                    "status": case["status"],
                    "execution_status": case["execution_status"],
                    "aggregate_score": case["aggregate_score"],
                    "failure_reason": failure_reason,
                    "input": case["input"],
                    "expected_output": case["expected_output"],
                    "trial_count": case["trial_count"],
                    "score_stddev": case["score_stddev"],
                    "flaky": case["flaky"],
                    "observed_metrics": case["observed_metrics"],
                    "trace": case["trace"],
                    "scores": [],
                }
            )
        return public_cases, total

    async def compare_experiment_runs(
        self,
        *,
        tenant_id: str,
        baseline_run_id: str,
        candidate_run_id: str,
    ) -> dict[str, Any] | None:
        baseline = await self.get_experiment_run(tenant_id=tenant_id, run_id=baseline_run_id)
        candidate = await self.get_experiment_run(tenant_id=tenant_id, run_id=candidate_run_id)
        if not baseline or not candidate:
            return None
        baseline_summary = baseline.get("score_summary") or {}
        candidate_summary = candidate.get("score_summary") or {}
        baseline_metrics = baseline.get("metrics") or {}
        candidate_metrics = candidate.get("metrics") or {}
        reasons: list[str] = []
        if not _has_versioned_gate_metrics(baseline_summary):
            reasons.append("baseline_gate_metrics_unverifiable")
        if not _has_versioned_gate_metrics(candidate_summary):
            reasons.append("candidate_gate_metrics_unverifiable")
        if (
            baseline.get("run_mode") != "live_candidate"
            or candidate.get("run_mode") != "live_candidate"
        ):
            reasons.append("legacy_unverified")
        if baseline.get("status") != "succeeded" or candidate.get("status") != "succeeded":
            reasons.append("run_not_succeeded")
        if baseline.get("experiment_id") != candidate.get("experiment_id"):
            reasons.append("different_experiment")
        for field, reason in (
            ("dataset_manifest_hash", "dataset_manifest_mismatch"),
            ("evaluator_suite_hash", "evaluator_suite_mismatch"),
        ):
            left = baseline.get(field)
            right = candidate.get(field)
            if not left or not right or left != right:
                reasons.append(reason)
        if int(baseline.get("repetitions") or 1) != int(candidate.get("repetitions") or 1):
            reasons.append("trial_plan_mismatch")
        if baseline_metrics.get("mixed_runtime") or candidate_metrics.get("mixed_runtime"):
            reasons.append("mixed_runtime_fingerprint")

        baseline_rows = await self.list_experiment_run_cases(
            tenant_id=tenant_id,
            run_id=baseline_run_id,
        )
        candidate_rows = await self.list_experiment_run_cases(
            tenant_id=tenant_id,
            run_id=candidate_run_id,
        )
        if any(
            row.get("status") not in {"succeeded", "failed", "skipped"}
            for row in [*baseline_rows, *candidate_rows]
        ):
            reasons.append("incomplete_run_cases")
        baseline_cases = _aggregate_live_case_rows(baseline_rows)
        candidate_cases = _aggregate_live_case_rows(candidate_rows)
        if any(
            case["status"] == "unscored"
            for case in [*baseline_cases.values(), *candidate_cases.values()]
        ):
            reasons.append("unscored_case_results")
        if set(baseline_cases) != set(candidate_cases) or not baseline_cases:
            reasons.append("case_set_mismatch")
        else:
            for case_id in baseline_cases:
                if (
                    baseline_cases[case_id]["trial_count"]
                    != candidate_cases[case_id]["trial_count"]
                ):
                    reasons.append("trial_plan_mismatch")
                    break

        baseline_fingerprint = (
            baseline_metrics.get("actual_fingerprint")
            if isinstance(baseline_metrics.get("actual_fingerprint"), dict)
            else {}
        )
        candidate_fingerprint = (
            candidate_metrics.get("actual_fingerprint")
            if isinstance(candidate_metrics.get("actual_fingerprint"), dict)
            else {}
        )
        required_fingerprint_keys = (
            "system_prompt_hash",
            "tool_schema_hash",
            "model_id",
            "provider",
            "runtime_revision",
        )
        if any(not baseline_fingerprint.get(key) for key in required_fingerprint_keys) or any(
            not candidate_fingerprint.get(key) for key in required_fingerprint_keys
        ):
            reasons.append("missing_runtime_fingerprint")
        for run, fingerprint in ((baseline, baseline_fingerprint), (candidate, candidate_fingerprint)):
            snapshot = run.get("target_snapshot") if isinstance(run.get("target_snapshot"), dict) else {}
            if snapshot.get("candidate_type") != "agent_version":
                continue
            if any(
                not snapshot.get(key) or snapshot.get(key) != fingerprint.get(key)
                for key in (
                    "agent_id", "agent_version_id", "agent_spec_hash",
                    "agent_runtime_snapshot_hash",
                )
            ):
                reasons.append("agent_version_fingerprint_mismatch")
        if (baseline.get("target_snapshot") or {}).get("candidate_type") != (
            candidate.get("target_snapshot") or {}
        ).get("candidate_type"):
            reasons.append("candidate_type_mismatch")

        fingerprint_dimensions = {
            "agent_version": (
                "agent_id", "agent_version_id", "agent_spec_hash",
                "agent_runtime_snapshot_hash",
            ),
            "prompt": ("system_prompt_hash",),
            "tools": ("tool_schema_hash",),
            "model": ("model_id",),
            "provider": ("provider",),
            "sampling": ("sampling",),
            "runtime": ("runtime_revision",),
            "rag": ("rag_config_hash", "rag_revision_hash"),
            "execution_policy": ("execution_policy",),
        }
        changed_dimensions = [
            dimension
            for dimension, keys in fingerprint_dimensions.items()
            if any(baseline_fingerprint.get(key) != candidate_fingerprint.get(key) for key in keys)
        ]
        metric_specs = {
            "quality_score": ("overall_score", "higher"),
            "behavior_pass_rate": ("behavior_pass_rate", "higher"),
            "critical_pass_rate": ("critical_pass_rate", "higher"),
            "flaky_rate": ("flaky_rate", "lower"),
            "latency_ms": ("latency_p50_ms", "lower"),
            "latency_p95_ms": ("latency_p95_ms", "lower"),
            "input_tokens_per_task": ("input_tokens_per_task", "lower"),
            "output_tokens_per_task": ("output_tokens_per_task", "lower"),
            "total_tokens_per_task": ("total_tokens_per_task", "lower"),
            "cost_per_task_cents": ("cost_per_task_cents", "lower"),
            "execution_error_rate": ("execution_error_rate", "lower"),
            "behavior_failure_rate": ("behavior_failure_rate", "lower"),
        }

        def _metric_value(
            run_summary: dict[str, Any], run_metrics: dict[str, Any], key: str
        ) -> float | None:
            summary_value = _known_number(run_summary.get(key))
            return (
                summary_value if summary_value is not None else _known_number(run_metrics.get(key))
            )

        metric_diffs: dict[str, dict[str, Any]] = {}
        deltas: dict[str, float | None] = {}
        for public_key, (stored_key, direction) in metric_specs.items():
            left = _metric_value(baseline_summary, baseline_metrics, stored_key)
            right = _metric_value(candidate_summary, candidate_metrics, stored_key)
            delta = round(right - left, 4) if left is not None and right is not None else None
            status = "unknown"
            if delta is not None:
                signed = delta if direction == "higher" else -delta
                status = "improved" if signed > 0 else "regressed" if signed < 0 else "unchanged"
            metric_diffs[public_key] = {
                "baseline": left,
                "candidate": right,
                "delta": delta,
                "direction": direction,
                "status": status,
            }
            deltas[public_key] = delta
            deltas.setdefault(stored_key, delta)

        if (
            metric_diffs["quality_score"]["baseline"] is None
            or metric_diffs["quality_score"]["candidate"] is None
        ):
            reasons.append("missing_quality_score")
        if (
            metric_diffs["execution_error_rate"]["baseline"] is None
            or metric_diffs["execution_error_rate"]["candidate"] is None
        ):
            reasons.append("missing_execution_error_rate")
        if (
            metric_diffs["latency_ms"]["baseline"] is None
            or metric_diffs["latency_ms"]["candidate"] is None
        ):
            reasons.append("missing_latency")
        attribution = (
            "unverifiable"
            if reasons
            else "repeatability"
            if not changed_dimensions
            else "isolated_change"
            if len(changed_dimensions) == 1
            else "confounded"
        )

        case_diffs: list[dict[str, Any]] = []
        paired_score_deltas: list[float] = []
        for case_id in sorted(set(baseline_cases) & set(candidate_cases)):
            left = baseline_cases[case_id]
            right = candidate_cases[case_id]
            left_score = _known_number(left.get("aggregate_score"))
            right_score = _known_number(right.get("aggregate_score"))
            score_delta = (
                round(right_score - left_score, 4)
                if left_score is not None and right_score is not None
                else None
            )
            if score_delta is not None and left["status"] != "unscored" and right["status"] != "unscored":
                paired_score_deltas.append(score_delta)
            if left["status"] == "unscored" or right["status"] == "unscored":
                classification = "unscored"
            elif left["behavior_pass"] is True and right["behavior_pass"] is False:
                classification = "regressed"
            elif left["behavior_pass"] is False and right["behavior_pass"] is True:
                classification = "improved"
            elif left["behavior_pass"] is False and right["behavior_pass"] is False:
                classification = "same_failure"
            elif score_delta is not None and score_delta < -0.02:
                classification = "regressed"
            elif score_delta is not None and score_delta > 0.02:
                classification = "improved"
            elif right["flaky"]:
                classification = "flaky"
            else:
                classification = "unchanged"
            left_tools = [
                {"name": item.get("name"), "status": item.get("status")}
                for item in left.get("tool_trajectory") or []
                if isinstance(item, dict)
            ]
            right_tools = [
                {"name": item.get("name"), "status": item.get("status")}
                for item in right.get("tool_trajectory") or []
                if isinstance(item, dict)
            ]
            tool_diffs = (
                [{"type": "trajectory_changed", "baseline": left_tools, "candidate": right_tools}]
                if left_tools != right_tools
                else []
            )
            left_rag = left.get("rag_evidence") or []
            right_rag = right.get("rag_evidence") or []
            rag_diffs = (
                [{"type": "evidence_changed", "baseline": left_rag, "candidate": right_rag}]
                if left_rag != right_rag
                else []
            )
            case_diffs.append(
                {
                    "case_id": case_id,
                    "status": classification,
                    "baseline_quality_status": left["status"],
                    "candidate_quality_status": right["status"],
                    "baseline_execution_status": left["execution_status"],
                    "candidate_execution_status": right["execution_status"],
                    "critical": right["critical"],
                    "baseline_score": left_score,
                    "candidate_score": right_score,
                    "score_delta": score_delta,
                    "baseline_trace_id": left.get("candidate_trace_id"),
                    "candidate_trace_id": right.get("candidate_trace_id"),
                    "baseline_output": left.get("output_preview") or "",
                    "candidate_output": right.get("output_preview") or "",
                    "baseline_metrics": left.get("observed_metrics") or {},
                    "candidate_metrics": right.get("observed_metrics") or {},
                    "baseline_trial_count": left["trial_count"],
                    "candidate_trial_count": right["trial_count"],
                    "flaky": right["flaky"],
                    "failure_reason": "; ".join(right["errors"] or right["contract_failures"])
                    or None,
                    "tool_diffs": tool_diffs,
                    "rag_diffs": rag_diffs,
                }
            )

        rank = {"regressed": 0, "unscored": 1, "flaky": 2, "same_failure": 3, "improved": 4, "unchanged": 5}
        case_diffs.sort(key=lambda item: (rank.get(str(item["status"]), 9), str(item["case_id"])))
        confidence_interval = _paired_bootstrap_ci(paired_score_deltas) if not reasons else None
        evidence_status = "insufficient_evidence"
        if reasons:
            evidence_status = "unverifiable"
        elif len(paired_score_deltas) >= 10 and confidence_interval:
            evidence_status = (
                "improvement"
                if confidence_interval[0] > 0
                else "regression"
                if confidence_interval[1] < 0
                else "inconclusive"
            )

        gate_failures = list(dict.fromkeys(reasons))
        gate_warnings: list[str] = []
        critical_flips = [
            case_id
            for case_id in sorted(set(baseline_cases) & set(candidate_cases))
            if candidate_cases[case_id]["critical"]
            and baseline_cases[case_id]["behavior_pass"] is True
            and candidate_cases[case_id]["behavior_pass"] is False
        ]
        if critical_flips:
            gate_failures.append("critical_case_regression")
        if any(
            case["critical"] and case["behavior_pass"] is False
            for case in candidate_cases.values()
        ):
            gate_failures.append("candidate_critical_case_failed")
        quality_delta = deltas.get("quality_score")
        if quality_delta is not None and quality_delta < -0.02:
            gate_failures.append("quality_regression")
        baseline_errors = _known_number(baseline_metrics.get("failed_trials"))
        candidate_errors = _known_number(candidate_metrics.get("failed_trials"))
        error_delta = deltas.get("execution_error_rate")
        if (
            baseline_errors is not None
            and candidate_errors is not None
            and candidate_errors > baseline_errors
        ) or (error_delta is not None and error_delta > 0):
            gate_failures.append("execution_error_regression")

        performance_assertions = {
            "latency_ms_lt": "latency_ms",
            "total_tokens_lt": "total_tokens",
            "cost_cents_lt": "cost_cents",
        }
        for row in candidate_rows:
            observed = row.get("observed_metrics") or {}
            for assertion in row.get("assertions") or []:
                if not isinstance(assertion, dict):
                    continue
                metric_key = performance_assertions.get(str(assertion.get("type") or ""))
                if not metric_key:
                    continue
                actual = _known_number(observed.get(metric_key))
                limit = _known_number(assertion.get("value"))
                if actual is None or limit is None or actual >= limit:
                    gate_failures.append("explicit_performance_constraint_failed")
                    break

        for metric_key in ("latency_ms", "total_tokens_per_task", "cost_per_task_cents"):
            if metric_diffs[metric_key]["status"] == "regressed":
                gate_warnings.append(f"{metric_key}_increased")
        if evidence_status in {"insufficient_evidence", "inconclusive"}:
            gate_warnings.append(evidence_status)
        if any(item["status"] == "regressed" and not item["critical"] for item in case_diffs):
            gate_warnings.append("noncritical_case_regressions")
        if any(item["flaky"] for item in case_diffs):
            gate_warnings.append("flaky_cases_present")
        typed_agent_pair = all(
            (run.get("target_snapshot") or {}).get("candidate_type") == "agent_version"
            for run in (baseline, candidate)
        )
        if typed_agent_pair:
            gate_warnings.append("fixed_sample_only")
        gate_failures = list(dict.fromkeys(gate_failures))
        gate_warnings = list(dict.fromkeys(gate_warnings))
        regressed_cases = [item for item in case_diffs if item["status"] == "regressed"]
        regression_summary = {
            "baseline_status": baseline.get("status"),
            "candidate_status": candidate.get("status"),
            "regressed_metrics": [
                key for key, item in metric_diffs.items() if item["status"] == "regressed"
            ],
            "regressed_case_count": len(regressed_cases),
            "improved_case_count": sum(1 for item in case_diffs if item["status"] == "improved"),
            "unchanged_case_count": sum(1 for item in case_diffs if item["status"] == "unchanged"),
            "flaky_case_count": sum(1 for item in case_diffs if item["flaky"]),
            "same_failure_case_count": sum(1 for item in case_diffs if item["status"] == "same_failure"),
            "unscored_case_count": sum(1 for item in case_diffs if item["status"] == "unscored"),
            "critical_regressions": critical_flips,
            "attribution_status": attribution,
        }
        return {
            "baseline_run_id": baseline_run_id,
            "candidate_run_id": candidate_run_id,
            "baseline_summary": baseline_summary,
            "candidate_summary": candidate_summary,
            "compatibility": {
                "status": "compatible" if not reasons else "incompatible",
                "compatible": not reasons,
                "reasons": list(dict.fromkeys(reasons)),
            },
            "changed_dimensions": changed_dimensions,
            "attribution": attribution,
            "deltas": deltas,
            "metric_diffs": metric_diffs,
            "regression_summary": regression_summary,
            "statistics": {
                "paired_case_count": len(paired_score_deltas),
                "quality_delta_ci_95": confidence_interval,
                "evidence_status": evidence_status,
                "wins": sum(1 for value in paired_score_deltas if value > 0.02),
                "ties": sum(1 for value in paired_score_deltas if -0.02 <= value <= 0.02),
                "losses": sum(1 for value in paired_score_deltas if value < -0.02),
                "seed": 42,
            },
            "gate": {
                "status": "fail" if gate_failures else "warning" if typed_agent_pair else "pass",
                "failures": gate_failures,
                "warnings": gate_warnings,
            },
            "case_diffs": case_diffs,
        }

    async def get_dashboard(
        self,
        *,
        tenant_id: str,
        days: int = 7,
    ) -> dict[str, Any]:
        summary = await self.get_summary(tenant_id=tenant_id, days=days)
        counts = await self.fetchrow(
            """
            SELECT
                (SELECT COUNT(*) FROM eval_datasets WHERE tenant_id = $1)::int AS dataset_count,
                (SELECT COUNT(*) FROM eval_examples WHERE tenant_id = $1)::int AS example_count,
                (SELECT COUNT(*) FROM eval_evaluators WHERE tenant_id = $1)::int AS evaluator_count,
                (SELECT COUNT(*) FROM eval_experiments WHERE tenant_id = $1)::int AS experiment_count,
                (SELECT COUNT(*) FROM eval_experiment_runs WHERE tenant_id = $1)::int AS run_count,
                (SELECT COUNT(*) FROM eval_examples
                 WHERE tenant_id = $1 AND metadata->>'review_status' = 'pending')::int AS judge_pending_count
            """,
            tenant_id,
        )
        run_health = await self.fetchrow(
            """
            SELECT
                COUNT(*) FILTER (WHERE status = 'queued')::int AS queued_runs,
                COUNT(*) FILTER (WHERE status = 'running')::int AS running_runs,
                COUNT(*) FILTER (WHERE status = 'succeeded')::int AS succeeded_runs,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_runs
            FROM eval_experiment_runs
            WHERE tenant_id = $1
            """,
            tenant_id,
        )
        queue_health = await self.fetchrow(
            """
            SELECT
                COUNT(*) FILTER (WHERE status = 'queued')::int AS queued_jobs,
                COUNT(*) FILTER (WHERE status = 'running')::int AS running_jobs,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_jobs
            FROM agent_trace_outbox
            WHERE tenant_id = $1
            """,
            tenant_id,
        )
        runtime_health = await self.fetchrow(
            """
            SELECT
                COUNT(*) FILTER (WHERE trace_family = 'assistant')::int AS assistant_captured_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant' AND status = 'succeeded')::int AS assistant_succeeded_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant' AND status = 'failed')::int AS assistant_failed_traces,
                COUNT(*) FILTER (WHERE trace_family = 'rag')::int AS rag_captured_traces,
                COUNT(*) FILTER (WHERE trace_family = 'langgraph_proxy')::int AS langgraph_captured_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant' AND metadata ? 'runtime_trajectory')::int AS runtime_trajectory_traces,
                COUNT(*) FILTER (
                    WHERE trace_family = 'assistant'
                      AND COALESCE(metadata->'runtime_trajectory'->'trace_writer_health'->>'issue_count', '0')::int > 0
                )::int AS trace_writer_issue_traces,
                COUNT(*) FILTER (
                    WHERE trace_family = 'assistant'
                      AND COALESCE(metadata->'runtime_trajectory'->>'exit_reason', '') IN (
                          'tool_error',
                          'approval_denied',
                          'approval_required',
                          'max_iterations',
                          'interrupted',
                          'cancelled',
                          'timeout'
                      )
                )::int AS critical_runtime_failures
            FROM agent_traces
            WHERE tenant_id = $1
              AND created_at >= NOW() - make_interval(days => $2::int)
            """,
            tenant_id,
            max(1, days),
        )
        tool_safety_row = await self.fetchrow(
            """
            SELECT COUNT(DISTINCT s.trace_id)::int AS tool_safety_failures
            FROM agent_trace_spans s
            INNER JOIN agent_traces t ON t.trace_id = s.trace_id
            WHERE t.tenant_id = $1
              AND t.created_at >= NOW() - make_interval(days => $2::int)
              AND (
                  s.attributes->>'direct_registry_denied' = 'true'
                  OR COALESCE(s.attributes->'gateway_policy_decision'->>'decision', '') IN ('deny', 'denied', 'blocked')
                  OR COALESCE(s.attributes->'sandbox_decision'->>'decision', '') IN ('deny', 'denied', 'blocked')
                  OR COALESCE(s.attributes->'sandbox_decision'->>'available', 'true') = 'false'
              )
            """,
            tenant_id,
            max(1, days),
        )
        latest_run_row = await self.fetchrow(
            """
            SELECT
                (
                    SELECT COALESCE(target_snapshot->>'candidate_label', run_id::text)
                    FROM eval_experiment_runs
                    WHERE tenant_id = $1
                      AND COALESCE(target_snapshot->>'candidate_label', '') ILIKE '%baseline%'
                    ORDER BY created_at DESC
                    LIMIT 1
                ) AS latest_baseline,
                (
                    SELECT COALESCE(target_snapshot->>'candidate_label', run_id::text)
                    FROM eval_experiment_runs
                    WHERE tenant_id = $1
                      AND (
                          COALESCE(target_snapshot->>'candidate_label', '') ILIKE '%candidate%'
                          OR COALESCE(target_snapshot->>'candidate_label', '') = ''
                      )
                    ORDER BY created_at DESC
                    LIMIT 1
                ) AS latest_candidate
            """,
            tenant_id,
        )
        metrics = {**summary, **dict(counts or {})}
        runtime_metrics = dict(runtime_health or {})
        assistant_total = int(runtime_metrics.get("assistant_captured_traces") or 0)
        assistant_succeeded = int(runtime_metrics.get("assistant_succeeded_traces") or 0)
        runtime_trajectory_traces = int(runtime_metrics.get("runtime_trajectory_traces") or 0)
        runtime_metrics["tool_safety_failures"] = int(
            (tool_safety_row or {}).get("tool_safety_failures") or 0
        )
        runtime_metrics["pass_rate"] = (
            round(assistant_succeeded / assistant_total, 4) if assistant_total else 0
        )
        runtime_metrics["trajectory_pass_rate"] = (
            round(runtime_trajectory_traces / assistant_total, 4) if assistant_total else 0
        )
        runtime_metrics["assistant_status"] = "enabled"
        runtime_metrics["rag_status"] = (
            "wired" if int(runtime_metrics.get("rag_captured_traces") or 0) > 0 else "partial"
        )
        runtime_metrics["langgraph_status"] = (
            "wired" if int(runtime_metrics.get("langgraph_captured_traces") or 0) > 0 else "partial"
        )
        metrics["latest_baseline"] = (latest_run_row or {}).get("latest_baseline")
        metrics["latest_candidate"] = (latest_run_row or {}).get("latest_candidate")
        metrics["pass_rate"] = runtime_metrics["pass_rate"]
        metrics["trajectory_pass_rate"] = runtime_metrics["trajectory_pass_rate"]
        metrics["critical_failures"] = (
            int(runtime_metrics.get("critical_runtime_failures") or 0)
            + runtime_metrics["tool_safety_failures"]
        )
        metrics["tool_safety_failures"] = runtime_metrics["tool_safety_failures"]
        return {
            "metrics": metrics,
            "run_health": dict(run_health or {}),
            "queue_health": dict(queue_health or {}),
            "runtime_health": runtime_metrics,
            "latest_gate_status": {"status": "not_run", "source": "offline"},
        }

    async def get_evaluator(
        self,
        *,
        tenant_id: str,
        evaluator_id: str,
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            "SELECT * FROM eval_evaluators WHERE tenant_id = $1 AND evaluator_id = $2::uuid",
            tenant_id,
            evaluator_id,
        )
        return self._decode_eval_row(row) if row else None

    async def list_datasets(
        self,
        *,
        tenant_id: str,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        count_row = await self.fetchrow(
            "SELECT COUNT(*) AS total FROM eval_datasets WHERE tenant_id = $1",
            tenant_id,
        )
        total = int(count_row.get("total") or 0) if count_row else 0
        rows = await self.fetch(
            """
            SELECT * FROM eval_datasets
            WHERE tenant_id = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            """,
            tenant_id,
            limit,
            offset,
        )
        return [self._decode_eval_row(row) for row in rows], total

    async def list_dataset_manifest(self, *, tenant_id: str) -> list[dict[str, Any]]:
        rows = await self.fetch(
            """
            SELECT * FROM eval_datasets
            WHERE tenant_id = $1
            ORDER BY created_at DESC, dataset_id DESC
            """,
            tenant_id,
        )
        return [self._decode_eval_row(row) for row in rows]

    async def get_dataset(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> dict[str, Any] | None:
        row = await self.fetchrow(
            "SELECT * FROM eval_datasets WHERE tenant_id = $1 AND dataset_id = $2::uuid",
            tenant_id,
            dataset_id,
        )
        return self._decode_eval_row(row) if row else None

    async def list_examples(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        split: str | None = None,
        limit: int = 200,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        params: list[Any] = [tenant_id, dataset_id]
        filters = ["tenant_id = $1", "dataset_id = $2::uuid"]
        if split:
            params.append(split)
            filters.append(f"split = ${len(params)}")
        where_clause = " AND ".join(filters)
        count_row = await self.fetchrow(
            f"SELECT COUNT(*) AS total FROM eval_examples WHERE {where_clause}",
            *params,
        )
        total = int(count_row.get("total") or 0) if count_row else 0
        page_params = [*params, limit, offset]
        rows = await self.fetch(
            f"""
            SELECT * FROM eval_examples
            WHERE {where_clause}
            ORDER BY created_at DESC
            LIMIT ${len(params) + 1} OFFSET ${len(params) + 2}
            """,
            *page_params,
        )
        return [self._decode_eval_row(row) for row in rows], total

    async def list_example_manifest(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
    ) -> list[dict[str, Any]]:
        rows = await self.fetch(
            """
            SELECT * FROM eval_examples
            WHERE tenant_id = $1 AND dataset_id = $2::uuid
            ORDER BY created_at DESC, example_id DESC
            """,
            tenant_id,
            dataset_id,
        )
        return [self._decode_eval_row(row) for row in rows]

    async def list_evaluators(
        self,
        *,
        tenant_id: str,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        count_row = await self.fetchrow(
            "SELECT COUNT(*) AS total FROM eval_evaluators WHERE tenant_id = $1",
            tenant_id,
        )
        total = int(count_row.get("total") or 0) if count_row else 0
        rows = await self.fetch(
            """
            SELECT * FROM eval_evaluators
            WHERE tenant_id = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            """,
            tenant_id,
            limit,
            offset,
        )
        return [self._decode_eval_row(row) for row in rows], total

    async def list_experiments(
        self,
        *,
        tenant_id: str,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        count_row = await self.fetchrow(
            "SELECT COUNT(*) AS total FROM eval_experiments WHERE tenant_id = $1",
            tenant_id,
        )
        total = int(count_row.get("total") or 0) if count_row else 0
        rows = await self.fetch(
            """
            SELECT * FROM eval_experiments
            WHERE tenant_id = $1
            ORDER BY created_at DESC
            LIMIT $2 OFFSET $3
            """,
            tenant_id,
            limit,
            offset,
        )
        return [self._decode_eval_row(row) for row in rows], total

    async def create_eval_score(
        self,
        *,
        tenant_id: str,
        trace_id: str,
        created_by: str,
        payload: dict[str, Any],
        trace_family: str = "assistant",
    ) -> dict[str, Any] | None:
        return await self.create_score(
            tenant_id=tenant_id,
            trace_id=trace_id,
            created_by=created_by,
            payload=payload,
            trace_family=trace_family,
        )

    async def get_agent_operations_summary(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        agent_version_id: str | None = None,
        publication_id: str | None = None,
        channel: str | None = None,
        started_after: Any | None = None,
        started_before: Any | None = None,
    ) -> dict[str, Any]:
        """Aggregate only explicit Agent runtime dimensions, never metadata hints."""

        params: list[Any] = [tenant_id, agent_id]
        filters = ["tenant_id = $1", "agent_id = $2::uuid"]
        if agent_version_id:
            params.append(agent_version_id)
            filters.append(f"agent_version_id = ${len(params)}::uuid")
        if publication_id:
            params.append(publication_id)
            filters.append(f"publication_id = ${len(params)}::uuid")
        if channel:
            params.append(channel)
            filters.append(f"channel = ${len(params)}")
        if started_after is not None:
            params.append(started_after)
            filters.append(f"started_at >= ${len(params)}")
        if started_before is not None:
            params.append(started_before)
            filters.append(f"started_at <= ${len(params)}")
        where_clause = " AND ".join(filters)
        row = await self.fetchrow(
            f"""
            WITH filtered_traces AS (
                SELECT
                    trace_id, tenant_id, session_id, agent_version_id,
                    publication_id, channel, status, total_latency_ms,
                    first_token_latency_ms, total_tokens, total_cost_cents,
                    created_at
                FROM agent_traces
                WHERE {where_clause}
            ),
            trace_metrics AS (
                SELECT
                    COUNT(*)::int AS total_runs,
                    COUNT(*) FILTER (WHERE status = 'succeeded')::int AS succeeded_runs,
                    COUNT(*) FILTER (WHERE status IN ('failed', 'timeout'))::int AS failed_runs,
                    COUNT(DISTINCT session_id)::int AS sessions,
                    COALESCE(AVG(total_latency_ms), 0)::int AS avg_latency_ms,
                    COALESCE(PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p50_latency_ms,
                    COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p95_latency_ms,
                    COALESCE(AVG(first_token_latency_ms), 0)::int AS avg_ttft_ms,
                    COALESCE(PERCENTILE_CONT(0.50) WITHIN GROUP (ORDER BY first_token_latency_ms), 0)::int AS p50_ttft_ms,
                    COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY first_token_latency_ms), 0)::int AS p95_ttft_ms,
                    COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                    COALESCE(SUM(total_cost_cents), 0)::bigint AS total_cost_cents,
                    MIN(created_at) AS oldest_trace_at,
                    MAX(created_at) AS newest_trace_at
                FROM filtered_traces
            ),
            classified_spans AS (
                SELECT
                    s.span_kind,
                    s.status,
                    CASE
                        WHEN COALESCE(
                            s.attributes->>'retrieval.document_count',
                            s.attributes#>>'{{retrieval,document_count}}'
                        ) ~ '^[0-9]+$'
                        THEN COALESCE(
                            s.attributes->>'retrieval.document_count',
                            s.attributes#>>'{{retrieval,document_count}}'
                        )::int
                        WHEN jsonb_typeof(COALESCE(
                            s.attributes->'retrieval.documents',
                            s.attributes#>'{{retrieval,documents}}'
                        )) = 'array'
                        THEN jsonb_array_length(COALESCE(
                            s.attributes->'retrieval.documents',
                            s.attributes#>'{{retrieval,documents}}'
                        ))
                        ELSE 0
                    END AS retrieved_document_count
                FROM agent_trace_spans s
                INNER JOIN filtered_traces t ON t.trace_id = s.trace_id
                WHERE s.span_kind IN ('tool_execution', 'retriever')
            ),
            span_metrics AS (
                SELECT
                    COUNT(*) FILTER (WHERE span_kind = 'tool_execution')::int AS tool_calls,
                    COUNT(*) FILTER (
                        WHERE span_kind = 'tool_execution' AND status = 'succeeded'
                    )::int AS tool_succeeded,
                    COUNT(*) FILTER (WHERE span_kind = 'retriever')::int AS knowledge_queries,
                    COUNT(*) FILTER (
                        WHERE span_kind = 'retriever'
                          AND status = 'succeeded'
                          AND retrieved_document_count > 0
                    )::int AS knowledge_hits
                FROM classified_spans
            ),
            feedback_metrics AS (
                SELECT
                    COUNT(*)::int AS feedback_count,
                    COUNT(*) FILTER (WHERE f.rating = 1)::int AS positive_feedback_count
                FROM agent_runtime_feedback f
                WHERE f.tenant_id = $1
                  AND EXISTS (
                      SELECT 1
                      FROM filtered_traces t
                      WHERE t.tenant_id = f.tenant_id
                        AND t.agent_version_id = f.agent_version_id
                        AND t.publication_id = f.publication_id
                        AND t.channel = f.channel
                        AND t.session_id = f.session_id
                  )
            )
            SELECT trace_metrics.*, span_metrics.*, feedback_metrics.*
            FROM trace_metrics
            CROSS JOIN span_metrics
            CROSS JOIN feedback_metrics
            """,
            *params,
        )
        breakdown_rows = await self.fetch(
            f"""
            SELECT
                channel,
                agent_version_id,
                publication_id,
                COUNT(*)::int AS run_count,
                COUNT(*) FILTER (WHERE status IN ('failed', 'timeout'))::int AS failed_count,
                COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p95_latency_ms,
                COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                COALESCE(SUM(total_cost_cents), 0)::bigint AS total_cost_cents
            FROM agent_traces
            WHERE {where_clause}
            GROUP BY channel, agent_version_id, publication_id
            ORDER BY run_count DESC, channel ASC
            """,
            *params,
        )
        retention = await self.fetchrow(
            """
            SELECT
                COALESCE(p.trace_retention_days, 90)::int AS trace_retention_days,
                COALESCE(p.legal_hold, FALSE) AS legal_hold,
                (
                    SELECT MAX(r.completed_at)
                    FROM agent_data_deletion_requests r
                    WHERE r.tenant_id = $1 AND r.agent_id = $2::uuid
                      AND r.scope = 'retention' AND r.status = 'completed'
                ) AS last_retention_cleanup_at
            FROM agents a
            LEFT JOIN agent_governance_policies p
              ON p.tenant_id = a.tenant_id AND p.agent_id = a.agent_id
            WHERE a.tenant_id = $1 AND a.agent_id = $2::uuid
            """,
            tenant_id,
            agent_id,
        )
        result = dict(row or {})
        total = int(result.get("total_runs") or 0)
        succeeded = int(result.get("succeeded_runs") or 0)
        tool_calls = int(result.get("tool_calls") or 0)
        tool_succeeded = int(result.get("tool_succeeded") or 0)
        knowledge_queries = int(result.get("knowledge_queries") or 0)
        knowledge_hits = int(result.get("knowledge_hits") or 0)
        feedback_count = int(result.get("feedback_count") or 0)
        positive_feedback = int(result.get("positive_feedback_count") or 0)
        result["success_rate"] = (succeeded / total) if total else None
        result["tool_success_rate"] = tool_succeeded / tool_calls if tool_calls else None
        result["knowledge_hit_rate"] = (
            knowledge_hits / knowledge_queries if knowledge_queries else None
        )
        result["feedback_positive_rate"] = (
            positive_feedback / feedback_count if feedback_count else None
        )
        result["breakdown"] = [self._decode_trace_row(item) for item in breakdown_rows]
        result["retention"] = self._decode_trace_row(retention or {})
        result["retention_limited"] = bool((retention or {}).get("last_retention_cleanup_at"))
        return self._decode_trace_row(result)

    async def get_summary(
        self,
        *,
        tenant_id: str,
        user_id: str | None = None,
        days: int = 7,
    ) -> dict[str, Any]:
        params: list[Any] = [tenant_id, max(1, days)]
        filters = [
            "tenant_id = $1",
            "created_at >= NOW() - make_interval(days => $2::int)",
        ]
        if user_id:
            params.append(user_id)
            filters.append(f"user_id = ${len(params)}")
        where_clause = " AND ".join(filters)
        row = await self.fetchrow(
            f"""
            SELECT
                COUNT(*)::int AS total_traces,
                COUNT(*) FILTER (WHERE status = 'failed')::int AS failed_traces,
                COUNT(*) FILTER (WHERE status = 'succeeded')::int AS succeeded_traces,
                COUNT(*) FILTER (WHERE trace_family = 'assistant')::int AS assistant_traces,
                COUNT(*) FILTER (WHERE trace_family = 'langgraph_proxy')::int AS langgraph_traces,
                COUNT(*) FILTER (WHERE trace_family = 'rag')::int AS rag_traces,
                COALESCE(AVG(total_latency_ms), 0)::int AS avg_latency_ms,
                COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY total_latency_ms), 0)::int AS p95_latency_ms,
                COALESCE(SUM(total_tokens), 0)::bigint AS total_tokens,
                COALESCE(SUM(total_cost_cents), 0)::bigint AS total_cost_cents
            FROM agent_traces
            WHERE {where_clause}
            """,
            *params,
        )
        scored_filters = [
            "t.tenant_id = $1",
            "t.created_at >= NOW() - make_interval(days => $2::int)",
        ]
        if user_id:
            scored_filters.append(f"t.user_id = ${len(params)}")
        scored_row = await self.fetchrow(
            f"""
            SELECT COUNT(DISTINCT s.trace_id)::int AS scored_traces
            FROM agent_trace_scores s
            INNER JOIN agent_traces t ON t.trace_id = s.trace_id
            WHERE {" AND ".join(scored_filters)}
            """,
            *params,
        )
        summary = dict(row or {})
        summary["scored_traces"] = int((scored_row or {}).get("scored_traces") or 0)
        summary["window_days"] = max(1, days)
        return summary

    async def get_kb_ragas_summary(
        self,
        *,
        tenant_id: str,
        days: int = 7,
        dataset_id: str | None = None,
    ) -> dict[str, Any]:
        params: list[Any] = [tenant_id, max(1, days)]
        trace_filters = [
            "t.tenant_id = $1",
            "t.trace_family = 'rag'",
            "t.created_at >= NOW() - make_interval(days => $2::int)",
        ]
        if dataset_id:
            params.append(dataset_id)
            trace_filters.append(f"t.metadata->>'dataset_id' = ${len(params)}")
        trace_where = " AND ".join(trace_filters)
        latest_scores_ctes = f"""
            in_window_traces AS (
                SELECT t.trace_id
                FROM agent_traces t
                WHERE {trace_where}
            ),
            latest_scores AS (
                SELECT ranked_scores.*
                FROM (
                    SELECT
                        s.*,
                        ROW_NUMBER() OVER (
                            PARTITION BY s.trace_id, s.evaluator_id,
                                COALESCE(s.evaluator_version, ''), s.score_name
                            ORDER BY s.created_at DESC, s.score_id DESC
                        ) AS score_revision
                    FROM agent_trace_scores s
                    INNER JOIN in_window_traces t ON t.trace_id = s.trace_id
                    WHERE s.score_source = 'kb_ragas'
                ) ranked_scores
                WHERE ranked_scores.score_revision = 1
            )
        """

        trace_row = await self.fetchrow(
            f"""
            WITH {latest_scores_ctes}
            SELECT
                (
                    SELECT COUNT(DISTINCT t.trace_id)
                    FROM in_window_traces t
                )::int AS rag_traces,
                COUNT(DISTINCT CASE
                    WHEN s.label IN ('pass', 'fail') THEN s.trace_id
                END)::int AS ragas_scored_traces
            FROM latest_scores s
            """,
            *params,
        )

        metric_rows = await self.fetch(
            f"""
            WITH {latest_scores_ctes}
            SELECT
                s.score_name AS metric,
                COALESCE(
                    AVG(s.numeric_value) FILTER (WHERE s.label IN ('pass', 'fail')),
                    0
                )::float AS average_score,
                COUNT(*) FILTER (WHERE s.label IN ('pass', 'fail'))::int AS scored_count,
                COUNT(*) FILTER (WHERE s.label = 'pass')::int AS pass_count,
                COUNT(*) FILTER (WHERE s.label = 'fail')::int AS fail_count,
                COUNT(*) FILTER (WHERE s.label = 'review')::int AS review_count
            FROM latest_scores s
            GROUP BY s.score_name
            ORDER BY s.score_name
            """,
            *params,
        )

        judge_row = await self.fetchrow(
            f"""
            WITH {latest_scores_ctes}
            SELECT s.metadata->>'judge_model' AS judge_model
            FROM latest_scores s
            WHERE COALESCE(s.metadata->>'judge_model', '') <> ''
            ORDER BY s.created_at DESC, s.score_id DESC
            LIMIT 1
            """,
            *params,
        )

        return {
            "window_days": max(1, days),
            "dataset_id": dataset_id,
            "rag_traces": int((trace_row or {}).get("rag_traces") or 0),
            "ragas_scored_traces": int((trace_row or {}).get("ragas_scored_traces") or 0),
            "metrics": [dict(row) for row in metric_rows],
            "latest_judge_model": (judge_row or {}).get("judge_model"),
        }

    async def trace_has_kb_ragas_score(
        self,
        *,
        tenant_id: str,
        trace_id: str,
        evaluator_id: str | None = None,
    ) -> bool:
        params: list[Any] = [tenant_id, trace_id]
        filters = [
            "s.trace_id = $2::uuid",
            "t.tenant_id = $1",
            "s.score_source = 'kb_ragas'",
            "s.label IN ('pass', 'fail')",
        ]
        if evaluator_id:
            params.append(evaluator_id)
            filters.append(f"s.evaluator_id = ${len(params)}::uuid")
        row = await self.fetchrow(
            f"""
            SELECT 1
            FROM agent_trace_scores s
            INNER JOIN agent_traces t ON t.trace_id = s.trace_id
            WHERE {" AND ".join(filters)}
            LIMIT 1
            """,
            *params,
        )
        return row is not None

    async def purge_expired_traces(
        self,
        *,
        default_retention_days: int = 90,
        batch_size: int = 500,
    ) -> int:
        rows = await self.fetch(
            """
            WITH doomed AS (
                SELECT trace_id
                FROM agent_traces
                WHERE COALESCE(
                    retention_expires_at,
                    created_at + make_interval(days => $1::int)
                ) < NOW()
                ORDER BY created_at ASC
                LIMIT $2
            )
            DELETE FROM agent_traces t
            USING doomed d
            WHERE t.trace_id = d.trace_id
            RETURNING t.trace_id
            """,
            max(1, default_retention_days),
            max(1, batch_size),
        )
        return len(rows)

    def _decode_trace_row(self, row: dict[str, Any]) -> dict[str, Any]:
        decoded = dict(row)
        for key in (
            "trace_id",
            "agent_id",
            "agent_version_id",
            "publication_id",
        ):
            if decoded.get(key) is not None:
                decoded[key] = str(decoded[key])
        for key in ("redaction_state", "metadata", "metrics", "privacy"):
            decoded[key] = self._decode_json(decoded.get(key), default={})
        decoded["scores_count"] = int(decoded.get("scores_count") or 0)
        return decoded

    def _decode_span_row(self, row: dict[str, Any]) -> dict[str, Any]:
        decoded = dict(row)
        for key in ("span_id", "trace_id", "parent_span_id"):
            if decoded.get(key) is not None:
                decoded[key] = str(decoded[key])
        decoded["attributes"] = self._decode_json(decoded.get("attributes"), default={})
        return decoded

    def _decode_event_row(self, row: dict[str, Any]) -> dict[str, Any]:
        decoded = dict(row)
        for key in ("event_id", "trace_id", "span_id"):
            if decoded.get(key) is not None:
                decoded[key] = str(decoded[key])
        decoded["payload"] = self._decode_json(decoded.get("payload"), default={})
        return decoded

    def _decode_score_row(self, row: dict[str, Any]) -> dict[str, Any]:
        decoded = dict(row)
        for key in ("score_id", "trace_id", "span_id", "evaluator_id"):
            if decoded.get(key) is not None:
                decoded[key] = str(decoded[key])
        decoded["metadata"] = self._decode_json(decoded.get("metadata"), default={})
        return decoded

    def _decode_eval_row(self, row: dict[str, Any] | None) -> dict[str, Any]:
        decoded = dict(row or {})
        for key, value in list(decoded.items()):
            if key.endswith("_id") and value is not None:
                decoded[key] = str(value)
        for key in (
            "schema",
            "metadata",
            "input",
            "expected_output",
            "expected_trajectory",
            "sampling_config",
            "filter_config",
            "target_config",
            "target_snapshot",
            "score_summary",
            "metrics",
            "candidate_fingerprint",
            "execution_config",
            "observed_metrics",
            "runtime_handle",
            "payload",
        ):
            if key in decoded:
                decoded[key] = self._decode_json(decoded.get(key), default={})
        if "assertions" in decoded:
            decoded["assertions"] = self._decode_json(decoded.get("assertions"), default=[])
        if "target_snapshot" in decoded and isinstance(decoded["target_snapshot"], dict):
            # Older rescore runs may have persisted case contents here. Both
            # GET run and GET experiment use this projection, including after
            # a linked KB source grant has been revoked.
            decoded["target_snapshot"] = {
                key: value for key, value in decoded["target_snapshot"].items()
                if key != "dataset_manifest"
            }
        return decoded

    def _decode_json(self, value: Any, *, default: Any) -> Any:
        if value is None:
            return default
        if isinstance(value, str):
            try:
                return json.loads(value)
            except json.JSONDecodeError:
                return default
        return value

    def _json_dumps(self, value: Any) -> str:
        return json.dumps(value, ensure_ascii=False, separators=(",", ":"), default=str)
