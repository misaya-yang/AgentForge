# Chaos runner plan

Copy the example to a private mode-0600 file outside the repository, fill current
source/binary SHA-256 values and distinct fixture configurations, and add the
actual scoped task/tool/state assertions described in the owning README. The
example is a skeleton, not a complete acceptance suite. It performs read load
before killing A, then probes surviving B after the owner-lease expiry window.
Add real scoped state/effect assertions and explicit recovery of A when the
owning full acceptance plan requires a restored-capacity measurement.

```
gateway-ha-chaos --execute --plan /operator/private/ha-plan.json --out /operator/evidence/new-ha-run
```

`probe` accepts an optional JSON body, exact expected status, JSON-pointer
assertions (`pointer`, `equals`), and optional `capture`/`capture_pointer`.
A capture stores a digest, never raw user/provider data. `equal_captures` compares
two named digests, useful for a duplicate submission's stable task identity across
replicas. Never compare volatile timestamps or progress accidentally.

`spawn` requires an absolute binary path and matching SHA, and uses argv directly
without a shell. Gateway uses `--config-stdin` with the optional private
`stdin_file` (a typed GatewayDocument JSON, mode 0600, at most 4 MiB). The runner
passes that file descriptor directly, without logging or copying its secrets.
`--config-env` has no filename argument; use it only with an explicitly prepared
inherited environment. Never put secrets in argv. `kill9`
only targets a label created by the current run. There is no command for killing
an existing process/container or mutating an unrelated deployment. `load` performs
bounded GETs round-robin across the declared origins, with 1–128 concurrency and
1–100000 requests, checking complete bounded responses; RPS and p95 are recorded.
Unexpected status, assertion, network or process state stops the plan. Unsafe
requests are never retried after an unknown response.

Child stdout/stderr are private captures and may contain application diagnostics;
keep the entire output root private. The summary only records hashes, status,
counts and timings. Credentials come from one private file and are sent only to
the exact admitted origins; TLS validates normal hostnames and the optional
explicit CA bundle. Redirects, proxy environment and automatic retries are off.
The tool does not carry workload certificates into public/provider requests.

Every receipt retains `single_instance_assumption_retired=false`. Root must join
it to real duplicate-effect counters, lease/fence evidence, the capacity contract
and the approved ADR before changing the release assumption.
