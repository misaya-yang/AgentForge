"""
Exception hierarchy for the AI Assistant SDK.

All exceptions carry structured metadata (status_code, message, request_id)
so callers can log, retry, or surface errors without parsing strings.
"""

from __future__ import annotations


class AssistantError(Exception):
    """Base exception for all SDK errors."""

    def __init__(
        self,
        message: str,
        *,
        status_code: int | None = None,
        request_id: str | None = None,
    ) -> None:
        super().__init__(message)
        self.message = message
        self.status_code = status_code
        self.request_id = request_id

    def __repr__(self) -> str:
        parts = [f"message={self.message!r}"]
        if self.status_code is not None:
            parts.append(f"status_code={self.status_code}")
        if self.request_id is not None:
            parts.append(f"request_id={self.request_id!r}")
        return f"{type(self).__name__}({', '.join(parts)})"


class AuthError(AssistantError):
    """Authentication or authorization failure (HTTP 401 / 403)."""


class NotFoundError(AssistantError):
    """Requested resource does not exist (HTTP 404)."""


class RateLimitError(AssistantError):
    """Too many requests (HTTP 429).

    Attributes:
        retry_after: Seconds the server suggests waiting before retrying.
    """

    def __init__(
        self,
        message: str,
        *,
        status_code: int | None = 429,
        request_id: str | None = None,
        retry_after: float | None = None,
    ) -> None:
        super().__init__(message, status_code=status_code, request_id=request_id)
        self.retry_after = retry_after


class ValidationError(AssistantError):
    """Request payload failed server-side validation (HTTP 422)."""


class ServerError(AssistantError):
    """Internal server error (HTTP 5xx)."""


class UnknownOutcomeError(AssistantError):
    """A submitted mutation may have completed. Read its state before repeating it."""


class StreamError(AssistantError):
    """SSE connection or parsing failure."""


class TimeoutError(AssistantError):  # noqa: A001 — intentionally shadows builtins.TimeoutError
    """Request exceeded the configured timeout."""


# ---------------------------------------------------------------------------
# Mapping helpers (used by transport layer)
# ---------------------------------------------------------------------------

_STATUS_MAP: dict[int, type[AssistantError]] = {
    401: AuthError,
    403: AuthError,
    404: NotFoundError,
    422: ValidationError,
    429: RateLimitError,
    500: ServerError,
    502: ServerError,
    503: ServerError,
    504: ServerError,
}


def error_from_status(
    status_code: int,
    message: str,
    *,
    request_id: str | None = None,
    retry_after: float | None = None,
) -> AssistantError:
    """Instantiate the appropriate exception for an HTTP status code."""
    cls = _STATUS_MAP.get(status_code, AssistantError)
    if cls is RateLimitError:
        return RateLimitError(
            message,
            status_code=status_code,
            request_id=request_id,
            retry_after=retry_after,
        )
    return cls(message, status_code=status_code, request_id=request_id)
