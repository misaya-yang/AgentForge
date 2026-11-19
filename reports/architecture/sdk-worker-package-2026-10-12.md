# SDK Worker package verification checkpoints

Passed on 2026-10-12T07:13:44Z. All six actual-library synthetic-transport scenarios pass against a freshly installed current Worker wheel. No production edits are needed.

## Artifact and source freshness

- Wheel: `dist/ai_platform_sdk_agent_worker-0.1.0-py3-none-any.whl` (21,487 bytes)
- Wheel SHA256: `db418e2a1378675094df731109dafd9caa993c7f4f4f1171d9eb5fecafa04207`
- A second isolated offline build produces the identical wheel SHA256.
- All 11 Worker Python files match live source, staged build source, wheel contents and installed package byte-for-byte. Wheel RECORD hashes are valid.
- Current `engines.py`: `5fb9a73026af9a91ca7b68763537794749b862f2e9bd5e6aaee53fec08dc1c7a`
- Exactly six prior installed files were stale: capabilities.py, contract.py, engines.py, knowledge.py, server.py, usage.py. The old environment is preserved.
- Python runs with `-I -B`; every Worker import resolves under this stage's `venv/lib/python3.12/site-packages/sdk_agent_worker`. No repository, build-source or old Worker path shadows it.

## Packaging and dependency checks

The unchanged Worker lock passes `uv 0.13.0 lock --check --offline --no-cache`. Only the local Worker wheel was built using the previously installed locked Hatchling environment. Nothing was downloaded or dependency-built.

The fresh lightweight uv venv reuses immutable installed dependency payloads through symlinks; 39 small distribution metadata trees are copied locally for uv's directory scanner. The Worker wheel is installed normally with `--no-deps --no-index --offline --no-cache`. This is an external verification environment, not a relocatable deployment artifact.

All 40 installed distributions exactly equal the active recursive runtime dependency closure and match locked versions. `uv pip check` passes. All 11 Worker modules and all relevant SDK modules import, and runtime `--help` passes. There are no build/test-tool leaks, extra packages, unresolved imports, Gateway imports or sibling-app imports.

Actual library versions: OpenAI Agents 0.23.1; OpenAI 3.29.0; Claude Agent SDK 0.2.165; httpx2 2.13.1; MCP 2.3.0. The previous harness's legacy `httpx` dependency was test-only; the pinned production SDKs use httpx2. The final harness uses httpx2's native MockTransport without adding an unnecessary production dependency.

## Actual SDK scenarios passed

1. OpenAI Runner/Responses model/client and schema-valid SSE execute a calculate tool and serialize the real call ID and output into the following model request. A second Worker invocation reopens the real SQLiteSession and verifies six durable conversation items. Current per-response usage audit evidence and totals are asserted.
2. OpenAI governed tool dispatch reaches the production GovernedTools callback with the actual SDK call ID. Only the outgoing Gateway request returns a synthetic receipt.
3. OpenAI streaming cancellation closes the actual client and response stream and emits no completion.
4. Claude's actual client, Query, parser, SDK MCP bridge and MCP server perform initialization, tool listing, calculation, PreToolUse identity binding, governed callback and denied permission callback over a custom in-memory SDK Transport. Two invocations persist and reuse the resume option; current cache/cost usage observations are asserted.
5. Claude cancellation cancels an actual asynchronous MCP handler, closes the custom transport and commits neither completion nor session ID.
6. An actual SDK-parsed Claude error result is rejected without committing a session or completion.

No unfinished asynchronous tasks remained. The existing CanUseToolShadowedWarning remains visible: exact allowed_tools entries bypass can_use_tool. The governed handler and identity hook remain checked, and an unlisted tool's permission callback denial is separately exercised. This does not establish CLI enforcement in a real launch.

## Isolation and limits

All contract scenarios run under `env -i`, empty private HOME, disabled tracing/telemetry and no real credentials. The sole API-key value is an explicit nonsecret fixture string. The Python audit hook rejects network socket creation, DNS, bind/connect and process launch. There were zero network attempts or launched provider/CLI processes. Four `uname -p` metadata probes were refused before launch and safely ignored. Only asyncio's internal Unix socketpair is allowed; no listener is created.

This is package and actual-library integration evidence with synthetic transports. Real providers, credentials/models, Claude CLI launch and genuine transcript resumption, images/containers, arm64 execution, live Gateway/Runtime/database authority, and performance remain unverified. No Docker, production state, old evidence environment or live source was changed. The stage consumes roughly 3.5 MiB of allocated storage and does not copy the large cached SDK/CLI payloads.

## Evidence and reproduction

`receipt.json` is the consolidated result. Detailed receipts and provenance are in `verification/`; exact run logs are in `logs/`. `verification/fixture-adaptation.diff` documents the narrow harness changes. Initial diagnostic failures are preserved and explained in the consolidated receipt; they are not production defects.

