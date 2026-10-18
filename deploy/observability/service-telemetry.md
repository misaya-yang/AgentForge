# Service OTLP integration (U-PE-11 / U-RT-09)

The native Gateway, Knowledge API and Knowledge worker share
`platform-service-primitives::distributed_tracing`. Gateway foundation re-exports this
module instead of maintaining a second exporter. Runtime and Capability Worker use the
platform overlay's `telemetry` module. Both platform exporters use OTel 0.32.0 / SDK
0.32.1; upstream Codex harness sources and their own dependency graph stay unchanged.

The deployable collector/backends remain in [`docker/monitoring`](../../docker/monitoring/README.md).
This document does not introduce a second observability deployment.

Native `OTEL_EXPORTER_OTLP_ENDPOINT` is an HTTP(S) base URL, normally
`http://otel-collector:4318`. An absent/empty endpoint preserves in-process tracing and
creates no exporter or network client. `OTEL_SDK_DISABLED=true` disables native export.
Runtime/Worker also require `AI_PLATFORM_OTEL_ENABLED=true`. All exporters send explicit
HTTP/protobuf `/v1/traces`, `/v1/metrics`, `/v1/logs`. Endpoint user-info, query and fragment
are rejected; transport requests do not follow redirects. Collector credentials are
transport configuration and never request attributes.

Native initialization happens on a dedicated thread; SDK export happens on bounded batch
threads (1024 queued spans/logs, batches of 128, two-second scheduling). Requests do not
await exporter I/O. Every signal uses a three-second network timeout. Initialization or
export failure logs fixed, rate-limited diagnostics without transport error text, endpoint
or response bodies. Each exporter receives a four-second shutdown attempt after business
drain; an earlier signal's failure does not skip later signals. Existing native process
entrypoint calls to `provider.shutdown()` now flush all signals, and run on blocking threads.
The guard retains SDK tracer method access for existing tests/callers.

Inbound HTTP middleware extracts W3C context and attaches it on every future poll. It
returns the original response body without buffering SSE/downloads. It records canonical
HTTP methods, registered route templates and response status only. Unmatched paths use the
fixed `unmatched` route and unknown methods use `_OTHER`. URL/query strings, dynamic path
segments, user-agent, authorization, request IDs, bodies and exception strings are excluded.
Each request also emits `http.server.request.duration` and a fixed correlated
`http.request.finished` OTLP log. Arbitrary application fmt logs are not bridged.

GenAI conventions are pinned to **1.37.0**, schema
`https://opentelemetry.io/schemas/1.37.0`. Runtime wraps actual generation streams with
`chat`; admission/terminal callbacks delimit `invoke_agent`; actual dispatch/Worker execution
uses `execute_tool`; approval persistence boundaries delimit `approval_wait`. The first text
delta records time to first chunk without examining text. Completed events supply actual
nonnegative input/output tokens. Errors and cancellation end incomplete operations with a
fixed status, preserving the original stream, content, cancellation and business errors.

`genai_telemetry::record_settled_usage(tenant, model, input, output, usd)` records native
`agentforge.gen_ai.token.usage` cumulative counters by token direction and
`agentforge.gen_ai.cost` in USD. Call it only after an actual newly committed accounting
observation, using actual charged USD, preserving existing reconciliation/deduplication.
A rejected telemetry dimension or amount never fails accounting. Runtime does not estimate
cost or claim to provide an accounting ledger. Runtime uses the standard
`gen_ai.client.token.usage` histogram for individual provider observations, and standard
operation-duration/time-to-first-chunk histograms. Counter deltas and request histograms
have distinct semantics.

All tenant dimensions are `sha256:<full SHA256 of the exact authenticated tenant ID>`;
`genai_telemetry::tenant_dimension` computes the same lookup label. This supports the
existing case-sensitive varchar tenant contract without exporting names. Empty, oversized
or control-bearing IDs are rejected. Model labels allow at most 128 ASCII identifier
characters. The collector must retain `agentforge.tenant.id`, `gen_ai.token.type`,
`gen_ai.request.model` and `outcome` on metric datapoints and `gen_ai.semconv.version` on
resources, or distinct tenants/token directions would collapse after privacy processing.
Token/cost metrics are observations and may be dropped during outages; the durable billing
store remains authoritative. Dashboards exposing per-tenant metrics require the operator's
existing observability access controls.

## Verification boundaries

Unit tests include W3C remote parent/sampling propagation, nonbuffering pending response
bodies, seeded query/path/header privacy, safe failure diagnostics, disabled exporters,
invalid endpoints, tenant/model dimensions, stream cancellation and fixed terminal spans.
The native loopback test drives real SDK HTTP exporters and shutdown to receive all three
signal paths; it exercises transport without a collector backend. It is not browser-to-Worker
acceptance and does not establish collector transform/sampling/backend correctness.

Product acceptance remains mandatory: real browser → Gateway → Runtime → model → Worker
same-trace hierarchy; actual tenant token and settled cost metrics after collector processing;
OTLP logs correlated to their spans; exporter disabled/unavailable, queue saturation,
cancellation and shutdown cases; raw export inspection with seeded sensitive content. Run
these only in the authorized controlled deployment. Source formatting or isolated SDK tests
do not close U-PE-11/U-RT-09 by themselves.
