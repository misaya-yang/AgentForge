"""Process-local counters for gateway hot-path round-trips.

Uses the same bounded-label pattern as other Runtime metrics:
exercisable counters on the shipped path, used by tests to prove RTT ceilings
(SPO-02 gate: warm chat/proxy path ≤ 4 Redis round-trips).
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class GatewayHotPathMetrics:
    """Exercisable counters on the shipped gateway request path."""

    redis_round_trips: int = 0

    def reset(self) -> None:
        self.redis_round_trips = 0


gateway_hot_path_metrics = GatewayHotPathMetrics()
