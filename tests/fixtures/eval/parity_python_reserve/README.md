# Python work-product execution fixtures

These eight regression cases retain every source/behavior assertion formerly exercised by the
RustPython-backed reserve interpreter tests, plus the original reservation bug and a missing-negative
exception. `harness.py` is trusted **sandbox input**, not a platform Python service, host command or
replacement handwritten Python parser. The standard CPython `ast` parser and interpreter run only
inside an explicitly selected preloaded sandbox; native Rust validates the captured outcomes.

The old interpreter understood a partial Python grammar and i64 arithmetic. It is removed rather
than relabelled. Candidate artifact format (`patched_source`), task prompt, required reservation
cases, negative ValueError and source restrictions are retained. Correct Python semantics now come
from CPython; code that the old incomplete interpreter falsely rejected may become correctly
executable. Execution is bounded by opcode count and container resource/time limits, not claimed
bit-identical to the old interpreter's AST step counter. The evaluator receipt identifies this
backend as `cpython-sandbox/v1`, with exact image/config/harness/source/oracle hashes. Historical
RustPython evaluator receipts are not current validation.

The harness uses a real AST safety walk: one ordinary `reserve` function, no import/global/nonlocal/
with/class/attribute/lambda, and only the original allowed ValueError/TypeError/isinstance calls.
Decorators are rejected. That predicate is a contract check, not the security boundary: the container
has no network, host mounts, secrets or inherited host environment; it is read-only, non-root,
capability-free and resource-limited. A fresh namespace/function is compiled per test case.

An operator supplies an external JSON contract with these required fields:

```json
{
  "schema_version": "python-work-product-sandbox/v1",
  "sandbox_image_reference": "REGISTRY/EXISTING_PYTHON_IMAGE@sha256:REAL_MANIFEST_DIGEST",
  "sandbox_image_config_digest": "sha256:REAL_CONFIG_DIGEST",
  "sandbox_platform": "linux/amd64"
}
```

Use the already authorized optional user-code sandbox image containing `python3`; `linux/arm64`
is also supported. The optional `python_entrypoint` can select the known absolute
`/usr/bin/python3`; its default is `/usr/local/bin/python3`. The runner always passes this via
Docker `--entrypoint` before the image, so an existing Worker ENTRYPOINT cannot intercept the
harness. Relative paths and other executables are rejected. Placeholder values are not a working configuration. The runner never builds,
pulls or selects an image automatically. Image identity uses the existing native coding sandbox
inspection. It runs a small readiness probe before any paid/provider turn.

After the primary's serialized build, execute all assertions without model/provider calls:

```sh
native-agent-parity-benchmark --reserve-sandbox-contract /absolute/external/contract.json --verify-python-work-product-fixtures
```

This command only returns `passed: true` after all eight cases actually execute and match their
positive/negative oracles. It does not claim product quality acceptance. Nothing is silently
ignored when the image, interpreter, contract or execution result is unavailable.

For the unchanged three-system benchmark add the same explicit contract to the existing command:

```sh
native-agent-parity-benchmark --manifest src/services/eval/fixtures/native_agent_parity_v1/manifest.json --reserve-sandbox-contract /absolute/external/contract.json --output-dir /absolute/external/results
```

Without it a selected `python_reserve` task fails preflight before provider calls. A selected suite
that does not use this validator has no new sandbox requirement. `--validate-only` retains its
no-provider/no-Docker manifest check. Direct synchronous `validate_turn("python_reserve", ...)`
callers must migrate to the explicit async executor or `run_suite`; it refuses to infer code behavior
from source strings. Pure native validators, typed scenario assertions, external adapter calls,
approval behavior and unknown-effect scoring remain unchanged.

Timeout, process/network-control failure, malformed output or unknown cleanup is infrastructure
failure, never a candidate success, skip, retry or imputed score. Original candidate output and
`parsed_output` receipt shapes remain; the execution evidence is an additive sibling field.

Every execution has the explicit project label `io.agentforge.project=agentforge-dev-7801d4a9a4`,
a fresh `io.agentforge.execution-owner` token and execution-kind label. Docker writes an immutable
container ID to a private per-invocation cidfile (not a container mount). Timeout cleanup inspects
that ID and requires exact ID, name, image config digest and all three labels before `rm -f ID`.
It never removes by name. Missing/mismatched identity or failed inspection/removal yields unknown
cleanup and no deletion of an unverified container. The existing coding sandbox Docker factory
and pinned-image inspection are reused; its older name-only cleanup is deliberately not reused.

Repository call-site audit: the only real caller of `validators::validate_turn` is `run_suite`,
which routes `python_reserve` directly to the prepared explicit async executor before reaching
the synchronous native-validator branch. The standalone fixture command also uses that executor.
There is no remaining in-repository synchronous Python-reserve evaluation call. The synchronous
API refuses external legacy callers and documents the required migration rather than reporting
an unsupported/skipped task as passing.
