# Cross-service infrastructure primitives

This crate owns bounded infrastructure helpers rather than hosted Gateway/Knowledge domain
implementation. It has no local workspace crate dependencies. It deliberately contains no actor
store, AuthService, provider registry, routing/admission/quota policy, Knowledge retrieval/ingestion,
connector broker, schemas/migrations or service bootstrap. HTTP middleware and telemetry perform
I/O, so this is runtime infrastructure support, **not a pure wire-schema contract crate**.

Extracted code preserves cipher/HMAC envelope spelling, deterministic JSON/float formatting,
explicit-environment endpoint precedence, DNS address classification, HTTP deadlines/stream ownership,
redacted PostgreSQL errors and cooperative cancellation. Cipher and tracing modules are byte copies;
other modules keep the original algorithm blocks with only local imports or narrowed dependencies.
The cipher service module contains only its trait/error, never provider management implementation.
HTTP client errors contain only the failure/validation classifications needed by bounded clients,
without importing Gateway capacity/admission errors. The cancellation signal contains no admission
policy and retains its original watch-channel ownership behavior.

Gateway may continue to use its existing implementations until canonical reexports are integrated.
The mandatory PostgreSQL error reexport described in the external integration note preserves one
Rust error identity for Gateway content adapters; duplicate error types cannot replace that step.
Tests are local contract checks, not a live-service or completed architecture-boundary acceptance.

`actor_verifier` is an explicitly injected JWT-principal verification port plus the unchanged
credential-selection predicate. It holds no signing keys or JWT parser. AuthService implements the
port in its own crate and retains current account/session/tenant authority. The port has no
anonymous/default success implementation; Knowledge signed HMAC ingress remains separate.
