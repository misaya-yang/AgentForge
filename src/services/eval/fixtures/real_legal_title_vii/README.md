# Real legal-analysis acceptance fixture

This fixture evaluates an end-to-end federal Title VII memorandum rather than
a recall question. It combines a fixed legal cutoff, official primary sources,
a vacated lower-court rule, distinct discrimination and retaliation standards,
employer coverage, a FEPA deadline, proof limitations, a non-Title-VII hidden
counterexample, adversarial notes, parallel delegation, and a required
read-only community plugin.

The Gateway collector sends only the `prompt` from `scenarios.v1.json` to the
candidate. It does not send `expected_assertions`. The source excerpts and
citations in the prompt are public inputs; the expected outcome matrix remains
host-side. `source_packet.md` is the scenario-owned canonical source artifact.
Its path cannot escape this directory, symlinks are rejected, and its exact
bytes are bound by the SHA-256 declared in `source_artifacts`.

Offline contract validation:

```bash
uv run python - <<'PY'
from pathlib import Path
from scripts.real_agent_scenario_runner import load_scenarios, verify_source_artifacts

path = Path("src/services/eval/fixtures/real_legal_title_vii/scenarios.v1.json")
suite = load_scenarios(path)
receipts = verify_source_artifacts(suite, scenario_directory=path.parent)
print(suite["suite_id"], receipts)
PY
```

Collect three real-provider trials from the running Gateway:

```bash
uv run python scripts/real_agent_scenario_runner.py collect \
  --scenarios src/services/eval/fixtures/real_legal_title_vii/scenarios.v1.json \
  --output /tmp/legal-title-vii-observations.json
```

The Gateway URL and credentials are read only from the documented
`GENERAL_AGENT_GATEWAY_*` / `ASSISTANT_ISOLATION_*` environment variables.
`GENERAL_AGENT_COLLECTOR_HMAC_KEY` and `GENERAL_AGENT_GOLDEN_HMAC_KEY` are
both required, must each contain at least 32 bytes, and must be different.
The operator must also inject one fresh 64-hex `GENERAL_AGENT_SUITE_NONCE` for
the entire collect/validate/merge cycle. `GENERAL_AGENT_RUNTIME_ATTESTATION`
is accepted only when its SHA-256 matches the separately supplied
`GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256`. The scenario's per-agent
task requirements hard-check detailed child prompts, exact identities, at
least 25 ms overlap, successful aggregation, and child-to-parent event order.
For evaluation only, `canonical_delegation` also pins the complete
`spawn_subagent` arguments object and its canonical-JSON SHA-256; a candidate
cannot append an ACK-only override or extra task field while retaining credit.
Validate and seal the hidden goldens independently:

```bash
uv run python scripts/real_agent_scenario_runner.py validate \
  --scenarios src/services/eval/fixtures/real_legal_title_vii/scenarios.v1.json \
  --observations /tmp/legal-title-vii-observations.json \
  --output /tmp/legal-title-vii-validation.json

uv run python scripts/real_agent_scenario_runner.py merge \
  --scenarios src/services/eval/fixtures/real_legal_title_vii/scenarios.v1.json \
  --observations /tmp/legal-title-vii-observations.json \
  --validation /tmp/legal-title-vii-validation.json \
  --output /tmp/legal-title-vii-merged.json
```

Every deterministic assertion is mandatory in every run. The external judge
prompt is in `llm_judge_prompt.md`; it may only reduce the host result. Final
acceptance is the minimum unrounded score across all three trials, with
`92.000` required and no invalid execution receipt.

Official sources used by the pinned packet:

- GPO 2023 United States Code: 42 U.S.C. §§ 2000e(b), 2000e-2(a)(1),
  2000e-3(a), and 2000e-5(e)(1);
- Supreme Court: *Walters*, 519 U.S. 202 (1997); *Burlington Northern*,
  548 U.S. 53 (2006); and *Muldrow*, No. 22-193 (Apr. 17, 2024);
- Eighth Circuit: the vacated 2022 *Muldrow* opinion, included only as the stale
  conflicting source;
- EEOC: official charge-filing guidance.
