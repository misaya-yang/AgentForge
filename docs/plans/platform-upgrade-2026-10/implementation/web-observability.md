# U-WEB-06 browser observability implementation

## Enablement and data boundary

The existing `VITE_TELEMETRY_ENDPOINT` / `runtime.telemetryEndpoint` must explicitly
name `/api/v1/telemetry/browser` on the console's own origin. Empty, external,
query-bearing, credential-bearing or alternate paths install no observer/exporter.
No external SaaS is used. The previous unrestricted chat `sendBeacon` path is
replaced by this collector; its local CustomEvent remains compatible.

At boot, the browser asks the authenticated config endpoint for a server-sampled
15-minute page session. Only sampled, unexpired sessions collect/export or inject
trace context. Unsampled sessions do not poll to obtain a better sample. Auth token,
account, permission or role changes invalidate queued data and pending config/export
requests synchronously, including A-B-A transitions. Nothing is stored persistently.

The wire schema contains only fixed metric/error names, route families, finite
numeric values, trace/span identifiers and up to five same-origin bundled JS
locations. It contains no user/tenant fields, arbitrary attributes, messages,
component/function stacks, URLs, queries, fragments, prompts, DOM elements, referrers
or user-agent strings. Legacy chat telemetry exports only numeric first-token and
run-duration observations. Both client and server enforce field allowlists.

## Production wiring

- `main.tsx` installs the bounded observer/exporter before rendering, without
  awaiting collection. `RouteTelemetry` records route commit-to-next-frame time;
  it is explicitly not a full backend/navigation duration.
- LCP uses the last candidate before input/hidden; CLS uses the largest 1-second-gap,
  5-second session window, excluding recent-input shifts. INP groups entries by
  interaction and uses the 98th-percentile candidate; quality is `native` with
  native interaction count and `observed` when the count is estimated. Unsupported
  browsers emit no fabricated metric. No element/target/source details are read.
- The shared Axios interceptor and fetch wrapper inject W3C `traceparent` only for
  sampled same-origin API requests; existing explicit trace context is preserved.
  SSE bytes/protocols remain untouched. Fetch header latency uses `api_headers`.
- Global errors, unhandled rejections and `RootErrorBoundary` feed the scrubbed error
  reporter. Error text/rejection values are discarded. Collector failures are
  swallowed and never retried or recursively reported.
- Export is bounded to 20 records per batch, 60 per browser authority/minute,
  with five-second transport cancellation and a no-retry same-origin POST.

## Gateway mounting contract

`platform-gateway/src/browser_telemetry.rs` provides `router(BrowserTelemetryApi)`.
The primary writer must export the module and merge its router in the existing
native request/identity context; it must supply the existing verified
`Arc<dyn migration_content::ContentAccess>`. Construct with:

```
BrowserTelemetryApi::new(identity, allowed_tenants, allowed_origins, sample_bps)
```

Tenant/origin lists are explicit; empty lists fail closed. Sample rate is capped
at 1000 basis points (10%); 100 means 1%. The server resolves user/tenant anew for
every request, binds page sessions to that verified authority, rejects forged
identity/body fields, requires an allowed Origin for POST, limits body to 16 KiB,
batch to 20, per-authority records to 60/minute, tenant records to 600/minute and
sampling config to 12/minute. Memory maps cap at 4096 entries with TTL cleanup.
The sink is the existing `tracing` pipeline (`browser_telemetry` target); it sends
nothing directly to an external network destination.

Routes: GET `/api/v1/telemetry/browser/config`, POST `/api/v1/telemetry/browser`.

## Verification boundary

Node tests cover the real browser boot module and fetch wrapper using synthetic
browser surfaces, the production Axios interceptor, allowlisting, sampling,
credential replacement, export limits, error scrubbing and vital calculations.
Gateway route tests cover verified tenant isolation, forged fields, sample binding,
origin, batch/body and admission limits. Test execution status belongs to the
primary integration evidence; source existence is not evidence that Rust tests ran.

Still required: mount and compile/test the collector, then perform managed-browser
acceptance against the actual authenticated collector/tenant policy and verify the
existing tracing/OTel sink receives the sampled records. RUM p75 INP dashboards and
production sampling effectiveness require measured traffic.

Private source-map upload is intentionally **not enabled** (`sourceMapsEnabled=false`).
No map uploader, public map serving or CI release success is claimed. The existing
production build does not enable source maps. A private build-artifact uploader,
access-controlled symbolication and an actual release verification remain required
before claiming that part of U-WEB-06 accepted.
