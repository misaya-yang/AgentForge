# ADR-011: Pure shared contracts and Gateway metrics ownership

Status: Accepted, 2026-09-07.

Scope: The reviewed phase-one contracts allowlist additions and removal of import exceptions.
This does not expand deployment units or relax the prohibition on new domain implementations in core.

## Decision

Three modules are explicitly admitted to `ai_gateway_contracts`:

| Module | Shared content | Authority retained elsewhere |
| --- | --- | --- |
| `pricing` | Decimal-preserving price snapshot, tenant/provider/model binding and deterministic version verification | Gateway selects prices, authorizes models and settles usage |
| `database_revision` | Baseline identifier and minimum/maximum compatible epoch constants | Database authority migrates/verifies; each service enforces its startup precondition |
| `retrieval_metrics` | Deterministic ranking metrics and their judgement/report data structures | Knowledge owns retrieval/evaluation data; Gateway owns its evaluation orchestration |

These modules contain pure values, validation or computation. They must not gain filesystem,
environment, database, provider, HTTP, cache or service configuration access. The contracts content
and stdlib allowlists record this bounded review; they are not permission to move arbitrary
implementations into the shared package.

The metrics collector remains at `src/services/metrics/collector.py`, owned by Gateway.
`src/core/observability/metrics.py` is a Gateway-local compatibility export.
`ai_gateway_core.comm.client.configure_service_metrics` accepts an explicitly injected metric
factory; Gateway startup binds `get_service_metrics`. Without a sink, transport still works without
instrumentation. Core neither imports Gateway nor contains a duplicate collector implementation.
The temporary new `ai_gateway_core.metrics.collector` module was removed.

## Enforcement and evidence

`make core-boundary-gate` checks the reviewed contracts, absence of core module growth and the
existing consumer/shim ledger. `make architecture-boundary-gate` checks import direction with zero
exceptions. Both gates passed after the ownership correction; transport tests cover injected,
missing and unavailable metric sinks. These are source/contract checks, not proof of live metrics,
provider behavior or platform release completion. Final evidence belongs in the vNext receipts.
