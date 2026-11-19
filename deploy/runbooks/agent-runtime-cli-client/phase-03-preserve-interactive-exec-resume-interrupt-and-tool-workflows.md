# Phase 03 - Add bounded Chat Completions compatibility

- PHASE_ID: CLI-03
- FEATURE_ID: CLI-F004
- DEPENDS_ON: CLI-01

## Outcome

A loopback adapter maps losslessly representable Responses text/function-tool input to a Chat Completions provider and projects ordered Responses SSE without creating an Agent loop.

## Scope

In:

- `sdk/cli/src/provider/chat_responses_proxy.ts`, its deterministic provider fixtures, launcher integration, and compatibility documentation.

Out:

- Images, hosted tools, remote Runtime, silent lossy conversion, or provider-specific behavior without an explicit profile contract.

## Done when

- [x] Text, reasoning, function-call deltas, usage, and terminal status project to ordered Responses SSE.
- [x] Tool transcript round trips to Chat messages and unsupported image input fails closed.
- [x] Provider secret stays out of Rust child/config/output and local proxy requires an ephemeral credential.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Adapter fixture | `npm --prefix sdk/cli test -- src/provider/chat_responses_proxy.test.ts` | Chat request/SSE/tool/error/retry projection is deterministic. |
| Package build | `npm --prefix sdk/cli run build` | Published launcher contains config, adapter, and native dispatch. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Stop on any input that cannot be represented without loss, or if the adapter would need to own tools/approvals/session state.
