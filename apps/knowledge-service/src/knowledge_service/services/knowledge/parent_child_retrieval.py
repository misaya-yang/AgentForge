"""T9 (PRD): parent-child retrieval activation + summary-sibling merge.

The ingestion side already emits the parent-child structure — child points
carry a ``parent_segment_id`` payload (see
``chunking_manager._segment_vector_payload`` and
``hierarchical_indexer._create_l2_l3_chunks``) and parent rows live in the
``segments`` table — but the main retrieval pipeline ignored it and returned
child hits as-is. This module implements the retrieval half of PRD T9 item 2
using the mechanism pinned by the Dify addendum (§0-1 / §1 T9-1):

* resolve parentage from the ``parent_segment_id`` payload (no query-time
  Postgres join on the hot child leg);
* fold to parents **after** rerank, with the parent score aggregated as
  ``max(child)`` (never child-level truncation before the fold);
* the fan-out headroom that keeps ``parents returned >= top_k`` is an
  explicit ``parent_child.fanout_top_k`` configuration, applied to the
  recall-k values before the child retrieval legs run.

The optional summary-index layer (PRD T9 item 4, retrieval-side semantics)
folds ``is_summary`` sibling hits into their ``original_chunk_id`` block with
``score = max(block, summary)`` — the Dify override bug (its
retrieval_service.py:698-706) is explicitly not reproduced: a summary hit
never replaces the block's own score, it can only raise it.
"""

from __future__ import annotations

import asyncio
from dataclasses import dataclass
from typing import Any

from ...core.exceptions import ValidationFailedError

PARENT_CHILD_MODES = frozenset({"parent", "context"})
# Explicit default for the fan-out ceiling; ``fanout_top_k`` in the dataset
# config overrides it and is surfaced in the meta report either way.
DEFAULT_FANOUT_TOP_K = 48
_FANOUT_UPPER_BOUND = 2000
# Same per-leg ceilings the request/config gate enforces in
# ``_require_bounded_retrieval_config``.
_LEG_K_UPPER_BOUND = 1000
_KEYWORD_POOL_UPPER_BOUND = 500
_CANDIDATE_UPPER_BOUND = 2000


@dataclass(frozen=True)
class ParentChildSettings:
    """Resolved ``index_config.retrieval.parent_child`` switch."""

    return_mode: str = "parent"
    fanout_top_k: int = DEFAULT_FANOUT_TOP_K
    fanout_source: str = "default"  # "default" | "config"


@dataclass(frozen=True)
class SummaryIndexSettings:
    """Resolved ``index_config.retrieval.summary_index`` switch (optional layer)."""

    prepend_summary: bool = True


def _bounded_int(raw: Any, *, path: str, lower: int, upper: int) -> int:
    if isinstance(raw, bool) or not isinstance(raw, int):
        raise ValidationFailedError(
            f"stored retrieval config {path} must be an integer between "
            f"{lower} and {upper}"
        )
    if not lower <= raw <= upper:
        raise ValidationFailedError(
            f"stored retrieval config {path} must be an integer between "
            f"{lower} and {upper}"
        )
    return int(raw)


def parse_parent_child_settings(
    retrieval_defaults: dict[str, Any],
) -> ParentChildSettings | None:
    """Parse the dormant parent-child switch; ``None`` means disabled.

    Accepts the boolean shorthand (``parent_child: true``) and the dict form
    ``{enabled, return_mode, fanout_top_k}``. Invalid stored configs fail the
    request before any embedding/vector/Postgres call runs.
    """
    raw = (retrieval_defaults or {}).get("parent_child")
    if raw is None or raw is False:
        return None
    if raw is True:
        return ParentChildSettings()
    if not isinstance(raw, dict):
        raise ValidationFailedError(
            "stored retrieval config parent_child must be a boolean or object"
        )
    if not bool(raw.get("enabled", True)):
        return None
    mode_raw = raw.get("return_mode")
    if mode_raw is None:
        mode = "parent"
    else:
        mode = str(mode_raw).strip().lower()
        if mode not in PARENT_CHILD_MODES:
            raise ValidationFailedError(
                "stored retrieval config parent_child.return_mode must be "
                "'parent' or 'context'"
            )
    fanout_raw = raw.get("fanout_top_k")
    if fanout_raw is None:
        fanout = DEFAULT_FANOUT_TOP_K
        fanout_source = "default"
    else:
        fanout = _bounded_int(
            fanout_raw,
            path="parent_child.fanout_top_k",
            lower=1,
            upper=_FANOUT_UPPER_BOUND,
        )
        fanout_source = "config"
    return ParentChildSettings(
        return_mode=mode,
        fanout_top_k=fanout,
        fanout_source=fanout_source,
    )


