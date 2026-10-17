# U-PE-03 — Hosted CI capacity

Scope/state: [loop-state.json](loop-state.json). Parent acceptance:
[platform engineering](../../../docs/plans/platform-upgrade-2026-10/08-platform-engineering.md).

## Activation checkpoint — 2026-10-17

The repository is public. Read-only GitHub inspection found two historical push CI
runs, both failed; neither is a valid PR latency cohort. Current main's run
[38867440866](https://github.com/misaya-yang/AgentForge/actions/runs/38867440866)
took 27m59s. Its failures include missing native watermark input, incomplete upstream
workspace setup, missing operational tool installation and frontend route smoke.
Treat these as correctness/assembly failures before interpreting CI capacity.

The watermark input is restored byte-for-byte under the Ph0 Responses prerequisite;
native Gateway compilation and five cache-helper regressions passed. The other CI
failures and any hosted candidate rerun remain open. Local target output is external,
one-job, without debug/incremental data. No workflow has been dispatched or published.

Next: inspect existing CI builder/test policy, verify official pinned sccache/nextest
inputs, prepare standard-runner resource limits and preserved test populations, then
review and run local gates. Hosted execution and PR p50 remain separate acceptance.
