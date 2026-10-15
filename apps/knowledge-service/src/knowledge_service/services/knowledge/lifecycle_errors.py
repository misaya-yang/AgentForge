"""Transport-free lifecycle refusal shared by routes and the real service."""

from __future__ import annotations


class Bm25V2LifecycleError(RuntimeError):
    """Protocol refusal. ``http_status`` is a hint for the route integrator."""

    def __init__(self, message: str, *, code: str, http_status: int = 409) -> None:
        super().__init__(message)
        self.code = code
        self.http_status = http_status


# The real service re-exports this same class under its established public path.
Bm25V2LifecycleError.__module__ = __name__.rsplit(".", 1)[0] + ".bm25_v2_lifecycle"
