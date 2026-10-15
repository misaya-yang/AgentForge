"""Pure validation shared by Dataset writes and ingestion; no provider calls."""

from __future__ import annotations

import math
from typing import Any

from ....core.exceptions import ValidationFailedError
from .cascade import CascadeConfig, default_cascade_config
from .registry import register_defaults

MAX_STAGES = 8
MAX_PARALLELISM = 8
_ALLOWED_OPTIONS = {
    "text_layer": {"confidence_floor_chars", "preserve_boundaries"},
    "legacy_ocr_vlm": {"confidence"},
    "general_vlm_fallback": {"confidence"},
    "mineru": set(),
    "paddle_ppstructure_v3": set(),
    "paddleocr_vl": set(),
}


def _fail(field: str, reason: str) -> None:
    raise ValidationFailedError(f"index_config.parsing.{field}: {reason}")


def _boolean(value: Any, field: str) -> None:
    if type(value) is not bool:
        _fail(field, "must be a boolean")


def _confidence(value: Any, field: str) -> None:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
        or not 0 <= value <= 1
    ):
        _fail(field, "must be a finite number in 0..1")


def resolve_parsing_config(
    index_config: dict[str, Any],
) -> tuple[CascadeConfig, dict[str, Any]] | None:
    raw = index_config.get("parsing")
    if raw is None:
        return None
    if not isinstance(raw, dict):
        _fail("config", "must be an object")
    if set(raw) - {"enabled", "cascade"}:
        _fail("config", "contains unsupported fields")
    enabled = raw.get("enabled", False)
    _boolean(enabled, "enabled")
    cascade = raw.get("cascade")
    if cascade is None:
        cascade = default_cascade_config().to_dict()
    if not isinstance(cascade, dict):
        _fail("cascade", "must be an object")
    if set(cascade) - {"stages", "backend_options", "parallelism", "allow_partial"}:
        _fail("cascade", "contains unsupported fields")
    stages = cascade.get("stages")
    if not isinstance(stages, list) or not 1 <= len(stages) <= MAX_STAGES:
        _fail("cascade.stages", f"must contain 1..{MAX_STAGES} stages")
    known = set(register_defaults().names())
    for i, stage in enumerate(stages):
        field = f"cascade.stages[{i}]"
        if not isinstance(stage, dict) or set(stage) - {
            "backend",
            "min_confidence",
            "require_text_layer",
            "require_image",
        }:
            _fail(field, "must be an object with supported stage fields")
        if not isinstance(stage.get("backend"), str) or stage["backend"] not in known:
            _fail(field + ".backend", "unknown parser backend")
        _confidence(stage.get("min_confidence", 0.0), field + ".min_confidence")
        for key in ("require_text_layer", "require_image"):
            _boolean(stage.get(key, False), field + "." + key)
    parallelism = cascade.get("parallelism", 1)
    if type(parallelism) is not int or not 1 <= parallelism <= MAX_PARALLELISM:
        _fail("cascade.parallelism", f"must be an integer in 1..{MAX_PARALLELISM}")
    _boolean(cascade.get("allow_partial", False), "cascade.allow_partial")
    options = cascade.get("backend_options", {})
    if not isinstance(options, dict):
        _fail("cascade.backend_options", "must be an object")
    for backend, values in options.items():
        if backend not in known or not isinstance(values, dict):
            _fail("cascade.backend_options", "must map known backends to option objects")
        if set(values) - _ALLOWED_OPTIONS.get(backend, set()):
            _fail(f"cascade.backend_options.{backend}", "unsupported or server-owned option")
        if "confidence" in values:
            _confidence(values["confidence"], f"cascade.backend_options.{backend}.confidence")
        if "preserve_boundaries" in values:
            _boolean(
                values["preserve_boundaries"],
                "cascade.backend_options.text_layer.preserve_boundaries",
            )
        if "confidence_floor_chars" in values:
            count = values["confidence_floor_chars"]
            if type(count) is not int or not 1 <= count <= 100000:
                _fail(
                    "cascade.backend_options.text_layer.confidence_floor_chars",
                    "must be an integer in 1..100000",
                )
    if not enabled:
        return None
    config = CascadeConfig.from_dict(cascade)
    if any(stage.backend == "text_layer" for stage in config.stages):
        config.backend_options.setdefault("text_layer", {})["preserve_boundaries"] = True
    return config, config.to_dict()


def parsing_config_report(index_config: dict[str, Any]) -> dict[str, Any]:
    resolved = resolve_parsing_config(index_config)
    if resolved is None:
        return {
            "enabled": False,
            "source": "dataset" if "parsing" in index_config else "default",
            "backends": [],
            "warnings": [],
        }
    config, payload = resolved
    registry = register_defaults()
    backends = [
        {
            "name": stage.backend,
            "available": registry.create(
                stage.backend, **config.backend_options.get(stage.backend, {})
            ).is_available(),
        }
        for stage in config.stages
    ]
    return {
        "enabled": True,
        "source": "dataset",
        "cascade": payload,
        "backends": backends,
        "warnings": [
            f"{item['name']}: backend is not configured"
            for item in backends
            if not item["available"]
        ],
    }
