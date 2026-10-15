# Phase 00 - Freeze independent product and source contracts

- PHASE_ID: CLI-00
- FEATURE_ID: CLI-F001
- DEPENDS_ON: none

## Outcome

The migration has one reviewed contract: CLI owns local provider profiles, the composed Rust CLI/App Server is its only Agent loop, and the hosted Gateway/Runtime remains a separate product path.

## Scope

In:

- `sdk/cli/**`, composed Rust CLI/exec/App Server/provider source, `docs/architecture/ADR-009-independent-cli-local-runtime.md`.
- Upstream `codex-harness@94cbbddafc1776d5e377bca1b05932c697e82238` CLI, exec, provider, and app-server-client behavior as read-only evidence.

Out:

- Hosted Runtime/Worker/Gateway/database implementation changes, source-lock changes, Docker mutation, or provider calls.

## Done when

- [x] The README and ADR record the independent local Runtime/provider boundary.
- [x] Existing CLI baseline typecheck and unit tests pass before implementation.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Upstream identity | `git -C /Users/yang/projects/opensource-harness/codex-harness cat-file -t 94cbbddafc1776d5e377bca1b05932c697e82238` | The fixed upstream commit is locally readable without checkout mutation. |
| CLI baseline | `npm --prefix sdk/cli run typecheck && npm --prefix sdk/cli test` | Existing CLI compiles and its 10 baseline tests pass. |
| Contract structure | `python3 /Users/yang/.codex/skills/prd-phase-harness/scripts/validate_harness_prd.py deploy/runbooks/agent-runtime-cli-client --strict` | The executable independent-product contract has no placeholders or state drift. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Stop if a proposal creates a TypeScript Agent loop, sends provider secrets to the hosted Runtime, or bypasses fixed composed-source identity.
