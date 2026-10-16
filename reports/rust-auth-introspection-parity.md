# Read-only credential introspection parity

2026-10-15. Source inspection and code only; no database/provider execution.

The retained source `src/api/v1/auth.py:428` resolves `/auth/me` via
`get_user_context`, permits absent profiles, and always returns the verified
credential tenant. `src/api/deps.py:352` explicitly preserves the external-JWT /
service-key contract when `get_user` returns None, but rejects inactive accounts,
changed tenant bindings, malformed authority and lookup failures. The four
profile-present/absent JWT/API-key cases in `tests/api/test_auth_tenant_identity.py`
are therefore current business assertions, not obsolete framework mechanics.

Root approved the narrow source-compatible GET-only metadata adapter. Auth
`router_with_introspection` delegates only `/auth/me` to an injected typed
resolver; the Gateway implementation uses the canonical cryptographic JWT decoder,
configured Redis allowlist, active/expiry-filtered DB keys, and current account
metadata/permission lookups. No caller user/tenant headers are consulted. A local
account mismatch denies; a failed lookup returns 503, never absent-profile fallback.
The source's configured static-key branch also remains available.

This adapter never constructs or caches a Principal. Existing protected-route
admission, current account requirements, session revision validation, API-key
restriction enforcement, and password/logout/validation handlers are unchanged.
The source's broader missing-profile external identity access to protected routes
remains a separate federation policy difference; the current change must not be
reported as closing that policy question.

Gateway owner coordinates actual mount and ignored isolated database regression
matrices. Compilation, execution and final mapping are pending coordinator proof.
