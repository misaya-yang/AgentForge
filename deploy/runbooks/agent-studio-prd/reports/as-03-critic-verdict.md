# AS-03 Independent Critic Verdict

**Phase:** AS-03 — MCP and Connector Registry, Credential Principals, Secret Boundary, and Health  
**Feature:** AS-F004  
**Verdict:** `approved`  
**Reviewer:** independent fresh-context Critic, iteration-3 amended review  
**Date:** 2026-07-18

## Decision

AS-03 / AS-F004 is approved on the final frozen source. The first three
`changes_requested` artifacts remain preserved as iteration history. The third
Critic amended its verdict only after independently reviewing the C-04
remediation and rerunning every required final-source gate.

No material open finding remains. Real third-party OAuth success remains an
honest external-input deferral because no approved credential or production
Secret Store writer was supplied; it is not represented as production-provider
evidence and does not waive any required security gate.

## Finding closure

### C-01 — connection-effective DNS pinning

Closed. MCP and OAuth requests connect to an already validated IP-literal
target while retaining the reviewed hostname in HTTP Host and TLS SNI. OAuth
requests across authorities use isolated, non-reused request connections.
Redirects, rebinding, mixed-address DNS and unsafe addresses fail closed.

### C-02 — untrusted remote read-only/risk metadata

Closed. Remote catalog hints are normalized to medium risk and not read-only.
Public/embed requires a Tenant Admin's service-account, read-only, exact current
schema-hash approval. Schema drift, delegated principals, wrong channel, and
legacy null-hash grants fail closed.

### C-03/C-03b — conservative recursive schema compatibility

Closed. Nested object and array schemas are compared recursively. Property
additions, including an optional typed property under a previously open schema,
constraint/type changes and unknown validation changes are breaking.
Annotation-only changes and required removals remain compatible.

### C-04 — save-time private destination validation

Closed. The public schema normalizes literal IPv4, IPv6 and IPv4-mapped IPv6
and rejects non-global targets. Before persistence, create and update resolve
MCP base and OAuth metadata hostnames through the shared destination policy;
all DNS records must be global. DNS failure and policy denial return stable,
URL/IP-free `422` responses. Negative API tests prove rejected create leaves the
registry empty and rejected update leaves the original URL unchanged. Runtime
still repeats destination validation and pins the actual connection.

## Requirement assessment

| Requirement | Verdict |
| --- | --- |
| R1 tenant registry and secret boundary | approved — composite tenant references, redacted secret-ref-only schemas, immutable discovery rows and idempotent migration pass |
| R2 protocol and network security | approved — save-time plus runtime SSRF policy, actual socket target pinning, TLS/Origin/redirect/OAuth/limits/session checks pass |
| R3 credential principal and Connector boundary | approved — exact current caller/principal/channel/scope/audience/revoke checks and public exact-hash Admin approval pass |
| R4 immutable discovery and runtime authorization | approved — exact Version/hash binding, conservative diff, timeout/concurrency/circuit and drift denial pass |
| R5 degradation, rollback and compatibility | approved — stable redacted failures, Agent-only rollback flag and unchanged built-in Assistant behavior pass |

## Independent final-source validation

- Exact MCP API/runtime command: API `6 passed`; Assistant runtime/policy and
  Connector `23 passed`; zero skips.
- Exact MCP security command: `19 passed`; zero skips.
- Exact AS-03 Ruff command: `All checks passed!`.
- Supplemental PostgreSQL migration command: `3 passed`; zero skips.
- Required live regression: isolation `6 passed`; zero skips; AHR groups
  `28/77/8/98`; golden pass, critical pass, and trajectory pass rates all
  `1.0`.

## Runtime and scope evidence

- All eight repository-owned services were healthy after review.
- Every Compose ownership label was
  `/Users/yang/projects/AI--Platfform`.
- Gateway and Assistant were restored to
  `ASSISTANT_E2E_STUB_LLM=false`.
- Gateway/Assistant hot-source hashes matched the host.
- Sampled total memory was approximately `730.5 MiB`, below the operator's
  `3.5 GiB` ceiling.
- No image build, prune, deployment, commit, push, production secret write, or
  provider call occurred.
- No AS-04, Studio UI, publication channel, new Connector type, or production
  MCP configuration was added.

The Critic changed no source, report, Oracle, loop, gate, or Git state.
