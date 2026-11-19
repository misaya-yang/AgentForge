# Phase 05 - SDKs speak the public runtime contract

- PHASE_ID: ACU-05
- FEATURE_ID: ACU-F006
- DEPENDS_ON: ACU-04
- UNLOCKS: none

## Outcome

A client program can run an agent through the public runtime contract using a shipped SDK:
fetch the agent config, stream a run to completion, and submit feedback. This is what turns
"embed a widget in a web page" into "reach an agent from an app, a desktop client, or a backend".

No SDK covers this today. A repo-wide search for `agent-public`, `agent_public`, `public_id`, and
`publicId` across `sdk/` returns nothing — all four SDKs (python, cli, java, dart) target the
authenticated assistant API. The Dart SDK in particular has no other reason to exist than mobile
and desktop clients, which is exactly the surface that cannot use an iframe widget.

## Scope

In:
- Extend at least the Python and Dart SDKs to the public runtime contract: config, streaming run,
  feedback.
- Generate or derive the client types from the contract rather than hand-writing a fifth copy.
- Model defaults come from the server, not from a constant in each SDK. The default model is a
  property of the deployment; `qwen3.7-plus` is currently hardcoded in thirteen places across four
  SDKs and has already drifted from the server once.
- One end-to-end example per covered SDK, runnable against a local stack.

Out:
- Java and CLI coverage, unless they come free from generation. Record what was left out.
- Publishing new SDK versions to package registries.
- Any UI work.

## Done when

- [ ] A Python SDK example fetches a public agent config, streams a run to completion, and submits feedback.
- [ ] The Dart SDK does the same, proving the mobile and desktop path.
- [ ] Neither SDK hardcodes a provider-specific default model; the server supplies it.
- [ ] A request from a disallowed origin or over the rate limit fails in the SDK with a clear typed error.
- [ ] The examples run against a stack started with `make quickstart` and are documented.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| SDK contract | `uv run --all-packages --extra test pytest -q --no-cov tests/sdk/test_public_runtime_client.py` | Config fetch, streaming to completion, feedback, and a typed rejection for a disallowed origin |
| No hardcoded model | `rg -n 'qwen3\.[0-9]-plus' sdk/` | The default model comes from the server, not from a constant in each SDK |
| Live stack | `make status` | The examples ran against a real stack started with `make quickstart` |

## Stop or confirm

- Publishing any SDK version to a package registry.
- Changing an existing public SDK signature in a breaking way.
- Removing the hardcoded default model if it would break an already published version — propose a deprecation path instead.
