# AS-03 Independent Critic Verdict — Iteration 3

**Phase:** AS-03 — MCP and Connector Registry, Credential Principals, Secret Boundary, and Health  
**Feature:** AS-F004  
**Verdict:** `changes_requested`  
**Reviewer:** independent fresh-context Critic  
**Date:** 2026-07-18

## Release-blocking finding

### C-04 [P1] MCP registry accepts private destinations at save time

`AS-MCP-003` explicitly requires URL, Origin, and private-network policy
validation before saving. AS-F004 likewise requires private and disallowed
destinations to be rejected.

- `src/api/schemas/mcp.py::_validate_https_url` validates the scheme,
  hostname syntax, userinfo, and fragment only; it does not classify literal
  addresses or resolve hostnames.
- The create and update routes pass the accepted values directly to the
  repository.
- Repository create/update persist `base_url` without a destination-policy
  check.
- The existing API negative case covers `http://127.0.0.1`, which exercises
  only the TLS rule. It does not cover an HTTPS loopback, link-local/metadata,
  private literal, or a hostname resolving to a disallowed address.
- The durable security matrix therefore overstates save-time SSRF validation.

The Critic independently reproduced the gap with `MCPServerCreate`: both
`https://127.0.0.1` and `https://169.254.169.254` were accepted. Runtime
discovery and invocation still reject these before network access, so this is
not evidence of successful SSRF egress. It is nevertheless a direct,
non-waivable save-time contract failure.

Create and update must reject literal private, loopback, link-local, metadata,
reserved and non-global targets, as well as hostnames resolving to disallowed
addresses, before persistence. Negative API coverage and corrected durable
evidence are required.

## Prior-finding revalidation

The requested iteration-1/2 findings are closed on the reviewed source:

- **C-01:** MCP and OAuth connect to a validated IP-literal target while
  retaining the original Host and TLS SNI; OAuth authorities use isolated,
  non-reused request connections.
- **C-02:** remote risk/read-only annotations are normalized to
  `medium`/false. Public/embed grants require a Tenant Admin, a service-account
  principal, a read-only approval, and the exact current schema hash. Drift and
  legacy null hashes fail closed.
- **C-03/C-03b:** compatibility is recursive and conservative. Annotation-only
  changes and required removals remain compatible; property additions,
  including optional typed additions under a previously open schema, and
  unknown validation changes are breaking.

Secret references, composite tenant isolation, Streamable HTTP, OAuth
PKCE/resource/audience, redirect denial, response limits, exact Version
allowlisting, Connector principal parity, legacy Assistant compatibility,
rollback/degradation, migration 074, and runtime authorization otherwise match
the phase contract.

## Independent command evidence

All required commands ran with zero skips:

- MCP API/runtime: API `5 passed`; Assistant runtime/policy/Connector
  `23 passed`.
- MCP security: `19 passed`.
- Exact AS-03 Ruff command: `All checks passed!`.
- Supplemental migration contract: `3 passed`.
- Required live regression: isolation `6 passed`; AHR groups
  `28/77/8/98`; golden pass, critical pass, and trajectory pass rates all
  `1.0`.

Passing tests did not cover the save-time HTTPS-private counterexample.

## Docker restoration

Before mutation, all repository-owned `ai-gateway-*` containers had
`com.docker.compose.project.working_dir=/Users/yang/projects/AI--Platfform`.
Current source was hot-copied without a build or prune. After validation:

- all eight services were healthy;
- Gateway and Assistant were restored to `ASSISTANT_E2E_STUB_LLM=false`;
- all ownership labels still pointed to this repository; and
- sampled total memory was approximately `731.5 MiB`, below the `3.5 GiB`
  ceiling.

## Waiver and scope judgment

- The real third-party OAuth smoke remains an honest external deferral because
  no approved credential or production Secret Store writer was supplied.
- No waiver applies to C-04; save-time private-destination validation is an
  explicit SSRF control.
- No AS-04, UI, deployment, or unrelated Connector expansion was found in the
  AS-03 slice.
- The Critic did not edit source, Oracle, loop state, completion gates, or Git
  state.

## Decision

`changes_requested`. AS-F004 must remain `failing`; do not run the AS-03
completion gate until C-04 is remediated and independently revalidated.
