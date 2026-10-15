# Phase 01 - Normalize standalone CLI configuration and third-party API compatibility

- PHASE_ID: CLI-01
- FEATURE_ID: CLI-F002
- DEPENDS_ON: CLI-00

## Outcome

The product launcher owns a private home and strict non-secret provider profile, generates upstream-compatible Runtime config, and starts the composed Rust CLI without Gateway configuration.

## Scope

In:

- `sdk/cli/src/provider/config.ts`, `sdk/cli/src/native/launcher.ts`, their tests, package entrypoint, and CLI README.

Out:

- Agent loop behavior, hosted Gateway/Runtime, raw secret storage, and native compilation.

## Done when

- [x] Config rejects unsafe endpoints, secret static fields, unknown fields, and missing credential env references.
- [x] Direct Responses config preserves base URL/header/query/retry/idle semantics without storing secret values.
- [x] Launcher isolates `CODEX_HOME`, forwards native args, and keeps Chat provider secrets out of the Rust child.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Focused product | `npm --prefix sdk/cli test -- src/provider/config.test.ts src/native/launcher.test.ts` | Config/home/secret/launcher behavior passes. |
| Type contract | `npm --prefix sdk/cli run typecheck` | Independent config and launcher types compile. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Stop if provider secret values must enter JSON/TOML/argv or the launcher must implement thread/tool state.
