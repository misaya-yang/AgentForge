"""Transport-free identities shared by retrieval and the Qdrant implementation.

The implementation re-exports these same objects for its existing callers.
Route construction can use errors and deadline state without importing a client.
"""
from __future__ import annotations

import time
from contextvars import ContextVar

_INTERACTIVE_DEADLINE: ContextVar[float | None] = ContextVar(
    "knowledge_qdrant_interactive_deadline",
    default=None,
)


def remaining_interactive_budget_seconds() -> float | None:
    """Seconds left on the active retrieval-entrypoint budget, if one is set.

    ``None`` means no entrypoint opened a budget (e.g. unit tests calling the
    pipeline directly). Callers outside the Qdrant path — notably the rerank
    stage, whose own HTTP timeout sits outside this budget — use this to cap
    themselves to what remains of the interactive budget (PRD T2-3).
    """

    deadline = _INTERACTIVE_DEADLINE.get()
    if deadline is None:
        return None
    return deadline - time.monotonic()


class VectorStoreError(RuntimeError):
    pass


class CollectionReadAuthorityError(VectorStoreError):
    """Qdrant collection ownership or lexical metadata is unsafe for reads."""


# Preserve the established exception qualification and pickle import path.
VectorStoreError.__module__ = __name__.rsplit(".", 1)[0] + ".vector_store"
CollectionReadAuthorityError.__module__ = VectorStoreError.__module__
