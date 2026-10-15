from __future__ import annotations

import httpx
import pytest
from ai_gateway_core.comm.retry import RetryPolicy


@pytest.mark.parametrize("method", ["POST", "PUT", "PATCH"])
def test_mutation_exception_retry_requires_replayable_body_and_idempotency_key(
    method: str,
) -> None:
    policy = RetryPolicy(max_attempts=2)
    error = httpx.ConnectError("temporary failure")

    assert not policy.can_retry_exception(
        error,
        method=method,
        body_replayable=True,
        idempotency_key=False,
    )
    assert not policy.can_retry_exception(
        error,
        method=method,
        body_replayable=False,
        idempotency_key=True,
    )
    assert not policy.can_retry_exception(
        error,
        method=method,
        body_replayable=True,
        idempotency_key=True,
    )


@pytest.mark.parametrize("method", ["GET", "HEAD", "OPTIONS"])
def test_safe_method_exception_retry_does_not_require_body_or_idempotency_key(
    method: str,
) -> None:
    policy = RetryPolicy(max_attempts=2)

    assert policy.can_retry_exception(
        httpx.RemoteProtocolError("temporary failure"),
        method=method,
        body_replayable=False,
        idempotency_key=False,
    )


def test_exception_retry_rejects_non_transient_error_and_disabled_attempts() -> None:
    error = httpx.ConnectError("temporary failure")

    assert not RetryPolicy(max_attempts=1).can_retry_exception(
        error,
        method="GET",
        body_replayable=False,
    )
    assert not RetryPolicy(max_attempts=2).can_retry_exception(
        ValueError("not a transport retry"),
        method="GET",
        body_replayable=False,
    )


def test_mutation_retry_requires_explicit_upstream_contract():
    assert RetryPolicy(idempotency_guaranteed=True).can_retry_exception(
        httpx.ConnectError("offline"), method="POST", body_replayable=True, idempotency_key=True,
    )
    assert not RetryPolicy().can_retry_response(
        503, method="POST", body_replayable=True, idempotency_key=True,
    )
