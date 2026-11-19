# Preliminary incident notes — not yet validated

These notes were written before reproduction and may be wrong.

- `INCREASE_IDEMPOTENCY_TTL`: Operations suspects Redis evicted the retry key because `idempotency_ttl_seconds` is only 30. Suggested action: raise it to 3600.
- `ROUND_EACH_SHARE_INDEPENDENTLY`: Finance suspects shares merely need `ROUND_HALF_UP` independently before conversion to cents.

Do not implement either suggestion unless the tests and code path prove it is the root cause.
