# AS-03 Independent Critic Verdict - Iteration 1

**Phase:** AS-03 - MCP Registry, Secret Boundary, and Health

**Feature:** AS-F004

**Critic:** independent fresh context reviewer

**Critic Verdict:** changes requested

**Actor Report:** `docs/agent-studio-prd/reports/as-03-mcp-registry-secret-boundary-and-health-report.md`

**Date:** 2026-07-18

## Verdict Summary

AS-03 iteration 1 is not approved. The registry, secret-reference boundary,
credential principals, exact Agent binding, redaction, rollback flag, and local
mock coverage are substantial, but three material fail-closed requirements are
not yet satisfied. AS-F004 must remain failing and no completion gate may be
claimed from this verdict.

## Material Findings

### C-01 - DNS validation does not pin the actual network connection (P1)

`MCPClient` and the OAuth coordinator validate a resolved address set before a
request, but `httpx` still receives the original hostname and performs another
resolution for the socket. The test named for DNS rebinding compares two
preflight resolver results; it does not prove that the request reaches the
validated address. A resolver change between validation and transport remains
possible.

Required correction: bind the actual transport connection to a validated IP,
or use an equivalently controlled egress transport, while preserving the
original Host and TLS SNI. Add a test that observes the real request target and
fails if the transport receives the hostname.

### C-02 - Public read-only eligibility trusts remote MCP metadata (P1)

Discovery maps the remote-controlled `annotations.readOnlyHint` into persisted
`read_only=true` and low risk. Public/embed grant and runtime authorization then
use that value. A malicious write-capable MCP server can therefore self-declare
its tool read-only and become eligible for an anonymous service-account grant.

Required correction: treat catalog annotations as untrusted. Public read-only
eligibility must be an Admin or platform-controlled assertion, bound to the
exact reviewed tool/schema, and must fail closed after schema drift.

### C-03 - Schema compatibility diff is shallow (P2)

`schema_diff` checks only top-level properties, required fields, and immediate
property types. Nested object, array-item, enum, or other validation changes can
be reported non-breaking even when prior calls become invalid.

Required correction: use a conservative recursive compatibility check. Any
changed validation keyword whose safety is not proved must default to breaking,
with negative coverage for nested drift.

## Independent Validation

The Critic independently reproduced the following on the reviewed iteration-1
source:

- MCP API: 5 passed;
- Assistant MCP/runtime/principal suite: 21 passed;
- MCP security: 17 passed;
- PostgreSQL MCP migration suite: 3 passed;
- exact Ruff command: passed;
- Assistant runtime verification: passed; and
- environment-default isolation: 4 passed and 2 skipped.

The Critic did not independently reproduce the Actor's explicit offline-stub
6/6 isolation run. Consequently, even apart from C-01 through C-03, the full
required regression evidence was not independently established in this review.

## Requirement Assessment

| Requirement | Iteration-1 assessment | Reason |
| --- | --- | --- |
| R1 registry and secret boundary | pass | Tenant CRUD, principal separation, redaction, and secret-ref persistence are supported. |
| R2 protocol and network security | fail | The actual socket is not bound to the validated DNS result. |
| R3 principal and public-channel boundary | fail | Remote read-only metadata can establish public eligibility. |
| R4 immutable discovery and runtime | partial | Exact hashes are enforced, but nested compatibility reporting is incomplete. |
| R5 honest degradation and rollback | mostly pass | Stable errors, rollback flag, and external-smoke boundaries are honestly reported. |

## Accepted Boundaries

The additive rollback path, absence of a real third-party OAuth success claim,
and the stated external-smoke limitation are acceptable. No unrelated scope
expansion was identified as a reason for rejection.

## Handoff Decision

The Actor must close C-01, C-02, and C-03, rerun every required command with an
unskipped final isolation result, preserve this finding history, and request a
new fresh-context Critic review. This iteration cannot authorize an Oracle state
transition or AS-03 completion gate.
