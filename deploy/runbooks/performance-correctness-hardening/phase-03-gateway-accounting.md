# PCH-03 Gateway Accounting and Admission

Scope: quota boundaries, usage idempotency/pricing/stream semantics, Redis admission/rate-limit
and usage query/flush amplification.

Required gates:

- Calendar-boundary and cumulative-stream fixtures.
- PG query-shape and unique-conflict tests.
- Redis command-count/concurrency tests.
- Gateway API and billing regression suites.
