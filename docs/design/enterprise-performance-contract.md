# Enterprise Gateway performance acceptance contract

Status: executable exploratory harness, not a measured performance result or an
accepted release. Owner: Gateway control/data-plane maintainers, with Runtime,
Knowledge and Capability owners accountable for their named timing boundaries.
The active implementation/evidence ledger is
[`native-enterprise-upgrade/loop-state.json`](../../deploy/runbooks/native-enterprise-upgrade/loop-state.json).

Performance is a core enterprise AI Gateway requirement. The acceptance unit is
a named workload on an identified deployment profile with correctness preserved.
Language choice, compilation, a healthy liveness probe, and a fast fake provider
do not individually establish that requirement.

## 1. Current executable baseline

[`scripts/harness/gateway_performance_baseline.py`](../../scripts/harness/gateway_performance_baseline.py)
is a dependency-free Python 3.10+ Linux measurement library. Its command-line
entrypoint only prints an offline plan. Live calls belong inside an explicitly
reviewed existing-fixture controller that already owns the current Gateway
process, application database, source/artifact receipts and authenticated actor.
The library never starts services, logs in, provisions accounts, reads a secret
file, changes grants, resets counters, dispatches a model, or retries a request.

Two fixed GET profiles prevent accidental expansion into paid/remote execution:

1. `/health/live`: validates exactly `{"status":"alive"}`. This is a transport and
   process lower-bound, bypassing actual auth, policy and database work. It cannot
   stand in for business-path throughput.
2. `/api/v1/assistant/runs/{canonical UUID}`: reads a verified owner-cancelled,
   synthetic inert SDK run, using only the controller's existing in-memory JWT.
   The body must retain that exact run ID, declared official framework, cancelled
   status, absent managed Runtime thread/turn identity and no leaked private
   request/service lookup fields. A false 200 is a failed sample.

The controller must verify the accepted binary SHA256 against the live process,
source manifest against the actual native source, `/proc` start identity, and
fixed owned numeric-loopback listener before calls and again after measurement.
It must preserve the exact terminal/revoked fixture state. It owns cleanup and
must retain all failed/partial receipts. A lost fixture, binary or credential is
not permission to reconstruct one or substitute an arbitrary running service.

Each profile sweeps concurrency 1, 2 and 4 on a finite closed-loop batch. One
warmup is issued on each connection, retained separately and excluded from the
measured distribution; failed warmup blocks that case. The default controller
uses 100 measured liveness requests and 12 measured owner reads per case. This
small authenticated sample is deliberately exploratory because the fixture's
real rate policy must remain enabled. Business cases must be separated by a
full naturally elapsed policy window and coordinated with other fixture calls.
No spoofed IP, altered limiter, counter deletion or grant expansion is allowed.

An explicitly selected 40-read/concurrency-4 rejection case may exercise the
actual rate guard. Its 429 count and rejection latency are recorded separately.
A 429 establishes policy rejection; it is not proof of exhausted database pools,
CPU saturation, admission-backpressure correctness, or maximum sustainable RPS.
If no rejection is observed, report that fact without manufacturing one.

### Built-in safety bounds

- Exact numeric loopback `127.0.0.1:18080`, no DNS, proxy, redirects or retries
- Fixed GET paths; owner token remains in memory and is never serialized
- At most 8 workers and 200 measured requests per case; default sweep uses 1/2/4
- Absolute per-request deadline at most 5 seconds and per-case deadline at most
  30 seconds; normal controller defaults are 2 and 15 seconds respectively
- Headers and chunk framing at most 8 KiB, response body at most 64 KiB
- Optional operator-supplied Gateway RSS stop budget, a resource safety limit,
  never a product SLO; stale process identity or sampler failure fails closed.
  The sampled stop prevents new requests; already in-flight requests can finish
  within their absolute per-request timeout. This is a bounded stop, not an
  instantaneous process kill
- Interrupted or timed-out client requests close their own transports. This
  does not prove remote execution cancellation or side-effect cessation.

## 2. Metric semantics and evidence limits

Every case retains per-request observations, including failures and unstarted
requests, plus count/denominator summaries. Units are explicit in field names.

| Metric | Definition and boundary |
| --- | --- |
| Successful request latency | Monotonic dispatch to complete validated JSON body; includes client framing, loopback transport, Gateway work and any connection setup, excludes the client's prior batch queue |
| First response byte | Monotonic dispatch to first observed HTTP status/header byte, measured separately from completed headers |
| First body byte | Monotonic dispatch to first observed decoded response body byte; for these JSON reads this is neither a generated token nor SSE TTFT |
| P50/P95/P99 | Empirical nearest-rank quantiles over the stated population, with count/min/max/mean. Empty observations are null, not zero. Small samples carry a tail warning; removing that warning never self-certifies precision or an SLO |
| Throughput | Scheduled, finished-attempt, HTTP-response and validated-success counts per measured wall-second, each separately named. Finite closed-loop results are not an open-loop offered-arrival capacity test |
| Errors | All status codes and outcome counts, including 429, 503, timeout, invalid 200, malformed/truncated transport, deadline cancellation and unstarted work. Fast failures cannot improve successful-throughput claims |
| Client queue | Age of a request waiting in this finite load-generator batch before a bounded worker dispatches it. Never relabel this as Gateway queue, SDK execution queue or PostgreSQL acquire time |
| Gateway CPU | Delta of this exact Gateway PID's user+system CPU ticks, divided by its sampled wall window; percent of one core can exceed 100. Includes Gateway background work; does not include the whole host or database |
| Gateway RSS | Sampled start/end and peak resident bytes from the same process. A fast case can miss an instantaneous peak; it is not an allocation profile |
| FD/socket pressure | Sampled open-descriptor and unique socket counts for that PID. Does not identify connection-pool size, idle connections, waiters, saturation or leaks |
| Load generator CPU | Separately recorded same-process CPU over the measured batch, excluding warmup like its wall window; failed warmup is explicitly labelled as having no measured batch. The client shares the host and can affect results |