def parse_summary_index_settings(
    retrieval_defaults: dict[str, Any],
) -> SummaryIndexSettings | None:
    """Parse the optional summary-index switch; ``None`` means disabled."""
    raw = (retrieval_defaults or {}).get("summary_index")
    if raw is None or raw is False:
        return None
    if raw is True:
        return SummaryIndexSettings()
    if not isinstance(raw, dict):
        raise ValidationFailedError(
            "stored retrieval config summary_index must be a boolean or object"
        )
    if not bool(raw.get("enabled", True)):
        return None
    prepend = raw.get("prepend_summary", True)
    if not isinstance(prepend, bool):
        raise ValidationFailedError(
            "stored retrieval config summary_index.prepend_summary must be a boolean"
        )
    return SummaryIndexSettings(prepend_summary=prepend)


def apply_recall_fanout(
    settings: ParentChildSettings,
    *,
    vector_k: int,
    keyword_k: int,
    candidate_k: int,
    keyword_pool_k: int,
    top_k: int,
) -> tuple[int, int, int, int, dict[str, Any]]:
    """Raise the child-recall k values so the post-fold parent count can
    still reach ``top_k`` (PRD: fan-out top-k ceiling is explicit config).

    Returns the widened k values plus a report dict for retrieval meta.
    """
    fanout = max(int(settings.fanout_top_k), int(top_k))
    widened = {
        "vector_k": max(int(vector_k), min(fanout, _LEG_K_UPPER_BOUND)),
        "keyword_k": max(int(keyword_k), min(fanout, _LEG_K_UPPER_BOUND)),
        "candidate_k": max(int(candidate_k), min(fanout, _CANDIDATE_UPPER_BOUND)),
        "keyword_pool_k": max(
            int(keyword_pool_k), min(fanout, _KEYWORD_POOL_UPPER_BOUND)
        ),
    }
    report = {
        "enabled": True,
        "return_mode": settings.return_mode,
        "fanout": {
            "top_k": fanout,
            "source": settings.fanout_source,
            "vector_k": widened["vector_k"],
            "keyword_k": widened["keyword_k"],
            "candidate_k": widened["candidate_k"],
            "keyword_pool_k": widened["keyword_pool_k"],
        },
    }
    return (
        widened["vector_k"],
        widened["keyword_k"],
        widened["candidate_k"],
        widened["keyword_pool_k"],
        report,
    )


def _candidate_parent_id(candidate: dict[str, Any]) -> str:
    """Resolve the child's parent id from the payload (flat or nested form).

    The Qdrant payload carries ``parent_segment_id`` at top level; the
    PostgreSQL FTS leg projects the segment ``metadata`` column, where the
    hierarchical indexer stores the same link.
    """
    payload = candidate.get("metadata") or {}
    if not isinstance(payload, dict):
        return ""
    direct = str(payload.get("parent_segment_id") or "").strip()
    if direct:
        return direct
    nested = payload.get("metadata")
    if isinstance(nested, dict):
        return str(nested.get("parent_segment_id") or "").strip()
    return ""


def _final_score(candidate: dict[str, Any]) -> float:
    try:
        return float(candidate.get("_final_score") or 0.0)
    except (TypeError, ValueError):
        return 0.0


def _group_children(
    candidates: list[dict[str, Any]],
) -> tuple[list[tuple[str, list[int]]], dict[int, dict[str, Any]]]:
    """Order-preserving group-by-parent over the ranked candidate list.

    Returns (ordered groups, passthrough-by-index). Each group is
    ``(parent_id, [candidate indices...])`` in best-child-first emission
    order; indices are positions in ``candidates``. Passthrough entries
    (no parent link) are returned separately so the caller can keep them
    in place.
    """
    groups: dict[str, list[int]] = {}
    order: list[str] = []
    passthrough: dict[int, dict[str, Any]] = {}
    for index, candidate in enumerate(candidates):
        parent_id = _candidate_parent_id(candidate)
        if not parent_id:
            passthrough[index] = candidate
            continue
        if parent_id not in groups:
            groups[parent_id] = []
            order.append(parent_id)
        groups[parent_id].append(index)
    ordered_groups = [
        (parent_id, sorted(groups[parent_id], key=lambda i: -_final_score(candidates[i])))
        for parent_id in order
    ]
    return ordered_groups, passthrough


