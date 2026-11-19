# Phase 02 - Package the composed native Agent Runtime CLI

- PHASE_ID: CLI-02
- FEATURE_ID: CLI-F003
- DEPENDS_ON: CLI-01

## Outcome

The fixed upstream source plus overlay builds a native `codex` CLI artifact whose TUI/exec starts the in-process App Server and owns Thread/Turn/Item, approval, interruption, and resume locally.

## Scope

In:

- `deploy/agent-runtime-source/Dockerfile.cli`, `scripts/harness/build_agent_runtime_cli_package.sh`, native artifact receipt, and packaging/CI wiring.

Out:

- Host Cargo, hosted Runtime HTTP, source-lock mutation, publication, or pretending a dry-run is a compiled artifact.

## Done when

- [ ] Docker-contained build produces the native binary at the recorded upstream+overlay identity.
- [ ] Native `--version`, mock provider exec, JSONL, interrupt, approval, and resume tests pass in hosted CI.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Source preflight | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/clean/codex-harness scripts/harness/build_agent_runtime_cli_package.sh --dry-run` | Pinned source and overlay identity are consistent. |
| Native build | `AI_PLATFORM_AGENT_RUNTIME_SOURCE=/clean/codex-harness make agent-runtime-cli-build-local` | Docker builds and smokes the composed native CLI. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Stop if native behavior requires the hosted `/internal/v1` service or a non-pinned source binary.