The report explicitly marks gateway-exclusive overhead, provider latency, SSE
first-token/inter-chunk timing, server queue delay, Knowledge retrieval, database
pool pressure, server saturation capacity and execution cancellation as
unavailable for these two JSON profiles. Provider-free means no model was called;
it does not mean a measured model latency of zero or a measured subtraction from
end-to-end latency. Do not subtract unrelated percentile distributions.

## 3. Required product acceptance expansion

A releasable enterprise performance claim needs owner-reviewed budgets for a
specific workload and reference environment. There are intentionally no invented
millisecond/RPS targets in this contract. Record CPU/memory limits, process count,
DB/pool limits, route/middleware, tenant/ACL scope, payload/token sizes, dataset
size/index/retrieval settings, topology, build flags, source/binary identity,
provider/model/variant, warm/cold cache condition and background load.

Required stages beyond the current baseline:

1. **Gateway overhead separated from model latency.** Exercise an actual admitted
   public model route against an explicitly owned deterministic upstream. Record
   per-request Gateway receive/admit/upstream dispatch/upstream-first-byte/body
   completion and client observations under the same correlation identity. Run
   the same upstream directly as a control only where equivalent semantics are
   supported. Then run the unchanged route with an authorized real provider;
   report real model waiting separately, never mask it by reducing model quality,
   reasoning or tenant/auth/accounting guarantees. A synthetic upstream result
   remains synthetic and cannot substitute for provider acceptance.
2. **Streaming.** Exercise actual managed Runtime and explicitly selected SDK
   paths separately. Capture response headers, first SSE data event, first useful
   reasoning/text token, inter-event and inter-text-delta gaps, terminal event,
   disconnect/reconnect, slow-reader backpressure and long-lived heartbeat
   behavior. Byte/chunk/event/token are different units. The native middleware's
   `next.run` duration ends at a response object, not necessarily its stream end.
3. **Queueing and overload.** Collect authoritative per-attempt admission, claim
   and execution-start timestamps for each domain. Generic jobs, Knowledge,
   managed turns, scheduled tasks and SDK runs have different state machines.
   Report admitted→claim and claim→execution separately where available. Apply
   bounded open-loop load with measured scheduled arrivals, drops and queue age
   so closed-loop backpressure cannot hide coordinated omission. Correlate real
   queue/pool waiters, rejection status, recovery and tail latency; prove tenant
   fairness and that overload does not corrupt billing or execution identities.
4. **Knowledge.** Exercise Gateway-signed resource-authorized retrieval against a
   fixed corpus/index and workload. Split embedding, cache lookup, lexical/vector
   retrieval, fusion/rerank and serialization where traced; report end-to-end
   P50/P95/P99 and recall/relevance together. Separate interactive queries from
   concurrent ingestion. No disabling ACL/reranking without a declared workload
   change to manufacture speed.
5. **Resource and pool pressure.** Gateway/Runtime/Worker/Knowledge and database
   resources are separate observations. Add real pool size/idle/waiter/acquire
   histograms and request-scoped ownership, connection reuse and HTTP pool waits.
   FD counts and client semaphore waits are only supporting evidence. Bound
   sampling overhead and report sample resolution and missing data.
6. **Cancellation and recovery.** Measure cancel-request acceptance, durable
   terminal state, capacity/connection release, replayable terminal event, actual
   provider/tool termination where observable, and final usage/accounting as
   separate facts. Client TCP close or an inert cancelled row proves none of the
   remote-effect facts. Never auto-replay uncertain writes to improve throughput.

Comparisons must use identical profiles and source/environment identity, retain
all errors, and report multiple independent runs and distribution uncertainty.
Cold start and warm steady state are separate. Changes that weaken isolation,
correctness, cancellation, release compatibility or accounting fail regardless
of measured speed. `COMPLETE_EXPLORATORY_READ_BASELINE` means only that the named
read measurements completed; it is not a production-readiness or enterprise-SLO
pass.

## 4. Verification and reproduction

Offline plan, no network or service action:

```sh
python -B scripts/harness/gateway_performance_baseline.py
```

Canonical repository regression command:

```sh
uv run --all-packages --extra test pytest -q --no-cov tests/harness/test_gateway_performance_baseline.py
```

The regressions use standard-library fake byte streams and mocked connections.
When the full workspace environment is unavailable, the identical offline cases
can also run without installing dependencies:

```sh
PYTHONDONTWRITEBYTECODE=1 python -B -m unittest discover -s tests/harness -p test_gateway_performance_baseline.py -v
```

These checks cover math, denominators, fixed targets/owner identity, framing,
timeout/local cancellation, bounded workers, warmup accounting and resource
failure behavior. They are not measurements of a real Gateway and open no
network listener. Live results must be a separate controller receipt with exact
current source/artifact/PID verification, measured observations and cleanup facts.
Previous historical fixture results do not become a fresh baseline after an
environment replacement.
