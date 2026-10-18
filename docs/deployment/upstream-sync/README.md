# Monthly upstream sync gate (U-RT-07)

The native `platform-tools upstream-sync` command implements monthly evidence
admission. It never fetches, rebases, edits Codex, builds, starts a service,
publishes a proposal or promotes a source lock. The only program ledger remains
`deploy/runbooks/native-enterprise-upgrade/loop-state.json`; window/run/report
JSON files are immutable evidence referenced by U-RT-07, not another task ledger.

This package is source implementation, not completion of two monthly windows.
The [current read-only observation](current-patch-observation.json) found **39
modified upstream files, 3,544 added and 263 removed text lines** against the
cached immutable `ce0323f2f2d3b25e82806cd555237da3db4e1897`. It includes Cargo.lock,
Cargo.toml and tests; platform-added files do not hide these modifications. This
is a live-overlay inspection, not an admitted source-unit or a sync window.
There is no stable `rust-v` tag at that cached commit, no N-1 window package was
supplied for a three-way conflict measurement, and no time-to-green is claimed.
The target remains **at most 15 modified upstream files within two real windows**.
The code does not rewrite that threshold or manufacture past/future windows.

## Native command registration and offline checks

The existing platform-tools main registers `mod upstream_sync;` and dispatches
`upstream-sync` to its strict subcommand parser. Unknown operations/options,
duplicate valued options, unsupported flags and extra positionals reject before
opening input files. Existing dispatch treats `result=blocked` as a failed
command. No new Cargo dependency or upstream/shared manifest is needed.

Make provides `agent-runtime-upstream-sync-{begin,inspect,compare,record-gate,verify}`.
Supply the documented input options through `ARGS`, excluding `--repo-root` and
`--output`, which the wrapper supplies. Outputs default to the existing external
`agentforge-upgrade/evidence/upstream-sync` cache (or `RUNNER_TEMP` in CI).
Override `UPSTREAM_SYNC_OUTPUT` with a fresh absolute filename for retries; the
native command refuses overwrite and in-repository evidence output. `inspect`
remains a read-only metric command and cannot substitute for `verify`.

`make agent-runtime-upstream-sync-l0` runs only the offline module tests through
`scripts/new/native-container.sh` and its existing single cache. CI adds the same
module test filter in the existing hosted native-tools setup, followed by actual
CLI rejection cases for unknown operation, duplicate option, unsupported flag
and missing window. It stores logs under `RUNNER_TEMP/upstream-sync-l0` and
uploads a distinctly named L0 artifact. CI opens no monthly window, calls no
replay/provider, and makes no monthly acceptance claim. No second compiler
cache, scheduler or program ledger is introduced.

The tests live in `src/upstream_sync_tests.rs` and are included by the module.
Through the single controlled native build, run the platform-tools binary test
filter `upstream_sync`; tests exercise real Git numstat/merge-file probes,
source-stale receipts, immutable output, input tampering, sole-ledger clock pins,
scenario coverage, adjacent months and the unchanged 15-file target. No Cargo
build or unit test was executed while writing this package.

## Open a real window before repair work

Use one stable `rust-vMAJOR.MINOR.PATCH` tag each actual UTC month. The command
resolves and pins both tag object and commit/tree from a clean existing checkout
whose origin is the canonical OpenAI repository. It does not fetch missing tags
or infer a tag from an arbitrary commit. The baseline is the actual previous
accepted source-unit package, source lock, source-built App Server schema and
both Runtime/Worker CycloneDX SBOMs. The previous source receipt must bind the
baseline source-unit hash, tree and resolved lock to that source lock. Existing
source packages/archives are inputs;
no original application Git history is requested or reconstructed.

```sh
platform-tools upstream-sync begin --repo-root /workspace \
  --month YYYY-MM --tag rust-vMAJOR.MINOR.PATCH \
  --package /external/candidate --baseline-package /external/previous-source-unit \
  --upstream /external/pinned-clean-upstream \
  --baseline-lock /external/previous-lock.json \
  --baseline-source-receipt /external/previous-source-receipt.json \
  --baseline-schema /external/previous-schema \
  --baseline-sbom /external/previous-sbom.cdx.json \
  --baseline-worker-sbom /external/previous-worker-sbom.cdx.json \
  --output /external/month/window.json
```