def _build_parent_row(
    *,
    best_child: dict[str, Any],
    group_indices: list[int],
    candidates: list[dict[str, Any]],
    parent_id: str,
    parent_row: dict[str, Any] | None,
    mode: str,
) -> dict[str, Any]:
    row = dict(best_child)
    children = [
        {
            "segment_id": str(candidates[i].get("segment_id") or ""),
            "score": round(_final_score(candidates[i]), 6),
        }
        for i in group_indices
    ]
    payload = dict(best_child.get("metadata") or {})
    if not isinstance(payload, dict):
        payload = {}
    status = "resolved"
    parent_text = str((parent_row or {}).get("text") or "").strip()
    parent_meta = (parent_row or {}).get("metadata")
    if isinstance(parent_meta, dict):
        merged = dict(parent_meta)
        for key, value in payload.items():
            merged.setdefault(key, value)
        payload = merged
    payload["parent_segment_id"] = parent_id
    if mode == "parent":
        if parent_text:
            # The returned row IS the parent: child identity must not leak.
            row["segment_id"] = parent_id
            row["text"] = parent_text
            payload["segment_id"] = parent_id
        else:
            status = "parent_text_missing"
    elif parent_text:
        # Context mode serves the best child with parent context attached,
        # so the child keeps its identity and the link lives in provenance.
        payload["parent_context"] = parent_text
    else:
        status = "parent_text_missing"
    row["document_id"] = str(
        (parent_row or {}).get("document_id") or best_child.get("document_id") or ""
    )
    content_type = str((parent_row or {}).get("content_type") or "").strip()
    if content_type:
        payload["content_type"] = content_type
    sources: set[Any] = set()
    for i in group_indices:
        cand_sources = candidates[i].get("_sources")
        if isinstance(cand_sources, set):
            sources |= cand_sources
        elif isinstance(cand_sources, list):
            sources.update(cand_sources)
    if sources:
        row["_sources"] = sources
    payload["_parent_child"] = {
        "mode": mode,
        "status": status,
        "parent_segment_id": parent_id,
        "children": children,
    }
    row["metadata"] = payload
    return row


def _summary_link(candidate: dict[str, Any]) -> tuple[bool, str]:
    payload = candidate.get("metadata") or {}
    if not isinstance(payload, dict):
        return False, ""
    nested = payload.get("metadata")
    sources = [payload]
    if isinstance(nested, dict):
        sources.append(nested)
    for source in sources:
        if bool(source.get("is_summary")):
            return True, str(source.get("original_chunk_id") or "").strip()
    return False, ""


def merge_summary_siblings(
    candidates: dict[str, dict[str, Any]],
    *,
    settings: SummaryIndexSettings,
) -> dict[str, Any]:
    """Fold ``is_summary`` sibling-vector hits into their original block.

    Merge semantics are pinned by the Dify addendum (§1 T9-3): a summary hit
    returns the ORIGINAL block and the combined score is
    ``max(block score, summary score)`` — the summary never overrides the
    block's own score (the Dify coverage bug). The summary text is attached
    under ``_summary_prefix`` for LLM-context prepending. Summaries whose
    original block is not in the candidate set pass through unchanged and
    are reported as unresolved. Mutates ``candidates`` in place.
    """
    stats: dict[str, Any] = {"enabled": True, "merged": 0, "unresolved": 0}
    summary_ids = [
        seg_id
        for seg_id, candidate in candidates.items()
        if _summary_link(candidate)[0]
    ]
    for seg_id in summary_ids:
        summary = candidates[seg_id]
        _, original_id = _summary_link(summary)
        target = candidates.get(original_id) if original_id else None
        if target is None or target is summary:
            stats["unresolved"] += 1
            continue
        summary_score = _final_score(summary)
        target_score = _final_score(target)
        combined = max(summary_score, target_score)
        target["_final_score"] = combined
        if combined > target_score:
            target["_fusion_score"] = combined
        payload = dict(target.get("metadata") or {})
        if settings.prepend_summary:
            payload["_summary_prefix"] = str(summary.get("text") or "")
        payload["_summary_hit"] = True
        payload["_summary_segment_id"] = seg_id
        target["metadata"] = payload
        sources = target.get("_sources")
        summary_sources = summary.get("_sources")
        if isinstance(sources, set) and isinstance(summary_sources, (set, list)):
            sources.update(summary_sources)
        del candidates[seg_id]
        stats["merged"] += 1
    return stats


