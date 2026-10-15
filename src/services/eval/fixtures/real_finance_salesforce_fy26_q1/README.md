# Real finance acceptance fixture

This fixture tests a complete public-company analysis trajectory rather than a
toy arithmetic prompt. It combines immutable SEC accessions, exact calculations,
period/unit traps, GAAP/non-GAAP reconciliation, skeptical review, parallel
delegation receipts, and three independent live runs.

`scenario.v1.json` is the live `real-agent-scenarios/v1` contract. Its `prompt`
contains only the fixed primary-source facts and output requirements; the 47
deterministic expected assertions remain in the host-side validation section.
The collector sends only `prompt` to the candidate model.

Run the offline integrity gate:

```bash
uv run python scripts/validate_real_finance_eval.py --fixture-only
```

Validate a real three-run receipt:

```bash
uv run python scripts/validate_real_finance_eval.py path/to/receipt.json --output path/to/report.json
```

Collect, independently validate, and seal the real Gateway trajectory:

The release runner fails closed unless `GENERAL_AGENT_COLLECTOR_HMAC_KEY` and
`GENERAL_AGENT_GOLDEN_HMAC_KEY` are both at least 32 bytes and are different.
One externally generated 64-hex `GENERAL_AGENT_SUITE_NONCE` must remain fixed
across collect, validate, and merge. The runtime attestation is accepted only
when `GENERAL_AGENT_EXPECTED_RUNTIME_ATTESTATION_SHA256` independently matches
the SHA-256 of `GENERAL_AGENT_RUNTIME_ATTESTATION`. Per-specialist task prompts
must meet the scenario's high-entropy role contract; generic acknowledgements,
sub-25 ms overlap, failed aggregation, or out-of-order synthesis fail closed.
This is an evaluation-only constraint: `canonical_delegation` fixes the entire
spawn arguments object and canonical-JSON digest, so prompt suffixes or extra
fields cannot satisfy the release receipt.

```bash
uv run python scripts/real_agent_scenario_runner.py collect \
  --scenarios src/services/eval/fixtures/real_finance_salesforce_fy26_q1/scenario.v1.json \
  --output /tmp/finance-observations.json
uv run python scripts/real_agent_scenario_runner.py validate \
  --scenarios src/services/eval/fixtures/real_finance_salesforce_fy26_q1/scenario.v1.json \
  --observations /tmp/finance-observations.json \
  --output /tmp/finance-validation.json
uv run python scripts/real_agent_scenario_runner.py merge \
  --scenarios src/services/eval/fixtures/real_finance_salesforce_fy26_q1/scenario.v1.json \
  --observations /tmp/finance-observations.json \
  --validation /tmp/finance-validation.json \
  --output /tmp/finance-merged-receipts.json
```

Optionally re-fetch the SEC documents and verify the pinned byte hashes:

```bash
uv run python scripts/validate_real_finance_eval.py --fixture-only --verify-source-bytes
```

The source-byte check requires network access and sends an identifying SEC user
agent. Normal unit tests remain offline. The deterministic gate is not the LLM
judge: final live acceptance is `min(deterministic_score, judge_score)` for each
run, and the case score is the minimum of all three runs. Every run must reach
an unrounded 92.000. `llm_judge_prompt.md` defines semantic weights and explicit
maximum scores for accepting the misleading annualization, misclassifying
non-GAAP, ignoring working-capital seasonality, using unapproved sources,
missing critical metrics, or faking/sequentializing delegation.