Before any gate runs, root records exactly one `sync_windows` entry in the
existing U-RT-07 work package, with `month`, `window_sha256`, `started_at` and
`target_commit` from that output. Later add the actual final `report_sha256` to
this same entry. The gate reads this canonical ledger: another clock, duplicate
month or changed target fails. Reuse the original window after a failed build;
repair may change the candidate source-unit, but never refreshes its clock or
upstream commit. Native outputs use create-new plus fsync and cannot overwrite
an earlier failed report. Use separate evidence filenames for retries.

## Observe the actual patch and review schema/SBOM changes

```sh
platform-tools upstream-sync inspect --repo-root /workspace \
  --package /external/candidate --baseline-package /external/previous-source-unit \
  --upstream /external/pinned-clean-upstream
platform-tools upstream-sync compare --repo-root /workspace \
  --window /external/month/window.json --source-lock /workspace/deploy/agent-runtime-source/lock.json \
  --schema-dir /external/candidate-schema \
  --sbom /external/candidate-sbom.cdx.json \
  --worker-sbom /external/candidate-worker-sbom.cdx.json \
  --output /external/month/diff.json
```

`inspect` verifies existing source-unit payload digests and compares actual
immutable upstream blobs to payload files. It reports every modified upstream
path, mode change and Git numstat added/removed lines. Binary changes have null
line counts and a separate count. New platform files are counted separately.
For each old platform patch overlapping a new upstream change it actually runs
`git merge-file --diff3 -p` over private temporary copies and records the conflict
hunks/exit code; binary/unmergeable/deleted-upstream cases are explicit. It does
not report an overlap as a conflict, nor a manually repaired candidate as a
zero-conflict original rebase. No source-unit/v1 deletion operation is invented.

`compare` hashes every source-generated App Server schema file, reports
added/removed/changed paths, and diffs complete SBOM components by `bom-ref`, dependency edges and document
metadata for both packages. Snapshot keys identify `component:`, `dependency:`
and `document`; duplicate or dangling dependency refs reject. It validates candidate schema/SBOM hashes
against the existing source lock. It does not call a source-only lock runnable.

Save a reviewed JSON file with schema
`agentforge/app-server-schema-review/v1`, `result=pass`,
`baseline_schema_sha256`, `candidate_schema_sha256` and `diff_sha256` equal to
`schema_diff_sha256` in the actual diff report. Review removals, changed wire
fields and version/compatibility implications using the existing App Server
schema tooling. The review is an explicit source-bound decision, never generated
as PASS by this command. The final report retains its digest.

## Run and bind the three execution gates

`record-gate` invokes only an explicitly supplied digest-pinned prebuilt native
runner, without a shell, inherited credentials, downloads or builds. It binds
source-unit and lock hashes before/after execution, all declared input hashes,
executable hash, command/spec hashes, actual exit status, monotonic elapsed time
and wall timestamps. It enforces a 1–3600 second bound, kills the process group on
timeout, discards process logs and requires a fresh bounded JSON receipt. The
receipt itself stays pinned and is rechecked during final admission. Runner
arguments are hashed, not printed. Outputs must remain outside
source/package paths; private history is neither printed nor copied into the
monthly report.

```sh
platform-tools upstream-sync record-gate --repo-root /workspace \
  --window /external/month/window.json --package /external/candidate \
  --source-lock /workspace/deploy/agent-runtime-source/lock.json \
  --kind history --spec /external/history-command.json \
  --output /external/month/history-run.json
```

Repeat for `--kind replay` and `--kind eval` with their actual runner specs.
[command-spec.example.json](command-spec.example.json) is an intentionally
non-runnable template. Runner names are `ai-platform-history-gate`,
`agent-runtime-replay`, and `agent-runtime-eval-smoke`. A missing prebuilt replay
or aggregate eval adapter blocks; renaming an arbitrary successful executable
cannot satisfy the semantic evidence requirements below. Existing evaluator
JSON alone does not prove an aggregate smoke set or recorded provider replay.
This package supplies no invented replay/eval receipt and makes no claim that
these currently missing monthly executions ran.

