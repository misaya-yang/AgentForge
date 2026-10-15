"""T2-8 (PRD §T2-#8): eval-gated query-transformation switches, flag-only.

PRD line 603: 多轮会话改写 + 多查询扩展为评测门控开关(预设层暴露); HyDE 仅旗标.
These knobs are accepted, validated, fingerprinted, and echoed into retrieval
meta, but they never alter the serving pipeline — the serving behaviour lands
only after the T0 evaluation gate proves a net win (same dormancy contract the
T9 parent-child switch follows).

Stored shape, under ``index_config.retrieval``::

    query_rewrite:         true | {enabled, preset}     preset ∈ QUERY_REWRITE_PRESETS
    multi_query_expansion: true | {enabled, preset}     preset ∈ MULTI_QUERY_EXPANSION_PRESETS
    hyde:                  true | {enabled}             flag only — no preset

Absent, ``false``, or ``{enabled: false}`` mean "not configured": the pipeline
runs exactly as before and nothing is echoed. A malformed value raises
``ValidationFailedError`` before any embedding or store call, and the same
parse is reused by the dataset write-time validator so a bad config can never
be persisted. Because the switches alter no results yet, the echo always
carries ``applied: false`` with the reason the flag is inert.
"""

from __future__ import annotations

from typing import Any

from ...core.exceptions import ValidationFailedError

QUERY_REWRITE_PRESETS = frozenset({"pronoun_resolution", "multi_turn_merge"})
MULTI_QUERY_EXPANSION_PRESETS = frozenset({"lexical_synonym", "llm_paraphrase"})

_DEFAULT_PRESETS: dict[str, str] = {
    "query_rewrite": "pronoun_resolution",
    "multi_query_expansion": "lexical_synonym",
}
_PRESET_ALLOWLISTS: dict[str, frozenset[str]] = {
    "query_rewrite": QUERY_REWRITE_PRESETS,
    "multi_query_expansion": MULTI_QUERY_EXPANSION_PRESETS,
}
# Every flag is inert until its own gate passes; the reason is echoed verbatim
# so callers can tell "off" from "on but not yet serving".
_INERT_REASONS = {
    "query_rewrite": "eval_gate_pending",
    "multi_query_expansion": "eval_gate_pending",
    "hyde": "flag_only",
}
_ORDERED_KEYS = ("query_rewrite", "multi_query_expansion", "hyde")


def _parse_one(
    key: str,
    raw: Any,
) -> dict[str, Any] | None:
    """Return the inert sub-report for one configured switch, or None if off."""
    if raw is None or raw is False:
        return None
    if raw is True:
        enabled = True
        preset = _DEFAULT_PRESETS.get(key)
    elif isinstance(raw, dict):
        enabled_raw = raw.get("enabled", True)
        if not isinstance(enabled_raw, bool):
            raise ValidationFailedError(f"stored retrieval config {key}.enabled must be a boolean")
        enabled = enabled_raw
        if key == "hyde" and "preset" in raw:
            raise ValidationFailedError(
                "stored retrieval config hyde is flag-only and takes no preset"
            )
        preset_raw = raw.get("preset")
        if preset_raw is None:
            preset = _DEFAULT_PRESETS.get(key)
        else:
            preset = str(preset_raw).strip().lower()
            allowed = _PRESET_ALLOWLISTS[key]
            if preset not in allowed:
                raise ValidationFailedError(
                    f"stored retrieval config {key}.preset must be one of {sorted(allowed)}"
                )
    else:
        raise ValidationFailedError(f"stored retrieval config {key} must be a boolean or object")
    if not enabled:
        return None
    report: dict[str, Any] = {
        "enabled": True,
        "applied": False,
        "reason": _INERT_REASONS[key],
    }
    if preset is not None:
        report["preset"] = preset
    return report


def parse_query_preset_settings(
    retrieval_defaults: dict[str, Any],
) -> dict[str, Any] | None:
    """Validate + report the T2-8 switches; ``None`` means none are configured.

    Fail-closed on malformed stored configs, exactly like the T9 parsers. The
    return value is meta-only evidence — every sub-report carries
    ``applied: false`` until the evaluation gate promotes the behaviour.
    """
    source = retrieval_defaults or {}
    report: dict[str, Any] = {}
    for key in _ORDERED_KEYS:
        sub = _parse_one(key, source.get(key))
        if sub is not None:
            report[key] = sub
    return report or None


__all__ = [
    "MULTI_QUERY_EXPANSION_PRESETS",
    "QUERY_REWRITE_PRESETS",
    "parse_query_preset_settings",
]
