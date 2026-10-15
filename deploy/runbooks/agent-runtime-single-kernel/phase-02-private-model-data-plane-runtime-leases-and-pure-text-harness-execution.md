# Phase 02 - Private model data plane, runtime leases, and pure-text Harness execution

- PHASE_ID: CHR-02
- FEATURE_ID: CHR-F003
- DEPENDS_ON: CHR-01

## Outcome

A pure-text user Turn executes only in Agent Runtime source through a private, lease-bound model data plane.

## Scope

In:

- Immutable runtime snapshots, signed leases, private model Responses stream, capability adapters, text/reasoning projection, and control/candidate eval.

Out:

- Tool execution, production canary, or public V2 endpoints.

## Done when

- [x] No internal request can enter either Agent loop; credentials never enter snapshots or model context.
- [x] Real simple/long/multi-turn scenarios have one billing path, zero provider 400s, and measured TTFT.
- [x] Existing affected behavior still passes its smallest relevant regression check.

## Current checkpoint

- Qwen catalog, open-source defaults, Provider templates, and the default-tenant
  additive migration now select native `responses_v1`. An explicitly configured
  tenant `chat_completions` value remains a compatibility fallback.
- The Gateway issues immutable snapshots and signed, scope-bound model leases;
  the Rust Runtime reserves the platform Run ID as the Agent Turn ID and calls a
  private model-only Responses endpoint that cannot enter either Agent loop.
- A real isolated Docker chain carried the normal Agent tool catalog (five
  top-level entries and 8,844 input tokens) through Qwen Responses. The first
  provider-visible reasoning token arrived in 3.931 seconds, followed by text
  and one successful terminal event; accounting completed and Runtime cgroup
  memory was 38,825,984 bytes.
- The reproducible text gate covers long and multi-turn resume cases as well as
  the simple live-provider case.
- The canonical `make agent-runtime-text-gate` now owns those four scenarios.
  Running it against the prior locked image passed simple exact output, long
  Transformer explanation, and multi-turn setup, then correctly exposed that
  a process restart resumed with Agent's default OpenAI provider instead of the
  platform model plane. Gateway and Runtime now reapply a bounded, non-secret
  model-plane configuration before issuing the next lease; the Runtime rejects
  partial, credential-bearing, or non-platform resume endpoints.
- The current resume rebind and final bounded-URL/`exclude_turns` hardening pass
  the full Rust Runtime unit cohort. A development Runtime image also passes
  Docker lifecycle and the real four-scenario Qwen gate: first-visible token
  times were 3.865s, 1.978s, 1.665s, and 1.959s; the long response contained
  1,755 non-whitespace characters; four Turns produced exactly four completed
  model calls; and the restarted Runtime used 47,476,736 bytes.
- CHR-02 is closed at controlled fork `7640138305a0`. The matching source
  receipt, deterministic SBOM, App Server OCI, and Agent Runtime OCI identities
  pass the fail-closed contract. The locked Runtime passed isolated restart
  smoke and the real four-scenario Qwen gate with first-visible times of 3.715s,
  1.773s, 1.830s, and 2.086s; four Turns produced exactly four model calls.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Pure-text candidate | `make agent-runtime-text-gate` | Candidate response, resume, reasoning metadata, billing, and latency receipts satisfy the phase contract. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Real provider traffic is limited to the pre-authorized local E2E account and configured provider readiness.
