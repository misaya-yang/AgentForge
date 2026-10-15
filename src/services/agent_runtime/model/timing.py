"""Structured additive model-plane timing evidence."""

from __future__ import annotations

import logging
import re

from ..timing import TIMING_SCHEMA_VERSION, ModelPlaneTiming
from .authorization import _AuthorizedCall


def _log_model_plane_timing(
    self,
    wire: str,
    call: _AuthorizedCall,
    timing: ModelPlaneTiming,
    *,
    _logger: logging.Logger,
) -> None:
    """Server-side evidence for PPR-00: one parseable line per completed call.

    Internal observability only — no public API, SSE envelope, or schema
    surface carries these values.
    """
    components = timing.components()
    # Fixed-point 6-decimal rendering: str(float) can emit scientific
    # notation (e.g. 9.7e-05), which downstream log parsers must not have
    # to special-case. "None" stays literal for unset stamps.
    rendered = " ".join(
        f"{key}={'None' if value is None else format(value, '.6f')}"
        for key, value in components.items()
    )
    _logger.info(
        "Agent model-plane timing schema=%s wire=%s run_id=%s call_id=%s model=%s %s",
        TIMING_SCHEMA_VERSION,
        wire,
        call.run_id,
        call.call_id,
        # model_id is tenant-editable (schemas/providers.py has no character
        # pattern): whitespace folding prevents forged key=value pairs or
        # extra lines in this parsed-evidence channel. run_id/call_id are
        # UUID-typed and cannot carry separators.
        re.sub(r"\s+", "_", call.model_id),
        rendered,
    )
