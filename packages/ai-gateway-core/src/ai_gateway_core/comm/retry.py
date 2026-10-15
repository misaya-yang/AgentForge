"""Retry policy and retry-budget primitives for internal HTTP calls."""

from __future__ import annotations

import random
import threading
from dataclasses import dataclass, field

import httpx

_RETRIABLE_EXCEPTIONS = (
    httpx.ConnectError,
    httpx.RemoteProtocolError,
    httpx.PoolTimeout,
)


class UpstreamOutcomeUnknown(RuntimeError):
    """The upstream may have accepted a side effect; replay needs reconciliation."""


def outcome_is_unknown(exc: BaseException, *, method: str) -> bool:
    if method.upper() in {"GET", "HEAD", "OPTIONS"}:
        return False
    if isinstance(exc, (httpx.ReadTimeout, httpx.WriteTimeout, httpx.ReadError,
                        httpx.WriteError, httpx.RemoteProtocolError)):
        return True
    return isinstance(exc, httpx.HTTPStatusError) and exc.response.status_code >= 500


@dataclass(frozen=True)
class RetryPolicy:
    """Bounded retry policy for service-to-service HTTP calls.

    ``max_attempts`` is total attempts, not retries. ``2`` means one retry.
    429 is intentionally excluded from the default retryable statuses because
    retrying a rate limit response can amplify overload unless a caller has a
    route-specific policy that honors ``Retry-After``.
    """

    max_attempts: int = 2
    base_delay_ms: int = 50
    max_delay_ms: int = 500
    retry_status_codes: frozenset[int] = field(
        default_factory=lambda: frozenset({502, 503, 504})
    )
    idempotent_methods: frozenset[str] = field(
        default_factory=lambda: frozenset({"GET", "HEAD", "OPTIONS"})
    )
    jitter: bool = True
    # Set only for a documented upstream deduplication contract. A header alone is insufficient.
    idempotency_guaranteed: bool = False

    def can_retry_exception(
        self,
        exc: BaseException,
        *,
        method: str,
        body_replayable: bool,
        stream_started: bool = False,
        idempotency_key: bool = False,
    ) -> bool:
        if stream_started or self.max_attempts <= 1:
            return False
        if not isinstance(exc, _RETRIABLE_EXCEPTIONS):
            return False
        return self._method_can_retry(
            method,
            body_replayable=body_replayable,
            idempotency_key=idempotency_key,
        )

    def can_retry_response(
        self,
        status_code: int,
        *,
        method: str,
        body_replayable: bool,
        stream_started: bool = False,
        idempotency_key: bool = False,
    ) -> bool:
        if stream_started or self.max_attempts <= 1:
            return False
        if status_code not in self.retry_status_codes:
            return False
        return self._method_can_retry(
            method,
            body_replayable=body_replayable,
            idempotency_key=idempotency_key,
        )

    def delay_seconds(self, retry_index: int) -> float:
        """Return bounded exponential backoff for retry #1, #2, ..."""
        if self.base_delay_ms <= 0 or self.max_delay_ms <= 0:
            return 0.0
        delay_ms = min(
            self.max_delay_ms,
            self.base_delay_ms * (2 ** max(retry_index - 1, 0)),
        )
        if self.jitter:
            delay_ms = random.uniform(0, delay_ms)
        return delay_ms / 1000.0

    def _method_can_retry(
        self,
        method: str,
        *,
        body_replayable: bool,
        idempotency_key: bool,
    ) -> bool:
        normalized = method.upper()
        if normalized in self.idempotent_methods:
            return True
        return bool(body_replayable and idempotency_key and self.idempotency_guaranteed)


class RetryBudget:
    """Small in-process retry budget to avoid unlimited retry amplification."""

    def __init__(self, *, budget_ratio: float = 0.1, min_retry_tokens: int = 10) -> None:
        self.budget_ratio = max(float(budget_ratio), 0.0)
        self.min_retry_tokens = max(int(min_retry_tokens), 0)
        self._originals = 0
        self._retries = 0
        self._lock = threading.Lock()

    def record_original(self) -> None:
        with self._lock:
            self._originals += 1

    def try_acquire_retry(self) -> bool:
        with self._lock:
            allowed = max(
                self.min_retry_tokens,
                int(self._originals * self.budget_ratio),
            )
            if self._retries >= allowed:
                return False
            self._retries += 1
            return True

    @property
    def snapshot(self) -> tuple[int, int]:
        with self._lock:
            return self._originals, self._retries
