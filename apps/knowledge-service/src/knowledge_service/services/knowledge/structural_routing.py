"""T9 (PRD): retrieval-side structural routing (heading-breadcrumb affinity).

PRD T9 item 3 mandates structural routing "without changing the boundary
algorithm": markdown documents route heading-first at chunking time and the
heading breadcrumb travels in the segment payload (``section_header`` /
``metadata.heading`` / ``metadata.section_title`` / ``metadata.breadcrumb``).
This module is the retrieval half: when a dataset enables
``index_config.retrieval.structural_routing`` with ``mode: heading_priority``,
candidates whose breadcrumb overlaps the query receive a bounded score bonus
**inside the fusion score space, before the single fusion sort** — so rerank
and MMR downstream consume the routed order through their existing stages and
see no new score space (the pipeline's "do not mix incompatible score spaces
with another sort" discipline holds: this changes scores, then the one sort
happens).

With rerank enabled the bonus still routes: it reorders the candidate list
that feeds the rerank truncation (``ranked[:candidate_k]``) and the MMR
relevance input, which is what "routing" means here — which structural
contexts survive into the final stages.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from ...core.exceptions import ValidationFailedError
from .retrieval import tokenize

STRUCTURAL_ROUTING_MODES = frozenset({"heading_priority"})
_DEFAULT_BOOST = 0.15
_DEFAULT_MIN_AFFINITY = 0.25
_BOOST_UPPER_BOUND = 1.0


@dataclass(frozen=True)
class StructuralRoutingSettings:
    """Resolved ``index_config.retrieval.structural_routing`` switch."""

    mode: str = "heading_priority"
    boost: float = _DEFAULT_BOOST
    min_affinity: float = _DEFAULT_MIN_AFFINITY


def parse_structural_settings(
    retrieval_defaults: dict[str, Any],
) -> StructuralRoutingSettings | None:
    """Parse the structural-routing switch; ``None`` means disabled."""
    raw = (retrieval_defaults or {}).get("structural_routing")
    if raw is None or raw is False:
        return None
    if raw is True:
        return StructuralRoutingSettings()
    if not isinstance(raw, dict):
        raise ValidationFailedError(
            "stored retrieval config structural_routing must be a boolean or object"
        )
    if not bool(raw.get("enabled", True)):
        return None
    mode_raw = raw.get("mode")
    if mode_raw is None:
        mode = "heading_priority"
    else:
        mode = str(mode_raw).strip().lower()
        if mode not in STRUCTURAL_ROUTING_MODES:
            raise ValidationFailedError(
                "stored retrieval config structural_routing.mode must be one of "
                f"{sorted(STRUCTURAL_ROUTING_MODES)}"
            )

    def _unit_interval(value: Any, path: str, default: float) -> float:
        if value is None:
            return default
        if isinstance(value, bool) or not isinstance(value, (int, float)):
            raise ValidationFailedError(
                f"stored retrieval config {path} must be a number between 0 and 1"
            )
        numeric = float(value)
        if not 0.0 <= numeric <= 1.0:
            raise ValidationFailedError(
                f"stored retrieval config {path} must be a number between 0 and 1"
            )
        return numeric

    return StructuralRoutingSettings(
        mode=mode,
        boost=_unit_interval(raw.get("boost"), "structural_routing.boost", _DEFAULT_BOOST),
        min_affinity=_unit_interval(
            raw.get("min_affinity"),
            "structural_routing.min_affinity",
            _DEFAULT_MIN_AFFINITY,
        ),
    )


def extract_breadcrumb(candidate: dict[str, Any]) -> str:
    """Collect the heading breadcrumb from a serving payload, if present."""
    payload = candidate.get("metadata")
    if not isinstance(payload, dict):
        return ""
    parts: list[str] = []

    def _add(value: Any) -> None:
        if isinstance(value, str) and value.strip():
            parts.append(value.strip())
        elif isinstance(value, list):
            for item in value:
                if isinstance(item, str) and item.strip():
                    parts.append(item.strip())

    _add(payload.get("section_header"))
    nested = payload.get("metadata")
    if isinstance(nested, dict):
        # The BM25/FTS leg nests the segment ``metadata`` column under this
        # key, so column-stored heading fields live here.
        _add(nested.get("section_header"))
        _add(nested.get("breadcrumb"))
        _add(nested.get("heading_path"))
        _add(nested.get("heading"))
        _add(nested.get("section_title"))
    _add(payload.get("breadcrumb"))
    _add(payload.get("heading"))
    _add(payload.get("section_title"))
    return " > ".join(dict.fromkeys(parts))


def _token_set(text: str) -> set[str]:
    return {token.lower() for token in tokenize(text, keep_original=True) if token}


def apply_structural_routing(
    candidates: dict[str, dict[str, Any]],
    query: str,
    *,
    settings: StructuralRoutingSettings,
) -> dict[str, Any]:
    """Boost fusion/final scores of breadcrumb-affine candidates in place.

    Affinity is query-token coverage of the candidate's heading breadcrumb;
    only candidates that actually carry a breadcrumb participate, so a corpus
    without structural metadata behaves exactly as before. The boost is
    additive in the fusion score space and clamped to 1.0.
    """
    stats: dict[str, Any] = {
        "enabled": True,
        "mode": settings.mode,
        "candidates_with_breadcrumb": 0,
        "boosted": 0,
    }
    query_tokens = _token_set(query)
    if not query_tokens:
        return stats
    for candidate in candidates.values():
        breadcrumb = extract_breadcrumb(candidate)
        if not breadcrumb:
            continue
        stats["candidates_with_breadcrumb"] += 1
        crumb_tokens = _token_set(breadcrumb)
        if not crumb_tokens:
            continue
        affinity = len(query_tokens & crumb_tokens) / len(query_tokens)
        if affinity < settings.min_affinity:
            continue
        payload = candidate.get("metadata")
        routed = {
            "mode": settings.mode,
            "breadcrumb": breadcrumb,
            "affinity": round(affinity, 4),
        }
        fusion = candidate.get("_fusion_score")
        if fusion is not None:
            candidate["_fusion_score"] = min(1.0, float(fusion) + settings.boost * affinity)
        final = candidate.get("_final_score")
        if final is not None:
            candidate["_final_score"] = min(1.0, float(final) + settings.boost * affinity)
        if isinstance(payload, dict):
            updated = dict(payload)
            updated["_structural_route"] = routed
            candidate["metadata"] = updated
        stats["boosted"] += 1
    return stats
