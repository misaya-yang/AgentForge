"""Process-local counters for the knowledge vector store hot path.

Uses the same bounded-label pattern as other Runtime metrics:
exercisable counters on the shipped retrieval path, used by tests to prove
``get_collection`` call ceilings (SPO-04 gate: ≤ 1 per interactive retrieve
without rerank).
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass
class VectorStoreMetrics:
    """Exercisable counters on the shipped vector store path."""

    get_collection_calls: int = 0

    def reset(self) -> None:
        self.get_collection_calls = 0


vector_store_metrics = VectorStoreMetrics()