async def fold_candidates_to_parents(
    candidates: list[dict[str, Any]],
    *,
    settings: ParentChildSettings,
    db: Any,
    dataset_id: str,
    tenant_id: str,
    max_concurrency: int = 8,
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    """Collapse child hits to their parent segments (child -> parent expansion).

    Input order is the pre-fold ranking (fusion/rerank/MMR order is already
    settled; this stage must not re-sort score spaces, it only groups). Each
    parent is emitted once, positioned at its best child, with
    ``_final_score = max(child scores)``. Children whose parent cannot be
    resolved (missing row, inactive under the scoped authority, or no DB
    support) are retained as single best-child representatives — the fold
    degrades per-parent, never by dropping evidence — and the stats report
    the shortfall. Scoped parent reads use at most ``max_concurrency``
    workers, rather than one task per parent. The retrieval service supplies
    its resolved query concurrency; the direct-call default matches the
    Knowledge processing default of eight. Cancellation drains every worker.
    """
    ordered_groups, passthrough = _group_children(candidates)
    stats: dict[str, Any] = {
        "enabled": True,
        "mode": settings.return_mode,
        "child_hits": sum(len(indices) for _, indices in ordered_groups),
        "parents": len(ordered_groups),
        "collapsed_children": sum(
            len(indices) - 1 for _, indices in ordered_groups
        ),
        "unresolved_parents": 0,
    }
    if not ordered_groups:
        return list(candidates), stats

    get_segment = getattr(db, "get_segment_scoped", None)
    parent_rows: dict[str, dict[str, Any]] = {}
    if callable(get_segment):
        pending_groups = iter(ordered_groups)

        async def fetch_parents() -> None:
            # Taking the next item does not suspend, so each unique parent is
            # owned by exactly one worker without a task/queue entry per read.
            for parent_id, _ in pending_groups:
                try:
                    fetched = await get_segment(
                        segment_id=parent_id,
                        dataset_id=dataset_id,
                        tenant_id=tenant_id,
                    )
                except Exception:
                    # Preserve per-parent degradation; CancelledError escapes.
                    continue
                if isinstance(fetched, dict) and str(fetched.get("segment_id") or "") == parent_id:
                    parent_rows[parent_id] = fetched

        workers = [
            asyncio.create_task(fetch_parents())
            for _ in range(min(max(int(max_concurrency), 1), len(ordered_groups)))
        ]
        try:
            await asyncio.gather(*workers)
        finally:
            # gather alone does not cancel siblings when a worker is cancelled.
            # Neither caller cancellation nor a cancelled read may orphan work.
            for worker in workers:
                worker.cancel()
            await asyncio.gather(*workers, return_exceptions=True)
    else:
        stats["authority"] = "db.get_segment_scoped unavailable"

    # Build representatives keyed by the index of their best child so the
    # emission pass keeps the surviving candidate slots stable.
    reps_by_best_index: dict[int, dict[str, Any]] = {}
    skip_indices: set[int] = set()
    for parent_id, indices in ordered_groups:
        best_index = indices[0]
        group_candidates = [candidates[i] for i in indices]
        best_child = max(group_candidates, key=_final_score)
        rep_score = max(_final_score(c) for c in group_candidates)
        parent_row = parent_rows.get(parent_id)
        if parent_row is None:
            stats["unresolved_parents"] += 1
        rep = _build_parent_row(
            best_child=best_child,
            group_indices=indices,
            candidates=candidates,
            parent_id=parent_id,
            parent_row=parent_row,
            mode=settings.return_mode,
        )
        rep["_final_score"] = rep_score
        reps_by_best_index[best_index] = rep
        skip_indices.update(indices)
        skip_indices.discard(best_index)

    folded: list[dict[str, Any]] = []
    for index, candidate in enumerate(candidates):
        if index in reps_by_best_index:
            folded.append(reps_by_best_index[index])
        elif index in skip_indices:
            continue
        else:
            folded.append(candidate)
    return folded, stats
