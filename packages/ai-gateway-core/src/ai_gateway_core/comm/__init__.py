"""Internal service-to-service communication primitives."""

from .client import (
    InternalServiceClient,
    InternalServiceClientConfig,
    InternalServiceHTTPError,
    TokenBucketRateLimiter,
)
from .idempotency import (
    IdempotencyMiddleware,
    InMemoryIdempotencyStore,
    RedisIdempotencyStore,
)
from .retry import RetryBudget, RetryPolicy

__all__ = [
    "InternalServiceClient",
    "InternalServiceClientConfig",
    "InternalServiceHTTPError",
    "TokenBucketRateLimiter",
    "RetryBudget",
    "RetryPolicy",
    "IdempotencyMiddleware",
    "InMemoryIdempotencyStore",
    "RedisIdempotencyStore",
]
