# Phase 00 - Freeze full-product compatibility baseline and rollback bundle

- PHASE_ID: FRC-00
- FEATURE_ID: FRC-F001
- DEPENDS_ON: none

## Outcome

One immutable baseline describes and exercises every Python-era public behavior, tool contract, and rollback artifact before replacement begins.

## Scope

In:

- Public OpenAPI/SSE snapshots, tool catalog/schema/effect inventory, AgentSpec/system-prompt hashes, database schema, image digests, Compose resolution, real provider transcripts, browser journeys, and Office semantic/visual goldens.
- A rollback manifest pinned to Git `4968068`, its Compose file, schema revision, and deployed image digests.

Out:

- Rust worker implementation or deletion of any Python Assistant code.
- Production deployment or destructive Docker cleanup.

## Done when

- [ ] Docker ownership is this checkout; `make validate` and `make status` pass with all required services healthy.
- [ ] Assistant, Studio, Hosted/Public, Knowledge, Office, Artifacts, approvals, stop/resume, multi-Agent, Local Node, MCP/Connector, and Agent Eval baseline cases have no skipped or infrastructure-error result.
- [ ] Language-neutral API/tool fixtures and Office semantic/visual goldens are stored under existing fixture/evidence conventions.
- [ ] The rollback manifest contains no secrets and resolves every Git/schema/image identity exactly.
- [ ] Existing affected behavior still passes its smallest relevant regression check.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Machine preflight | `./deploy/runbooks/agent-runtime-full-rust-cutover/init.sh` | Toolchain, ownership, ports, and memory are fit for the baseline. |
| Runtime | `make validate && make status` | Repository-owned Docker services and dependencies are healthy. |
| Agent contracts | `make verify-assistant-runtime-dev && make agent-eval-core-gate` | Existing deterministic Runtime and Eval baselines pass unchanged. |
| Product evidence | Authenticated browser and live-provider baseline matrix | Every named user-visible capability works before replacement. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Do not unlock FRC-01 while Docker/provider/Local Node/Office evidence is skipped; diagnose or record a user-authorized waiver instead.