The history runner is the existing production-codec `ai-platform-history-gate`,
using its existing corpus contract and eight native scenarios. Its forward
result must be PASS with no skips, the actual candidate writer/schema, source
lock and manifest hash; its previous writer must be this window's real baseline.
Declare the actual corpus manifest among the pinned command inputs. The existing
history gate validates raw captures, capture/source receipts and semantic native
roundtrip; a projected event fixture cannot substitute for raw N-1 history.

The replay adapter must emit schema `ai-platform/deterministic-turn-replay/v1`,
`result=PASS`, `candidate={upstream_revision,schema_sha256}`,
`transport=deterministic_mock_provider`, `external_provider_calls=0`, explicit
unsigned `seed`, `skipped=0`, and `observations`. Required scenarios are
`ordinary_turn`, `tool_roundtrip`, `approval_pause`, `cancelled_turn`, and
`compacted_resume`. Each observation has positive `mock_requests`,
`repetitions>=2`, equal 64-hex `expected_sha256`/`observed_sha256`, and pinned
`oracle_sha256` plus `capture_receipt_sha256`. The capture must reuse the actual
`ai-platform/history-capture/v1`, `method=postgresql_rollout_items`, baseline
writer, positive item count and pinned raw history bytes. The oracle is JSON and
is pinned separately from observed output. Mock replay remains execution of the
real admitted kernel against a deterministic mock, not comparison of two copies
of a hand-authored expected file; the runner/executable provenance is retained.

The eval adapter reuses existing Runtime observation evaluators (including
`agent-runtime-eval-contract`), emitting aggregate schema
`ai-platform/runtime-eval-smoke/v1`, `result=PASS`, exact candidate writer,
`skipped=0` and the same five scenarios. Each observation has positive `cases`,
`passed=true`, an empty `failures` list and `observation_sha256` referring to
actual pinned evaluator input JSON. Run the existing evaluator for each case;
an isolated `{passed:true}` or a successful build is insufficient. Mock
repeatability, native history decoding and smoke evaluation remain distinct.

## Final admission and source-bound time-to-green

```sh
platform-tools upstream-sync verify --repo-root /workspace \
  --window /external/month/window.json --package /external/candidate \
  --baseline-package /external/previous-source-unit \
  --upstream /external/pinned-clean-upstream \
  --source-lock /workspace/deploy/agent-runtime-source/lock.json \
  --schema-dir /external/candidate-schema --schema-review /external/month/schema-review.json \
  --sbom /external/candidate-sbom.cdx.json \
  --worker-sbom /external/candidate-worker-sbom.cdx.json \
  --history /external/month/history-run.json \
  --replay /external/month/replay-run.json --eval /external/month/eval-run.json \
  --output /external/month/report.json
```

This reruns existing `source-candidate verify` and `source-lock validate` directly,
including compiled test inventories and source receipts. Every gate must bind
the final source-unit/lock and original registered window. A source repair
invalidates earlier gate runs. Missing/failed/stale inputs block and leave
`time_to_green_ms=null`; elapsed time since opening is still reported. Source
green records the actual observation time minus the original window start,
including repair/build time. No build-only duration impersonates time-to-green.

For the second window also supply `--previous-report` from the previous adjacent
month. That report must be source-green, pinned by its own existing-ledger entry,
and its upstream commit/tree must equal the new baseline. Intervening platform
source changes are reported explicitly; the new baseline still requires its own
source-lock/source-unit receipt binding. Two
reports from one month, skipped months, another candidate, a missing gate or
more than 15 modified files cannot meet the two-window target. Monthly source
green and the reduction target are separate fields; `product_acceptance=false`
and the existing rollout/rollback/live release gates remain authoritative.

[Extension-point proposals](extension-proposals.md) describe reviewable upstream
API changes and their migrations. They are local proposals, not published PRs
or applied upstream changes.
