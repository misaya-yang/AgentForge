# Real parallel coding patch-artifact fixture

This fixture evaluates a path the shipped Assistant can actually execute. The model receives a bounded inline repository snapshot, dispatches the two installed read-only plugin specialists in one parallel batch, and returns two complete replacement-file artifacts. It does not pretend the Assistant has a filesystem or code-execution tool.

Run the three-trial live path from the repository root:

```bash
uv run python scripts/eval_fixtures/verify_coding_parallel_fixture.py \
  --observations-out /tmp/general-agent-coding-observations.json \
  --receipt-out /tmp/general-agent-coding-receipt.json \
  --require-hmac
```

The live collector uses environment-only credentials and three separate trust
roots. `GENERAL_AGENT_COLLECTOR_HMAC_KEY` seals fresh raw SSE/observation
evidence, `GENERAL_AGENT_GOLDEN_HMAC_KEY` seals deterministic golden
validation, and `GENERAL_AGENT_CODING_HOST_TEST_HMAC_KEY` seals the live Docker
receipt. All three keys must contain at least 32 bytes and be pairwise
different. The operator supplies one fresh 64-hex `GENERAL_AGENT_SUITE_NONCE`
across all stages. `GENERAL_AGENT_RUNTIME_ATTESTATION` binds the observed
Gateway/container runtime only when its SHA-256 matches the separately supplied
`GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256`. No provider key is accepted
on the command line or written to an artifact.

The validator contract pins the sandbox by registry digest, not a mutable tag.
After the verifier produces the receipt, the coding merge must include it:

```bash
uv run python scripts/real_agent_scenario_runner.py merge \
  --scenarios tests/fixtures/eval/general_agent/coding_parallel_v1/scenario.json \
  --observations /tmp/general-agent-coding-observations.json \
  --validation /tmp/general-agent-coding-golden.json \
  --coding-host-test-receipt /tmp/general-agent-coding-receipt.json \
  --output /tmp/general-agent-coding-merged.json
```

Merge reopens the mode-0600 receipt, verifies its independent HMAC, exact
observation/source/plugin/runtime/raw-SSE bindings, 3/3 accepted trials, patch
scope, digest-pinned image, and 3/3 commands per trial. It embeds the full
structured patch/image/test evidence plus the actual file digest in merged
provenance; both collector and golden merged HMACs bind that object.

Acceptance requires all three trials to satisfy every layer:

1. source and installed plugin-definition digests match;
2. collector HMAC, the externally issued suite nonce, unique collector challenges, raw SSE reconstruction, and runtime binding validate;
3. exactly the two required specialists receive detailed task-contract prompts and complete with at least 25 ms of real monotonic overlap;
4. both child terminals precede the aggregate `spawn_subagent` result, which precedes the parent's single final patch artifact and terminal event;
5. the artifact changes exactly the two allowed implementation files within the 48-line budget;
6. all three tests pass in every trial (nine command receipts total) in a
   non-root, read-only, no-network Docker container with no inherited
   credentials.

Hand-authored or offline observations are intentionally ineligible for acceptance. Unit tests may exercise the parser, timeline, diff, and sandbox mechanics, but their receipt always remains `acceptance_eligible=false`.

`canonical_delegation` is a host-owned evaluation-only object, not a production
Assistant restriction. The collector requires the observed `spawn_subagent`
arguments to match its canonical JSON and SHA-256 exactly, including task order,
prompts, descriptions, identities, and concurrency.
