"""Gateway-owned additive timing schema for model-plane calls (PPR-00).

Single clock domain: all stamps are `time.perf_counter` readings taken inside the
gateway process at the model-plane boundary (the gateway container runs a single
uvicorn worker, so every stream path shares one monotonic clock).  This module is
internal observability only — the values never enter the public SSE envelope,
OpenAPI schema, SDK behavior, or any database column.

The four capture points and derived components are defined by the pre-declared
gates in `deploy/runbooks/platform-plane-restructure/phase-00-*.md`:

    t_stream_entry  ->  t_dispatch  ->  t_first_frame  ->  t_first_visible
      local_pre_provider   provider_wait    local_projection

Caveat fixed in the methodology: `provider_wait` measures provider TTFB to the
first parsed upstream frame; provider token *pacing* after that frame is
attributed to `local_projection` by definition.  This is a deliberate SLI
decomposition, not a causal claim about where the time physically went.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass

TIMING_SCHEMA_VERSION = "ppr-timing/v1"

# Pre-declared tolerances (phase-00 gates G1 and G3). Fixed before
# implementation; must never be relaxed to accommodate failing evidence.
IDENTITY_TOLERANCE_SECONDS = 1e-9  # fake-clock tests; same arithmetic
REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS = 5e-3  # per-trial rounding allowance
CLIENT_LOWER_BOUND_SECONDS = 0.010  # server window is a sub-interval of client
CLIENT_RESIDUAL_ABS_SECONDS = 0.200  # transport + kernel hop + auth + parse
CLIENT_RESIDUAL_RELATIVE = 0.05  # or 5% of client TTFT, whichever is larger


def _delta(start: float | None, end: float | None) -> float | None:
    if start is None or end is None:
        return None
    return end - start


@dataclass
class ModelPlaneTiming:
    """Additive decomposition of one model-plane call's time-to-first-visible.

    Each note* stamp is first-write-wins so a second provider event in the same
    call never overwrites the TTFT boundary.  A tool-only or errored call may
    leave later components as `None`; those are honestly missing measurements,
    never fabricated zeros.
    """

    clock: Callable[[], float]
    stream_started: float | None = None
    dispatched: float | None = None
    first_frame: float | None = None
    first_visible: float | None = None

    @classmethod
    def start(cls, clock: Callable[[], float]) -> ModelPlaneTiming:
        timing = cls(clock=clock)
        timing.stream_started = clock()
        return timing

    def note_dispatch(self) -> None:
        if self.dispatched is None:
            self.dispatched = self.clock()

    def note_first_frame(self) -> None:
        if self.first_frame is None:
            self.first_frame = self.clock()

    def note_first_visible(self) -> None:
        if self.first_visible is None:
            self.first_visible = self.clock()

    @property
    def local_pre_provider_seconds(self) -> float | None:
        return _delta(self.stream_started, self.dispatched)

    @property
    def provider_wait_seconds(self) -> float | None:
        return _delta(self.dispatched, self.first_frame)

    @property
    def local_projection_seconds(self) -> float | None:
        return _delta(self.first_frame, self.first_visible)

    @property
    def local_overhead_seconds(self) -> float | None:
        if self.local_pre_provider_seconds is None or self.local_projection_seconds is None:
            return None
        return self.local_pre_provider_seconds + self.local_projection_seconds

    @property
    def model_plane_ttft_seconds(self) -> float | None:
        return _delta(self.stream_started, self.first_visible)

    def components(self, *, round_digits: int | None = 6) -> dict[str, float | None]:
        """The five schema fields (predeclared names) for receipts and logs."""

        def _round(value: float | None) -> float | None:
            return None if value is None or round_digits is None else round(value, round_digits)

        return {
            "local_pre_provider_seconds": _round(self.local_pre_provider_seconds),
            "provider_wait_seconds": _round(self.provider_wait_seconds),
            "local_projection_seconds": _round(self.local_projection_seconds),
            "local_overhead_seconds": _round(self.local_overhead_seconds),
            "model_plane_ttft_seconds": _round(self.model_plane_ttft_seconds),
        }

    def identity_residual(self) -> float | None:
        """|sum(components) − model_plane_ttft|; None until all stamps exist."""
        if (
            self.local_pre_provider_seconds is None
            or self.provider_wait_seconds is None
            or self.local_projection_seconds is None
            or self.model_plane_ttft_seconds is None
        ):
            return None
        return abs(
            self.local_pre_provider_seconds
            + self.provider_wait_seconds
            + self.local_projection_seconds
            - self.model_plane_ttft_seconds
        )


def client_residual_within_tolerance(
    model_plane_ttft_seconds: float | None,
    client_ttft_seconds: float | None,
) -> bool:
    """Gate G3: reconcile the server sub-interval with client-observed TTFT.

    The server window starts after the client sent the request and ends at the
    gateway's yield boundary, so the client value must not be smaller than the
    server value (beyond `CLIENT_LOWER_BOUND_SECONDS`), and the residual
    (transport, kernel hop, gateway auth/control work, client parse) must stay
    within the pre-declared `max(abs, relative)` budget.
    """
    if model_plane_ttft_seconds is None or client_ttft_seconds is None:
        return False
    residual = client_ttft_seconds - model_plane_ttft_seconds
    if residual < -CLIENT_LOWER_BOUND_SECONDS:
        return False
    tolerance = max(CLIENT_RESIDUAL_ABS_SECONDS, CLIENT_RESIDUAL_RELATIVE * client_ttft_seconds)
    return residual <= tolerance


__all__ = [
    "CLIENT_LOWER_BOUND_SECONDS",
    "CLIENT_RESIDUAL_ABS_SECONDS",
    "CLIENT_RESIDUAL_RELATIVE",
    "IDENTITY_TOLERANCE_SECONDS",
    "ModelPlaneTiming",
    "REAL_CLOCK_IDENTITY_TOLERANCE_SECONDS",
    "TIMING_SCHEMA_VERSION",
    "client_residual_within_tolerance",
]
