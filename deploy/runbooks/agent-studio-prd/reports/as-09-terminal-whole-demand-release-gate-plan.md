# AS-09 Terminal Whole-Demand Release Gate Plan

## Scope

Execute the AS-08-approved, versioned 39-gate aggregate once from one stable source snapshot. AS-09 is evidence-only: application source, migrations, tests, frontend/deployment configuration and the aggregate manifest remain unchanged. Any product failure returns to its owning Phase and requires a fresh aggregate.

## Preconditions

1. Confirm AS-F001 through AS-F009 are `passing` and each Phase has Actor plus independent Critic evidence.
2. Confirm `tests/fixtures/agent-studio/regression_manifest.json` still hashes to `6630592d04cf04b60e1dba1f42068fdfd3bd19a049c36dfff0ad3aafa057dc1d` and contains 39 required gates.
3. Verify every running `ai-gateway-*` container is owned by `/Users/yang/projects/AI--Platfform`, all eight Compose services are healthy, and migrations 071 through 081 have no pending successor.
4. Use a disposable local E2E account and provider-free Assistant Stub for the aggregate so credential-dependent isolation tests execute with zero skips. Do not read, print, change or invent provider API keys.

## Execution

1. Record the preflight ownership, health, migration, manifest and runtime-mode receipt.
2. Recreate only Gateway and Assistant with `ASSISTANT_E2E_STUB_LLM=true`, preserving OpenAI as unset, then synchronize the current full local Python source trees into those versioned development containers and wait for health.
3. Create a disposable account with the repository helper, load its generated values without printing them, and run `make verify-agent-studio` once.
4. Accept the run only if all 39 required gates ran and passed with zero skips, the runner records `source_stable=true`, and its manifest hash equals the approved AS-08 hash.
5. Restore Gateway and Assistant to `ASSISTANT_E2E_STUB_LLM=false`, resynchronize the same source trees, and verify both health and runtime mode.
6. Run the required Harness structure command separately and report its metadata-only meaning precisely.

## Evidence and Decision

1. Map the aggregate result to AS-F001 through AS-F010, including browser viewports/accessibility, built Hosted/Embed headers, migration/rollback, security/privacy, governance and built-in Assistant compatibility.
2. Write `reports/agent-studio/as-09-whole-demand-matrix.md`, `as-09-build-and-manifest.json`, `as-09-release-decision.md` and the Phase Actor report.
3. Issue `ready-but-not-deployed` only if every non-waivable local gate passes. Keep provider-backed production quality, production Secret/OAuth/egress configuration, monitoring credentials/window and deployment authorization explicit residual readiness items.
4. Request a fresh independent terminal release Critic. The Actor does not update AS-F010 or run the completion gate.
5. After Critic approval, the orchestrator updates Oracle/runtime artifacts and runs the post-Critic strict completion claim check.

## Stop Conditions

- Stop and route to the owning Phase if any gate fails, skips, mutates source, or cannot prove the same source snapshot.
- Stop if Compose ownership differs, migration/rollback becomes destructive, or a provider/deployment action would be required.
- Do not edit source/tests or weaken the manifest inside AS-09.
