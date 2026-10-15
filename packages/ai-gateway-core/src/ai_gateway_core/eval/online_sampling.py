from __future__ import annotations

import hashlib
import math
import uuid
from typing import Any

from ai_gateway_core.logging import get_logger
from ai_gateway_core.persistence.repositories.agent_trace_repository import AgentTraceRepository

logger = get_logger(__name__)

_DEFAULT_ONLINE_RATE = 0.05
_DEFAULT_MAX_PENDING_ONLINE_RUNS = 200
_FAILURE_STATUSES = frozenset({"failed", "cancelled", "timeout", "error"})


def _online_config(evaluator: dict[str, Any]) -> dict[str, Any]:
    sampling = evaluator.get("sampling_config") if isinstance(evaluator.get("sampling_config"), dict) else {}
    online = sampling.get("online") if isinstance(sampling.get("online"), dict) else {}
    return online


def should_sample_trace_id(trace_id: str, rate: float) -> bool:
    try:
        raw_rate = float(rate)
    except (TypeError, ValueError):
        return False
    if not math.isfinite(raw_rate):
        return False
    normalized_rate = max(0.0, min(raw_rate, 1.0))
    if normalized_rate <= 0:
        return False
    if normalized_rate >= 1:
        return True
    try:
        bucket = uuid.UUID(str(trace_id)).int % 10_000
    except (ValueError, AttributeError):
        digest = hashlib.sha256(str(trace_id).encode("utf-8")).digest()
        bucket = int.from_bytes(digest[:8], "big") % 10_000
    return bucket < int(normalized_rate * 10_000)


def evaluator_matches_online_trace(
    evaluator: dict[str, Any],
    *,
    trace_family: str,
    status: str,
) -> bool:
    online = _online_config(evaluator)
    if not online.get("enabled"):
        return False
    evaluator_type = str(evaluator.get("evaluator_type") or "human")
    if evaluator_type == "human":
        return False
    families = online.get("trace_families")
    if not isinstance(families, list) or not families:
        families = ["assistant", "langgraph_proxy", "rag"]
    if trace_family not in {str(item) for item in families}:
        return False
    if online.get("only_failed") and status not in _FAILURE_STATUSES:
        return False
    return not online.get("only_succeeded") or status == "succeeded"


def resolve_online_queue_cap(evaluator: dict[str, Any], *, default_cap: int) -> int:
    online = _online_config(evaluator)
    if "max_pending_runs" in online:
        try:
            return max(1, int(online.get("max_pending_runs") or default_cap))
        except (TypeError, ValueError):
            logger.warning(
                "Invalid online eval max_pending_runs for evaluator=%s; using default %s",
                evaluator.get("evaluator_id"),
                default_cap,
            )
    return default_cap


def resolve_online_sample_rate(evaluator: dict[str, Any], *, default_rate: float) -> float:
    online = _online_config(evaluator)
    if "rate" in online:
        try:
            return float(online.get("rate") or 0.0)
        except (TypeError, ValueError):
            logger.warning(
                "Invalid online eval sample rate for evaluator=%s; using default %s",
                evaluator.get("evaluator_id"),
                default_rate,
            )
            return default_rate
    return default_rate


async def schedule_online_eval_for_trace(
    repository: AgentTraceRepository,
    *,
    tenant_id: str,
    payload: dict[str, Any],
    created_by: str = "eval-online-sampler",
    default_rate: float = _DEFAULT_ONLINE_RATE,
    evaluator_limit: int = 100,
    max_pending_online_runs: int = _DEFAULT_MAX_PENDING_ONLINE_RUNS,
) -> dict[str, Any]:
    trace_id = str(payload.get("trace_id") or "").strip()
    if not trace_id:
        return {"scheduled": 0, "reason": "missing_trace_id"}

    trace_detail = await repository.get_trace_detail(
        tenant_id=tenant_id,
        trace_id=trace_id,
        trace_family=str(payload.get("trace_family") or "assistant"),
    )
    trace = (trace_detail or {}).get("trace") or {}
    trace_metadata = trace.get("metadata") if isinstance(trace.get("metadata"), dict) else {}
    eval_origin = str(
        payload.get("eval_origin")
        or trace_metadata.get("eval_origin")
        or trace.get("user_id")
        or ""
    ).strip()
    if eval_origin in {"candidate", "judge", "eval-candidate", "eval-worker"}:
        return {
            "scheduled": 0,
            "matched": 0,
            "trace_id": trace_id,
            "reason": "internal_eval_trace",
        }

    pending_online_runs = await repository.count_pending_online_eval_runs(tenant_id=tenant_id)
    if pending_online_runs >= max_pending_online_runs:
        logger.warning(
            "online_eval_queue_full tenant=%s trace=%s pending=%s cap=%s",
            tenant_id,
            trace_id,
            pending_online_runs,
            max_pending_online_runs,
        )
        return {
            "scheduled": 0,
            "matched": 0,
            "trace_id": trace_id,
            "reason": "online_eval_queue_full",
            "pending_online_runs": pending_online_runs,
        }

    trace_family = str(payload.get("trace_family") or "assistant").strip()
    if trace_family not in {"assistant", "langgraph_proxy", "rag"}:
        trace_family = "assistant"
    status = str(payload.get("status") or "succeeded").strip().lower()

    evaluators: list[dict[str, Any]] = []
    offset = 0
    page_size = max(1, evaluator_limit)
    while True:
        page, total = await repository.list_evaluators(
            tenant_id=tenant_id,
            limit=page_size,
            offset=offset,
        )
        evaluators.extend(page)
        offset += len(page)
        if not page or offset >= total:
            break
    scheduled = 0
    matched = 0
    for evaluator in evaluators:
        if not evaluator_matches_online_trace(
            evaluator,
            trace_family=trace_family,
            status=status,
        ):
            continue
        matched += 1
        rate = resolve_online_sample_rate(evaluator, default_rate=default_rate)
        if not should_sample_trace_id(trace_id, rate):
            continue
        evaluator_id = str(evaluator.get("evaluator_id") or "")
        if not evaluator_id:
            continue
        if await repository.has_active_evaluator_run_for_trace(
            tenant_id=tenant_id,
            evaluator_id=evaluator_id,
            trace_id=trace_id,
        ):
            continue
        queue_cap = resolve_online_queue_cap(evaluator, default_cap=max_pending_online_runs)
        if pending_online_runs + scheduled >= queue_cap:
            continue
        await repository.enqueue_evaluator_run(
            tenant_id=tenant_id,
            evaluator_id=evaluator_id,
            created_by=created_by,
            payload={
                "trace_id": trace_id,
                "target_snapshot": {
                    "trace_id": trace_id,
                    "trace_family": trace_family,
                    "source": "online_sampling",
                    "status": status,
                    "source_adapter": payload.get("source_adapter"),
                },
                "metadata": {"online_sampling": True},
            },
        )
        scheduled += 1

    if scheduled:
        logger.info(
            "online_eval_scheduled tenant=%s trace=%s family=%s scheduled=%s matched=%s",
            tenant_id,
            trace_id,
            trace_family,
            scheduled,
            matched,
        )
    return {"scheduled": scheduled, "matched": matched, "trace_id": trace_id}