Re-run the installed-package contract from this stage:

```sh
env -i PATH=/usr/bin:/bin HOME="$PWD/home" PYTHONDONTWRITEBYTECODE=1 \
  "$PWD/venv/bin/python" -I -B "$PWD/verify_actual_loops.py"
```

This regenerates the new stage's contract receipt and retains fresh synthetic-state directories. The existing historical evidence is unchanged. `prepare_and_build.py` documents fresh-stage build steps; do not rerun it over this already populated stage. `audit_package.py` runs using the existing external build-venv Python, whose packaging parser is used only by the verifier and is absent from the runtime environment.


## Restored-environment verification at 11:44 UTC

The earlier environment was replaced at 09:35 UTC; its environment paths and cached payloads above are historical, not currently retained installations. A fresh official locked environment under `platform-verification/sdk-validation-restored` now reproduces the same 21,487-byte wheel and SHA256, twice. The source snapshot, all 11 modules, installed wheel RECORD and 40-distribution recursive dependency closure match. Dependency extras are followed transitively, including MCP → PyJWT[crypto] → cryptography/cffi/pycparser. The initial verifier omission is retained as a failed attempt; no production dependency changed.

All six rebuilt actual-library scenarios passed against unchanged production engine code and official SDK loops. The strict original-client run correctly failed aggregate acceptance on a denied `uname -p` platform-metadata probe, despite six passing scenario bodies. That failed receipt is preserved. The final strict run fixtures only OpenAI's platform header metadata (`get_platform` returns Linux), keeps API serialization and SDK loops unchanged, and reports zero network attempts, zero blocked process attempts and zero unfinished asynchronous tasks. Claude cancellation additionally uses one disclosed synthetic blocking-tool handler. The error-result case asserts transport closure and no fixture failures. Root separately reran 13 production usage unit tests successfully.

These direct-engine tests exclude Worker HTTP/admission and child-process lifecycle, actual Claude CLI flags and allow-list enforcement, genuine transcript resume, real provider inference, paired Rust services and containers. Current default client platform-detection behavior is excluded from the strict-zero run; the preserved failed run records it. No host service or listener was started. Reproducible scripts, bounded receipts and failed-attempt evidence are retained externally; virtual environments, synthetic conversation state and caches are not recovery-source deliverables.

Current receipts: `platform-verification/sdk-validation-restored/verification/package-audit.json`, `verification/actual-loops-receipt.json`, and `logs/root-usage-tests.log`.


## Integrated lifecycle fix and current wheel at 11:57 UTC

A subsequent reviewed three-file change fixes a real Worker deadline gap: child creation and stdin backpressure now share the existing execution timeout instead of preceding it. Request-body staging rejects an oversized ASGI piece before extending its bytearray. Thirteen production ASGI/filesystem tests pass with explicit subprocess/signal/Gateway doubles; the exact preimage fails the three intended regression methods. Current in-process ASGI disconnect, replay/tenant fencing and cleanup-before-unlock checks pass. These do not prove actual OS process creation or descendant cleanup; existing reaping/watcher waits remain outside a hard cleanup bound.

The post-fix wheel is now `f29f26e8af3a930d53480da00b60a2b82c897c0d9e43daf5245945257bd88d7c`, 21,547 bytes, reproduced twice. Only `server.py` changed among the 11 package modules. All module/RECORD and40-distribution closure checks pass, and all six actual-library synthetic-transport scenarios pass again with zero network/process attempts and no unfinished async tasks. The previously verified `db418e2a…` wheel is retained as pre-fix evidence and is no longer the current source wheel. The same metadata/blocking-tool fixtures and provider/CLI/live-service exclusions apply.

Root's 13 lifecycle tests and canonical harness/architecture gates passed. Exact evidence lives in `platform-verification/sdk-admission-next/receipt.json`, `logs/root-integrated.log` and `platform-verification/sdk-validation-lifecycle-candidate/verification/`.


## Current cancellation-fenced package at 12:24 UTC

The reviewed cancellation fence now catches cancellation while a synchronous capability request remains in flight, marks the governed run uncertain before releasing its request lock, and prevents queued/future calls from dispatching. Cancellation while only waiting for that lock retains its previous behavior. This does not stop the remote request or establish its outcome.

Current wheel: `9f3df4ebe6a42150f8f87c1c18b9f4e1e636d3ed023c3412c5328769a1cd7060`, 21,655 bytes, twice reproducible. All11 modules/RECORD and40-distribution closure match current source. Six actual-library scenarios pass again with the already described synthetic transports/metadata/blocking-tool fixtures. Separate production bridge8, ASGI lifecycle13 and usage13 tests also pass; these counts are not provider or distributed acceptance. Historical wheels and failed verification attempts remain immutable. Current evidence: `platform-verification/sdk-validation-cancellation-candidate/`.
