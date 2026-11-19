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

## Direct implementation checkpoint — 2026-10-17

Implemented and integrated pinned sccache 0.18.0, nextest 0.9.148 and a shared,
dependency-free Rust capacity policy. The action defaults to one build job; only
the explicit standard GitHub-hosted Linux profile can select up to four, based on
observed CPU/memory. Local and self-hosted builds remain one job. The build command
and gate children explicitly apply the policy. Receipts retain ambient limits too.

Nextest ignores configured default filters and uses the original package/workspace
selection. Enabled library doctests run through Cargo. Only explicit nextest
no-tests exit 4 falls back to the complete original Cargo test command; test/build/
tool failures remain failures. Source assembly, metadata and tests share a 55-minute
budget, with progress evidence and external scratch. Final enforcement and release
readiness now install their native tools.

Actual direct results: 95 related Cargo tests passed (5 library, 68 binary units,
2 capacity CLI, 20 CI/harness integration), zero failed/ignored. A final CLI/CI
rerun passed 22. Pinned nextest ran the selected 8 policy/gate tests successfully
(60 unrelated tests unselected). Four actual tool fixtures proved default filters
cannot hide the selected unit, a failing doctest returns 101, an empty binary
returns nextest 4, and original Cargo succeeds for that empty binary. Cargo doctest
for platform-tools passed with zero doctests. Actionlint 1.7.12, focused format,
harness, enforcement self-test and diff whitespace checks passed.

The broad unit pass exposed two macOS test-fixture path aliases (`/var` versus
`/private/var`); only fixture roots were canonicalized. Production containment and
negative assertions remain intact. Two existing formatting failures received
format-only repair. Independent read-only review found no blocker/high issue;
the final job's 10-minute cold-tool setup still needs hosted measurement.

Receipt: [U-PE-03.json](../../../reports/platform-upgrade-20261017/U-PE-03.json).
State is `direct_verified`, with hosted acceptance open. No workflow dispatch,
public push, paid capacity or PR latency result occurred. Remaining observed CI
failures (composed workspace input, upstream V8 fetch, frontend route smoke) remain
explicit. Continue the single-project Ph0 local acceptance while preparing those
repairs; a valid PR cohort must still prove p50 <=20 minutes.
